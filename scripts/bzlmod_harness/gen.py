from hconf import VBZ
import os,subprocess,shutil,random,sys,io,tarfile,zipfile,struct,zlib,json
A=VBZ+'/arch'; S=VBZ+'/src'
random.seed(5)
def sh(c,cwd=None):
    r=subprocess.run(c,shell=True,cwd=cwd,capture_output=True)
    if r.returncode: print('GENFAIL',c,r.stderr[:200])
def tree(name,kind):
    d=f'{S}/{name}'; shutil.rmtree(d,ignore_errors=True); os.makedirs(d)
    def w(p,c,mode=0o644):
        p=f'{d}/{p}'; os.makedirs(os.path.dirname(p),exist_ok=True)
        open(p,'wb').write(c if isinstance(c,bytes) else c.encode()); os.chmod(p,mode)
    if kind=='small':
        w('top/a.txt','hello\n'); w('top/b/c.txt','c\n'); w('top/run.sh','#!/bin/sh\n',0o755); w('top/ro.txt','ro',0o444)
    elif kind=='many':
        for i in range(400): w(f'top/d{i%17}/f{i}.txt',f'file {i}\n'*(i%7+1),0o755 if i%5==0 else 0o644)
    elif kind=='big':
        w('top/rand.bin',bytes(random.getrandbits(8) for _ in range(300000))*40)  # 12MB repeating
        w('top/z.bin',b'\0'*5000000)
    elif kind=='uni':
        w('top/héllo wörld.txt','u'); w('top/日本語/ファイル.txt','j'); w('top/sp ace/x y.txt','s'); w('top/-dash','d')
    elif kind=='empty': os.makedirs(d+'/top/emptydir'); w('top/.keep','')
    elif kind=='links':
        w('top/real.txt','r'); os.symlink('real.txt',d+'/top/ln'); os.symlink('../top/real.txt',d+'/top/up'); os.symlink('/etc/hosts',d+'/top/abs'); os.symlink('nonexist',d+'/top/dang')
        os.makedirs(d+'/top/sub'); os.symlink('..',d+'/top/sub/dotdot'); os.symlink('../../escape',d+'/top/sub/esc')
    elif kind=='deep': w('top/'+'/'.join(['d%d'%i for i in range(60)])+'/f.txt','deep'); w('top/'+('l'*200)+'/'+('m'*200)+'.txt','longname')
    return d
kinds=['small','many','big','uni','empty','links','deep']
for k in kinds: tree(k,k)
def out(n): return f'{A}/{n}'
n=0
# tar family
for k in kinds:
    d=f'{S}/{k}'
    sh(f'tar czf {out(k+".tar.gz")} -C {d} .')
    sh(f'tar czf {out(k+".tgz")} -C {d} top')
    sh(f'tar cf {out(k+".tar")} -C {d} top')
    sh(f'tar cjf {out(k+".tar.bz2")} -C {d} top'); sh(f'cp {out(k+".tar.bz2")} {out(k+".tbz")}')
    for lvl in ['-0','-1','-6','-9e']:
        sh(f'tar cf - -C {d} top | xz {lvl} -c > {out(k+".x"+lvl+".tar.xz")}')
    for chk in ['none','crc32','crc64','sha256']:
        sh(f'tar cf - -C {d} top | xz --check={chk} -c > {out(k+".c"+chk+".tar.xz")}')
    sh(f'tar cf - -C {d} top | xz --lzma2=preset=6,dict=64KiB -c > {out(k+".d64k.tar.xz")}')
    sh(f'tar cf - -C {d} top | xz --x86 --lzma2 -c > {out(k+".bcj.tar.xz")}')
    sh(f'tar cf - -C {d} top | xz -T2 --block-size=1MiB -c > {out(k+".blocks.tar.xz")}')
    sh(f'tar cf - -C {d} top | xz --format=lzma -c > {out(k+".tar.lzma")}')
    for lvl in ['-1','-3','-19','--ultra -22']:
        sh(f'tar cf - -C {d} top | zstd {lvl} -q -c > {out(k+".z"+lvl.replace(" ","")+".tar.zst")}')
    sh(f'tar cf - -C {d} top | zstd --long=27 -q -c > {out(k+".zlong.tar.zst")}')
    sh(f'tar cf - -C {d} top | zstd --no-check -q -c > {out(k+".znock.tar.zst")}')
    sh(f'tar cf - -C {d} top | zstd -q -c > {out(k+".tzst")}')
    sh(f'tar cf - -C {d} top | zstd -T2 --zstd=strategy=1,windowLog=10 -q -c > {out(k+".zw10.tar.zst")}')
    for fmt in ['ustar','gnu','pax','v7','posix']:
        sh(f'tar --format={fmt} -cf {out(k+"."+fmt+".tar")} -C {d} top 2>/dev/null || bsdtar -cf {out(k+"."+fmt+".tar")} --format {fmt} -C {d} top 2>/dev/null')
    sh(f'rm -f {out(k+".zip")}; cd {d} && zip -qry {out(k+".zip")} top')
    sh(f'cd {d} && zip -qr -0 {out(k+".store.zip")} top')
    sh(f'cd {d} && zip -qr -Z bzip2 {out(k+".bz.zip")} top')
    sh(f'cd {d} && zip -qr -fz- {out(k+".z64.zip")} top 2>/dev/null; zip -qr -fz {out(k+".fz.zip")} top')
    sh(f'cd {d} && zip -qr -9 -z {out(k+".cmt.zip")} top <<< "comment here"')
    sh(f'cp {out(k+".zip")} {out(k+".jar")}; cp {out(k+".zip")} {out(k+".war")}; cp {out(k+".zip")} {out(k+".aar")}; cp {out(k+".zip")} {out(k+".nupkg")}; cp {out(k+".zip")} {out(k+".whl")}')
    for m in ['lzma','lzma2','ppmd','bzip2','deflate','deflate64','copy']:
        sh(f'rm -f {out(k+"."+m+".7z")}; cd {d} && 7zz a -bso0 -bsp0 -m0={m} {out(k+"."+m+".7z")} top')
    sh(f'rm -f {out(k+".nosolid.7z")}; cd {d} && 7zz a -bso0 -bsp0 -ms=off {out(k+".nosolid.7z")} top')
    sh(f'rm -f {out(k+".bcj.7z")}; cd {d} && 7zz a -bso0 -bsp0 -mf=BCJ {out(k+".bcj.7z")} top')
    sh(f'rm -f {out(k+".arm.7z")}; cd {d} && 7zz a -bso0 -bsp0 -mf=ARM64 {out(k+".arm.7z")} top')
    sh(f'rm -f {out(k+".hdrenc.7z")}; cd {d} && 7zz a -bso0 -bsp0 -mhc=off {out(k+".hdrenc.7z")} top')
    sh(f'rm -f {out(k+".mt.7z")}; cd {d} && 7zz a -bso0 -bsp0 -mmt=4 -mx9 {out(k+".mt.7z")} top')
    sh(f'rm -f {out(k+".7z")}; cd {d} && 7zz a -bso0 -bsp0 {out(k+".7z")} top')
    sh(f'rm -f {out(k+".7z.7z")}; cd {d} && 7zz a -bso0 -bsp0 -pSECRET {out(k+".enc.7z")} top')
# bare compressed single files
for ext,cmd in [('gz','gzip -c'),('xz','xz -c'),('bz2','bzip2 -c'),('zst','zstd -q -c')]:
    sh(f'echo payload | {cmd} > {out("bare."+ext)}'); sh(f'cp {out("bare."+ext)} {out("bare_named.f.txt."+ext)}')
sh(f'cd /tmp && gzip -c -N {S}/small/top/a.txt > {out("hdrname.gz")}')
sh(f'cd /tmp && cp {S}/small/top/a.txt {VBZ}/orig_name.txt && gzip -N -c {VBZ}/orig_name.txt > {out("hdrname2.gz")}')
# ar / deb
sh(f'cd {S}/small/top && ar rcs {out("lib.a")} a.txt run.sh')
sh(f'cd {S}/small/top && ar rcsD {out("libD.ar")} a.txt')
# deb
dd=f'{S}/debpkg'; shutil.rmtree(dd,ignore_errors=True); os.makedirs(dd+'/DEBIAN'); os.makedirs(dd+'/usr/bin'); os.makedirs(dd+'/usr/share/doc/x')
open(dd+'/DEBIAN/control','w').write('Package: x\nVersion: 1\nArchitecture: all\nMaintainer: a <a@b>\nDescription: t\n')
open(dd+'/usr/bin/x','w').write('#!/bin/sh\n'); os.chmod(dd+'/usr/bin/x',0o755); open(dd+'/usr/share/doc/x/c','w').write('copyright')
os.symlink('x',dd+'/usr/bin/xl')
for comp in ['gzip','xz','zstd','none']:
    sh(f'dpkg-deb -Z{comp} --build {dd} {out("pkg_"+comp+".deb")} >/dev/null')
sh(f'cp {out("pkg_gzip.deb")} {out("pkg.udeb")}')
# manual deb with bz2 data member
sh(f'cd {VBZ} && rm -rf dm && mkdir dm && cd dm && tar cjf data.tar.bz2 -C {dd} usr && echo 2.0 > debian-binary && tar czf control.tar.gz -C {dd}/DEBIAN . && ar rc {out("pkg_bz2.deb")} debian-binary control.tar.gz data.tar.bz2')
# corruption variants
def corrupt(src,name,mode):
    b=open(out(src),'rb').read()
    if mode=='trunc': b=b[:len(b)//2]
    elif mode=='trunc_tail': b=b[:-7]
    elif mode=='flip_mid': i=len(b)//2; b=b[:i]+bytes([b[i]^0xff])+b[i+1:]
    elif mode=='flip_head': b=bytes([b[0]^1])+b[1:]
    elif mode=='garbage_tail': b=b+b'GARBAGE_TRAILING'
    elif mode=='zeros_tail': b=b+b'\0'*1024
    elif mode=='empty': b=b''
    elif mode=='flip_crc': b=b[:-5]+bytes([b[-5]^0x55])+b[-4:]
    open(out(name),'wb').write(b)
for src in ['small.tar.gz','many.tar.xz' if False else 'small.c crc64.tar.xz'.replace(' ',''),'small.z-3.tar.zst','small.tar.bz2','small.zip','small.lzma2.7z','small.tar','small.tar.xz' if False else 'small.cnone.tar.xz','pkg_gzip.deb','lib.a','small.deflate.7z','many.z-3.tar.zst','many.zip','many.lzma.7z','big.tar.gz','big.zip']:
    ext=src.split('.',1)[1]
    for m in ['trunc','trunc_tail','flip_mid','flip_head','garbage_tail','zeros_tail','empty','flip_crc']:
        corrupt(src,f'corrupt_{m}_{src.split(".")[0]}.{ext}'.replace(' ','_'),m)
# python-generated tricky tars
def mk(name,entries,fmt=tarfile.GNU_FORMAT):
    with tarfile.open(out(name),'w',format=fmt) as t:
        for e in entries:
            ti=tarfile.TarInfo(e['n']); ti.type=e.get('t',tarfile.REGTYPE); ti.mode=e.get('m',0o644)
            data=e.get('d',b''); ti.size=len(data) if ti.type==tarfile.REGTYPE else 0
            if 'l' in e: ti.linkname=e['l']
            if 'mt' in e: ti.mtime=e['mt']
            t.addfile(ti,io.BytesIO(data) if ti.type==tarfile.REGTYPE else None)
R=tarfile.REGTYPE; SY=tarfile.SYMTYPE; LN=tarfile.LNKTYPE; DI=tarfile.DIRTYPE
mk('t_dotdot_file.tar',[{'n':'../evil.txt','d':b'x'},{'n':'ok.txt','d':b'o'}])
mk('t_dotdot_mid.tar',[{'n':'a/../b.txt','d':b'x'}])
mk('t_dotdot_mid2.tar',[{'n':'a/../../b.txt','d':b'x'}])
mk('t_abs.tar',[{'n':'/abs/path.txt','d':b'x'}])
mk('t_dot.tar',[{'n':'./','t':DI},{'n':'./x.txt','d':b'x'}])
mk('t_empty_name.tar',[{'n':'','d':b'x'},{'n':'ok','d':b'o'}])
mk('t_sym_up.tar',[{'n':'a/l','t':SY,'l':'../../x'},{'n':'a/ok','d':b'o'}])
mk('t_sym_up2.tar',[{'n':'a/l','t':SY,'l':'../x'},{'n':'x','d':b'o'}])
mk('t_sym_abs.tar',[{'n':'l','t':SY,'l':'/etc/passwd'}])
mk('t_sym_then_file.tar',[{'n':'l','t':SY,'l':'target'},{'n':'l/f.txt','d':b'through'}])
mk('t_sym_dir_over.tar',[{'n':'d','t':DI},{'n':'d','t':SY,'l':'x'}])
mk('t_file_over_dir.tar',[{'n':'d/f','d':b'1'},{'n':'d','d':b'2'}])
mk('t_dup.tar',[{'n':'x','d':b'1'},{'n':'x','d':b'2'}])
mk('t_hardlink.tar',[{'n':'a','d':b'1'},{'n':'b','t':LN,'l':'a'}])
mk('t_hardlink_missing.tar',[{'n':'b','t':LN,'l':'nonexist'}])
mk('t_hardlink_up.tar',[{'n':'b','t':LN,'l':'../x'}])
mk('t_fifo.tar',[{'n':'f','t':tarfile.FIFOTYPE},{'n':'ok','d':b'o'}])
mk('t_chr.tar',[{'n':'c','t':tarfile.CHRTYPE},{'n':'ok','d':b'o'}])
mk('t_blk.tar',[{'n':'c','t':tarfile.BLKTYPE},{'n':'ok','d':b'o'}])
mk('t_modes.tar',[{'n':'x0','m':0,'d':b'1'},{'n':'x4755','m':0o4755,'d':b'1'},{'n':'x1777','m':0o1777,'d':b'1'},{'n':'x700','m':0o700,'d':b'1'},{'n':'d0','m':0o000,'t':DI},{'n':'d500','m':0o500,'t':DI}])
mk('t_longname.tar',[{'n':'x'*300,'d':b'1'}])
mk('t_longname_pax.tar',[{'n':'y'*300+'/z','d':b'1'},{'n':'l'*300,'t':SY,'l':'t'*300}],tarfile.PAX_FORMAT)
mk('t_case.tar',[{'n':'File','d':b'1'},{'n':'file','d':b'2'},{'n':'FILE','d':b'3'}])
mk('t_unicode_nfd.tar',[{'n':'e\u0301.txt','d':b'1'},{'n':'\u00e9.txt','d':b'2'}])
mk('t_trailing_slash_file.tar',[{'n':'dd/','d':b''}])
mk('t_mtime0.tar',[{'n':'x','d':b'1','mt':0},{'n':'y','d':b'1','mt':2**33}])
mk('t_only_dirs.tar',[{'n':'a','t':DI},{'n':'a/b','t':DI}])
mk('t_zero.tar',[])
mk('t_pax_big.tar',[{'n':'x','d':b'1'}],tarfile.PAX_FORMAT)
open(out('t_garbage.tar'),'wb').write(b'x'*2000)
open(out('t_short.tar'),'wb').write(b'x'*100)
open(out('t_zero_blocks.tar'),'wb').write(b'\0'*10240)
# bad checksum header
b=bytearray(open(out('small.tar'),'rb').read()); b[148]=ord('9'); open(out('t_badsum.tar'),'wb').write(b)
# gz tricks
import gzip
open(out('g_multi.tar.gz'),'wb').write(gzip.compress(open(out('small.tar'),'rb').read()[:2560])+gzip.compress(open(out('small.tar'),'rb').read()[2560:]))
open(out('g_concat_tar.tar.gz'),'wb').write(gzip.compress(open(out('t_dup.tar'),'rb').read()[:-1024])+gzip.compress(open(out('small.tar'),'rb').read()))
# zip tricks
def mkz(name,entries,comp=zipfile.ZIP_DEFLATED):
    with zipfile.ZipFile(out(name),'w',comp) as z:
        for e in entries:
            zi=zipfile.ZipInfo(e['n']); zi.compress_type=e.get('c',comp); zi.external_attr=(e.get('m',0o644)|e.get('ft',0o100000))<<16
            z.writestr(zi,e.get('d',b''))
mkz('z_dotdot.zip',[{'n':'../evil','d':b'x'},{'n':'ok','d':b'o'}])
mkz('z_abs.zip',[{'n':'/abs.txt','d':b'x'}])
mkz('z_sym.zip',[{'n':'a','d':b'1'},{'n':'l','d':b'a','ft':0o120000,'m':0o777}])
mkz('z_sym_up.zip',[{'n':'l','d':b'../x','ft':0o120000,'m':0o777}])
mkz('z_dup.zip',[{'n':'x','d':b'1'},{'n':'x','d':b'2'}])
mkz('z_dir_file.zip',[{'n':'d/','d':b'','ft':0o040000,'m':0o755},{'n':'d/f','d':b'1'}])
mkz('z_file_as_dir.zip',[{'n':'d','d':b'1'},{'n':'d/f','d':b'1'}])
mkz('z_modes.zip',[{'n':'a','m':0o000,'d':b'1'},{'n':'b','m':0o755,'d':b'1'},{'n':'c','m':0o4755,'d':b'1'}])
mkz('z_bs.zip',[{'n':'a\\b.txt','d':b'1'}])
mkz('z_empty.zip',[])
mkz('z_dos.zip',[{'n':'x','d':b'1','m':0,'ft':0}])
mkz('z_stored.zip',[{'n':'x','d':b'1'*1000}],zipfile.ZIP_STORED)
mkz('z_lzma.zip',[{'n':'x','d':b'1'*1000}],zipfile.ZIP_LZMA)
mkz('z_bzip2.zip',[{'n':'x','d':b'1'*1000}],zipfile.ZIP_BZIP2)
mkz('z_case.zip',[{'n':'File','d':b'1'},{'n':'file','d':b'2'}])
open(out('z_garbage_prefix.zip'),'wb').write(b'PREFIXJUNK'+open(out('small.zip'),'rb').read())
open(out('z_sfx.zip'),'wb').write(b'#!/bin/sh\nexit\n'+open(out('small.zip'),'rb').read())
# 7z with symlinks etc from 7zz
print('done',len(os.listdir(A)))
