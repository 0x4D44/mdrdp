# Third-party code: see tools/NOTICE for provenance and licence.
import random

def i16(v):
    return ((v + 32768) & 0xFFFF) - 32768

# ---------------- C: rfx_dwt.c rfx_dwt_2d_decode_block ----------------
def c_block(buffer, idwt, sw):
    tw = sw * 2
    ll_o = sw*sw*3
    hl_o = 0
    lh_o = sw*sw
    hh_o = sw*sw*2
    l_dst_o = 0
    h_dst_o = sw*sw*2

    for y in range(sw):
        idwt[l_dst_o+0] = i16(buffer[ll_o+0] - ((buffer[hl_o+0] + buffer[hl_o+0] + 1) >> 1))
        idwt[h_dst_o+0] = i16(buffer[lh_o+0] - ((buffer[hh_o+0] + buffer[hh_o+0] + 1) >> 1))
        for n in range(1, sw):
            x = n << 1
            idwt[l_dst_o+x] = i16(buffer[ll_o+n] - ((buffer[hl_o+n-1] + buffer[hl_o+n] + 1) >> 1))
            idwt[h_dst_o+x] = i16(buffer[lh_o+n] - ((buffer[hh_o+n-1] + buffer[hh_o+n] + 1) >> 1))
        n = 0
        while n < sw - 1:
            x = n << 1
            ld = (buffer[hl_o+n] << 1) + ((idwt[l_dst_o+x] + idwt[l_dst_o+x+2]) >> 1)
            hd = (buffer[hh_o+n] << 1) + ((idwt[h_dst_o+x] + idwt[h_dst_o+x+2]) >> 1)
            idwt[l_dst_o+x+1] = i16(ld)
            idwt[h_dst_o+x+1] = i16(hd)
            n += 1
        x = n << 1
        ld = (buffer[hl_o+n] << 1) + idwt[l_dst_o+x]
        hd = (buffer[hh_o+n] << 1) + idwt[h_dst_o+x]
        idwt[l_dst_o+x+1] = i16(ld)
        idwt[h_dst_o+x+1] = i16(hd)

        ll_o += sw; hl_o += sw; l_dst_o += tw
        lh_o += sw; hh_o += sw; h_dst_o += tw

    for x in range(tw):
        l = x
        h = x + sw*tw
        dst = x
        buffer[dst] = i16(idwt[l] - ((idwt[h]*2 + 1) >> 1))
        for n in range(1, sw):
            l += tw; h += tw
            buffer[dst + 2*tw] = i16(idwt[l] - ((idwt[h-tw] + idwt[h] + 1) >> 1))
            buffer[dst + tw] = i16((idwt[h-tw] << 1) + ((buffer[dst] + buffer[dst+2*tw]) >> 1))
            dst += 2*tw
        buffer[dst + tw] = i16((idwt[h] << 1) + ((buffer[dst]*2) >> 1))

# ---------------- Rust: dwt.rs decode_block ----------------
def r_block(buffer, temp, sw):
    tw = sw * 2
    ssw = sw*sw
    # inverse_horizontal
    hl = 0; lh = ssw; hh = 2*ssw; ll = 3*ssw
    l_dst = 0; h_dst = 2*ssw
    for _ in range(sw):
        temp[l_dst+0] = i16(buffer[ll+0] - ((buffer[hl+0] + buffer[hl+0] + 1) >> 1))
        temp[h_dst+0] = i16(buffer[lh+0] - ((buffer[hh+0] + buffer[hh+0] + 1) >> 1))
        for n in range(1, sw):
            x = n*2
            temp[l_dst+x] = i16(buffer[ll+n] - ((buffer[hl+n-1] + buffer[hl+n] + 1) >> 1))
            temp[h_dst+x] = i16(buffer[lh+n] - ((buffer[hh+n-1] + buffer[hh+n] + 1) >> 1))
        for n in range(0, sw-1):
            x = n*2
            # NOTE: Rust shifts in i16 -> wraps
            temp[l_dst+x+1] = i16(i16(buffer[hl+n]*2) + ((temp[l_dst+x] + temp[l_dst+x+2]) >> 1))
            temp[h_dst+x+1] = i16(i16(buffer[hh+n]*2) + ((temp[h_dst+x] + temp[h_dst+x+2]) >> 1))
        n = sw-1; x = n*2
        temp[l_dst+x+1] = i16(i16(buffer[hl+n]*2) + temp[l_dst+x])
        temp[h_dst+x+1] = i16(i16(buffer[hh+n]*2) + temp[h_dst+x])
        hl += sw; lh += sw; hh += sw; ll += sw
        l_dst += tw; h_dst += tw

    # inverse_vertical
    tb = 0   # temp_buffer base
    bb = 0   # buffer base
    for _ in range(tw):
        buffer[bb+0] = i16(temp[tb+0] - ((temp[tb + sw*tw]*2 + 1) >> 1))
        l = tb
        lhv = tb + (sw-1)*tw
        h = tb + sw*tw
        dst = bb
        for _ in range(1, sw):
            l += tw; lhv += tw; h += tw
            buffer[dst + 2*tw] = i16(temp[l] - ((temp[lhv] + temp[h] + 1) >> 1))
            buffer[dst + tw] = i16(i16(temp[lhv]*2) + ((buffer[dst] + buffer[dst+2*tw]) >> 1))
            dst += 2*tw
        buffer[dst + tw] = i16(i16(temp[lhv + tw]*2) + ((buffer[dst] + buffer[dst]) >> 1))
        tb += 1; bb += 1


def c_decode(buf, tmp):
    for off, sw in ((3840,8),(3072,16),(0,32)):
        sub = buf[off:]
        c_block(sub, tmp, sw)
        buf[off:] = sub

def r_decode(buf, tmp):
    for off, sw in ((3840,8),(3072,16),(0,32)):
        sub = buf[off:]
        r_block(sub, tmp, sw)
        buf[off:] = sub


random.seed(7)
worst = 0
for trial in range(60):
    if trial < 30:
        src = [random.randint(-2000, 2000) for _ in range(4096)]
    else:
        src = [random.randint(-32768, 32767) for _ in range(4096)]
    a = list(src); b = list(src)
    c_decode(a, [0]*4096)
    r_decode(b, [0]*4096)
    diffs = sum(1 for i in range(4096) if a[i] != b[i])
    if diffs:
        first = next(i for i in range(4096) if a[i] != b[i])
        print(f"trial {trial}: {diffs} mismatches, first at {first}: C={a[first]} R={b[first]}")
        worst = max(worst, diffs)
print("worst mismatch count:", worst)
