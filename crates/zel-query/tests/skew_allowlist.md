# Query differential harness: known, accepted differences

Harness: `scripts/query_harness/run.py` (see its docstring; `ZEL_BIN`, `BAZEL_BIN`, `QH_ROOT`).
Baselines of real Bazel 9.3.2 live in `scripts/query_harness/baseline/`.

1. **Nondeterministic in Bazel itself** (hash-ordered `Digraph`/identity hash codes): `--output=graph|minrank|maxrank`
   with `--order_output=auto|no|deps`, and `--order_output=no`. Only the set of nodes/edges is comparable
   (`STDOUT(order-only)` rows in the harness).
2. **Binary proto length prefixes**: the harness normalizes the workspace path to `@WS@` *after* the
   run, so length-delimited `streamed_proto` output differs by the path-length difference of the two
   workspace copies (decoded `--output=proto` is compared field by field and is not affected).
3. **`@bazel_tools` BUILD line numbers** (e.g. `src/conditions/BUILD:115:15` vs `:124:15`): zel embeds the
   `@bazel_tools` sources of Bazel HEAD, the 9.3.2 binary embeds the release sources.
4. **(was: `rdeps(..., depth)` located edge error — NOT skew, fixed in round 6A.)** The located
   `ERROR: <BUILD>:L:C: no such package 'X': ... - X and referenced by '//pkg:target'` line is printed
   by `Engine::report_closure_errors` for `deps`/`rdeps` with and without depth, with and without
   `--keep_going`. There is no allowlist entry for it any more.
5. **`rule { test, executable }` of `--output=proto`**: HEAD's `ProtoOutputFormatter.toTargetProtoBuffer`
   (`query2/query/output/ProtoOutputFormatter.java:192-193`) sets `Rule.test`/`Rule.executable`; the 9.3.x
   binary does not (decoded output has neither field). Added to HEAD by bazel commit `0b40802204`
   (2026-08-19, PR 27955 "Add `test` and `executable` to `Rule` protobuf message"), after 9.3.1. zel follows HEAD;
   the harness strips these two lines before comparing.
6. **`$rule_implementation_hash` value** of Starlark rules: the structure (last, unsorted STRING attribute)
   follows `ProtoOutputFormatter.java:199-207`, but the digest is `BzlLoadFunction`'s
   `fp.addBytes(compileValue.getDigest()) ... getAndDigestPredeclaredEnvironment(...)`, which folds in the
   transitive digest of the `@_builtins` pseudo-repository of the *binary* (`builtins.transitiveDigest`);
   zel has no such counterpart, so the hex value differs (harness class `hash-only`).
7. **Rule-class table**: `scripts/verify_rule_defs_vs_bazel.py` diffs the source-derived table against the
   binary; the differences are all HEAD-vs-9.3.x (`KNOWN_SKEW` there, each with its source evidence):
   `label_flag/label_setting.on_leave_scope` (`LabelBuildSettings.java`), `platform.remote_execution_properties`
   removed (`PlatformRule.java`), `:collect_coverage_script` vs `$collect_coverage_script`
   (`StarlarkRuleClassFunctions.java`).
8. **Client startup-option warning text** (`WARNING: Running Bazel server needs to be killed ...  - Only in
   old server: --workspace_directory=... bazel(ws)`): printed by the client when a shard alternates
   workspaces; zel's text lacks the `bazel(<ws>)` pseudo-option. Not a query difference (the harness
   ignores these lines).
9. **Strict action environment default** (`aquery` `Environment:` / `environment_variables` and, through the
   `ActionEnvironment` fingerprint, the `ActionKey` of every action using the default shell env, e.g. all
   `Genrule` actions): HEAD's `BazelRuleClassProvider.pathOrDefault` (`BazelRuleClassProvider.java:355-366`)
   yields `PATH=/bin:/usr/bin:/sbin:/usr/sbin` plus `LC_CTYPE=C.UTF-8`; the 9.3.2 server jar's
   `BazelRuleClassProvider` contains the constant `/bin:/usr/bin:/usr/local/bin` and no `LC_CTYPE`
   (`javap -c -constants` of `A-server.jar`). zel follows HEAD. With `--action_env=PATH=/bin:/usr/bin:/usr/local/bin
   --action_env==LC_CTYPE` zel reproduces the real Genrule `ActionKey` bit for bit (`63704e26…`), so the key
   computation itself is verified; the harness (`run.py`) no longer blanks those keys: zel is run with `ZEL_ACTION_KEY_COMPAT=9.3.2`, which makes `action_key.rs` use the 9.3.2 layout (`KeyFlavor::V932`: `remoteExecutionProperties`, trailing-period `missing_toolchain_error`, no unset env, and 9.3.2's default `PATH`/no `LC_CTYPE`), so the keys are compared bit for bit against the real 9.3.2 server. zel's own default keys follow HEAD (`PlatformInfo.addTo`, `PlatformRule.DEFAULT_MISSING_TOOLCHAIN_ERROR`, `ActionEnvironment.addTo`). Only the *printed* env (`PATH=`/`LC_CTYPE`) is rewritten by `skew()`.
10. **Keys of `SourceSymlinkManifest` / `SymlinkTree` actions**: `SourceManifestAction.computeKey` and
    `SymlinkTreeAction.computeKey` call `Runfiles.fingerprint(..., digestAbsolutePaths = true)`
    (`SourceManifestAction.java:362-368`, `SymlinkTreeAction.java:246-253`), i.e. they contain absolute execroot
    paths, which depend on the output base. Keys can only agree when both servers use the same output base; the
    harness uses different ones, so `run.py: skew()` blanks these keys (only these two mnemonics).
11. **`Action.progress_message` in aquery protos**: HEAD's `ActionGraphDump.dumpSingleAction` (`ActionGraphDump.java:189-192`)
    calls `setProgressMessage`; the 9.3.2 jar's `ActionGraphDump` has no such call (`javap -c`, 0 occurrences), so real
    output carries no `progress_message`. zel follows HEAD; `run.py: skew()` drops the field before comparing.
9. **Server-state dependent analysis errors** (`aquery //tests:mixed` after other commands in the same server,
   verifier cmds 1984–2014): a fresh real Bazel server reports the `test_suite` visibility error
   (`cquery //tests:mixed` → `ERROR: ... in test_suite rule //:all_t: Visibility error: ...`, verified by hand
   on a fresh output base), while the same command after an analysis-cache discard in a long-lived server
   printed `Found 2 targets` / `Empty query results`. zel implements the fresh-server behaviour; the
   state-dependent variant is not modelled. `target_compatible_with` edges are *not* visibility-checked
   (verified on real Bazel 9.3.1: `cquery //b:x` with a package-private `constraint_value` succeeds).
10. **`query //...` over a workspace mixing a package that cannot be loaded (failing `load()`) with packages that
    only contain errors** (verifier items 12/1/2; `wsbroken` of both fixture sets): real Bazel's stderr is
    nondeterministic — 4 fresh servers on the same tree produced 3 distinct outputs (md5 of stderr); the
    `package contains errors: <pkg>` lines and which fatal error ends the evaluation depend on which package
    loads complete before the abort (`RecursivePackageProvider` parallel loading). zel prints the located
    package events, then a single `error loading package under directory '<dir>': <fatal>` (no
    `package contains errors` lines) when a package fails fatally without `--keep_going`.
12. **`--incompatible_package_group_build_output`** (`CommonQueryOptions.java:173` in HEAD): accepted by zel, rejected
    by the 9.3.2 binary (flag absent there). HEAD-valid, so a version skew, not a zel difference.
13. **`ActionKey` layout**: see 9. Real 9.3.2 keys differ from zel's default (HEAD) keys by exactly the three layout
    changes above; the harness compares them in compat mode instead of ignoring them.
