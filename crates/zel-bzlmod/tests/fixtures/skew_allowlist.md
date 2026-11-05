# Bzlmod Version Skew Allowlist: Zel / Bazel HEAD vs Bazel 9.3.2 Release Binary

This document records deliberate behavioral and output divergences between Zel (implementing Bazel HEAD source as the SPEC) and the release ground-truth binary (`/opt/homebrew/bin/bazel`, version 9.3.2). Per repository instructions, where Bazel HEAD source differs from the release binary, Zel adheres to HEAD source semantics and records the version skew here.

---

### 1. Lockfile Format & Version (`lockFileVersion`: 30 vs 28)
- **Bazel HEAD source:** `BazelLockFileValue.java:50` sets `LOCK_FILE_VERSION = 30`.
- **Bazel 9.3.2 binary:** emits `lockFileVersion: 28`. (Both write the `facts` and `factsVersions` maps; 9.3.2's v28 lockfile contains `"facts": {}` and `"factsVersions": {}`, so those fields are *not* a difference and are no longer ignored by the lockfile comparison for that reason — only the version number is.)
- **Zel behavior:** writes `lockFileVersion: 30`; the harness drops only `lockFileVersion` (plus `moduleDepGraph`) before comparing.

### 2. Builtin Module Closure (`bazel_tools` Builtin Dependencies)
- **Bazel HEAD source:** Embedded `MODULE.tools` in Bazel HEAD depends on updated BCR rules: `rules_java` 9.9.0, `rules_cc` 0.2.25, `buildozer` 10.1.0.bcr.1.
- **Bazel 9.3.2 binary:** Bundles an older dependency closure selecting `rules_license`, `zlib`, and `buildozer` 8.5.1.
- **Zel behavior:** Zel embeds the Bazel HEAD `bazel_tools` MODULE.tools closure. Full transitive module graphs and `mod show_repo --all_repos` over builtin modules reflect HEAD versions.

### 3. Release name / version label
- **Bazel HEAD source:** `BlazeVersionInfo.getReleaseName()` (`analysis/BlazeVersionInfo.java:127`) is `release <label>` or `development version` when the binary has no label; `info release`, `help <cmd>` (`HelpCommand.emitBlazeVersionInfo`) and `bazel_compatibility` use it.
- **Bazel 9.3.2 binary:** label `9.3.2`.
- **Zel behavior:** zel is its own product and has its own label (`0.0.1` unless `ZEL_EMBED_LABEL`/`BAZEL_EMBED_LABEL` is set); it uses Bazel's naming (`release 0.0.1`, no `(zel)` suffix; `development version` for an empty label). The differential harness labels zel `9.3.2` (`ZEL_EMBED_LABEL`) so the text compares equal. `bazel_compatibility` checks use the label too, falling back to 9.3.2 only when a label is not a version.

### 4. Archive `../` escape rejection
- **HEAD evidence:** `CompressedTarFunction.java:119`, `ZipDecompressor.java:168`, `SevenZDecompressor.java:125`, `ArFunction.java:65`, `CompressedFunction.java:82` throw `Failed to extract %s, path is escaping the destination directory`. 9.3.2 wrote outside the repo. Zel follows HEAD.

### 5. `bazel_dep` on the module itself
- **HEAD evidence:** `ModuleFileGlobals.java:294` `module '%s' cannot depend on itself`. Zel follows HEAD.

### 6. Plain-http URLs to localhost without checksum
- **HEAD evidence:** `StarlarkBaseExternalContext.java:451,489` (`isLocalhostUrl`) exempts localhost from the http-needs-checksum rule. 9.3.2 refuses. Zel follows HEAD.

### 7. `usagesDigest` proxy trimming (lockfile `moduleExtensions.*.usagesDigest`)
- **HEAD evidence:** `ModuleExtensionUsage.trimProxiesForEvaluation()` reduces the proxies of a usage to at most two content-free proxies (non-dev and/or dev) before the usages value is serialized and hashed (`SingleExtensionUsagesValue.hashForEvaluation`).
- **Bazel 9.3.2 binary:** `ModuleExtensionUsage.trimForEvaluation` (javap of `A-server.jar`) does `setProxies(ImmutableList.of())`, so the JSON differs by the `"proxies":[...]` array.
- **Zel behavior:** `crates/zel-bzlmod/src/usages_digest.rs` follows HEAD. Everything else of the serialization (field order, Gson escaping, `AttributeValuesAdapter` label/string escaping, `normalizeLabelString`, `BUILTIN` locations, abridged modules, repo overrides) was verified byte-for-byte against 9.3.2: with the test-only switch `ZEL_TEST_LEGACY_USAGES_TRIM=1` (selects the 9.3.2 trim; used by `scripts/bzlmod_harness` and `scripts/diff_bzlmod.sh`) Zel's `bzlTransitiveDigest` and `usagesDigest` equal Bazel 9.3.2's for all 17 fixtures in `scripts/bzlmod_harness/lockfile_cases`.

### 8. `fetch` / `vendor` failure exit code
- **HEAD evidence:** commit `fac655f4d2` (2026-08-31): `FetchCommand` (`fetchAll`, `fetchRepos` incl. `RepositoryFetcherException`/"Invalid repo name"/failed repo values, `fetchTarget`) and `VendorCommand` (`vendorAll`, `vendorRepos`, `vendorTargets`, `IOException`) report failures as `FetchCommand.Code.QUERY_EVALUATION_ERROR` (exit code 2 in `failure_details.proto`). Not changed by that commit and therefore the same in 9.3.2 and HEAD: option errors (`OPTIONS_INVALID`, 2), `vendor` with both `--repo` and target patterns (`InterruptedFailureDetails`, 8), `RepositoryMappingResolutionException` (its own code, e.g. 48), interruptions (8).
- **Bazel 9.3.2 binary:** exits with 8 for all of the failures above (observed: `fetch --all --lockfile_mode=error` with a missing extension, `fetch --repo=@bad`, `vendor --repo=@bad`, failing repo rules).
- **Zel behavior:** follows HEAD consistently for `fetch --all/--repo/<targets>` and every `vendor` mode (`crates/zel-bzlmod/src/session.rs`, `EXIT_FETCH_FAILED`). `lockfile_cases/run.sh` and `harness.compare()` map bazel's 8 to 2 **only for fetch/vendor steps**.

### 9. Installation-dependent output
- `mod show_repo @bazel_tools` prints `Builtin or overridden repo located at: <install_base>/embedded_tools` with the install base of the *running* binary (zel prints its own install base; only the hash directory differs, normalized by the harness).
- `mod show_repo --all_repos` lists the repos of the builtin dependency closure (see 2), so it differs from 9.3.2 by exactly those module repos.

### 10. `help mod` text
- Zel renders HEAD's `mod.txt` (`tidy` and `show_repo` descriptions differ from 9.3.2's text, and the version footer shows the configured label).

Removed (no HEAD-source evidence of skew, so treated as Zel bugs): former entries "registryFileHashes for file:// registries" (still open; the harness drops `registryFileHashes`) and "`name` in generatedRepoSpecs attributes" (fixed: `RepoRule.RESERVED_ATTRIBUTES` contains `name`, so it is never serialized and None-valued attributes are dropped).

### 11. `Expected a file with a ... suffix` lists `.tar.br or .br`
- **HEAD evidence:** `DecompressorValue.java:77,80` registers `tar.br` and `br` formats (BrFunction/TarBrFunction); real 9.3.2 does not, so its unsupported-suffix error ends `.deb or .7z suffix`. Zel follows HEAD (`.deb, .7z, .tar.br or .br suffix`).

### 12. `http_file` / `http_jar` generated files (HEAD `http.bzl` vs 9.3.2)
- **HEAD evidence:** `tools/build_defs/repo/http.bzl` (`_HTTP_FILE_ROOT_REPO_ALIAS`, `_HTTP_FILE_ROOT_NAME_ALIAS`, `_http_file_impl`) writes a root `BUILD.bazel` with aliases (`@repo` / `@repo//:<downloaded file name>`) and a differently shaped `file/BUILD`; `_HTTP_JAR_BUILD` loads `{java_import_bzl}` (`str(Label("@rules_java//java:java_import.bzl"))`).
  The 9.3.2 `http.bzl` (read from a real 9.3.2 output base) writes only `WORKSPACE` + `file/BUILD` (`exports_files` + `filegroup`) for http_file and loads `@rules_java//java:java_import.bzl` literally for http_jar. Zel follows HEAD.
- Related: native failures are framed with the **9.3.2** `http.bzl` positions (`203:45`, `255:33`, `292:33`, `214:28`, `217:32`) because that is what real bazel prints; Zel's own embedded HEAD copy has them at 204/298/349.

### 13. Vendor/fetch marker hash (`@<repo>.marker`) — NOT interchangeable across Bazel versions
- Zel implements `DigestWriter.computePredeclaredInputHash` as in HEAD (`Fingerprint`: MARKER_FILE_VERSION 8, `BuildLanguageOptions.stableFingerprint`, bzl label, rule name, transitive bzl digest, repo name, `Starlark.repr` attrs, os name/arch, environ, repo mapping entries) and writes/reads the marker like `DigestWriter` (first line = hash, then recorded inputs, validated on read).
- **Why values cannot equal a 9.3.2 marker (evidence from the 9.3.2 `A-server.jar`, decompiled with `javap -c`):** `DigestWriter.MARKER_FILE_VERSION = 7` (HEAD: 8); 9.3.2 hashes the attributes as Gson JSON (`GsonTypeAdapterUtil.SINGLE_EXTENSION_USAGES_VALUE_GSON.toJson(attrValues)`) where HEAD uses `Starlark.repr` per attribute; and `BuildLanguageOptions.stableFingerprint` covers a different key set (9.3.2 has `incompatible_stop_exporting_language_modules`, `incompatible_autoload_externally`, `repositories_without_autoloads`, `incompatible_disable_autoloads_in_main_repo`, `incompatible_no_implicit_watch_label`, `experimental_disable_external_package`, `experimental_sibling_repository_layout`, `incompatible_do_not_split_linking_cmdline`, `incompatible_use_cc_configure_from_rules`, `experimental_starlark_type_checking` that HEAD removed, and lacks HEAD's `experimental_starlark_static_type_checking`/`..._dynamic_type_checking`, `max_bzl_file_size` ... keys). A vendor dir therefore round-trips within one implementation (`scripts/bzlmod_harness/vendor_interchange.sh` and the unit tests) and a marker written by the other version is treated as out of date, exactly as Bazel itself treats a marker written by another Bazel version (`Bazel version, flags, repo rule definition or attributes changed`).

### 14. `copy` in `dir(repository_ctx)` / `dir(module_ctx)`
- **HEAD evidence:** `StarlarkBaseExternalContext.java:1541` declares `copy`; 9.3.2 has no such method. Zel follows HEAD; the harness drops `"copy"` from `dir()` output.

### 15. Builtin closure modules missing from the registry
- With a registry lacking `bazel_tools`' dependencies Zel fails with `module <first missing dep of HEAD's MODULE.tools> not found in registries` (e.g. `bazel_skylib@1.9.0`), while 9.3.2 names the first missing module of its older closure (`rules_license@1.0.0`). Same behaviour (exit 37 for `mod`, 48 for `fetch`/`build`), different module name (see 2).

### 16. `--downloader_config` urlencode validation
- **HEAD evidence:** `UrlRewriterConfig.java` (`parseConfig`, `rewrite`) rejects `urlencode(` outside valid `${urlencode($N)}`/`urlencode($N)` tokens (`UrlRewriter.isValidUrlEncodeSyntax`). The 9.3.2 `UrlRewriterConfig`/`UrlRewriter` classes contain no `urlencode` handling and accept such a config. Zel follows HEAD.

### 17. Download retries of `ConnectException`
- **HEAD evidence:** `DownloadManager.isRetryableException` (`DownloadManager.java:384`) does not retry `ConnectException` ("HttpConnector already retries connection attempts"); the 9.3.2 `DownloadManager` retries every `SocketException`, so an unreachable host prints `WARNING: Download from … failed: class java.net.ConnectException Connection refused` six times in 9.3.2 and once in HEAD/Zel (an unknown host prints one warning in both).


### 21. Several `--downloader_config` flags
- **HEAD evidence:** `RepositoryOptions.getDownloaderConfigs()` is a `List<PathFragment>` (`allowMultiple = true`) and `UrlRewriter.getDownloaderUrlRewriter` merges all files (`Unable to find downloader config file a,b` lists every missing file). 9.3.2 behaves as if only the last occurrence counted (`Unable to find downloader config file WS/b.cfg`). Zel follows HEAD.

### 18. Archive decompressor suffix list (`.tar.br` / `.br`)
- **HEAD evidence:** `DecompressorValue.java` (`getDecompressor`) registers brotli, so the "Expected a file with a .zip, ... or .7z suffix" message ends with `.7z, .tar.br or .br suffix`. 9.3.2 ends with `.deb or .7z suffix`. Zel follows HEAD; `scripts/bzlmod_harness/harness.py` rewrites the HEAD tail to the 9.3.2 tail before comparing.

### 19. `dropping [...]` in the "canonical reproducible form" DEBUG line
- **HEAD evidence:** `RepositoryResolvedEvent.java:90,107` print the dropped attribute set with `Starlark.repr` (`dropping ["tag"]`). 9.3.2 prints the set with `toString` (`dropping [tag]`). Zel follows HEAD; the harness strips the quotes inside that bracket.

### 20. `@bazel_tools//tools/build_defs/repo/*.bzl` differ between HEAD (embedded in zel) and 9.3.2
- **Evidence:** `diff` of `tools/build_defs/repo/*.bzl` (HEAD source vs files materialized by 9.3.2, kept in `tests/fixtures/bazel_9.3.2_repo_bzl/`): HEAD `http_archive` has the `files` attribute (`attr.string_keyed_label_dict`), `utils.bzl` `workspace_and_buildfile` uses `ctx.copy(build_file, "BUILD.bazel")` (9.3.2: `ctx.file(..., ctx.read(...))`), `read_user_netrc` honours `NETRC`, `symlink_files` was removed, `local.bzl`/`jvm.bzl` return `rctx.repo_metadata(reproducible = True)`, `git_worker.bzl` fetches with `--`, `new_git_repository` is its own rule (9.3.2: alias of `git_repository`, so `defined by rule git_repository`).
- **Zel behaviour:** production runs the embedded HEAD files unmodified. For the differential harness the test-only variable `ZEL_TEST_BAZEL_TOOLS_REPO_BZL_DIR` overlays the 9.3.2 files (`harness.py` sets it unless `ZEL_HARNESS_HEAD_BZL=1`), so Starlark tracebacks (file/line/column) are comparable with the reference binary.

> Note: the harness comments that cite "skew_allowlist.md #9/#10/#11" for the suffix list, `dropping [...]` and bzl-overlay skews refer to entries 18/19/20 above (the file previously numbered them 9/10/11 a second time).

## Round 7B additions

14. **Native patch (`PatchUtil`) HEAD vs 9.3.2** — `crates/zel-bzlmod/tests/fixtures/patch_corpus` records HEAD's `PatchUtil` (compiled against the 9.3.x server jar's
    java-diff-utils 4.12) in `result`/`expected` and the jar's own 9.3.x answer in `result932`/`expected932`; zel follows HEAD. They differ only through HEAD commits
    `db1d260030` (multi-file patches without separators: `--- ` after a complete hunk starts a new file) and `1224606c0b` (symlink containment in `getFilePath`).
15. **`--include_builtin` / `show_repo --all_repos` builtin closure** (`n*_9`, `n*_93`, `m7_ovrshow2_*` first diff hunk): same as #2 (HEAD `src/MODULE.tools` vs 9.3.2 closure).
