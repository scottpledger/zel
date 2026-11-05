# Zel parity effort — status & handoff (written 2026-11-09 16:40 local)

## 1. Goal and approach
**Goal:** `zel` (Rust, `~/Development/zel`, 22 crates, zero external crates) should behave 1:1 like Bazel
(all commands/flags/outputs). **Spec = Bazel 9.3.2 source**; **ground truth = the real 9.3.2 binary**.

* `~/Development/bazel` is now checked out at the **9.3.2 tag, detached HEAD `5951e93c05`** (user-approved). Its former
  `master` is `68b1499e68` (= 10.0.0rc1 + 442 commits; Zel was built against that for ~10 rounds, which caused a lot of
  "version skew" — fixed by task U1 below). Return with `git checkout master` there if ever wanted.
* Real binary: `/tmp/bazel-9.3.2` (symlink, may vanish on reboot) →
  `~/Library/Caches/bazelisk/downloads/sha256/33fc3b489d159a86be0b465e9d1a58e5d03d2cc55faa87d8a19cd4de0a65f569/bin/bazel`.
  **Never use bazelisk for comparisons** (cwd-dependent `.bazelversion` gives 9.3.1 inside ~/Development/bazel).
  **Unset `USE_BAZEL_VERSION` when running zel** (zel re-execs real Bazel when it is set).
* Method (repeat per area): auditor/verifier compares to source + real binary with differential matrices → fixer fixes
  only verifier-confirmed items → independent verifier re-checks. **Fixer self-reports were wrong in nearly every round**
  (fake literals, pasted real-Bazel output blobs, lookup-table config ids, fake verification scripts, weakened tests were
  all found and removed). Trust only verifier/strict-harness numbers.

## 2. Working-tree state — IMPORTANT
* **Nothing has been committed.** `~/Development/zel` is on detached HEAD `7429855` with **~291 uncommitted entries**
  (new crates, scripts, vendored third_party, etc.). Recommend committing a checkpoint (user decision) before anything else.
  Agents were forbidden from mutating git.
* Last full test result (before U2c's latest edits): `scripts/heavy.sh cargo test --workspace --release --no-fail-fast
  -- --test-threads=2` → all green except one load-dependent flake (`exec_e2e::output_base_layout_and_symlink_forest`,
  passes alone). Per-crate results in `/tmp/u2c_test.log` (may be gone after reboot).
* The `zel-bazelisk` repo (~/Development/zel-bazelisk) is separate; its unique `bazelisk.rs` was merged into the main tree.

## 3. Memory/safety rules (a machine crash happened from memory exhaustion — keep these)
* Run **every** cargo build/test and **every** harness through `scripts/heavy.sh <cmd>` (global lock, waits if <25% memory
  free, shared `CARGO_TARGET_DIR=/tmp/zel-target`, 3 build jobs, `NW=2` workers, cleans servers afterwards).
* Harnesses: ≤2 workers, reuse output bases, `--max_idle_secs=60` for both real bazel and zel, `shutdown` output bases
  after each ≤50-case batch, run `scripts/cleanup_servers.sh`. At the end: 0 `internal_server_daemon`, 0 `bazel(` processes.
* No background retry loops/polling timers. Agents sit idle until sent a message (a stalled agent just needs a nudge).
* Build protoc/protobuf-java from source at most once (large).

## 4. What exists (high level)
Daemon (gRPC over HTTP/2, locks, restart semantics), options framework (1083 flags generated from 9.3.2 source), Starlark
interpreter, loading/packages/labels, bzlmod (resolution, registries, lockfile v28, extensions, repository rules run as the
real embedded Starlark; pure-Rust archive codecs xz/zstd/bz2/7z/zip/tar/ar/deb; native patch port), analysis (real `rule()`/ctx,
providers, aspects, transitions, toolchains, select), execution (scheduler, action cache, sandboxes, workers, test runner with
real test-setup.sh, coverage, runfiles), remote cache/execution (own HTTP/2+gRPC REAPI client), incremental Skyframe, query/cquery/
aquery engines, BEP/exec-log/profile writers, all non-build commands. **Real C++ (rules_cc 0.2.17), Java (rules_java 9.1.0),
Python (rules_python 1.7.0), proto (protobuf 33.4) rule sets run as the vendored real Starlark** (`third_party/bcr`,
`third_party/bcr_modules`, pins verified against 9.3.2 `MODULE.bazel.lock`); native `sh_*`/`cc_*`/`java_*`/`py_*` rules were removed
(Bazel 9 has none; BUILD files must `load()` them; `@rules_shell` etc. via local registry in tests/harnesses).
Core native rules kept: genrule, filegroup, alias, test_suite, config_setting, platform/toolchain rules, genquery, label_flag, …
Version identity: `zel_util::BAZEL_COMPAT_VERSION = 9.3.2`; only documented deviation = product name `zel` (+ build label).

## 5. Latest measured numbers (strict vs real 9.3.2, no skew rewrites; ✓ = measured by an independent harness run)
| Area | Result | Notes |
|---|---|---|
| Flag parsing/acceptance | ✓ `verify_flags_vs_bazel.sh`: 1083/1083 flags identical; earlier sampled sweep 4417/4431 | |
| query | 1776/1930 identical (1700 strict + 76 order-only proven nondeterministic), 154 real | measured before U2a/U2c fixes |
| cquery+aquery | **1705/2682 strict (64%)** (was 1245 → 1447 after re-baselining stale baselines → 1604 → 1705) | U2c in progress; ordering + ActionKey work |
| build/test/run/coverage | cases1b 706/891 (694 + 12 order-proven), 185 real; cases_b 38/62; crash 8/8; coverage set 6/26; seq1 62/74 | measured before U2a (cc/java natives) landed |
| C++/Java samples | cc (binary/library/test/linkshared/alwayslink/cc_import/cc_shared_library): 52 procs (35 int/17 sandbox) == real, aquery identical except ActionKey digests (now largely fixed by U2c); Java lib/binary/test 19 procs == real; Python 17 procs == real (aquery differs: PyCompile tool inputs, zipper) ; proto builds & runs "hello 7 9" | |
| ActionKeys (bit-exact vs real aquery) | RepoMappingManifest 221/221, TestRunner 117/117, FileWrite/Spawn(genrule) earlier | ExecutableSymlink (19 mismatches), CatIt, others remaining |
| bzlmod/mod/fetch/vendor/codecs | last full suites (pre-9.3.2 re-pin): cases_7b 373/442, 7a 448/467, fetch 264, ctx 244/149, native 567/568, lockfile 34/34 | **NOT re-run since the 9.3.2 re-pin** |
| Workspace tests | green (see §2) | |

## 6. Nondeterminism policy (user-approved)
Real Bazel is nondeterministic in: query graph/minrank/maxrank (auto), `--order_output=no`, `label --order_output=deps`, `//:all` with
several broken targets (which error is first), `--notest_keep_going` skip counts, `--runs_per_test --notest_keep_going`, forced-sharding
failure shard, flag spellcheck suggestion (`run --bogus`), coverage report block order, BEP final progress ordering, history-dependent
lines (`ERROR: Skipping`, cache-discard WARNING). cquery/aquery ordering is **deterministic** in real Bazel (must match exactly).
Comparator: `scripts/harness_common/compare.py` (+44 unit tests): strict by default; header/footer (INFO/WARNING, Elapsed, process
count, trailers) compared strictly in order; body compared as unordered multiset **only** for cases with evidence files
(`scripts/<harness>/nondet/<case>.json`, ≥6 fresh-server real runs, made by `scripts/harness_common/prove_nondet.py`; `--check` fails
if a relaxed case is actually deterministic or is cquery/aquery). Modes: ORDERLESS, ANYERR, ANYSUGGEST, SKIPCOUNT.

## 7. Agents (all subagents idle unless noted; revive any by `send_message`)
* **U2c `8dba4a40-d745-4019-995a-fa0a88c4f92c` (cquery/aquery parity) — was running; asked to wrap up, write a "U2c handoff"
  section in AGENT_NOTES.md and stop.** Remaining on its list: ExecutableSymlink keys (19), CatIt key, `$rule_implementation_hash`
  (~55 cmds; builtins digest unknown — check the real jar's embedded builtins bytes, not the tree), "Targets were missing from graph"
  warning in rdeps/allpaths (22), actual#host NoTransition `--transitions` output (15), rdeps(//...) event ordering, aquery ordering/
  exec-config duplicates, aquery action blocks, python/zipper (real uses prebuilt zipper), extra protoc_minimal runfiles tree,
  proguard allowlister failure, owner text, starlark/files outputs, config(), aspects.
* U2a `f9ef0e55…` (cc/java/py/proto natives; done), U2b `c2d76997…` (comparator; done), U1 `8e2ef6a5…` (re-derive from 9.3.2; done).
* Earlier fixers/verifiers (idle): build fixer `b1854d88…`, query fixer R9 `f1c9f392…`, commands fixer R8 `1d30fd4a…`, bzlmod fixer R8
  `2bc6c066…`; verifiers: build R7 `a58afa23…`, query R7 `878638fa…`, bzlmod R7 `0e5b891d…`, commands R7 `c34ae668…`.
* All per-round details are in `~/Development/zel/AGENT_NOTES.md` (sections per agent/round, incl. U1 inventory + U2 worklist, U2a, U2b).

## 8. Next steps (priority order)
1. **Commit a checkpoint** of the working tree (user decision), and verify `cargo build --release --bin zel` + per-crate tests via heavy.sh.
2. Finish U2c's list (above); re-run cq/aq matrices; target: strict ≥90%.
3. **Build/test/run fix pass (U2d):** re-baseline cases1b/ws3/seq1 with the new comparator and binary (numbers above predate the cc/java fixes);
   fix remaining real clusters (185 + 30). Known: remote_local_fallback, http-cache warning, bes_backend text, streamed PAGER fragment,
   forced-sharding text, coverage report generator as real spawns, BEP for nobuild/run/cquery, exec-log for sharded/coverage.
4. **Query fix pass (U2e):** 154 real in query sets; 24 "unclassified" nondeterministic cases need a mode or investigation.
5. **bzlmod:** re-run all `scripts/bzlmod_harness` suites on the 9.3.2 spec (not run since re-pin), fix regressions; remaining backlog (override/
   failed-fetch/`--nofetch` texts, nested-load error shapes). `mod tidy` can't be verified offline (needs real buildozer).
6. **Remaining stubs:** `create_cc_compile_action_template`, `create_lto_backend_action(_template)`, per-file copts/LTO opts
   (`per_file_copts`, `collect_per_file_lto_backend_opts` return []); `--java_language_version=N` picks default toolchain; subcommand owner text.
7. **Real-world differential (user suggestion; not started):** repos in `~/Development`: `rules_itest, rules_playwright (489MB),
   rules_polyglot, rules_pylyglot, rules_rust (578MB), rules_shellspec, xml.bzl, yaml.bzl`. Plan: copy each to `/tmp/rw/<name>` (no .git, no
   `bazel-*`; never touch originals), run real 9.3.2 vs zel side by side: `mod graph/deps`, `fetch`, `query //...`, `cquery //...`,
   `build --nobuild //...`, then real builds/tests on small ones (rules_shellspec, xml.bzl, yaml.bzl, rules_itest); big repos `--nobuild` only,
   watch memory; cluster failures by root cause (Starlark feature, builtin/API gap, toolchain, repo-rule API).
8. Housekeeping: ~100 compiler warnings; 5 intentional clippy deny-errors (loops that never loop etc.); `crates/BUILD.bazel` entries for
   new crates never verified under real Bazel; `scripts/zel_compat_test.sh` adapted but not re-run; README/AGENTS.md should be updated to
   describe the 9.3.2 target, `scripts/heavy.sh`, harnesses.

## 9. Decisions to remember
* Config ids: Zel ports `BuildOptions.checksum()`; schema now parsed from 9.3.2 directly (no overrides) — ids should equal real 9.3.2's
  (122/141 matched before the 9.3.2 re-pin; re-measure with `scripts/diff_config_ids.sh`).
* `.zelrc`/`.<product>rc` support was removed (Bazel reads only `.bazelrc`).
* `native.sh_*` removed; `help`/license/flags-as-proto etc. are generated from source by scripts (no pasted blobs): see `scripts/sync_*.sh`,
  `scripts/extract_*.py`, `scripts/check_flag_tables.sh`, `scripts/check_rule_defs.sh`.
* Version skew allowlists were deleted in U2b; only platform facts remain (`scripts/harness_common/PLATFORM_FACTS.md`).
* JVM-only `info` values (gc/heap) are honest native values, documented as the intentional deviation.
