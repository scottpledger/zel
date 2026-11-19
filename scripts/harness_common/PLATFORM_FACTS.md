# Genuinely platform/run-dependent facts (the ONLY normalizations beyond paths/timings/hashes)

The spec and ground truth are both Bazel 9.3.2; there is no version-skew allowlist.

| Fact | Where normalized | Evidence |
|---|---|---|
| `@bazel_tools` install base is `install/<md5 of the running binary>/embedded_tools` | bzlmod_harness/harness.py `norm` | Hash of the binary: any two binaries differ by construction. |
| ActionKeys of `SymlinkTree`/`SourceSymlinkManifest` actions fingerprint the absolute output-base path | cq_harness/run.py `pathkeys` | The real binary run in two different output bases produces different keys for these actions only. |
| Bazel's random `temp<digits>` scratch dir name | bzlmod_harness/harness.py `norm` | Random per run. |

Everything else (tool environment, PATH, LC_CTYPE, sharding text, ...) is compared exactly.
Real-bazel nondeterminism is handled only via evidence files: `scripts/harness_common/prove_nondet.py`.
