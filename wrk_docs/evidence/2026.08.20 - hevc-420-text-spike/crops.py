#!/usr/bin/env python3
"""Side-by-side crops: source / HEVC 4:4:4 / HEVC 4:2:0 / 4:2:0-with-lossless-luma.

Each crop is emitted twice: magnified nearest-neighbour (to show pixel structure)
and at 1:1 (to judge it at the size a user actually sees).
"""
import os, sys
from PIL import Image, ImageDraw, ImageFont

S = os.environ["TMPDIR"] + "/hevc420spike"
OUT = sys.argv[1]
os.makedirs(OUT, exist_ok=True)
LBL = ImageFont.truetype("/System/Library/Fonts/Supplemental/Arial Bold.ttf", 18)
LBLS = ImageFont.truetype("/System/Library/Fonts/Supplemental/Arial.ttf", 15)

SYNTH = ("card", Image.open(f"{S}/src-testcard.png").convert("RGB"))
REAL = ("real2", Image.open("~/language/mdrdp/baseline/freerdp-reference/ref2-full.png").convert("RGB"))

CROPS = {
    # name: (source, x, y, w, h, zoom)
    "01-cleartype-100pct":       (SYNTH, 2584, 1486, 620, 105, 4),
    "02-cleartype-200pct":       (SYNTH, 3868, 1486, 620, 130, 3),
    "03-code-dark-2x":           (SYNTH, 130, 78, 700, 210, 2),
    "04-code-dark-1x":           (SYNTH, 56, 1478, 620, 130, 4),
    "05-spreadsheet-1x-grid":    (SYNTH, 0, 2192, 700, 200, 3),
    "06-spreadsheet-2x-cells":   (SYNTH, 3640, 300, 700, 220, 2),
    "07-saturated-on-black":     (SYNTH, 2576, 1750, 700, 200, 3),
    "08-saturated-on-white":     (SYNTH, 2576, 2190, 700, 200, 3),
    "09-thin-coloured-lines":    (SYNTH, 2576, 2556, 700, 92, 3),
    "10-equiluminant-target":    (SYNTH, 2592, 2698, 700, 78, 3),
    "11-ansi-terminal":          (SYNTH, 2576, 2812, 700, 62, 4),
    "12-coloured-body-text-1x":  (SYNTH, 12, 2612, 700, 140, 4),
    "13-REAL-windows-cleartype": (REAL, 2250, 1495, 640, 45, 5),
    "14-REAL-windows-buttons":   (REAL, 2420, 1400, 640, 160, 3),
}

def panel(src, name, box, zoom, prefix):
    x, y, w, h = box
    rows = [("source (uncompressed)", src[1])]
    for label, tag in [("HEVC 4:4:4", "yuv444p-20M"),
                       ("HEVC 4:2:0", "yuv420p-20M"),
                       ("HEVC 4:2:0, LOSSLESS luma (subsampling loss only)", "yuv420p-lossless")]:
        p = f"{S}/{prefix}dec-{tag}.png"
        b = os.path.getsize(f"{S}/{prefix}-{tag}.265")
        rows.append((f"{label}   ({b//1024} KB IDR)" if "20M" in tag else label,
                     Image.open(p).convert("RGB")))
    tiles = [(lab, img.crop((x, y, x + w, y + h))) for lab, img in rows]
    for tag, z, suffix in [("mag", zoom, ""), ("1to1", 1, "-1to1")]:
        tw, th = w * z, h * z
        pad, hdr = 12, 26
        out = Image.new("RGB", (tw + 2 * pad, (th + hdr) * len(tiles) + pad + 16), (245, 245, 245))
        d = ImageDraw.Draw(out)
        yy = pad
        for lab, c in tiles:
            d.text((pad, yy), lab, font=LBL, fill=(0, 0, 0))
            out.paste(c.resize((tw, th), Image.NEAREST) if z != 1 else c, (pad, yy + hdr - 4))
            yy += th + hdr
        d.text((pad, out.height - 20),
               f"crop ({x},{y}) {w}x{h} of 5120x2880" + (f", magnified {z}x nearest-neighbour" if z != 1 else ", 1:1"),
               font=LBLS, fill=(90, 90, 90))
        out.save(f"{OUT}/{name}{suffix}.png")

for name, (src, x, y, w, h, z) in CROPS.items():
    panel(src, name, (x, y, w, h), z, src[0])
    print(name)
