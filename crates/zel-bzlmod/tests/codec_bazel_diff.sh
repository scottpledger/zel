#!/bin/bash
# Compares archive extraction (http_archive from file:// URLs) between real Bazel (bazelisk) and zel.
# usage: codec_bazel_diff.sh <zel-binary> [workdir]     (needs xz zstd bzip2 7zz/7z, bazel on PATH)
# Set ONLY=<regex> to restrict to some archives.
# Prints one line per archive: SAME / DIFF (with first differing lines) / rc mismatch.
ZEL=${1:?zel binary}; W=${2:-/tmp/zel-codec-bazel-diff}; BAZEL=${BAZEL:-bazel}
SZ=$(command -v 7zz || command -v 7z || command -v 7za)
rm -rf "$W"; mkdir -p "$W/fx/src/d/sub/deep" "$W/fx/src/d/emptydir"; cd "$W/fx"
S=src/d
head -c 300000 /dev/urandom > $S/rand.bin; yes "hello world line" | head -c 2000000 > $S/text.txt
: > $S/empty; echo x > $S/e1; echo '#!/bin/sh' > $S/run.sh; chmod 755 $S/run.sh
echo ro > $S/ro.txt; chmod 444 $S/ro.txt; echo sec > $S/sec.txt; chmod 600 $S/sec.txt
echo deep > $S/sub/deep/f; ln -s rand.bin $S/link; ln -s sub/deep/f $S/sub/ln2
head -c 5000000 /dev/zero > $S/zeros
(cd src && tar cf ../a.tar d)
xz -k -c a.tar > a.tar.xz; xz -c --check=sha256 -9e a.tar > a_9e.tar.xz; xz -c -T4 --block-size=500000 a.tar > a_mt.tar.xz
zstd -q -c a.tar > a.tar.zst; zstd -q -c --ultra -22 a.tar > a_22.tar.zst; (zstd -q -c -1 a.tar; zstd -q -c -3 a.tar) > a_2frames.tar.zst
bzip2 -c a.tar > a.tar.bz2
# multi-stream bz2 cut mid-tar
split -b 700000 a.tar part_; (for p in part_*; do bzip2 -c $p; done) > b_multi.tar.bz2; rm part_*
gzip -c a.tar > a.tar.gz
(cd src && "$SZ" a -bd -y -snl ../a.7z d >/dev/null); (cd src && "$SZ" a -bd -y -m0=bzip2 ../a_bz2.7z d >/dev/null)
(cd src && "$SZ" a -bd -y -m0=deflate ../a_defl.7z d >/dev/null); (cd src && "$SZ" a -bd -y -mf=BCJ ../a_bcj.7z d >/dev/null)
(cd src && "$SZ" a -bd -y -mx=0 ../a_copy.7z d >/dev/null)
(cd src && zip -qr -y ../a.zip d)
# empty tar variants and ar with symbol table
tar cf empty.tar -T /dev/null; xz -c empty.tar > empty.tar.xz; zstd -q -c empty.tar > empty.tar.zst; bzip2 -c empty.tar > empty.tar.bz2
head -c 10240 /dev/zero > zeros_only.tar
mkdir -p arsrc && (cd arsrc && echo 'int f(){return 1;}' > a.c && cc -c a.c -o a.o && ar rcs ../sym.ar a.o && ar rcS ../nosym.ar a.o) 2>/dev/null
C=$W/ws; mkdir -p $C; cd $C
echo 'module(name="m")' > MODULE.bazel
echo 'http_archive = use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl", "http_archive")' >> MODULE.bazel
touch BUILD.bazel
names=()
for f in $(cd $W/fx; ls | grep -E '\.(tar|tar\.xz|tar\.zst|tar\.bz2|tar\.gz|7z|zip|ar)$'); do
  n=$(echo $f | tr '.-' '__'); names+=($n)
  sp='d'; case $f in empty.*|zeros_only*|*.ar) sp="";; esac
  ty=""; case $f in *.ar) ty='type="ar",';; esac
  echo "http_archive(name=\"$n\", urls=[\"file://$W/fx/$f\"], strip_prefix=\"$sp\", build_file_content=\"\", $ty)" >> MODULE.bazel
done
run() { # tool tag name -> prints rc, leaves dir copy in $W/out/<tag>/<name>
  local tool=$1 tag=$2 n=$3 ob=$W/ob_$2
  out=$(timeout 600 $tool --output_base=$ob fetch --repo=@$n 2>&1); rc=$?
  d=$(ls -d $ob/external/*+$n $ob/external/*~$n $ob/external/$n 2>/dev/null | head -1)
  mkdir -p $W/out/$tag; rm -rf $W/out/$tag/$n
  [ -n "$d" ] && cp -R -P "$d" $W/out/$tag/$n 2>/dev/null
  echo $rc > $W/out/$tag/$n.rc; echo "$out" | grep -iE 'error|exception|panic' | head -1 | cut -c1-200 > $W/out/$tag/$n.err
}
for n in "${names[@]}"; do
  [ -n "$ONLY" ] && ! [[ $n =~ $ONLY ]] && continue
  run "$BAZEL" real $n; run "$ZEL" zel $n
  rr=$(cat $W/out/real/$n.rc); rz=$(cat $W/out/zel/$n.rc)
  if [ "$rr" != "$rz" ]; then echo "$n: RC MISMATCH real=$rr zel=$rz | real: $(cat $W/out/real/$n.err) | zel: $(cat $W/out/zel/$n.err)"; continue; fi
  if [ "$rr" != 0 ]; then echo "$n: both fail rc=$rr"; continue; fi
  # compare tree incl. symlinks and permissions
  A=$(cd $W/out/real/$n && find . -not -name '.http_archive_tmp' \( -name BUILD.bazel -o -name WORKSPACE -o -name REPO.bazel -o -name MODULE.bazel \) -prune -o -print0 | sort -z | xargs -0 stat -f '%N %Sp %z %Y' 2>/dev/null)
  B=$(cd $W/out/zel/$n && find . -not -name '.http_archive_tmp' \( -name BUILD.bazel -o -name WORKSPACE -o -name REPO.bazel -o -name MODULE.bazel \) -prune -o -print0 | sort -z | xargs -0 stat -f '%N %Sp %z %Y' 2>/dev/null)
  c=$(diff -r --no-dereference -x BUILD.bazel -x WORKSPACE -x REPO.bazel -x MODULE.bazel $W/out/real/$n $W/out/zel/$n 2>&1 | head -3 | tr '\n' ' ')
  m=$(diff <(echo "$A") <(echo "$B") | head -6 | tr '\n' ' ')
  if [ -z "$c" ] && [ -z "$m" ]; then echo "$n: SAME"; else echo "$n: DIFF content[$c] meta[$m]"; fi
done
