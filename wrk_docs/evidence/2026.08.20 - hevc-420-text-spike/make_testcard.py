#!/usr/bin/env python3
"""Synthesise a 5120x2880 text-heavy 'Windows desk' test card for the HEVC 4:2:0 spike.

Four quadrants:
  A (top-left)      dark-theme code editor, 2x HiDPI sizes
  B (top-right)     light spreadsheet, 2x HiDPI sizes
  C (bottom-left)   the same two at 1x (100% scaling) -- the no-HiDPI worst case
  D (bottom-right)  chroma stress: ClearType, saturated text, thin lines, terminal,
                    equiluminant chroma resolution targets

ClearType is simulated the way Windows does it: glyph coverage is sampled at 3x
horizontal resolution, run through the classic 5-tap [1,2,3,2,1]/9 subpixel FIR, and
the triplets become the R,G,B coverages of one output pixel.
"""
import sys
import numpy as np
from PIL import Image, ImageDraw, ImageFont

W, H = 5120, 2880
SUP = "/System/Library/Fonts/Supplemental/"
SYS = "/System/Library/Fonts/"

def f(path, size):
    return ImageFont.truetype(path, size)

TAHOMA = SUP + "Tahoma.ttf"
TAHOMA_B = SUP + "Tahoma Bold.ttf"
ARIAL = SUP + "Arial.ttf"
ARIAL_B = SUP + "Arial Bold.ttf"
MENLO = SYS + "Menlo.ttc"
COURIER = SUP + "Courier New.ttf"

canvas = Image.new("RGB", (W, H), (30, 30, 30))

# ---------------------------------------------------------------- ClearType
_FIR = np.array([1, 2, 3, 2, 1], dtype=np.float64) / 9.0

def cleartype_block(size, lines, font_path, px, fg, bg, line_gap=0, x0=0, y0=0):
    """Return an RGB Image of `size` with `lines` drawn using simulated ClearType."""
    w, h = size
    # 3x oversample in both axes, then box-downsample y only -> 3x horizontal coverage
    m = Image.new("L", (w * 3, h * 3), 0)
    d = ImageDraw.Draw(m)
    fnt = ImageFont.truetype(font_path, px * 3)
    y = y0 * 3
    for ln in lines:
        d.text((x0 * 3, y), ln, font=fnt, fill=255)
        y += (px + line_gap) * 3
    m = m.resize((w * 3, h), Image.BOX)
    cov = np.asarray(m, dtype=np.float64) / 255.0            # (h, 3w)
    # 5-tap FIR across the subpixel stream
    pad = np.pad(cov, ((0, 0), (2, 2)), mode="edge")
    filt = np.zeros_like(cov)
    for i, k in enumerate(_FIR):
        filt += k * pad[:, i:i + cov.shape[1]]
    rgbcov = filt.reshape(h, w, 3)
    fg = np.array(fg, dtype=np.float64)
    bg = np.array(bg, dtype=np.float64)
    out = bg[None, None, :] * (1 - rgbcov) + fg[None, None, :] * rgbcov
    return Image.fromarray(np.clip(out + 0.5, 0, 255).astype(np.uint8), "RGB")

# ---------------------------------------------------------------- quadrant A
# dark code editor, 2x HiDPI (Menlo 28px device = 14px logical)
A = Image.new("RGB", (2560, 1440), (30, 30, 30))
da = ImageDraw.Draw(A)
mono = f(MENLO, 28)
mono_b = f(MENLO, 28)
ui = f(TAHOMA, 24)

DARK = dict(kw=(86, 156, 214), s=(206, 145, 120), c=(106, 153, 85), fn=(220, 220, 170),
            num=(181, 206, 168), ty=(78, 201, 176), op=(212, 212, 212),
            var=(156, 220, 254), plain=(212, 212, 212))

code = [
    [("c", "// rhydra: encode one full 5K repaint as two half-width tiles")],
    [("kw", "pub fn "), ("fn", "encode_frame"), ("op", "("), ("var", "&mut self"), ("op", ", "),
     ("var", "src"), ("op", ": &"), ("ty", "Texture2D"), ("op", ") -> "), ("ty", "Result"),
     ("op", "<"), ("ty", "Vec"), ("op", "<"), ("ty", "u8"), ("op", ">> {")],
    [("op", "    "), ("kw", "let "), ("var", "tiles"), ("op", " = "), ("var", "self"),
     ("op", "."), ("fn", "split"), ("op", "("), ("var", "src"), ("op", ", "), ("num", "2560"),
     ("op", ", "), ("num", "2880"), ("op", ");")],
    [("op", "    "), ("kw", "let mut "), ("var", "out"), ("op", " = "), ("ty", "Vec"),
     ("op", "::"), ("fn", "with_capacity"), ("op", "("), ("num", "1 << 20"), ("op", ");")],
    [("op", "    "), ("kw", "for "), ("op", "("), ("var", "i"), ("op", ", "), ("var", "t"),
     ("op", ") "), ("kw", "in "), ("var", "tiles"), ("op", "."), ("fn", "iter"),
     ("op", "()."), ("fn", "enumerate"), ("op", "() {")],
    [("op", "        "), ("var", "self"), ("op", "."), ("var", "engines"), ("op", "["),
     ("var", "i"), ("op", "]."), ("fn", "submit"), ("op", "("), ("var", "t"), ("op", ")?;")],
    [("op", "    }")],
    [("op", "    "), ("c", "// SPS must be parsed back: Intel accepts 4:4:4 and emits 4:2:0")],
    [("op", "    "), ("kw", "let "), ("var", "sps"), ("op", " = "), ("fn", "parse_sps"),
     ("op", "("), ("op", "&"), ("var", "out"), ("op", ")?;")],
    [("op", "    "), ("fn", "assert_eq!"), ("op", "("), ("var", "sps"), ("op", "."),
     ("var", "chroma_format_idc"), ("op", ", "), ("num", "1"), ("op", ", "),
     ("s", "\"expected 4:2:0\""), ("op", ");")],
    [("op", "    "), ("ty", "Ok"), ("op", "("), ("var", "out"), ("op", ")")],
    [("op", "}")],
    [],
    [("c", "/// Chroma stays at half rate in both axes. At 2x HiDPI that is still")],
    [("c", "/// ~110 ppi of colour -- the open question this spike answers.")],
    [("kw", "const "), ("var", "CHROMA_SHIFT"), ("op", ": "), ("ty", "u32"), ("op", " = "),
     ("num", "1"), ("op", ";")],
    [("kw", "static "), ("var", "PROFILES"), ("op", ": &["), ("op", "&"), ("ty", "str"),
     ("op", "] = ["), ("s", "\"Main\""), ("op", ", "), ("s", "\"Main10\""), ("op", ", "),
     ("s", "\"Main444_8\""), ("op", "];")],
    [],
    [("kw", "impl "), ("ty", "Drop"), ("kw", " for "), ("ty", "TileEncoder"), ("op", " {")],
    [("op", "    "), ("kw", "fn "), ("fn", "drop"), ("op", "(&"), ("kw", "mut "),
     ("var", "self"), ("op", ") {")],
    [("op", "        "), ("var", "self"), ("op", "."), ("var", "engines"), ("op", "."),
     ("fn", "clear"), ("op", "();")],
    [("op", "    }")],
    [("op", "}")],
]

y = 90
for n, ln in enumerate(code, start=1):
    da.text((36, y), f"{n:3d}", font=mono, fill=(133, 133, 133))
    x = 130
    for kind, txt in ln:
        da.text((x, y), txt, font=mono, fill=DARK[kind])
        x += da.textlength(txt, font=mono)
    y += 40

# selection highlight + red squiggle, both common real-UI colour-on-dark cases
da.rectangle([130, 90 + 40 * 8, 130 + 720, 90 + 40 * 9 - 4], outline=(38, 79, 120), width=2)
sq_y = 90 + 40 * 9 + 30
for i in range(0, 560, 4):
    da.line([130 + i, sq_y, 130 + i + 2, sq_y + 3], fill=(244, 71, 71), width=2)
    da.line([130 + i + 2, sq_y + 3, 130 + i + 4, sq_y], fill=(244, 71, 71), width=2)

# tab strip + status bar (blue-on-dark chrome)
da.rectangle([0, 0, 2559, 60], fill=(37, 37, 38))
da.rectangle([0, 0, 300, 60], fill=(30, 30, 30))
da.text((20, 16), "encoder.rs", font=ui, fill=(255, 255, 255))
da.text((330, 16), "sps.rs", font=ui, fill=(150, 150, 150))
da.text((470, 16), "tiles.rs", font=ui, fill=(150, 150, 150))
da.rectangle([0, 1400, 2559, 1439], fill=(0, 122, 204))
da.text((20, 1408), "main*   Ln 9, Col 24   Spaces: 4   UTF-8   Rust", font=ui, fill=(255, 255, 255))
canvas.paste(A, (0, 0))

# ---------------------------------------------------------------- quadrant B
# light spreadsheet, 2x HiDPI
B = Image.new("RGB", (2560, 1440), (255, 255, 255))
db = ImageDraw.Draw(B)
sheet_f = f(TAHOMA, 24)
sheet_b = f(TAHOMA_B, 24)
small = f(TAHOMA, 22)

db.rectangle([0, 0, 2559, 70], fill=(33, 115, 70))          # Excel green ribbon
db.text((24, 20), "Book1  -  Excel", font=f(TAHOMA_B, 26), fill=(255, 255, 255))
db.rectangle([0, 70, 2559, 130], fill=(255, 255, 255))
db.rectangle([16, 82, 300, 122], outline=(190, 190, 190), width=2)
db.text((28, 90), "D14", font=small, fill=(60, 60, 60))
db.text((330, 90), "fx", font=f(ARIAL, 22), fill=(120, 120, 120))
db.text((390, 90), "=SUMIFS($E$4:$E$40,$B$4:$B$40,\"HEVC\")", font=small, fill=(0, 0, 0))

COLW = [160, 300, 220, 220, 220, 220, 220, 220, 260]
ROWH = 46
gx0, gy0 = 0, 140

# column headers
x = gx0
db.rectangle([0, gy0, 2559, gy0 + 40], fill=(243, 243, 243))
for i, cw in enumerate(COLW):
    db.line([x, gy0, x, gy0 + 40], fill=(190, 190, 190), width=2)
    db.text((x + cw // 2 - 8, gy0 + 8), chr(ord("A") + i), font=small, fill=(70, 70, 70))
    x += cw
db.line([gx0, gy0 + 40, 2559, gy0 + 40], fill=(190, 190, 190), width=2)

rows = [
    ["1", "Codec", "Chroma", "Patch", "p50 ms", "p95 ms", "Bytes", "Delta", "Note"],
    ["2", "HEVC", "4:2:0", "5120x2880", "12.9", "13.3", "412,880", "-3.2%", "single stream"],
    ["3", "HEVC", "4:2:0", "2560x2880", "6.7", "7.4", "208,112", "-48.1%", "half tile"],
    ["4", "HEVC", "4:2:0", "2560x1440", "3.7", "4.8", "104,400", "-71.3%", "quarter"],
    ["5", "HEVC", "4:2:0", "1920x1080", "2.2", "2.3", "58,904", "-82.9%", "ok"],
    ["6", "H.264", "4:2:0", "5120x2880", "-", "-", "0", "n/a", "refused"],
    ["7", "H.264", "4:2:0", "2560x2880", "11.6", "11.8", "301,776", "+73.1%", "slower"],
    ["8", "H.264", "4:2:0", "1280x720", "1.07", "1.82", "18,220", "-16.4%", "tiny win"],
    ["9", "VP9", "4:2:0", "1920x1080", "-", "-", "0", "n/a", "profile ignored"],
    ["10", "AV1", "4:2:0", "1920x1080", "-", "-", "0", "n/a", "no MFT"],
    ["11", "HEVC", "4:4:4", "1920x1080", "9.4", "11.2", "744,208", "+1163%", "software only"],
    ["12", "HEVC", "4:4:4", "5120x2880", "-", "-", "0", "n/a", "D3D12 only"],
    ["13", "AVC444", "4:4:4", "4096x2304", "18.1", "24.7", "690,004", "+67.1%", "two streams"],
    ["14", "Total", "", "", "65.7", "77.3", "2,538,504", "", ""],
]

y = gy0 + 40
for r, row in enumerate(rows):
    header = (r == 0)
    total = (row[1] == "Total")
    fill = None
    if header:
        fill = (217, 226, 243)
    elif total:
        fill = (226, 239, 218)
    elif r % 2 == 0:
        fill = (247, 247, 247)
    if fill:
        db.rectangle([gx0, y, 2559, y + ROWH], fill=fill)
    x = gx0
    for c, cell in enumerate(row):
        col = (0, 0, 0)
        fnt = sheet_b if (header or total) else sheet_f
        if c == 0:
            col = (70, 70, 70)
            db.rectangle([x, y, x + COLW[0], y + ROWH], fill=(243, 243, 243))
        elif c == 7 and cell.startswith("-"):
            col = (255, 0, 0)                                    # red negatives
        elif c == 7 and cell.startswith("+"):
            col = (0, 128, 0)
        elif c == 8 and cell in ("refused", "no MFT", "profile ignored"):
            db.rectangle([x + 2, y + 2, x + COLW[c] - 2, y + ROWH - 2], fill=(255, 199, 206))
            col = (156, 0, 6)                                    # Excel "bad" format
        elif c == 8 and cell in ("ok", "tiny win"):
            db.rectangle([x + 2, y + 2, x + COLW[c] - 2, y + ROWH - 2], fill=(198, 239, 206))
            col = (0, 97, 0)                                     # Excel "good" format
        elif c == 8 and cell in ("slower", "software only"):
            db.rectangle([x + 2, y + 2, x + COLW[c] - 2, y + ROWH - 2], fill=(255, 235, 156))
            col = (156, 101, 0)                                  # Excel "neutral" format
        if c in (4, 5, 6, 7) and not header:
            tw = db.textlength(cell, font=fnt)
            db.text((x + COLW[c] - tw - 12, y + 10), cell, font=fnt, fill=col)
        else:
            db.text((x + 12, y + 10), cell, font=fnt, fill=col)
        x += COLW[c]
    y += ROWH

# 2-device-px gridlines (what Excel draws at 200% scaling)
x = gx0
for cw in COLW:
    db.line([x, gy0, x, y], fill=(212, 212, 212), width=2)
    x += cw
db.line([x, gy0, x, y], fill=(212, 212, 212), width=2)
yy = gy0 + 40
for _ in rows:
    db.line([gx0, yy, x, yy], fill=(212, 212, 212), width=2)
    yy += ROWH
db.line([gx0, yy, x, yy], fill=(212, 212, 212), width=2)

# hyperlink + a small chart-ish legend below the grid
db.text((24, y + 40), "See wrk_docs/2026.08.20 - DEC - rhydra codec choice.md",
        font=sheet_f, fill=(5, 99, 193))
db.line([24, y + 40 + 30, 24 + db.textlength("See wrk_docs/2026.08.20 - DEC - rhydra codec choice.md",
        font=sheet_f), y + 40 + 30], fill=(5, 99, 193), width=2)
lx = 24
for label, col in [("HEVC", (68, 114, 196)), ("H.264", (237, 125, 49)),
                   ("VP9", (165, 165, 165)), ("AV1", (255, 192, 0)),
                   ("AVC444", (91, 155, 213))]:
    db.rectangle([lx, y + 100, lx + 28, y + 128], fill=col)
    db.text((lx + 40, y + 100), label, font=small, fill=(0, 0, 0))
    lx += 200
canvas.paste(B, (2560, 0))

# ---------------------------------------------------------------- quadrant C
# 100% scaling worst case: 1x fonts, 1px gridlines
C = Image.new("RGB", (2560, 1440), (255, 255, 255))
dc = ImageDraw.Draw(C)
dc.rectangle([0, 0, 2559, 719], fill=(30, 30, 30))
mono1 = f(MENLO, 14)
ui1 = f(TAHOMA, 12)
dc.text((20, 8), "100% SCALING (1x)  --  the no-HiDPI worst case", font=f(TAHOMA_B, 16),
        fill=(255, 255, 255))
y = 40
for n, ln in enumerate(code, start=1):
    dc.text((12, y), f"{n:3d}", font=mono1, fill=(133, 133, 133))
    x = 56
    for kind, txt in ln:
        dc.text((x, y), txt, font=mono1, fill=DARK[kind])
        x += dc.textlength(txt, font=mono1)
    y += 20
    if y > 700:
        break

# 1x spreadsheet with 1px gridlines
CW1 = [80, 150, 110, 110, 110, 110, 110, 110, 130]
RH1 = 23
gy = 760
dc.text((12, 730), "1x spreadsheet, 1-pixel gridlines", font=f(TAHOMA_B, 16), fill=(0, 0, 0))
yy = gy
for r, row in enumerate(rows):
    header = (r == 0)
    if header:
        dc.rectangle([0, yy, sum(CW1), yy + RH1], fill=(217, 226, 243))
    elif r % 2 == 0:
        dc.rectangle([0, yy, sum(CW1), yy + RH1], fill=(247, 247, 247))
    x = 0
    for c, cell in enumerate(row):
        col = (0, 0, 0)
        if c == 7 and cell.startswith("-"):
            col = (255, 0, 0)
        elif c == 7 and cell.startswith("+"):
            col = (0, 128, 0)
        elif c == 8 and cell in ("refused", "no MFT", "profile ignored"):
            dc.rectangle([x + 1, yy + 1, x + CW1[c] - 1, yy + RH1 - 1], fill=(255, 199, 206))
            col = (156, 0, 6)
        elif c == 8 and cell in ("ok", "tiny win"):
            dc.rectangle([x + 1, yy + 1, x + CW1[c] - 1, yy + RH1 - 1], fill=(198, 239, 206))
            col = (0, 97, 0)
        dc.text((x + 5, yy + 5), cell, font=ui1, fill=col)
        x += CW1[c]
    yy += RH1
x = 0
for cw in CW1:
    dc.line([x, gy, x, yy], fill=(212, 212, 212), width=1)
    x += cw
dc.line([x, gy, x, yy], fill=(212, 212, 212), width=1)
t = gy
for _ in range(len(rows) + 1):
    dc.line([0, t, x, t], fill=(212, 212, 212), width=1)
    t += RH1

# 1x coloured body text on white -- the classic "small coloured glyph" case
para = ("The quick brown fox jumps over the lazy dog. 0123456789 (){}[]<>#@$%&*"
        " -- l1I0O rn m cl d")
yy = gy + RH1 * (len(rows) + 2) + 20
for label, col in [("black", (0, 0, 0)), ("red", (200, 0, 0)), ("blue", (0, 0, 200)),
                   ("green", (0, 120, 0)), ("magenta", (170, 0, 170)),
                   ("teal", (0, 128, 128)), ("grey", (110, 110, 110))]:
    dc.text((12, yy), f"{label:9s} {para}", font=ui1, fill=col)
    yy += 18
canvas.paste(C, (0, 1440))

# ---------------------------------------------------------------- quadrant D
D = Image.new("RGB", (2560, 1440), (255, 255, 255))
dd = ImageDraw.Draw(D)
hdr = f(TAHOMA_B, 22)

# D1/D2: ClearType-simulated text, 1x and 2x, black and coloured
# D1: 100% scaling -- 13 DEVICE px, 1-pixel stems, the densest possible fringe
ct_lines_small = [
    "The quick brown fox jumps over the lazy dog, 0123456789.",
    "Subpixel fringes put real colour at the 1-pixel scale. Illegal Iliad, mm nn rn.",
    "Cell A1  =SUMIFS($E$4:$E$40)   -3.2%   412,880   half tile   refused",
    "The quick brown fox jumps over the lazy dog, 0123456789.",
    "Subpixel fringes put real colour at the 1-pixel scale. Illegal Iliad, mm nn rn.",
]
blk = cleartype_block((1240, 120), ct_lines_small, TAHOMA, 13, (0, 0, 0), (255, 255, 255),
                      line_gap=8, x0=8, y0=6)
D.paste(blk, (16, 40))
dd.text((16, 8), "D1  ClearType (simulated, 5-tap FIR) -- 13px device = 100% scaling",
        font=hdr, fill=(0, 0, 0))

# D2: 200% HiDPI -- the same 13px logical text at 26 device px
ct_lines_big = [
    "The quick brown fox jumps over the lazy dog, 0123456789.",
    "Subpixel fringes put real colour at the 1-pixel scale.",
    "Cell A1  =SUMIFS($E$4:$E$40)   -3.2%   412,880",
]
blk = cleartype_block((1240, 140), ct_lines_big, TAHOMA, 26, (0, 0, 0), (255, 255, 255),
                      line_gap=12, x0=8, y0=6)
D.paste(blk, (1300, 40))
dd.text((1300, 8), "D2  ClearType -- 26px device = 13px logical at 200% HiDPI",
        font=hdr, fill=(0, 0, 0))

# grayscale-AA controls right beside them for an A/B of the fringes themselves
dd.text((16, 176), "grayscale AA control (same text, no subpixel colour):", font=f(TAHOMA, 26),
        fill=(0, 0, 0))
dd.text((16, 212), "The quick brown fox jumps over the lazy dog, 0123456789.",
        font=f(TAHOMA, 26), fill=(0, 0, 0))

# D3: saturated text on black and on white
dd.rectangle([0, 270, 2559, 700], fill=(0, 0, 0))
dd.text((16, 278), "D3  saturated text on black (luma-poor: blue Y=0.07, red Y=0.21)",
        font=hdr, fill=(255, 255, 255))
sat = [("pure red   #FF0000", (255, 0, 0)), ("pure green #00FF00", (0, 255, 0)),
       ("pure blue  #0000FF", (0, 0, 255)), ("cyan       #00FFFF", (0, 255, 255)),
       ("magenta    #FF00FF", (255, 0, 255)), ("yellow     #FFFF00", (255, 255, 0)),
       ("VSCode kw  #569CD6", (86, 156, 214)), ("VSCode str #CE9178", (206, 145, 120))]
yy = 318
mono20 = f(MENLO, 20)
mono40 = f(MENLO, 40)
for label, col in sat:
    dd.text((16, yy), label + "   the quick brown fox 0123456789", font=mono20, fill=col)
    dd.text((1300, yy - 6), label[:11] + " fox 0123", font=mono40, fill=col)
    yy += 46

dd.text((16, 712), "D4  saturated text on white", font=hdr, fill=(0, 0, 0))
yy = 748
for label, col in sat:
    dd.text((16, yy), label + "   the quick brown fox 0123456789", font=mono20, fill=col)
    dd.text((1300, yy - 6), label[:11] + " fox 0123", font=mono40, fill=col)
    yy += 46

# D5: thin coloured lines, 1px / 2px / 3px, on white then on black
base = 1120
dd.text((16, base - 30), "D5  thin coloured lines: 1px / 2px / 3px, on white and on black",
        font=hdr, fill=(0, 0, 0))
lc = [(255, 0, 0), (0, 160, 0), (0, 0, 255), (0, 190, 190), (200, 0, 200), (150, 150, 150)]
x = 16
for wdt in (1, 2, 3):
    for col in lc:
        for k in range(6):
            dd.line([x, base, x, base + 90], fill=col, width=wdt)
            x += wdt + 5
        x += 14
    x += 26
dd.rectangle([1300, base - 6, 2559, base + 96], fill=(0, 0, 0))
x = 1316
for wdt in (1, 2, 3):
    for col in lc:
        for k in range(6):
            dd.line([x, base, x, base + 90], fill=col, width=wdt)
            x += wdt + 5
        x += 14
    x += 26

# D6: equiluminant chroma resolution target -- pure chroma detail, zero luma detail
def equiluma_pair():
    # BT.709 luma of pure red
    yr = 0.2126 * 255
    # find a blue-green with the same luma: (0, g, 210)
    g = (yr - 0.0722 * 210) / 0.7152
    return (255, 0, 0), (0, int(round(g)), 210)

ca, cb = equiluma_pair()
base2 = 1260
dd.rectangle([0, base2 - 34, 2559, 1439], fill=(255, 255, 255))
dd.text((16, base2 - 30),
        f"D6  equiluminant chroma target  {ca} vs {cb}  (same BT.709 luma; all detail is chroma)",
        font=hdr, fill=(0, 0, 0))
x = 16
for period in (1, 2, 4, 8, 16):
    for k in range(int(360 / (period * 2))):
        dd.rectangle([x, base2, x + period - 1, base2 + 70], fill=ca)
        dd.rectangle([x + period, base2, x + 2 * period - 1, base2 + 70], fill=cb)
        x += 2 * period
    dd.text((x - 340, base2 + 76), f"{period}px columns", font=f(TAHOMA, 20), fill=(0, 0, 0))
    x += 60

# terminal strip: ANSI 16 colours on black
dd.rectangle([0, 1370, 2559, 1439], fill=(12, 12, 12))
ansi = [(12, 12, 12), (197, 15, 31), (19, 161, 14), (193, 156, 0), (0, 55, 218),
        (136, 23, 152), (58, 150, 221), (204, 204, 204), (118, 118, 118), (231, 72, 86),
        (22, 198, 12), (249, 241, 165), (59, 120, 255), (180, 0, 158), (97, 214, 214),
        (242, 242, 242)]
x = 16
mono18 = f(MENLO, 18)
for i, col in enumerate(ansi):
    dd.text((x, 1382), f"ansi{i:02d} $ ls -la", font=mono18, fill=col)
    x += 158
canvas.paste(D, (2560, 1440))

canvas.save(sys.argv[1], "PNG", compress_level=6)
print("wrote", sys.argv[1], canvas.size)
