"""Creates the static files served by server.py (run after gen.py/mkreg.py/setup_native.py)."""
from hconf import VBZ
import os, shutil
W = VBZ + '/www'
os.makedirs(W + '/mirror/example.invalid', exist_ok=True)
open(W + '/hello.txt', 'w').write('hello download\n')
for n in ['pkg_gzip.deb', 'small.7z', 'small.tar.bz2', 'small.tar.gz', 'small.zip']:
    shutil.copy(f'{VBZ}/arch/{n}', f'{W}/{n}')
# registry mirror target used by the `mirrors` cases: a tarball of a registry module source
src = f'{VBZ}/arch/small.tar.gz'
shutil.copy(src, W + '/mirror/example.invalid/modx.tar.gz')
