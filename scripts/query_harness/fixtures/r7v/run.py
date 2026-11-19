#!/usr/bin/env python3
# usage: run.py bazel|zel [--only RE] cmds.tsv ; workspace copies at /tmp/r7v/wk{k}/fx (same path for both tools)
import os,re,sys,shutil,subprocess,threading,queue
R='/tmp/r7v'; tool=sys.argv[1]
BIN={'bazel':'/opt/homebrew/bin/bazel','zel':'/tmp/zel-target/release/zel'}[tool]
NW=2
only=None; args=sys.argv[2:]
if args[0]=='--only': only=re.compile(args[1]); args=args[2:]
cmds=[]
for l in open(args[0]):
    l=l.rstrip('\n')
    if not l: continue
    i,c=l.split('\t',1)
    if only and not only.search(c): continue
    cmds.append((i,c))
od=f'{R}/out_{tool}'; os.makedirs(od,exist_ok=True)
q=queue.Queue()
for c in cmds:q.put(c)
def worker(k):
    ws=f'{R}/wk{k}/fx'; ob=f'{R}/ob_{tool}{k}'
    if os.path.exists(f'{R}/wk{k}'): shutil.rmtree(f'{R}/wk{k}')
    shutil.copytree(f'{R}/fx',ws)
    n=0
    def sh():
        subprocess.run(f'timeout 120 {BIN} --output_base={ob} shutdown',shell=True,cwd=ws,capture_output=True,stdin=subprocess.DEVNULL)
    while True:
        try:i,c=q.get_nowait()
        except queue.Empty: break
        r=subprocess.run(f'timeout 300 {BIN} --max_idle_secs=60 --output_base={ob} {c}',shell=True,cwd=ws,capture_output=True,stdin=subprocess.DEVNULL)
        def nz(b): return b.replace(ob.encode(),b'@OB@').replace(os.path.realpath(ob).encode(),b'@OB@')
        for ext,d in(('out',nz(r.stdout)),('err',nz(r.stderr)),('rc',str(r.returncode).encode())):
            open(f'{od}/{i}.{ext}','wb').write(d)
        n+=1
        if n%50==0: sh()
    sh()
ts=[threading.Thread(target=worker,args=(k,)) for k in range(NW)]
[t.start() for t in ts];[t.join() for t in ts]
print('done',len(cmds))
