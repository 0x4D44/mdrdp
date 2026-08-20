#!/usr/bin/env python3
"""Per-region RGB / CIEDE2000 analysis of the 4:2:0 vs 4:4:4 decodes."""
import os, sys
import numpy as np
from PIL import Image

S = os.environ["TMPDIR"] + "/hevc420spike"

REGIONS = {
    "A  code editor, dark, 2x":      (0, 60, 2560, 1400),
    "B  spreadsheet, light, 2x":     (2560, 140, 5120, 900),
    "C  code editor, dark, 1x":      (0, 1480, 2560, 2160),
    "C  spreadsheet, light, 1x":     (0, 2195, 2560, 2560),
    "C  coloured body text, 1x":     (0, 2610, 2560, 2760),
    "D1 ClearType 1x (13px)":        (2576, 1480, 3816, 1600),
    "D2 ClearType 2x (26px)":        (3860, 1480, 5100, 1620),
    "D3 saturated text on black":    (2560, 1710, 5120, 2140),
    "D4 saturated text on white":    (2560, 2150, 5120, 2540),
    "D5 thin coloured lines":        (2576, 2555, 5119, 2650),
    "D6 equiluminant chroma target": (2576, 2700, 5119, 2775),
    "D7 ANSI terminal on black":     (2560, 2810, 5119, 2879),
    "-- whole frame --":             (0, 0, 5120, 2880),
}

# ---------------------------------------------------------------- CIE Lab / dE2000
def srgb_to_lab(rgb):
    c = rgb.astype(np.float64) / 255.0
    c = np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)
    M = np.array([[0.4124564, 0.3575761, 0.1804375],
                  [0.2126729, 0.7151522, 0.0721750],
                  [0.0193339, 0.1191920, 0.9503041]])
    xyz = c @ M.T
    wp = np.array([0.95047, 1.00000, 1.08883])
    t = xyz / wp
    d = 6.0 / 29.0
    fx = np.where(t > d ** 3, np.cbrt(t), t / (3 * d * d) + 4.0 / 29.0)
    L = 116 * fx[..., 1] - 16
    a = 500 * (fx[..., 0] - fx[..., 1])
    b = 200 * (fx[..., 1] - fx[..., 2])
    return np.stack([L, a, b], axis=-1)

def de2000(lab1, lab2):
    L1, a1, b1 = lab1[..., 0], lab1[..., 1], lab1[..., 2]
    L2, a2, b2 = lab2[..., 0], lab2[..., 1], lab2[..., 2]
    C1, C2 = np.hypot(a1, b1), np.hypot(a2, b2)
    Cb = (C1 + C2) / 2
    G = 0.5 * (1 - np.sqrt(Cb ** 7 / (Cb ** 7 + 25.0 ** 7 + 1e-30)))
    a1p, a2p = (1 + G) * a1, (1 + G) * a2
    C1p, C2p = np.hypot(a1p, b1), np.hypot(a2p, b2)
    h1p = np.degrees(np.arctan2(b1, a1p)) % 360
    h2p = np.degrees(np.arctan2(b2, a2p)) % 360
    dLp = L2 - L1
    dCp = C2p - C1p
    dhp = h2p - h1p
    dhp = np.where(dhp > 180, dhp - 360, np.where(dhp < -180, dhp + 360, dhp))
    dhp = np.where(C1p * C2p == 0, 0, dhp)
    dHp = 2 * np.sqrt(C1p * C2p) * np.sin(np.radians(dhp) / 2)
    Lbp = (L1 + L2) / 2
    Cbp = (C1p + C2p) / 2
    hsum = h1p + h2p
    hdiff = np.abs(h1p - h2p)
    hbp = np.where(C1p * C2p == 0, hsum,
          np.where(hdiff <= 180, hsum / 2,
          np.where(hsum < 360, (hsum + 360) / 2, (hsum - 360) / 2)))
    T = (1 - 0.17 * np.cos(np.radians(hbp - 30)) + 0.24 * np.cos(np.radians(2 * hbp))
         + 0.32 * np.cos(np.radians(3 * hbp + 6)) - 0.20 * np.cos(np.radians(4 * hbp - 63)))
    dth = 30 * np.exp(-(((hbp - 275) / 25) ** 2))
    Rc = 2 * np.sqrt(Cbp ** 7 / (Cbp ** 7 + 25.0 ** 7 + 1e-30))
    Sl = 1 + (0.015 * (Lbp - 50) ** 2) / np.sqrt(20 + (Lbp - 50) ** 2)
    Sc = 1 + 0.045 * Cbp
    Sh = 1 + 0.015 * Cbp * T
    Rt = -np.sin(np.radians(2 * dth)) * Rc
    return np.sqrt((dLp / Sl) ** 2 + (dCp / Sc) ** 2 + (dHp / Sh) ** 2
                   + Rt * (dCp / Sc) * (dHp / Sh))

# ---------------------------------------------------------------- run
src = np.asarray(Image.open(os.environ.get("SRC", f"{S}/src-testcard.png")).convert("RGB"))
variants = sys.argv[1:] or ["yuv444p-lossless", "yuv420p-lossless",
                            "yuv444p-20M", "yuv420p-20M"]
decs = {v: np.asarray(Image.open(f"{S}/{os.environ.get(chr(80)+chr(82)+chr(69)+chr(70),chr(100)+chr(101)+chr(99))}-{v}.png").convert("RGB")) for v in variants}

hdr = f"{'region':32s}" + "".join(f"{v:>26s}" for v in variants)
print(hdr)
print(f"{'':32s}" + "".join(f"{'dE50/dE95/dE99.9/dEmax':>26s}" for v in variants))
print("-" * len(hdr))
rows = {}
for name, (x0, y0, x1, y1) in REGIONS.items():
    s = src[y0:y1, x0:x1]
    ls = srgb_to_lab(s)
    line = f"{name:32s}"
    rows[name] = {}
    for v in variants:
        d = decs[v][y0:y1, x0:x1]
        e = de2000(ls, srgb_to_lab(d))
        q = np.percentile(e, [50, 95, 99.9])
        rows[name][v] = (q[0], q[1], q[2], e.max())
        line += f"{q[0]:6.2f}{q[1]:6.2f}{q[2]:7.2f}{e.max():7.2f}"
    print(line)

print()
print("percent of pixels above a perceptual threshold  (dE2000 > 2 = 'noticeable side by side';  > 5 = 'obvious')")
hdr = f"{'region':32s}" + "".join(f"{v:>22s}" for v in variants)
print(hdr); print(f"{'':32s}" + "".join(f"{'%>2      %>5':>22s}" for v in variants))
print("-" * len(hdr))
for name, (x0, y0, x1, y1) in REGIONS.items():
    s = srgb_to_lab(src[y0:y1, x0:x1])
    line = f"{name:32s}"
    for v in variants:
        e = de2000(s, srgb_to_lab(decs[v][y0:y1, x0:x1]))
        line += f"{100*(e>2).mean():10.3f}{100*(e>5).mean():12.3f}"
    print(line)
