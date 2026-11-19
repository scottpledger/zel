# Project library

## Bazel vs zel code differences

Full per-file review: [bazel-code-differences.md](bazel-code-differences.md)

- Bazel: tag `9.3.2`, commit `5951e93c05876016377063a150f8b56f38276461`
- Zel: `feat/more-functionality`, commit `2084b3f5a9bd343f38ffbc7633d65f21bab0f654`
- Code files reviewed: 6,707 / 6,707
- Git commit of the same list in the workspace: `71ac7e6`

Verdicts:

| Verdict | Files |
|---|---|
| missing | 3,649 |
| partial | 2,704 |
| equivalent | 334 |
| match | 20 |

`match` appears only on `src/main/cpp/util/` headers and sources (`bazel_log_handler`, `errors_windows`, `file`, `file_platform`, `file_windows`, `logging`, `md5`, `numbers`, `path`, `path_platform`, `path_posix`, `path_windows`, `port`, `strings`). The rest of the queue uses `missing`, `partial`, or `equivalent`.

By top-level path (missing / partial / equivalent or match):

| Path | Missing | Partial | Equivalent | Match |
|---|---:|---:|---:|---:|
| repo root | 4 | 2 | 0 | 0 |
| `examples` | 28 | 0 | 0 | 0 |
| `scripts` | 23 | 12 | 0 | 0 |
| `src` | 2,609 | 2,651 | 258 | 20 |
| `third_party` | 930 | 4 | 11 | 0 |
| `tools` | 55 | 35 | 65 | 0 |

## Failure categories

The 6,353 `missing` and `partial` entries are split into exhaustive,
non-overlapping subsystem files. Equivalent and match entries remain only in
the complete review.

| Category | Missing | Partial | Total |
|---|---:|---:|---:|
| [Project build and release](bazel-code-differences_project-build-release.md) | 27 | 14 | 41 |
| [Examples](bazel-code-differences_examples.md) | 28 | 0 | 28 |
| [Native client and platform](bazel-code-differences_native-client-platform.md) | 52 | 76 | 128 |
| [Starlark language and builtins](bazel-code-differences_starlark-language-builtins.md) | 151 | 165 | 316 |
| [Core build engine](bazel-code-differences_core-build-engine.md) | 880 | 740 | 1,620 |
| [Rules and toolchains](bazel-code-differences_rules-toolchains.md) | 54 | 104 | 158 |
| [CLI and runtime](bazel-code-differences_cli-runtime.md) | 80 | 171 | 251 |
| [Repositories and Bzlmod](bazel-code-differences_repositories-bzlmod.md) | 11 | 152 | 163 |
| [Execution, sandboxing, and workers](bazel-code-differences_execution-sandbox-workers.md) | 75 | 86 | 161 |
| [Remote execution](bazel-code-differences_remote-execution.md) | 83 | 73 | 156 |
| [Query, events, and profiling](bazel-code-differences_query-events-profiling.md) | 51 | 186 | 237 |
| [Java tools](bazel-code-differences_java-tools.md) | 161 | 69 | 230 |
| [Auxiliary tools](bazel-code-differences_auxiliary-tools.md) | 90 | 10 | 100 |
| [Tests](bazel-code-differences_tests.md) | 921 | 819 | 1,740 |
| [Third-party code](bazel-code-differences_third-party.md) | 930 | 4 | 934 |
| [Embedded Bazel tools](bazel-code-differences_embedded-bazel-tools.md) | 55 | 35 | 90 |
| **Total** | **3,649** | **2,704** | **6,353** |
