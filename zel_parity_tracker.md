# Zel parity tracker (status 2026-11-08, after fix round 8 / verifier round 7)

Method: verifiers (inherit model) compare each area to Bazel HEAD source (spec) and real Bazel 9.3.x (ground
truth; HEAD-vs-9.3.2 version skew is documented per area with source evidence); fixers fix confirmed items.
Fixer "closed" claims were wrong in nearly every round: only verifier numbers are authoritative.

Whole workspace: `scripts/heavy.sh cargo test --workspace --release --no-fail-fast -- --test-threads=2`
=> 79 suites ok, 0 failed (2026-11-08). 0 leaked daemons/servers afterwards.

| Area | Latest independent verifier | Latest fixer self-report (same harness family) | Main open items |
|---|---|---|---|
| build/test/run/coverage | R7: ~164/891 byte-identical on raw stderr when the process line is compared; 38 real items | cases1b 673/891 identical, 120 real (30-35 nondeterministic //:all), seq1 62/74, exec-log 24/28, BEP 23/28, profile_core 25/28 | remote_local_fallback, http cache warning, bes_backend text, streamed PAGER, forced-sharding text, real coverage-report generator spawns (Java/cc skew), BEP for nobuild/run/cquery |
| query/cquery/aquery | R7: 1003/1859 (query 672/807, cquery 229/649, aquery 102/403) | 1175/1859 after R9 (query 691/807, cquery 359/649, aquery 125/403) | RunfilesTree/RepoMapping/SymlinkTree action keys, cquery ordering (java_order not exact), build_options()/providers(), ~360 of 684 diffs are Bazel nondeterminism |
| bzlmod/repos/mod/fetch/vendor/codecs | R7: 22 REAL items | R8: cases_7b 373/442, 7a 448/467, fetch 264, ctx 244, native 567/568, lockfile 34/34 | 66 backlog (override/failed-fetch/--nofetch error text, nested-load shapes), mod tidy unverifiable offline, vendor marker cross-binary (legit skew) |
| commands/client/options | R7: 28 REAL items | R8: sampled flag sweep 4417/4431 (99.7%), value sweep 5363/5475, config ids 122/141 | config schema 9.0.0rc1 on purpose, non-tty progress lines, print_action ExtraActionSummary, server-restart messages, --batch profile error, rc ordering, output-base layout/modes, registry-unreachable for cquery/aquery/fetch/mod |

Known immovable differences: Bazel's own nondeterminism (//:all with several broken targets, graph/minrank order,
run --bogus spellcheck), 9.3.2 vs HEAD skew (rules_java 9.1.0 vs 9.9.0, rules_cc 0.2.17 vs 0.2.25, platform
fingerprint, PATH/LC_CTYPE test env, sharding text, test_result_expiration removed), JVM-only values (gc/heap).

Memory discipline: all heavy work via scripts/heavy.sh; cleanup via scripts/cleanup_servers.sh.
