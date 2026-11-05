import sys,os,runpy,functools
H=os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0,H); os.chdir(H)
import harness
orig=harness.run_one
def run_one(bin,args,ws,ob,env=None):
    return orig(bin,['--max_idle_secs=60']+args,ws,ob,env)
harness.run_one=run_one
om=harness.main
def main(cases,jobs=2):
    for c in cases: c.pop('skew',None)  # no skew allowlist: the spec is the real 9.3.2
    only=sys.argv[1:]
    sel=[c for c in cases if not only or any(o in c['name'] for o in only)]
    b=os.environ.get('BATCH')
    if b:
        i,j=map(int,b.split(':')); keep=set(c['name'] for c in sel[i:j])
        print('SELECTED',len(sel),flush=True)
        cases=[c for c in cases if c['name'] in keep]
        sys.argv=sys.argv[:1]
    return om(cases,2)
harness.main=main
case=sys.argv[1]; sys.argv=[case]+sys.argv[2:]
runpy.run_path(case if case.startswith('/') else H+'/'+case,run_name='__main__')
