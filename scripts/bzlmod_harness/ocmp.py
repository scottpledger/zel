import sys
from harness import *
import cases_ctx as cc
names=sys.argv[1:]
sel=[c for c in cc.cases if c['name'][4:] in names]
for c in sel: c['cmp_ob']=['external/*+t/o']; c['tree']=[]
for c in sel:
    n,res=do_case(c)
    b=res['b'][1].get('ob:external/*+t/o:content'); z=res['z'][1].get('ob:external/*+t/o:content')
    print('==',n,'SAME' if b==z else 'DIFF'); 
    if b!=z: print(' B:',repr(b)[:400]); print(' Z:',repr(z)[:400])
