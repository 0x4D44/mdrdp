import sys,subprocess,struct
def profile(path, step=5):
    if path.endswith('.png'):
        bmp=path.replace('.png','.cmp.bmp')
        subprocess.run(['sips','-s','format','bmp',path,'--out',bmp],capture_output=True)
        path=bmp
    f=open(path,'rb').read()
    off=struct.unpack_from('<I',f,10)[0]
    w=struct.unpack_from('<i',f,18)[0]; h=abs(struct.unpack_from('<i',f,22)[0])
    bpp=struct.unpack_from('<H',f,28)[0]//8
    row=w*bpp; stride=row+((4-(row%4))%4)
    tot=0;R=G=B=0;black=0
    for y in range(0,h,step):
        base=off+y*stride
        for x in range(0,w,step):
            p=base+x*bpp
            b,g,r=f[p],f[p+1],f[p+2]
            R+=r;G+=g;B+=b;tot+=1
            if r==0 and g==0 and b==0: black+=1
    return (w,h,R//tot,G//tot,B//tot,black*100/tot)
ref=profile(sys.argv[1]); ours=profile(sys.argv[2])
print(f"  reference {ref[0]}x{ref[1]}: mean RGB ({ref[2]:3},{ref[3]:3},{ref[4]:3})  black {ref[5]:5.1f}%")
print(f"  ours      {ours[0]}x{ours[1]}: mean RGB ({ours[2]:3},{ours[3]:3},{ours[4]:3})  black {ours[5]:5.1f}%")
d=abs(ref[2]-ours[2])+abs(ref[3]-ours[3])+abs(ref[4]-ours[4])
print(f"  colour distance: {d}   black delta: {abs(ref[5]-ours[5]):.1f} points")
