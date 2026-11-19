#!/bin/bash
# Vendor interchange: a vendor directory written by real Bazel must be consumed by Zel (no refetch, the
# tampered vendored file is visible) and a vendor directory written by Zel must be consumed by Bazel (no
# "out-of-date" warning). The repo rule is defined in the workspace, so its transitive .bzl digest is
# identical in both binaries (no @bazel_tools .bzl is loaded).
#
# Env: ZEL_BIN (required), BAZEL_BIN (default bazel), SCRATCH (default /tmp/zel-vendor-interchange).
# Note the predeclared input hash also covers the Starlark semantics fingerprint, which lists every
# BuildLanguageOptions key of the Bazel version; it only matches between binaries whose key sets agree
# (a real property of the binaries, compare with the real 9.3.2 only).
ZEL_BIN="${ZEL_BIN:?set ZEL_BIN}"
BAZEL_BIN="${BAZEL_BIN:-/tmp/bazel-9.3.2}"
SCRATCH="${SCRATCH:-/tmp/zel-vendor-interchange}"
rm -rf "$SCRATCH"; mkdir -p "$SCRATCH"
mkws() {
  mkdir -p "$1"
  echo "common --registry=file:///tmp/vendored-registry" >"$1/.bazelrc"
  cat >"$1/MODULE.bazel" <<'E'
module(name = "root")
u = use_repo_rule("//:r.bzl", "u")
u(name = "v", a = "x", b = ["1", "2"])
E
  touch "$1/BUILD.bazel"
  cat >"$1/r.bzl" <<'E'
def _i(rctx):
    rctx.file("BUILD.bazel", "")
    rctx.file("v.txt", "original")
u = repository_rule(_i, attrs = {"a": attr.string(), "b": attr.string_list()})
E
}
fail=0
check() { if [ "$2" = "$3" ]; then echo "  [OK] $1"; else echo "  [FAIL] $1: expected '$3' got '$2'"; fail=1; fi; }

# (a) Bazel writes, Zel consumes: matching 9.3.2 marker serialization means the vendored repo is current.
A="$SCRATCH/a"; mkws "$A/w"
(cd "$A/w" && "$BAZEL_BIN" --output_base="$A/ob_bazel" vendor --enable_workspace=false --vendor_dir=vd --repo=@v >"$A/out1" 2>&1)
"$BAZEL_BIN" --output_base="$A/ob_bazel" shutdown >/dev/null 2>&1
echo tampered >"$A/w/vd/+u+v/v.txt"
(cd "$A/w" && "$ZEL_BIN" --output_base="$A/ob_zel" fetch --enable_workspace=false --vendor_dir=vd --repo=@v >"$A/out2" 2>&1); rc=$?
azel=$(tr -d '\n' <"$A/ob_zel/external/+u+v/v.txt" 2>/dev/null)
"$ZEL_BIN" --output_base="$A/ob_zel" shutdown >/dev/null 2>&1
check "zel tolerates a bazel vendor dir (exit 0)" "$rc" "0"
check "zel consumed bazel's vendor dir" "$azel" "tampered"
check "no ERROR from zel" "$(grep -c '^ERROR' "$A/out2")" "0"

# (b) Zel writes, Bazel consumes (Bazel warns that the vendored repo is out of date and refetches).
B="$SCRATCH/b"; mkws "$B/w"
(cd "$B/w" && "$ZEL_BIN" --output_base="$B/ob_zel" vendor --enable_workspace=false --vendor_dir=vd --repo=@v >"$B/out1" 2>&1)
"$ZEL_BIN" --output_base="$B/ob_zel" shutdown >/dev/null 2>&1
echo tampered >"$B/w/vd/+u+v/v.txt"
(cd "$B/w" && "$BAZEL_BIN" --output_base="$B/ob_bazel" fetch --enable_workspace=false --vendor_dir=vd --repo=@v >"$B/out2" 2>&1); rc=$?
"$BAZEL_BIN" --output_base="$B/ob_bazel" shutdown >/dev/null 2>&1
check "bazel tolerates a zel vendor dir (exit 0)" "$rc" "0"
check "no ERROR from bazel" "$(grep -c '^ERROR' "$B/out2")" "0"

# (c) Zel's own round trip: a zel vendor dir is used by zel (tampered file visible, marker is a HEAD-format hash).
C="$SCRATCH/c"; mkws "$C/w"
(cd "$C/w" && "$ZEL_BIN" --output_base="$C/ob1" vendor --enable_workspace=false --vendor_dir=vd --repo=@v >"$C/out1" 2>&1)
"$ZEL_BIN" --output_base="$C/ob1" shutdown >/dev/null 2>&1
echo tampered >"$C/w/vd/+u+v/v.txt"
(cd "$C/w" && "$ZEL_BIN" --output_base="$C/ob2" fetch --enable_workspace=false --vendor_dir=vd --repo=@v >"$C/out2" 2>&1)
czel=$(tr -d '\n' <"$C/ob2/external/+u+v/v.txt" 2>/dev/null)
"$ZEL_BIN" --output_base="$C/ob2" shutdown >/dev/null 2>&1
check "zel consumes its own vendor dir" "$czel" "tampered"
check "marker is a 64-hex hash line" "$(head -1 "$C/w/vd/@+u+v.marker" | grep -Ec '^[0-9a-f]{64}$')" "1"
[ $fail -eq 0 ] && echo "vendor interchange: OK" || echo "vendor interchange: FAILED"
exit $fail
