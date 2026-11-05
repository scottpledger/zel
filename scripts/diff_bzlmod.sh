#!/bin/bash
set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
FX_DIR="$REPO_DIR/crates/zel-bzlmod/tests/fixtures"

SCRATCH="${DIFF_BZLMOD_DIR:-${TMPDIR:-/tmp}/diff_bzlmod}"
export DIFF_BZLMOD_DIR="$SCRATCH"
mkdir -p "$SCRATCH"

# Ensure fixtures and differential test cases are initialized
python3 "$FX_DIR/make_fixtures.py" >/dev/null 2>&1
python3 "$FX_DIR/setup_test_cases.py" >/dev/null 2>&1

ZEL_BIN="${ZEL_BIN:-$REPO_DIR/target/debug/zel}"
BAZEL_BIN="${BAZEL_BIN:-/tmp/bazel-9.3.2}"  # real 9.3.2 binary, never bazelisk

if [ ! -x "$ZEL_BIN" ]; then
    echo "ERROR: Zel binary not found at $ZEL_BIN."
    exit 1
fi

BOB="$SCRATCH/bob"
ZOB="$SCRATCH/zob"
rm -rf "$BOB" "$ZOB"
mkdir -p "$BOB" "$ZOB"

cleanup() {
    "$BAZEL_BIN" --output_base="$BOB" shutdown >/dev/null 2>&1 || true
    "$ZEL_BIN" --output_base="$ZOB" shutdown >/dev/null 2>&1 || true
}
trap cleanup EXIT

WS="$SCRATCH/ws"
rm -rf "$WS"
cp -R "$FX_DIR/main" "$WS"
ln -sfn "$FX_DIR/mods" "$WS/mods"

REG_FLAG="--registry=file://$FX_DIR/reg --allow_yanked_versions=a@1.1"

# Output normalization. Only absolute paths, timestamps/progress lines and hashes are normalized;
# every other line (tracebacks, source lines, error trailers, locations, warnings) is compared.
norm_common() {
    # $1 = file, $2 = output base used by that run, $3 = workspace dir used by that run
    tr '\r' '\n' < "$1" | sed -E \
        -e 's/\x1b\[[0-9;]*m//g' \
        -e "s|/private$2|OB|g" \
        -e "s|$2|OB|g" \
        -e "s|/private$3|WS|g" \
        -e "s|$3|WS|g" \
        -e "s|/private$FX_DIR|FX|g" \
        -e "s|$FX_DIR|FX|g" \
        -e 's|/private/tmp|/tmp|g' \
        -e 's/\b[0-9a-f]{64}\b/HASH64/g' \
        -e 's/sha256-[A-Za-z0-9+\/]{43}=/SRI/g' \
        | grep -vE '^(Starting local|Computing main|Loading:|Analyzing:|\[[0-9,]+ / [0-9,]+\]|INFO: (Elapsed time|Analyzed|Build completed|Found [0-9]+|Invocation ID|Streaming|Current date|[0-9]+ processes)|\([0-9]+:[0-9]+:[0-9]+\) |WARNING: Running Bazel server needs to be killed| no actions running$)' || true
}
norm_stdout() { norm_common "$1" "$2" "$3"; }
norm_stderr() { norm_common "$1" "$2" "$3"; }

# Lockfile normalization: none beyond path replacement (the spec is the real 9.3.2; no skew allowlist).
norm_lock() {
    python3 - "$1" "$2" "$3" <<'PY'
import json, sys, re
p, ob, ws = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    j = json.load(open(p))
except Exception as e:
    print("NO-LOCKFILE", e.__class__.__name__); sys.exit()
txt = json.dumps(j, indent=1, sort_keys=True)
for pre in ("/private" + ob, ob):
    txt = txt.replace(pre, "OB")
for pre in ("/private" + ws, ws):
    txt = txt.replace(pre, "WS")
print(txt)
PY
}

passed=0
failed=0
total=0

run_test_ws() {
    local ws_dir="$1"
    local desc="$2"
    local cmd="$3"
    local extra_flags="${4:-$REG_FLAG}"
    local check_tree="${5:-}"
    total=$((total + 1))

    local b_out="$SCRATCH/b.out"
    local b_err="$SCRATCH/b.err"
    local z_out="$SCRATCH/z.out"
    local z_err="$SCRATCH/z.err"

    local b_ob="$BOB"
    local z_ob="$ZOB"
    if [ "$ws_dir" != "$WS" ]; then
        b_ob="$BOB/$(basename "$ws_dir")"
        z_ob="$ZOB/$(basename "$ws_dir")"
        mkdir -p "$b_ob" "$z_ob"
    fi

    local be=0
    local ze=0

    rm -f "$ws_dir/MODULE.bazel.lock" "$b_ob/MODULE.bazel.lock" "$z_ob/MODULE.bazel.lock"

    # Each binary runs in its own copy of the workspace so lockfiles and trees do not interfere.
    local b_ws="$ws_dir.bz" z_ws="$ws_dir.zl"
    rm -rf "$b_ws" "$z_ws"; cp -R "$ws_dir" "$b_ws"; cp -R "$ws_dir" "$z_ws"
    local b_ws_lock="$b_ws/MODULE.bazel.lock" z_ws_lock="$z_ws/MODULE.bazel.lock"
    (cd "$b_ws" && eval "$BAZEL_BIN --output_base=\"$b_ob\" $cmd $extra_flags" >"$b_out" 2>"$b_err") || be=$?
    (cd "$z_ws" && eval "$ZEL_BIN --output_base=\"$z_ob\" $cmd $extra_flags" >"$z_out" 2>"$z_err") || ze=$?

    local st=""
    if [ "$be" != "$ze" ]; then
        st="EXIT b=$be z=$ze"
    fi
    if ! diff -q <(norm_stdout "$b_out" "$b_ob" "$b_ws") <(norm_stdout "$z_out" "$z_ob" "$z_ws") >/dev/null 2>&1; then
        st="$st STDOUT"
    fi
    if ! diff -q <(norm_stderr "$b_err" "$b_ob" "$b_ws") <(norm_stderr "$z_err" "$z_ob" "$z_ws") >/dev/null 2>&1; then
        st="$st STDERR"
    fi
    if ! diff -q <(norm_lock "$b_ws_lock" "$b_ob" "$b_ws") <(norm_lock "$z_ws_lock" "$z_ob" "$z_ws") >/dev/null 2>&1; then
        st="$st LOCKFILE"
    fi

    if [ -n "$check_tree" ]; then
        local b_matches
        b_matches=$(find "$b_ob" -path "*/$check_tree" 2>/dev/null | head -n 1)
        local z_matches
        z_matches=$(find "$z_ob" -path "*/$check_tree" 2>/dev/null | head -n 1)
        if [ -n "$b_matches" ] && [ -n "$z_matches" ]; then
            if ! diff -q "$b_matches" "$z_matches" >/dev/null 2>&1; then
                st="$st ARTIFACT"
            fi
        elif [ -n "$b_matches" ] || [ -n "$z_matches" ]; then
            st="$st ARTIFACT_MISSING"
        fi
    fi

    if [ -z "$st" ]; then
        passed=$((passed + 1))
        echo "  [OK] $desc"
    else
        failed=$((failed + 1))
        echo "  [$st] $desc ($cmd in $(basename "$ws_dir"))"
        if [ "${VERBOSE:-0}" -gt 0 ]; then
            echo "--- STDOUT DIFF ---"
            diff -u <(norm_stdout "$b_out" "$b_ob" "$b_ws") <(norm_stdout "$z_out" "$z_ob" "$z_ws") || true
            echo "--- STDERR DIFF ---"
            diff -u <(norm_stderr "$b_err" "$b_ob" "$b_ws") <(norm_stderr "$z_err" "$z_ob" "$z_ws") || true
            echo "--- LOCKFILE DIFF ---"
            diff -u <(norm_lock "$b_ws_lock" "$b_ob" "$b_ws") <(norm_lock "$z_ws_lock" "$z_ob" "$z_ws") || true
        fi
    fi
}

run_test() {
    run_test_ws "$WS" "$1" "$2" "${3:-$REG_FLAG}" "${4:-}"
}

echo "=== Running Bzlmod Differential Verification (Zel vs Bazel 9.3.2) ==="

# 1. Mod graph commands
run_test "mod graph" "mod graph"
run_test "mod graph text" "mod graph --output=text"
run_test "mod graph json" "mod graph --output=json"
run_test "mod graph verbose" "mod graph --verbose"
run_test "mod graph depth 1" "mod graph --depth=1"
run_test "mod graph depth 2" "mod graph --depth=2"
run_test "mod graph depth 3" "mod graph --depth=3"
run_test "mod graph include_unused" "mod graph --include_unused"
run_test "mod graph from module c@1.0" "mod graph --from=c@1.0"
run_test "mod graph from module b@1.0" "mod graph --from=b@1.0"
run_test "mod graph from module a@1.1" "mod graph --from=a@1.1"
run_test "mod graph extension_info all" "mod graph --extension_info=all"
run_test "mod graph extension_info usages" "mod graph --extension_info=usages"
run_test "mod graph extension_info repos" "mod graph --extension_info=repos"
run_test "mod graph charset ascii" "mod graph --charset=ascii"
run_test "mod graph charset utf8" "mod graph --charset=utf8"
run_test "mod graph extension_filter" "mod graph --extension_filter=@ext//:ext.bzl%ext"

# 2. Mod deps commands
run_test "mod deps <root>" "mod deps '<root>'"
run_test "mod deps <root> json" "mod deps '<root>' --output=json"
run_test "mod deps <root> depth 1" "mod deps '<root>' --depth=1"
run_test "mod deps specific c@1.0" "mod deps c@1.0"
run_test "mod deps c@1.0 depth 1" "mod deps c@1.0 --depth=1"
run_test "mod deps b@1.0" "mod deps b@1.0"
run_test "mod deps b@1.0 depth 1" "mod deps b@1.0 --depth=1"
run_test "mod deps a@1.1" "mod deps a@1.1"
run_test "mod deps devm@1.0" "mod deps devm@1.0"
run_test "mod deps include_unused" "mod deps --include_unused"
run_test "mod deps no args (root only)" "mod deps"
run_test "mod deps no args json" "mod deps --output=json"

# 3. Mod paths & explain
run_test "mod all_paths a@1.1" "mod all_paths a@1.1"
run_test "mod all_paths a@1.1 json" "mod all_paths a@1.1 --output=json"
run_test "mod all_paths c@1.0" "mod all_paths c@1.0"
run_test "mod all_paths b@1.0" "mod all_paths b@1.0"
run_test "mod all_paths devm@1.0" "mod all_paths devm@1.0"
run_test "mod all_paths depth 1" "mod all_paths --depth=1 a@1.1"
run_test "mod path a@1.1" "mod path a@1.1"
run_test "mod path c@1.0" "mod path c@1.0"
run_test "mod path b@1.0" "mod path b@1.0"
run_test "mod path devm@1.0" "mod path devm@1.0"
run_test "mod explain a" "mod explain a"
run_test "mod explain a json" "mod explain a --output=json"
run_test "mod explain a text" "mod explain --output=text a"
run_test "mod explain b" "mod explain b"
run_test "mod explain c" "mod explain c"
run_test "mod explain devm" "mod explain devm"
run_test "mod explain nodep" "mod explain nodep"

# 4. Mod show_repo & show_extension
run_test "mod show_repo canonical @@c+" "mod show_repo @@c+"
run_test "mod show_repo canonical @@b+" "mod show_repo @@b+"
run_test "mod show_repo canonical @@a+1.1" "mod show_repo @@a+1.1"
run_test "mod show_repo canonical @@devm+" "mod show_repo @@devm+"
run_test "mod show_repo apparent @aa" "mod show_repo @aa"
run_test "mod show_repo apparent @c" "mod show_repo @c"
run_test "mod show_repo apparent @devm" "mod show_repo @devm"
run_test "mod show_repo extension repo @from_main" "mod show_repo @from_main"
run_test "mod show_repo canonical extension repo" "mod show_repo @@ext~ext~from_main"
run_test "mod show_repo json @aa" "mod show_repo @aa --output=streamed_jsonproto"
run_test "mod show_repo json @@c+" "mod show_repo @@c+ --output=streamed_jsonproto"
run_test "mod show_repo json @c" "mod show_repo @c --output=streamed_jsonproto"

# 5. Mod show_extension
run_test "mod show_extension apparent label" "mod show_extension @ext//:ext.bzl%ext"
run_test "mod show_extension canonical label" "mod show_extension @@ext+//:ext.bzl%ext"
run_test "mod show_extension json" "mod show_extension @ext//:ext.bzl%ext --output=json"

# 6. Mod dump_repo_mapping
run_test "mod dump_repo_mapping root" "mod dump_repo_mapping ''"
run_test "mod dump_repo_mapping root json" "mod dump_repo_mapping '' --output=json"
run_test "mod dump_repo_mapping c+" "mod dump_repo_mapping c+"
run_test "mod dump_repo_mapping b+" "mod dump_repo_mapping b+"
run_test "mod dump_repo_mapping a+" "mod dump_repo_mapping a+"
run_test "mod dump_repo_mapping devm+" "mod dump_repo_mapping devm+"

# 7. Lockfile mode checks
run_test "lockfile_mode off" "mod graph --lockfile_mode=off"
run_test "lockfile_mode update" "mod graph --lockfile_mode=update"
run_test "lockfile_mode refresh" "mod graph --lockfile_mode=refresh"
run_test "lockfile_mode error" "mod graph --lockfile_mode=error"

# 8. MVS Yanked Version Enforcement
run_test "yanked version rejected without flag" "mod graph" "--registry=file://$FX_DIR/reg"
run_test "yanked version allowed specific" "mod graph --allow_yanked_versions=a@1.1"
run_test "yanked version allowed all" "mod graph --allow_yanked_versions=all"

# 9. Overrides on main workspace
run_test "override_module a" "mod graph --override_module=a=%workspace%/mods/a+1.2"
run_test "override_module b" "mod graph --override_module=b=%workspace%/mods/b+1.0"

# 10. Fetch command checks
run_test "fetch repo @aa" "fetch --repo=@aa"
run_test "fetch repo @c" "fetch --repo=@c"
run_test "fetch repo @from_main" "fetch --repo=@from_main"
run_test "fetch repo @@c+" "fetch --repo=@@c+"
run_test "fetch unknown canonical repo exit 8" "fetch --repo=@@unknown"
run_test "fetch unknown apparent repo exit 8" "fetch --repo=@unknown"
run_test "fetch repo=@ exit 8" "fetch --repo=@"
run_test "fetch repo=@@ exit 8" "fetch --repo=@@"

# 11. Vendor command checks
run_test "vendor repo @aa" "vendor --vendor_dir=vend_aa --repo=@aa"
run_test "vendor repo @c" "vendor --vendor_dir=vend_c --repo=@c"
run_test "vendor repo @from_main" "vendor --vendor_dir=vend_fm --repo=@from_main"
run_test "vendor unknown repo exit 8" "vendor --vendor_dir=vend_un --repo=@unknown"
run_test "vendor repo=@ exit 8" "vendor --vendor_dir=vend_at --repo=@"

# Differential test workspaces generated by setup_test_cases.py
CASES_DIR="$SCRATCH/cases"

# 12. MODULE.bazel evaluation & syntax restrictions
run_test_ws "$CASES_DIR/ws_if" "eval restriction if" "mod graph"
run_test_ws "$CASES_DIR/ws_for" "eval restriction for" "mod graph"
run_test_ws "$CASES_DIR/ws_def" "eval restriction def" "mod graph"
run_test_ws "$CASES_DIR/ws_load" "eval restriction load" "mod graph"
run_test_ws "$CASES_DIR/ws_unknown_fn" "eval restriction unknown_fn" "mod graph"
run_test_ws "$CASES_DIR/ws_syntax" "syntax error unclosed paren" "mod graph"
run_test_ws "$CASES_DIR/ws_dup_mod" "duplicate module call" "mod graph"
run_test_ws "$CASES_DIR/ws_mod_after" "module call after other rules" "mod graph"
run_test_ws "$CASES_DIR/ws_dup_dep" "duplicate bazel_dep" "mod graph"
run_test_ws "$CASES_DIR/ws_self_dep" "self bazel_dep" "mod graph"
run_test_ws "$CASES_DIR/ws_bad_mod_name" "bad module name" "mod graph"
run_test_ws "$CASES_DIR/ws_bad_repo_name" "bad repo name in dep" "mod graph"
run_test_ws "$CASES_DIR/ws_no_ver" "bazel_dep without version" "mod graph"
run_test_ws "$CASES_DIR/ws_multi_ver_one" "multiple_version_override with single version" "mod graph"
run_test_ws "$CASES_DIR/ws_fail" "fail() in MODULE.bazel" "mod graph"

# 13. include() validation
run_test_ws "$CASES_DIR/ws_inc_ok" "include root package ok" "mod graph"
run_test_ws "$CASES_DIR/ws_inc_sub" "include sub package ok" "mod graph"
run_test_ws "$CASES_DIR/ws_inc_missing" "include missing file" "mod graph"
run_test_ws "$CASES_DIR/ws_inc_bad_ext" "include bad extension" "mod graph"
run_test_ws "$CASES_DIR/ws_inc_dot" "include dot file" "mod graph"
run_test_ws "$CASES_DIR/ws_inc_rel" "include relative label" "mod graph"
run_test_ws "$CASES_DIR/ws_inc_repo" "include repo label" "mod graph"
run_test_ws "$CASES_DIR/ws_inc_cycle" "include cycle" "mod graph"

# 14. bazel_compatibility validation
run_test_ws "$CASES_DIR/ws_compat_ok" "bazel_compatibility ok" "mod graph"
run_test_ws "$CASES_DIR/ws_compat_fail" "bazel_compatibility fail" "mod graph"
run_test_ws "$CASES_DIR/ws_compat_neg" "bazel_compatibility negative exclusion" "mod graph"
run_test_ws "$CASES_DIR/ws_compat_neg_ok" "bazel_compatibility negative ok" "mod graph"
run_test_ws "$CASES_DIR/ws_compat_bad" "bazel_compatibility bad syntax" "mod graph"

# 15. Overrides & extension usages
run_test_ws "$CASES_DIR/ws_lpo_ok" "local_path_override ok" "mod graph"
run_test_ws "$CASES_DIR/ws_lpo_missing" "local_path_override missing dir" "mod graph"
run_test_ws "$CASES_DIR/ws_override_ws" "override_module %workspace%" "mod graph --override_module=dep=%workspace%/sub_dep"
run_test_ws "$CASES_DIR/ws_git_noargs" "git_override missing commit/tag/branch" "mod graph"
run_test_ws "$CASES_DIR/ws_use_repo_rule_dev" "use_repo_rule bad dev_dep keyword" "mod graph"
run_test_ws "$CASES_DIR/ws_isolate_err" "use_extension isolate without flag" "mod graph"

# 16. repository_ctx and module_ctx exactness
run_test_ws "$CASES_DIR/ws_ctx_unknown_kw" "rctx template unknown keyword" "fetch --repo=@rr"
run_test_ws "$CASES_DIR/ws_ctx_mctx_delete" "mctx delete not supported" "fetch --repo=@foo"
run_test_ws "$CASES_DIR/ws_ctx_watch_workdir" "rctx watch under working directory" "fetch --repo=@rr"
run_test_ws "$CASES_DIR/ws_ctx_read_watch_bad" "rctx read bad watch value" "fetch --repo=@rr"
run_test_ws "$CASES_DIR/ws_ctx_delete_outside" "rctx delete outside returns false" "fetch --repo=@rr"
run_test_ws "$CASES_DIR/ws_ctx_which_slash" "rctx which with slash errors" "fetch --repo=@rr"
run_test_ws "$CASES_DIR/ws_ctx_int_hex" "int radix 16 with 0x" "fetch --repo=@rr"
run_test_ws "$CASES_DIR/ws_ctx_dir_rctx" "dir(rctx) exposes methods" "fetch --repo=@rr"
run_test_ws "$CASES_DIR/ws_ctx_dir_mctx" "dir(mctx) exposes methods" "fetch --repo=@foo"
run_test_ws "$CASES_DIR/ws_ctx_label_str" "str(Label) in extension is canonical @@" "fetch --repo=@foo"
run_test_ws "$CASES_DIR/ws_ctx_attr_bad" "repo rule unknown attribute" "fetch --repo=@rr"
run_test_ws "$CASES_DIR/ws_ctx_ext_return_int" "extension invalid return int" "fetch --repo=@foo"
run_test_ws "$CASES_DIR/ws_ctx_ext_meta_bad" "extension_metadata bad root_module_direct_deps" "fetch --repo=@foo"
run_test_ws "$CASES_DIR/ws_ctx_exec_timeout" "rctx execute timeout handling" "fetch --repo=@rr"
run_test_ws "$CASES_DIR/ws_ctx_exec_nonexist" "rctx execute nonexistent binary" "fetch --repo=@rr"

# 17. Native repo rules
run_test_ws "$CASES_DIR/ws_native_new_local" "new_local_repository native rule" "fetch --repo=@loc"
run_test_ws "$CASES_DIR/ws_native_http_file" "http_file native rule" "fetch --repo=@hf"

# 18. Archive codecs and extraction
run_test_ws "$CASES_DIR/ws_tar_symlink_dotdot" "tar symlink dotdot outside repo" "fetch --repo=@ar"
run_test_ws "$CASES_DIR/ws_bare_gz" "bare gz extraction with FNAME" "fetch --repo=@gzfile"
run_test_ws "$CASES_DIR/ws_zip_extract" "zip archive extraction" "fetch --repo=@zar"

# 19. Build-based cases with genrules and stub platforms
run_test_ws "$CASES_DIR/ws_build" "build genrule g1" "build //:g1" "$REG_FLAG" "bin/g1.txt"
run_test_ws "$CASES_DIR/ws_build" "build genrule g2" "build //:g2" "$REG_FLAG" "bin/g2.txt"
run_test_ws "$CASES_DIR/ws_build" "build genrule g3" "build //:g3" "$REG_FLAG" "bin/g3.txt"
run_test_ws "$CASES_DIR/ws_build" "build genrules g1 g2" "build //:g1 //:g2" "$REG_FLAG"
run_test_ws "$CASES_DIR/ws_build" "build all targets //:all" "build //:all" "$REG_FLAG"
run_test_ws "$CASES_DIR/ws_build" "build nobuild flag" "build --nobuild //:all" "$REG_FLAG"
run_test_ws "$CASES_DIR/ws_build" "query deps of g3" "query 'deps(//:g3)'" "$REG_FLAG"
run_test_ws "$CASES_DIR/ws_build" "query kind genrule" 'query "kind(\"genrule\", //:all)"' "$REG_FLAG"
run_test_ws "$CASES_DIR/ws_build" "query allpaths g3 to g1" "query 'allpaths(//:g3, //:g1)'" "$REG_FLAG"
run_test_ws "$CASES_DIR/ws_build" "test all targets //:all" "test //:all" "$REG_FLAG"

echo ""
echo "=== Summary: $passed / $total passed ($failed failed) ==="

if [ "$failed" -gt 0 ]; then
    exit 1
fi
