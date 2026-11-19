import hashlib
class FP:
    def __init__(s): s.b=bytearray()
    def varint(s,n):
        n&=0xFFFFFFFFFFFFFFFF
        while True:
            if n<0x80: s.b.append(n); return
            s.b.append((n&0x7f)|0x80); n>>=7
    def int(s,x):  # int32 negative -> 10 byte varint
        s.varint(x if x>=0 else x+(1<<64))
    def str(s,x):
        d=x.encode('utf-8'); s.varint(len(d)); s.b+=d
    def bool(s,x): s.b.append(1 if x else 0)
    def strs(s,l):
        s.int(len(l)); [s.str(x) for x in l]
    def strmap(s,m):
        s.int(len(m)); 
        for k,v in m.items(): s.str(k); s.str(v)
    def nstr(s,x):
        if x is None: s.bool(False)
        else: s.bool(True); s.str(x)
    def hexd(s): return hashlib.sha256(bytes(s.b)).hexdigest()
def platform(fp,label,cons,flags=[],req=[],types=[],check=False,msg="For more information on platforms or toolchains see https://bazel.build/concepts/platforms-intro.",props={},rep=""):
    fp.bool(True)
    fp.str(label)
    fp.bool(False)            # no parent
    fp.int(len(cons))
    for setting,val in cons:
        fp.str(setting); fp.str(val)
    fp.nstr(rep); fp.strmap(props); fp.strs(flags); fp.strs(req); fp.strs(types); fp.bool(check); fp.nstr(msg)
def filewrite(exe,content,cons,label="@@platforms//host:host"):
    fp=FP(); fp.str("332877c7-ca9f-4731-b387-54f620408522"); fp.bool(exe); fp.str(content)
    platform(fp,label,cons)
    fp.strmap({}); fp.int(0)
    return fp.hexd()
if __name__=="__main__":
    cons=[("@@platforms//cpu:cpu","@@platforms//cpu:aarch64"),("@@platforms//os:os","@@platforms//os:osx")]
    # ConstraintValueInfo.addTo: setting.addTo(label) then value label
    print(filewrite(True,"#!/bin/sh\ntrue\n",cons))
