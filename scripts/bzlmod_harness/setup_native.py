"""Builds the extra fixtures used by cases_native.py under $ZEL_HARNESS_DIR:

  gitnat/      git repo: c1 (a.txt, sub/b.txt, BUILD.bazel), c2 (a.txt changed, p.txt, sub/d/e.txt, MODULE.bazel),
               lightweight tag v1 -> c1, annotated tag v2 -> c2, branches main, br1 (c1), feat/x (c2), submodule at subm/
  gitsub/      repo used as a submodule
  www/p.patch  patch for a.txt as found in small.tar.gz (strip_prefix top)  [only created if absent]
  www/ok.patch, www/bad.patch
  netrc files  netrc_basic, netrc_default, netrc_bad
  jars         jar/x.jar (a tiny zip), jar/x-sources.jar
Deterministic: fixed author/committer dates so commit ids are stable.
"""
import os, subprocess, shutil, zipfile
from hconf import VBZ

ENV = dict(os.environ, GIT_AUTHOR_NAME='t', GIT_AUTHOR_EMAIL='t@t', GIT_COMMITTER_NAME='t', GIT_COMMITTER_EMAIL='t@t',
           GIT_AUTHOR_DATE='2026-01-02T03:04:05 +0000', GIT_COMMITTER_DATE='2026-01-02T03:04:05 +0000',
           GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_SYSTEM='/dev/null')


def git(cwd, *a):
    return subprocess.run(['git', '-c', 'protocol.file.allow=always', '-c', 'init.defaultBranch=main'] + list(a), cwd=cwd, env=ENV,
                          check=True, capture_output=True, text=True).stdout.strip()


def w(p, t, mode=None):
    os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, 'w').write(t)
    if mode:
        os.chmod(p, mode)


def build():
    gs = VBZ + '/gitsub'
    gn = VBZ + '/gitnat'
    if not os.path.exists(gn + '/.git'):
        shutil.rmtree(gs, ignore_errors=True)
        shutil.rmtree(gn, ignore_errors=True)
        os.makedirs(gs)
        git(gs, 'init')
        w(gs + '/s.txt', 's\n')
        git(gs, 'add', '-A'); git(gs, 'commit', '-m', 's1')
        os.makedirs(gn)
        git(gn, 'init')
        w(gn + '/a.txt', 'a\n')
        w(gn + '/sub/b.txt', 'b\n')
        w(gn + '/BUILD.bazel', 'filegroup(name="all", srcs=glob(["**"]))\n')
        w(gn + '/x.sh', '#!/bin/sh\n', 0o755)
        git(gn, 'add', '-A'); git(gn, 'commit', '-m', 'one')
        git(gn, 'tag', 'v1')
        git(gn, 'branch', 'br1')
        w(gn + '/a.txt', 'A\n')
        w(gn + '/p.txt', 'p\n')
        w(gn + '/sub/d/e.txt', 'e\n')
        w(gn + '/MODULE.bazel', 'module(name="gitnat")\n')
        git(gn, 'add', '-A'); git(gn, 'commit', '-m', 'two')
        git(gn, 'tag', '-a', 'v2', '-m', 'annotated')
        git(gn, 'branch', 'feat/x')
        git(gn, 'submodule', 'add', gs, 'subm')
        git(gn, 'commit', '-m', 'three (submodule)')
    # patches
    www = VBZ + '/www'
    os.makedirs(www, exist_ok=True)
    w(www + '/ok.patch', '--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-hello\n+HELLO\n')
    w(www + '/bad.patch', '--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-nothello\n+X\n')
    if not os.path.exists(www + '/p.patch'):
        w(www + '/p.patch', '--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-hello\n+HELLO\n')
    # netrc
    w(VBZ + '/netrc_basic', 'machine 127.0.0.1 login user password pass\n')
    w(VBZ + '/netrc_default', 'default login user password pass\n')
    w(VBZ + '/netrc_bad', 'machine\n')
    # jars
    os.makedirs(VBZ + '/jar', exist_ok=True)
    for n in ('x.jar', 'x-sources.jar'):
        with zipfile.ZipFile(VBZ + '/jar/' + n, 'w') as z:
            zi = zipfile.ZipInfo('a/B.txt', (2020, 1, 1, 0, 0, 0))
            z.writestr(zi, n)
    return gn


if __name__ == '__main__':
    print(build())
    print(git(VBZ + '/gitnat', 'rev-parse', 'HEAD'))
