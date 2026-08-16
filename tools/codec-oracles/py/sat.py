import struct,subprocess,sys
bmp="/tmp/_s.bmp"
subprocess.run(["sips","-s","format","bmp","-z","270","480",sys.argv[1],"--out",bmp],capture_output=True)
f=open(bmp,'rb').read(); off=struct.unpack_from('<I',f,10)[0]
W=struct.unpack_from('<i',f,18)[0]; H=abs(struct.unpack_from('<i',f,22)[0]); bpp=struct.unpack_from('<H',f,28)[0]//8
stride=W*bpp+((4-(W*bpp%4))%4)
sat=0; lum=0; n=0
for y in range(H):
    for x in range(W):
        p=off+y*stride+x*bpp; b,g,r=f[p],f[p+1],f[p+2]
        sat+=max(r,g,b)-min(r,g,b); lum+=0.299*r+0.587*g+0.114*b; n+=1
print(f"    mean saturation {sat/n:6.1f}   mean luma {lum/n:6.1f}")
