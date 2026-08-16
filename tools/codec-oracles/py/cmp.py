# Third-party code: see tools/NOTICE for provenance and licence.
def clip(v): return 0 if v<0 else (255 if v>255 else v)

def ours(y,cb,cr):
    Y=(y+4096)<<16
    return (clip(((cr*91916+Y)>>16)>>5), clip(((Y-cb*22526-cr*46820)>>16)>>5), clip(((cb*115998+Y)>>16)>>5))

def ref_bgrx(y,cb,cr):
    Y=(y+4096)<<16
    return (clip(((cr*91916+Y)>>16)>>5), clip(((Y-cb*22527-cr*46819)>>16)>>5), clip(((cb*115992+Y)>>16)>>5))

def ref_gen(y,cb,cr):
    Y=(y+4096)<<16
    return (clip((cr*91916+Y)>>21), clip((Y-cb*22527-cr*46819)>>21), clip((cb*115992+Y)>>21))

# 1. are FreeRDP's two variants identical?
import random
random.seed(1)
bad=0
for _ in range(200000):
    y=random.randint(-4096,4095); cb=random.randint(-4096,4095); cr=random.randint(-4096,4095)
    if ref_bgrx(y,cb,cr)!=ref_gen(y,cb,cr): bad+=1
print("bgrx vs general mismatches:", bad)

# 2. ours vs reference, full-ish sweep
n=0; diffR=0; diffG=0; diffB=0; maxd=0
for _ in range(500000):
    y=random.randint(-4096,4095); cb=random.randint(-4096,4095); cr=random.randint(-4096,4095)
    a=ours(y,cb,cr); b=ref_bgrx(y,cb,cr); n+=1
    if a[0]!=b[0]: diffR+=1
    if a[1]!=b[1]: diffG+=1
    if a[2]!=b[2]: diffB+=1
    maxd=max(maxd, max(abs(a[i]-b[i]) for i in range(3)))
print("n=%d  R diff %.4f%%  G diff %.4f%%  B diff %.4f%%  max abs delta %d"%(n,100*diffR/n,100*diffG/n,100*diffB/n,maxd))
