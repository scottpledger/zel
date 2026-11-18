# Bazel vs zel code differences: embedded bazel tools

Category rule: `tools/` sources embedded into or distributed with `@bazel_tools`.

- Failure entries: 90
- Missing: 55
- Partial: 35
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `tools/android/android_extensions.bzl`
- Bazel lines: 126
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/android/android_extensions.bzl`
- Verdict: partial
- Differences:
  - [P1] `android_external_repository` sets `local = True`. Bazel therefore skips cached repo contents whenever fetching is enabled, keeps the repo out of the repo contents cache, and excludes it from vendoring by default. Zel stores `local` on the rule and never reads it; only rules named `local_repository` or `new_local_repository` are treated as local, so this repo is marker-cached, contents-cached, and eligible for vendoring.

### `tools/android/instrumentation_test_check_test.py`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No test asserts that an instrumentation manifest with `android:targetPackage="com.example"` yields `com.example`.
  - [P3] No test asserts that a target manifest with `package="com.example"` yields `com.example`.
  - [P3] No test asserts that matching instrumentation and target manifests validate as `("com.example", "com.example")`.
  - [P3] No test asserts that `android:targetPackage="not.com.example"` against package `com.example` raises `ManifestError`.

### `tools/aquery_differ/aquery_differ.py`
- Bazel lines: 298
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no CLI that loads two `ActionGraphContainer` dumps (`--before` and `--after`, both required) as binary proto (the default) or textproto and diffs them.
  - [P2] Actions are not matched by sorted, space-joined output artifact paths, and output sets present only on one side are not reported.
  - [P2] There is no `--attrs` selection (default `cmdline`, also `inputs`) of unified diffs of action arguments or sorted resolved input paths, no TTY coloring of `+++`/`---`/`@@`/`+`/`-` lines, and no `No difference` result.
  - [P2] Relative paths are not resolved with `~` expansion or `BUILD_WORKING_DIRECTORY`, non-Windows `RLIMIT_AS` is not capped by `--max_mem_alloc_mb` (default 3072), and the OOM path that prints the b/154620006 warning and exits 1 is absent.

### `tools/aquery_differ/aquery_differ_test.py`
- Bazel lines: 470
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no aquery differ and no tests that compare two action graphs by `cmdline` or `inputs`.
  - [P3] Identical graphs, including the same outputs in a different order, are not reported as `No difference`.
  - [P3] An output present only in the before graph is not reported as an extra action, with the path rebuilt from path fragments (`root/baz`).
  - [P3] Command-line changes are not emitted as unified diffs grouped by sorted output paths (`root/foo`, `root/bar`, `root/baz`).
  - [P3] Input dep sets are not resolved, de-duplicated when an artifact is both direct and transitive, or diffed as sorted input paths.

### `tools/aquery_differ/resolvers/dep_set_resolver.py`
- Bazel lines: 49
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel never indexes an aquery `dep_set_of_files` list by id or flattens a dep set into artifact paths. `crates/zel-query/src/aquery.rs` only emits `DepSetOfFiles` (`direct_artifact_ids`, `transitive_dep_set_ids`); Starlark `depset.to_list` and analysis `NSet::to_vec` flatten a different nested-set model.
  - [P2] `resolve` maps `direct_artifact_ids` through `artifact_id_to_path` first, then depth-first appends each `transitive_dep_set_ids` entry in list order. Duplicates are kept. There is no order enum and no element dedup.
  - [P2] The flattened list is cached under `dep_set.id` only after the subgraph is walked, and that same list object is returned. A repeat resolve returns the cached list. A cycle recurses until the stack overflows. A missing artifact id or transitive dep-set id raises `KeyError`.

### `tools/aquery_differ/resolvers/path_fragment_resolver.py`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no resolver that indexes aquery path fragments by id, walks `parent_id` while the id is truthy, raises `ValueError` when an id is missing, and joins labels root-to-leaf with `os.path.join`. `crates/zel-query/src/aquery.rs` only emits fragments (split on `/`, omit parent `0`); it never reconstructs a path from ids.

### `tools/build_defs.bzl`
- Bazel lines: 106
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/build_defs.bzl`
- Verdict: partial
- Differences:
  - [P1] `visibility("//tools/...")` does not limit who can load the file. Zel accepts the call and does not record `.bzl` load visibility, so any package can load this module. Bazel allows only packages under `//tools` in the file's repository.
  - [P0] `current_launcher_binary` requests the toolchain type `//tools/launcher:launcher_toolchain_type` in the main repository. Bazel resolves that `rule(toolchains=...)` string in the repository of this `.bzl` file, which is `@bazel_tools` when the file is loaded from bazel_tools, so the rule does not match the launcher toolchains registered there.

### `tools/build_defs/build_info/bazel_cc_build_info.bzl`
- Bazel lines: 81
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_actions.rs`
- Verdict: partial
- Differences:
  - [P1] `non_redacted_build_info_files` are expanded at analysis time from a fixed dictionary (`BUILD_TIMESTAMP`=`0`, `BUILD_SCM_REVISION`=`0`, `BUILD_SCM_STATUS` empty, `BUILD_EMBED_LABEL` empty, `BUILD_HOST`=`hostname`, `BUILD_USER`=`username`). Bazel reads `volatile-status.txt` and `stable-status.txt` at execution and applies `_transform_version` / `_transform_info` to those values, including `--workspace_status_command`, `--embed_label`, and the real user, host, and timestamp.
  - [P2] A missing `BUILD_TIMESTAMP` does not fail the volatile translation. The key is always supplied, so `_transform_version`'s `fail(...)` never runs.
  - [P1] The volatile output is not a constant-metadata artifact and the action does not run unconditionally. The action key contains the baked substitutions and the only input is the template, so later workspace-status changes do not regenerate `volatile_file.h` or `non_volatile_file.h`.

### `tools/build_defs/build_info/bazel_java_build_info.bzl`
- Bazel lines: 109
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/build_defs/build_info/bazel_java_build_info.bzl`
- Verdict: partial
- Differences:
  - [P1] The rule text matches, but `actions.transform_version_file` and `actions.transform_info_file` in `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_actions.rs` run the callbacks at analysis on fixed maps (`BUILD_TIMESTAMP` `0`, `FORMATTED_DATE` `1970 Jan 01 00 00 00 Thu`, `BUILD_EMBED_LABEL` `""`) and register a normal template expansion. Bazel runs them at execution on `volatile-status.txt` and `stable-status.txt`, so `{build.time}`, `{build.timestamp}`, `{build.timestamp.as.int}`, and `{build.label}` follow the real stamp, `SOURCE_DATE_EPOCH`, and `--embed_label` (colon escaping still applies to those values).
  - [P1] Zel's non-redacted `volatile_file.properties` is therefore always the epoch values (`build.time` `Thu Jan 01 00\:00\:00 1970 (0)`, both timestamp keys `0`), and `non_volatile_file.properties` always has an empty `build.label`. Those actions do not take the workspace status files as inputs.
  - [P1] The volatile output is not a constant-metadata artifact and is not run unconditionally. Bazel rewrites `volatile_file.properties` every build without invalidating dependents.
  - [P2] A missing `BUILD_TIMESTAMP` or `FORMATTED_DATE`, or a `FORMATTED_DATE` that is not seven tokens, never reaches the `fail()` paths. Zel's workspace status writer also never emits `FORMATTED_DATE`.
  - [P3] The redacted group (symlink `redacted_file.properties` to the redacted template), output group names, and property file names match.

### `tools/build_defs/hash/sha256_test.sh`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No test resolves the `sha256` tool through runfiles, appending `.exe` when `uname` matches `msys*`, and runs it as `sha256 <input> <output>`.
  - [P3] No test builds the 120MB input by writing `01234567890123456789012345678` plus a newline and doubling that file 22 times (`30 * 2^22` bytes) under `TEST_TMPDIR`.
  - [P3] No check that the tool's output file is exactly `b89e2ebd615b1d32be9cec7bf687f3a00476835fe2ea8fb560394d79f420390c`, or that a mismatch prints `Wrong hash <got>; expected <expected>` and exits 1.
  - [P3] Zel's SHA-256 tests only cover the empty string and `abc` through an in-memory digest API, not this CLI or this golden file.

### `tools/build_defs/inspect/struct_to_dict.bzl`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no depth-bounded recursive dump (default depth 5) of struct-like values: depsets flattened with `to_list`, lists walked, atoms copied, `File` reduced to `path`, `Label` reduced to `str`, readable fields stored with `_type` (non-inspectable native methods skipped), and `Target` also recording `JavaInfo` and `CcInfo`.
  - [P2] Values past the depth limit stay unconverted, and dict inputs and dict fields become empty dicts because the walk iterates the destination rather than the source keys.

### `tools/build_defs/pkg/archive_test.py`
- Bazel lines: 223
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Rust `TarFileWriter`. `crates/zel-bzlmod/src/archive.rs` only extracts archives; its tar-header helpers exist to build extractor fixtures. The Python copy under `crates/zel-bazel-tools/resources/tools/build_defs/pkg/archive.py` is the original writer, not a port, and nothing asserts these cases.
  - [P3] An empty writer must emit a tar with no members.
  - [P3] `add_file` inserts a leading `.` member and rewrites relative names to `./...` (`a`, `./c`, `./.d`, `..e`, `.f` become `./a`, `./c`, `./.d`, `./..e`, `./.f`) while leaving an absolute name such as `/b` unchanged.
  - [P3] `add_dir("./", tree, mode=0o644)` walks the tree, strips a trailing slash, writes `.` and directories at mode `0o755`, and writes file members with their bytes at mode `0o644`.
  - [P3] `add_file("d/f")` and nested names such as `a/b/f` insert missing parents (`.` and each directory) at mode `0o755` before the file.
  - [P3] `add_dir` then `add_file` in the same directory keeps the directory's existing files and appends the extra file; a deeper path still creates the intermediate directory.
  - [P3] Repeated `DIRTYPE` adds of the same path (`a/b`, `a/b` again, `a/b/`) collapse to one directory member. Adding `x/y/f` and only later `x` as `DIRTYPE` still yields a single parent-before-child sequence `./x`, `./x/y`, `./x/y/f`.
  - [P3] `root_directory="root"` prefixes every name (`root`, `root/d`, `root/d/f`, ...) with no `./`, still at directory mode `0o755`, with the same parent insertion and duplicate-directory collapse.

### `tools/build_defs/pkg/build_test.sh`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No `build_test` suite runs `test_tar`. Zel keeps an identical `testenv.sh`, but nothing sources it for this suite or exits 1 with `testenv.sh not found!` when that source fails.
  - [P3] Nothing checks that `tar tvf` of `test-tar.tar`, after `sed -e 's/^.*:00 //'`, is exactly `./`, `./etc/`, `./etc/nsswitch.conf`, `./usr/`, `./usr/titi`.
  - [P3] Nothing checks that `./usr/titi` and `./etc/nsswitch.conf` are mode `-rwxr-xr-x` (first field of the `tar tvf` line containing `00 $file`).

### `tools/build_defs/pkg/path_test.py`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No test asserts `_short_path_dirname` of `foo/bar/baz` is `foo/bar`, or that a top-level `baz` is `''`.
  - [P3] No test asserts `dest_path` of `foo/bar/baz` with strip prefix `foo` is `/bar/baz`, with `qux` stays `foo/bar/baz`, with `None` is `/baz`, or that a top-level file with `None` stays `baz`.
  - [P3] No test asserts `compute_data_path` of `foo/bar/baz.tar` is `foo/bar/a/b/c` for `a/b/c` and `./a/b/c`, `a/b/c` for `/a/b/c`, and `foo/bar` for `./` and `./.`.

### `tools/build_defs/repo/http.bzl`
- Bazel lines: 672
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/build_defs/repo/http.bzl`
- Verdict: partial
- Differences:
  - [P1] The embedded file matches this source and is what `http_archive`, `http_file`, and `http_jar` run. The gaps are in `ctx.download` and `ctx.download_and_extract` (`/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs`, `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs`, `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`).
  - [P2] A checksum-less `http://localhost`, `http://127.0.0.1`, or `http://::1` URL is kept (`get_urls` in `ctx.rs`). Bazel 9.3.2 drops every plain `http` URL that has no checksum and fails with `No URLs left after removing plain http URLs due to missing checksum`. That covers all three rules and the remote file, remote patch, and `remote_module_file_urls` downloads they start.
  - [P2] A malformed `sha256` or `integrity` is cleared, the URL is fetched, and the failure is then `Checksum error in repository @@...`. Bazel throws that checksum error before any fetch. If the URL also fails, zel reports the download error.
  - [P1] `download_info.integrity` is always `sha256-` plus the base64 of the SHA-256 of the bytes (`download.rs` `finish`). `http_archive` compares that map with `remote_file_integrity` and `remote_patches` and, when they differ, puts the SHA-256 map in `attrs_for_reproducibility`. Bazel returns the caller-supplied checksum, so a matching SHA-1, SHA-384, or SHA-512 value stays reproducible and is not rewritten. An archive `sha256` or `integrity` that is already set still skips the archive override in both.
  - [P2] `blake3-` Subresource Integrity is rejected as an unsupported algorithm (`hash.rs` `parse_integrity`). Bazel `Checksum.fromSubresourceIntegrity` accepts it. A string of several SRI tokens uses only the first token. Bazel base64-decodes the whole string and fails.
  - [P1] `https` URLs are fetched with `curl -fsSL` and `--connect-timeout 2`. A missing `curl` fails with `the system 'curl' binary is required for https URLs`. Bazel uses `HttpURLConnection`, `User-Agent: bazel/<version>`, and a connect timeout that starts at 1s and doubles up to 10s. Zel's plain `http` client sends `User-Agent: zel`. HTTPS failures are `GET <url> failed:` plus curl's stderr.
  - [P1] `--credential_helper` is not called. Bazel applies helper credentials ahead of `.netrc` and the `auth` dict from `get_auth`.
  - [P3] A refused connection is retried, and only the last attempt is logged. Bazel 9.3.2 logs `class java.net.ConnectException` on every `DownloadManager` attempt, six lines with the default retry count of 5.
  - [P1] An archive member whose normalized path starts with `..` fails with `path is escaping the destination directory`. The 9.3.2 decompressors have no such check. `type` or a URL suffix of `tar.br` or `br` extracts. Bazel's format list ends at `7z`, and the error ends with `.deb or .7z`. `SUPPORTED_ARCHIVE_FORMATS` in this file is documentation and already matches that 9.3.2 list. A GNU sparse tar member (typeflag `S`) is skipped. USTAR and GNU entry names go through `from_utf8_lossy`, so a non-UTF-8 byte becomes U+FFFD. Bazel decodes those names as ISO-8859-1.
  - [P1] `http_file` writes `file/BUILD` with `repr(downloaded_file_path)`, which is Rust `Debug` (`\u{fc}` for a non-ASCII character). Bazel's printer leaves that character in the string (`\xHH` only for code points below 32). The generated `file/BUILD` then does not parse. ASCII paths match.
  - [P1] On Windows, `str(ctx.path(...))` uses the OS path (`\` separators). Bazel uses `/` and an uppercased drive, so the `downloaded_file_path` prefix check can reject a path Bazel accepts. `files` calls `ctx.symlink`, which errors off Unix. Bazel copies the file or creates a junction.

### `tools/build_defs/repo/local.bzl`
- Bazel lines: 137
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/build_defs/repo/local.bzl`
- Verdict: partial
- Differences:
  - [P0] Both rules set `local = True`. Bazel refetches them whenever fetching is enabled (`RepositoryFetchFunction.shouldUseCachedRepoContents`). Zel records `local` on the rule and still treats `@<canonical>.marker` as current when it matches the predeclared-input hash. That marker is only the hash line, so `rctx.watch` in `local_repository` and the `readdir` directory-entry watch in `new_local_repository` are dropped. A later fetch leaves a dangling `local_repository` symlink after the source directory disappears, and `new_local_repository` keeps the old child symlinks when entries are added, removed, or renamed. A `BUILD.bazel` written from `build_file_content` is not regenerated.
  - [P1] On Windows this file depends on `rctx.symlink` copying files and using directory junctions, then watches those file copies. Zel's `symlink` fails on non-Unix with `symlink is not supported on this platform`, so both rules fail there.
  - [P2] A `local_repository` module override whose path exists but is not a repo root is rejected in `non_registry_module_dir` (`/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs`) without fetching. The error uses the placeholder `{OUTPUT_BASE}/external/<name>+`. Bazel fetches and reports the real output-directory path.

### `tools/build_rules/test_rules_test.py`
- Bazel lines: 155
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel test drives `file_test` in a scratch workspace and checks `test` pass/fail.
  - [P3] Exact content match is not checked: the full file text, including the trailing newline, passes, and the substring `quick` fails.
  - [P3] Regexp mode without `matches` is not checked: `o[vwx]` passes and `o[abc]` fails.
  - [P3] `matches` is not checked as a `grep -c` line count: `matches = 2` passes and `matches = 3` fails for `o[vwx]`.
  - [P3] Analysis failure is not checked when neither `content` nor `regexp` is set, when both are set, or when `content` is combined with `matches`.

### `tools/compliance/gather_packages.bzl`
- Bazel lines: 266
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no aspect that walks attributes, skips edges from the rule-kind filters, and folds `LicenseInfo` and `PackageInfo` into one transitive package provider.
  - [P2] Zel never short-circuits that walk for exec configurations (`-exec` in `bin_dir`), skips `_license` rules, or records external labels and targets tagged `maven_coordinates=`.
  - [P2] Zel does not start or cap dependency traces at 10, and it has no `packages_used` rule that writes the JSON object with `top_level_target`, `licenses`, `package_info`, and `packages`.

### `tools/compliance/packages_used_test.py`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No smoke test loads `tools/compliance/bazel_packages.json` from `$TEST_SRCDIR/$TEST_WORKSPACE` and parses the `packages_used` JSON.
  - [P3] No assertion that some `licenses` entry has label `//:license` or `//third_party/bazel:license`.
  - [P3] No check, run only when `TEST_WORKSPACE` is `bazel`, that `packages` contains `@@remoteapis+//:build_bazel_remote_execution_v2_remote_execution_proto`.

### `tools/compliance/sbom.bzl`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No `sbom` macro that runs `packages_used` as `_packages_<name>` writing `_packages_<name>.json` for `target`, then a private SBOM rule. Missing `out` still defaults to `<name>_sbom.json`, and `maven_install` still defaults to `//:maven_install.json`.
  - [P2] No `CreateSBOM` action running `//tools/compliance:write_sbom_private` with `--packages_used` and `--out`, adding `--maven_install` and that file as an input only when the label is set, and returning `DefaultInfo` of the single output.
  - [P2] Vendored `rules_license` `generate_sbom` is a different contract: it gathers license metadata from `deps`, passes `--licenses_info`, and also returns an `OutputGroupInfo` of that JSON. It does not consume a packages-used file or a Maven lock.

### `tools/compliance/to_json.bzl`
- Bazel lines: 144
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No Zel function strips a leading `@//` or `@@//` from a label, or derives `bazel_package` by dropping the target name and the preceding separator.
  - [P2] `license_info_to_json` is absent: license kinds are not sorted by `name` into `{target, name, conditions}` objects, and a license is not rendered with `label`, `bazel_package`, `license_kinds`, `copyright_notice`, `package_name`, `package_url`, `package_version`, and `license_text` set to `license.license_text.path`.
  - [P2] `licenses_to_json` is absent: a `LicenseInfo` depset is not sorted by label and joined into one JSON array.
  - [P2] `package_info_to_json` / `package_infos_to_json` are absent: a `PackageInfo` is not rendered as `{target, bazel_package, package_name, package_url, package_version}`, and a depset is not sorted by label into a JSON array.
  - [P2] `labels_to_json` is absent: labels are not sorted, null-repo-stripped, quoted, and joined into a JSON string array.
  - [P2] String fields are interpolated with no JSON escaping.

### `tools/compliance/user_filtered_rule_kinds.bzl`
- Bazel lines: 28
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Bazel exports `user_aspect_filters` as an empty dict, so `gather_packages.should_traverse` applies no extra rule-kind attribute filters from this table.
  - [P2] Zel has no `//tools/compliance` module and no equivalent empty user filter table for that aspect. The same empty dict under `third_party/bcr/rules_license` is a separate `@rules_license` load path, not this tool.

### `tools/compliance/write_sbom.py`
- Bazel lines: 230
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no tool that reads a `packages_used` JSON file and an optional Maven lock file and writes an SPDX-2.3 JSON SBOM (`--out`, `--packages_used`, `--maven_install`, `@` response files).
  - [P2] No SPDX document header (`SPDX-2.3`, `CC0-1.0`, fixed `documentNamespace`, document name from `top_level_target`, UTC `creationInfo` with the Bazel writer tool id and `Organization: Google LLC`).
  - [P2] No per-package records: MD5 hex `SPDXRef-GooglePackage-*` ids, `downloadLocation` defaulting to `NOASSERTION`, a `DESCRIBES` edge from the document to `SPDXRef-Package-main`, and a `CONTAINS` edge from that package to each id.
  - [P2] No Maven lock mapping: repository URL index with a duplicate-path warning, classifier/`shasums` keys other than `jar` rewritten as `group:artifact:jar:arch` and `version-arch`, download URL `{mirror}{repo}/{artifact}/{version}/{artifact}-{version}.jar`, and Bazel names from `.`, `-`, and `:` replaced by `_` plus a version suffix.
  - [P2] No `@maven//:` or `//file:file` package-name lookup into that map, and no `MISSING` line when a package has no Maven URL.

### `tools/cpp/build_info_translation_helper.cc`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No Rust reader opens the stable info file and the volatile version file and fills a caller-supplied map. Each line is split on the first space. The remainder, including later spaces, is the value and is not trimmed. A line with no space is stored as that key with an empty value.
  - [P2] A path that cannot be opened returns NotFound with `Could not open file: <path>`, and the map is left unchanged. A key already in the map that appears again on a line containing a space returns FailedPrecondition with `<key> is duplicated in the file.` after earlier lines have already been inserted. A repeated line with no space is inserted and does not fail. A read error after a successful open stops the loop and still returns success with the partial map.
  - [P1] `actions.transform_version_file` and `actions.transform_info_file` do not read those files. They pass a fixed unstamped dictionary at analysis time, and stamped builds are unsupported. `write_workspace_status_files` only writes `stable-status.txt` and `volatile-status.txt`. Its parse of `--workspace_status_command` stdout trims lines and values, skips empty lines, and overwrites duplicate keys.

### `tools/cpp/build_info_translation_helper.h`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] `WriteFile` truncates the destination and writes each string as a `\n`-terminated line, flushing after every line, and ignores open and write errors.
  - [P2] `BuildInfoTranslationHelper` keeps separate info-file and version-file paths. `ParseInfoFile` and `ParseVersionFile` fill the caller-owned map: a missing file is not-found (`Could not open file: <path>`), each line splits on the first space (no space yields an empty value), and a repeated key fails with `<key> is duplicated in the file.`
  - [P1] Zel does not parse workspace status files or write those translated lines. `transform_version_file` and `transform_info_file` substitute a fixed unstamped key set at analysis time.

### `tools/cpp/build_info_translation_helper_test.cc`
- Bazel lines: 120
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No parser loads an info file of `key value` lines, keeping spaces inside the value, into a map and reports success.
  - [P3] A missing version file is not reported as not-found with `Could not open file: <path>`.
  - [P3] A repeated key is not rejected with `<key> is duplicated in the file.`
  - [P3] A line with no space is not stored as that key with an empty value while still succeeding.
  - [P3] No writer emits each supplied string as its own newline-terminated line.

### `tools/cpp/modules_tools/aggregate-ddi/aggregate-ddi.cc`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `write_output` writes one JSON object with both `modules` (name to string) and `usages` (name to string array). Object keys are sorted, array order is preserved, empty maps are still emitted, and there is no trailing newline. Zel has no Rust writer for that document.

### `tools/cpp/modules_tools/aggregate-ddi/aggregate-ddi.h`
- Bazel lines: 26
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Rust `write_output` that takes a `Cpp20ModulesInfo` (`modules` name-to-path map and `usages` name-to-required-module lists) and writes it to a stream. Zel only vendors the same C++ declaration at `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/cpp/modules_tools/aggregate-ddi/aggregate-ddi.h`.

### `tools/cpp/modules_tools/aggregate-ddi/aggregate-ddi_test.cc`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never checks that `write_output` emits the compact JSON `{"modules":{"module1":"/path/to/module1","module2":"/path/to/module2"},"usages":{"module1":["module2"]}}` for modules `module1` and `module2` with usage `module1` to `module2`.

### `tools/cpp/modules_tools/aggregate-ddi/main.cc`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Rust program parses `-m` (cpp20 module-info files), `-d` (DDI files), `-f` (PCM paths), and `-o`, rejects unknown or incomplete arguments with exit 1, or requires equal `-d` and `-f` counts and a non-empty `-o`.
  - [P3] No Rust code merges parsed module info, and for each DDI with `gen_bmi` records `modules[name] = pcm path` and `usages[name] = require_list`, then writes the `modules`/`usages` JSON (failing if `-o` cannot be opened).
  - [P3] Zel still embeds this C++ `main` unchanged and Starlark still runs the `aggregate-ddi` tool, so the behavior is not reimplemented.

### `tools/cpp/modules_tools/common/common.h`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `Cpp20ModulesInfo::merge` overwrites `modules` (name to BMI path) and replaces each `usages` vector wholesale so a later info wins on the same key. Zel has no such maps or merge.
  - [P3] Zel has no `ModuleDep` (`gen_bmi`, provided module `name`, `require_list`) and no `parse_ddi` or `parse_info` that turn a DDI or modules-info stream into those values.

### `tools/cpp/modules_tools/common/common_test.cc`
- Bazel lines: 155
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `parse_info` is absent: a JSON object's `modules` map (module name to path) and `usages` map (module name to the list of required module names) are never read. The covered case is `module1` to `/path/to/module1` with usage `["module2"]` and `module2` to `/path/to/module2` with an empty usage list.
  - [P3] Merging two parsed module-info values is absent. The covered case keeps both inputs: `module1`/`module2` plus `foo` to `/path/to/foo` (empty usages) and `bar` to `/path/to/bar` with usage `["foo"]`.
  - [P3] `parse_ddi` is absent. A single rule whose `provides[0]["logical-name"]` is `foo` and whose `requires[0]["logical-name"]` is `bar` yields name `foo`, BMI generation enabled, and require list `["bar"]`.
  - [P3] A DDI rule that only has `primary-output` (`main.ddi`), ignoring `revision` and `version`, yields an empty name, BMI generation disabled, and an empty require list.
  - [P3] A DDI rule with `provides` logical name `foo` and no `requires` yields name `foo`, BMI generation enabled, and an empty require list.
  - [P3] A DDI rule with `requires` logical name `bar` and no `provides` yields an empty name, BMI generation disabled, and require list `["bar"]`.

### `tools/cpp/modules_tools/generate-modmap/generate-modmap.h`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No module-map entry of a module name and BMI path, equal on both fields and ordered by name then path.
  - [P3] No `process` that turns a module dependency and C++20 modules info into that ordered set of entries.
  - [P3] No `write_modmap` that writes compiler-specific modmap text and the BMI path list, including an optional generated module.
  - [P3] Debug text `ModmapItem{name: ..., path: ...}` and the `hash(name) ^ (hash(path) << 1)` hash are absent.

### `tools/cpp/modules_tools/generate-modmap/generate-modmap_test.cc`
- Bazel lines: 116
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `process` on an empty `ModuleDep` and `Cpp20ModulesInfo` is not checked to return an empty modmap.
  - [P3] `process` is not checked to return the transitive closure of `require_list` through `usages`, pairing each reached name with `modules[name]` and omitting modules that are not required or used.
  - [P3] `write_modmap` for `clang` with no generated module is not checked to emit the same modmap text and the same `.input` path list regardless of item insertion order.

### `tools/ctexplain/analyses/summary.py`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Bazel's `ctexplain --analysis=summary` counts unique `config_hash` values, unique labels, configured targets (input length, including duplicates), and labels that occur more than once, then prints `Configurations`, `Targets`, `Configured targets` with a signed one-decimal percent surplus versus the unique-target count, and `Targets with multiple configs`.
  - [P2] Zel has no configured-target graph summary that computes or prints those counts.

### `tools/ctexplain/analyses/summary_test.py`
- Bazel lines: 44
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ctexplain summary analysis and no test of it.
  - [P3] The Bazel test builds four configured targets (`//foo` under three config hashes, including a null configuration, and `//bar` under one) and expects `analyze` to report 3 configurations, 2 labels, 4 configured targets, and 1 label that appears more than once.

### `tools/ctexplain/bazel_api.py`
- Bazel lines: 180
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No ctexplain client invokes `blaze` in the current directory and returns `(exit code, stdout split on the OS line separator, raw stderr bytes)`.
  - [P2] No `cquery` wrapper always prepends `--show_config_fragments=transitive`, returns failure with empty targets on a non-zero exit, and otherwise keeps non-blank lines in order as configured targets whose `config` is unset, whose hash is the parenthesized token (`null` yields no fragments), and whose transitive fragments are the bracket list split on `", "`.
  - [P2] No `get_config` treats hash `HOST` as an empty host configuration and `null` as an empty null configuration without a subprocess, and otherwise runs `config --output=json`, errors on a non-zero exit, and builds fragment-to-option-class and option-class-to-string maps after stripping each Java name to its last `.` component.

### `tools/ctexplain/bazel_api_test.py`
- Bazel lines: 156
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs`
- Verdict: partial
- Differences:
  - [P2] `cquery` prints a 7-character configuration id (`short_hash`). `testBasicCquery` requires `len(config_hash) > 10`.
  - [P2] A `genrule` `tools` dep queried with `--universe_scope` of that genrule is not the config id `HOST`. The label is configured in the top-level target configuration (an exec-configured copy may also appear). `get_config` therefore does not return an empty host configuration (`fragments` and `options` both empty).
  - [P2] `--define a=b` stays on `CoreOptions` as `define=[a=b]` (`/workspace/zel-cursor/crates/zel-server/src/commands/config.rs`). It is not moved into the `user-defined` map as `--define:a`=`b`.

### `tools/ctexplain/ctexplain.py`
- Bazel lines: 157
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no `ctexplain` command or other tool that measures how flags and transitions change configured-target graph size.
  - [P2] No `--analysis` list (default `summary`; also `culprits`, `forked_targets`, `cloned_targets`) and no `-b`/`--build` invocation string.
  - [P2] No usage exit when `-b` is omitted, and no hard exit when more than one `-b` is given (multi-build shareability is unimplemented in Bazel too).
  - [P2] No split of a build line into `//` labels versus flags, then collection of configured targets via `cquery deps(...)` plus per-hash `config` lookup.
  - [P2] No `summary` report of configuration count, target count, configured-target count with percent surplus versus targets, and targets that have more than one config. The other three analyses only print that they are not implemented.

### `tools/ctexplain/lib.py`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no ctexplain `analyze_build`. Bazel runs `cquery` on `deps(<comma-joined labels>)` plus the given build flags, raises `invocation failed: ` plus UTF-8 stderr when that invocation fails, resolves each distinct `config_hash` once with `bazel config`, and returns the configured targets with the resolved config (including host and null), the hash, and transitive fragments.

### `tools/ctexplain/lib_test.py`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ctexplain library and no `analyze_build` that runs `cquery deps(...)` with `--show_config_fragments=transitive`, then turns each config hash into a configuration.
  - [P3] The test expects `//testapp:a`, `//testapp:h`, and `//testapp:h.src` in that order: the Starlark rule has a plain `Configuration`, the `cfg = "exec"` filegroup has config hash `HOST` and a `HostConfiguration`, and the source file has config hash `null` and a `NullConfiguration`.
  - [P3] A target referenced twice (`//testapp:other` as both an exec dep and a filegroup src) appears once, in the order `//testapp:a`, `//testapp:h`, `//testapp:other`, `//testapp:h.src`.

### `tools/ctexplain/types.py`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Bazel's `Configuration` is a frozen value object: equality and dict-key hashing follow the fragment map (fragment base name to required options-class names) and the nested options map (options class to string key/value pairs as `bazel config` prints them). Identical contents compare equal; any fragment or option change is a different key.
  - [P2] `ConfiguredTarget` stores a label, an optional `Configuration`, the Bazel-reported config hash, and transitive fragment base names (package prefixes stripped, `$` inner names kept).
  - [P2] `HostConfiguration` is a distinct `Configuration` subclass and a merge marker. It defaults `fragments` and `options` to empty, and those fields are not filled from `bazel config`.
  - [P2] `NullConfiguration` is a distinct subclass that has no fragments or options (empty defaults).
  - [P2] Zel has no ctexplain records. Analysis `ConfiguredTarget` always holds a `BuildConfigurationKey` and does not store an optional configuration, a Bazel config hash, transitive fragment names, or host/null markers whose fragments stay empty.

### `tools/ctexplain/types_test.py`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ctexplain `Configuration` value and no test that one can be used as a map key.
  - [P3] Two configurations with the same fragments and the same nested option maps share one key, including when the option maps are separately constructed (`fragments` `F1`, options `o1` / `k1` = `v1` stays a single entry).
  - [P3] A different fragments value (`F2`), option-class name (`o2`), option key (`k2`), or option value (`v2`) is a distinct key, so the map grows from 1 to 5 entries.
  - [P3] Equality is by those contents: matching fragments and options compare equal, and a fragments mismatch or an options mismatch compares unequal.

### `tools/ctexplain/util.py`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] `ProgressStep` prints `{msg}...` with no newline, then on exit (including when the body raises) ` done in {N} ms.` when elapsed is under 0.1s, otherwise ` done in {N.NN} s.`, unless `show_done` is false.
  - [P2] `percent_diff(val1, val2)` returns `(val2 - val1) / val1 * 100` as a signed one-decimal percent (`+50.0%` or `-20.0%`) and divides by zero when `val1` is 0. Zel has no ctexplain progress timer or this percent formatter.

### `tools/distributions/debian/deps.bzl`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `debian_deps()` is absent, so nothing registers the four Debian system repositories `debian_java_deps`, `debian_cc_deps`, `debian_proto_deps`, and `debian_bin_deps`.
  - [P3] No repository maps `java` to `/usr/share/java` and uses `//tools/distributions/debian:debian_java.BUILD`.
  - [P3] No C/C++ repository is created with an empty symlink map and `//tools/distributions/debian:debian_cc.BUILD` (system libs linked with `-l` only).
  - [P3] No repository maps `google/protobuf` to `/usr/include/google/protobuf` and uses `//tools/distributions/debian:debian_proto.BUILD`.
  - [P3] No repository maps `protoc`, `grpc_cpp_plugin`, and `grpc_java_plugin` to `/usr/bin/protoc`, `/usr/bin/grpc_cpp_plugin`, and `/usr/bin/grpc_java_plugin` and uses `//tools/distributions/debian:debian_bin.BUILD`.

### `tools/distributions/debian/protobuf/protobuf.bzl`
- Bazel lines: 18
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `py_proto_library(name, **kargs)` is an empty macro: any extra keywords are accepted and the call creates no rule, provider, file, or action, so `src/main/protobuf/BUILD` can still load and call it when the Debian `@com_google_protobuf` overlay replaces the real protobuf repository.
  - [P3] Zel does not define that no-op, or the Debian system-protobuf workspace that ships it. The only `py_proto_library` it loads is the upstream protobuf rule, which runs a proto aspect and emits Python sources and `.pyi` files.

### `tools/distributions/distribution_rules.bzl`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no distribution macros that publish a public alias over a private `{name}_checked_in` target and switch it with `select`.
  - [P3] `distrib_java_import` is absent: it would instantiate `java_import` as `{name}_checked_in`, then alias `name` to that target by default and to `@debian_java_deps//:{name}` under `//src/conditions:debian_build` when `"debian"` is in `enable_distributions`, forwarding `visibility` and `applicable_licenses` onto the alias.
  - [P3] `distrib_cc_library` is absent: same alias/`select` pattern using `cc_library` and `@debian_cc_deps//:{name}`, without forwarding `applicable_licenses`.
  - [P3] `distrib_cc_binary` is absent: same pattern using `cc_binary` and `@debian_bin_deps//:{name}`, without forwarding `applicable_licenses`.
  - [P3] `distrib_jar_filegroup` is absent: same pattern using `native.filegroup` and `@debian_java_deps//:{name}`, without forwarding `applicable_licenses`.

### `tools/distributions/system_repo.bzl`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `system_repo` repository rule, and `tools/distributions/system_repo.bzl` is not embedded in bazel tools.
  - [P3] Nothing creates the `symlinks` string-dict entries (repo-relative link path to an absolute system target), writes `WORKSPACE` as `workspace(name = "<ctx.name>")`, and writes `BUILD.bazel` from the mandatory single-file `build_file` label via `ctx.read`.

### `tools/jdk/launcher_flag_alias.bzl`
- Bazel lines: 43
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/jdk/launcher_flag_alias.bzl`
- Verdict: partial
- Differences:
  - [P1] `configuration_field(fragment = "java", name = "launcher")` stays unset. Bazel's `JavaConfiguration.getJavaLauncherLabel` returns the `--java_launcher` label (after the exec transition, the value copied from `--host_java_launcher`). Zel's `late_bound_label` reads the option key `launcher`, and `java_launcher` is not in the options map that function reads.
  - [P1] With `_launcher` unset, the implementation returns `None`. The target is an empty `DefaultInfo` and does not forward `CcInfo`, `CcLauncherInfo` (`cc_common.launcher_provider`), or the launcher's `DefaultInfo.files` and `default_runfiles`.
  - [P2] The attribute `providers` requirement (`CcInfo` and `CcLauncherInfo`) is never applied to the `--java_launcher` target.

### `tools/jdk/proguard_allowlister_test.py`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No test runs `ProguardConfigValidator.ValidateAndWriteOutput` on `proguard_allowlister_test_input.pgcfg` and checks that the output contains `# Merged from <input path>`.
  - [P3] No test writes a temp config and asserts `RuntimeError` text includes `-dontnote` for a bare `-dontnote`.
  - [P3] No test asserts `RuntimeError` text includes `-dontwarn` for a bare `-dontwarn`.
  - [P3] No test asserts `RuntimeError` text includes `-optimizations` for a global `-optimizations` rule.
  - [P3] No test asserts one failure reports both `-optimizations` and `-dontnote` when both illegal flags are present.

### `tools/mini_tar/mini_tar.py`
- Bazel lines: 287
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/build_defs/pkg/build_tar.py`
- Verdict: partial
- Differences:
  - [P3] Bazel `--mode` (octal, forced onto file members) and `--owner` (`uid.gid`, default `0.0`) are absent. Zel only has `--output`, `--file`, `--directory`, and `--root_directory` (default `./`); uid and gid stay 0.
  - [P3] Bazel `--directory` is the archive root (`strip('/')`, prepended unless the name is absolute or already under that root). Zel prefixes the dest (`lstrip('/')`, skipped when the directory is `/`) and also joins `--root_directory` (`rstrip('/')` turns `./` into `.`), so names are `./...` and a `./` directory member is inserted. The writer is `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/build_defs/pkg/archive.py`.
  - [P3] Bazel `main` always writes mtime 946684800 (2000-01-01 UTC). The writer also accepts `None`->0, `'portable'`, or an int. Zel always writes mtime 0; `PORTABLE_MTIME` is unused and `preserve_tar_mtimes` is never applied.
  - [P3] With no forced mode, Bazel `os.access`es the archive dest path (usually 0644). Zel `os.access`es the input path (0755 if executable, else 0644).
  - [P3] Bazel normalizes dest with `normpath` and forces `/`, and walks trees with `/` and no depth cap. Zel leaves `os.sep`, walks with `os.path.join`, and raises `TarFileWriter.Error` when a directory walk exceeds depth 100.
  - [P3] Zel prints `sys.argv` before building the archive. Bazel does not.

### `tools/mini_tar/mini_tar_test.py`
- Bazel lines: 200
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No writer test checks that an empty `TarFileWriter` produces a tar with zero entries.
  - [P3] No check that omitted `default_mtime` is 0, a passed integer is stored as-is, or `"portable"` is 946684800.
  - [P3] No check that `add_file_and_parents` keeps dotted names (`a`, `b`, `b/.c`, `..d`, `.e`) and inserts the parent directory `b`.
  - [P3] No check that `add_parents` emits only ancestor directories at mode 0755, a later file at 0644, and a second call adds only the new directory `a/b/e`.
  - [P3] No check that `add_file_at_dest` on a tree stores file bytes, uses 0640 for files and 0750 for directories, names entries `./...`, then with `root_directory="foo"` and dest `x` prefixes them as `foo`, `foo/x`, and `foo/x/...`.

### `tools/mini_tar/tar.bzl`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `mini_tar` rule and no `tar_filetype` list. Nothing registers an implicit exec tool `//tools/mini_tar:mini_tar` or runs a `PackageTar` action with `use_default_shell_env`.
  - [P3] The action arguments are absent: `--output`, `--mode` (default `0555`), `--owner` (default `0.0`), `--directory` only when `package_dir` is set, `--mtime=<n>` only when `mtime != -1` (default `-1`), and one `--file=<quoted path>=<dest>` per source.
  - [P3] Archive destinations are `file.short_path` with the rule package prefix (`ctx.label.package + "/"`) removed, and `\` and `=` in the source path are escaped. The arg list uses a `flag_per_line` param file (`@%s`), not always.
  - [P3] The `mini_tar` macro defaults `out` to `name + ".tar"` and still accepts an explicit `out`.

### `tools/osx/xcode_configure.bzl`
- Bazel lines: 329
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/osx/xcode_configure.bzl`
- Verdict: partial
- Differences:
  - [P1] `configure = True` has no effect. `is_configure_repo` returns false for any rule whose `.bzl` path contains `bazel_tools`, so `bazel sync --configure` skips `@local_config_xcode`, and vendor mode copies and reuses that repo.
  - [P2] `repository_ctx.execute` keeps the 600s default and `BAZEL_OSX_EXECUTE_TIMEOUT` unscaled. `--experimental_scale_timeouts` is not applied.
  - [P2] A failed spawn is return code 1 with a process-wrapper `execvp` line in the `# Error:` comment. A timeout is return code 256 with stderr `Timed out` and empty stdout.
  - [P2] Command output is lossy UTF-8, so a non-UTF-8 byte from `xcodebuild` or `xcode-locator` becomes U+FFFD in SDK versions and error comments.
  - [P2] SIGTERM is return code 143 and evaluation continues into the stub or error comment.

### `tools/python/pywrapper_test.py`
- Bazel lines: 177
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never runs this Unix integration test of the strict `py3wrapper.sh` (template expanded with version 3 and `STRICT=1`) on a PATH limited to the scratch directory.
  - [P3] No check that an executable `python` whose `-V` text matches ` 3.` is executed with stdout exactly `I am mock Python!`, empty stderr, and exit 0. The wrapper probes `-V` before `exec`.
  - [P3] No check that a PATH with neither `python3` nor `python` exits 1 and reports `Neither 'python3' nor 'python' were found`.
  - [P3] No check that an interpreter whose `-V` exits non-zero fails with `Could not get interpreter version`. Zel's wired autodetecting toolchain is `@rules_python//python:autodetecting_toolchain` (`runtime_env_toolchain_interpreter.sh`), which never runs `-V` and would exec that interpreter.
  - [P3] No check that a non-executable `python` is ignored. This test depends on `which` requiring the executable bit. The rules_python script uses `command -v`, which this wrapper's comments treat as not checking that bit, and it also rewrites `exec` when `pyvenv.cfg` is present.
  - [P3] `tools/python/BUILD` in Zel does not expand `pywrapper_template.txt` into `py3wrapper.sh` or `py3wrapper_nonstrict.sh`. The vendored template matches Bazel, but nothing invokes these cases.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/BranchCoverage.java`
- Bazel lines: 364
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P2] Evaluated state is a per-line sticky OR (`BitSet` by line number). Any evaluated branch on a line marks every branch on that line evaluated, and a later `evaluated=false` does not clear it. On merge or reprint, an unevaluated sibling is therefore reported as taken `0`, not `-`. The merger stores evaluated per `(line, block, branch)` and leaves that sibling as `-`.
  - [P2] Execution counts are `long` and wrap on overflow. The merger sums awk numbers (doubles), so totals past 2^53 are not exact and do not wrap at 2^64.
  - [P3] A negative line number throws `IndexOutOfBoundsException` from `BitSet` on set or get. The map also throws `IllegalStateException` once capacity reaches `0x40000000` (or when linear probing wraps a full table). The merger accepts those records and has no table-size cap.
  - [P3] A `null` branch id throws `IllegalArgumentException`, and a `null` block id throws on `equals` during lookup. The merger has no equivalent check.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/BranchCoverageItem.java`
- Bazel lines: 90
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P2] `wasExecuted()` treats an empty `blockNumber` as a BA taken-code (`true` only when `nrOfExecutions == 2`) and any other block as executed when `nrOfExecutions > 0`. The merger has no such predicate: `BRH` counts a branch when taken is not `-` and is `> 0`, so taken `1` with an empty block id is a hit and taken `2` is not a distinct BA state.
  - [P2] The BA shape this type documents (empty block and branch ids, `nrOfExecutions` holding 0, 1, or 2) is not represented. `BA:` lines are ignored, and `--legacy_branches` / `--nolegacy_branches` are accepted and discarded, so every kept branch is stored and printed as `BRDA`.
  - [P2] `evaluated` is an independent boolean (not evaluated prints `-` even if a count is stored). The merger collapses that to one taken token: `-` means not evaluated and does not add, and any other token is accumulated with awk numeric conversion rather than a Java `long`.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/BranchCoverageKey.java`
- Bazel lines: 99
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P2] Identity and print order are one `LC_ALL=C` byte string `sprintf("%012d", line) "," block "," branch`, not Java's int line compare followed by UTF-16 `String.compareTo` on block, then branch.
  - [P2] That encoding is not injective: `block="a,b", branch="c"` and `block="a", branch="b,c"` are one key and one summed `BRDA` line. Java `equals` and `compareTo` treat them as different keys.
  - [P2] Line order matches `int` order only for non-negative values that fit in 12 digits. Equal-width negatives sort by digit magnitude (`-1` before `-2`), and a 13-digit line sorts by its first digit (`1000000000000` before `999999999999`).
  - [P2] A block or branch byte below `','` (0x2C), or a non-ASCII id (UTF-8 bytes versus UTF-16 code units), sorts differently from `String.compareTo`.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/Constants.java`
- Bazel lines: 52
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P2] `BA:` is never read or written. Bazel parses `BA:<line>,<taken>` (`taken` is `-` when unevaluated) and, with `--legacy_branches`, prints `BA:<line>,<0|1|2>` instead of `BRDA:`. Zel accepts that flag and still only parses and prints `BRDA:` (count or `-`).
  - [P2] Gcov intermediate markers are unused: `version:`, `cwd:`, `file:`, `function:`, `lcount:`, `branch:`, and the branch words `notexec`, `nottaken`, and `taken`.
  - [P1] Input discovery is only `*.dat`. `.gcov`, `.gcov.json.gz`, and `.profdata` do not select files, so a lone `.profdata` input is not copied through to the output.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/Coverage.java`
- Bazel lines: 146
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P2] `maybeReplaceSourceFileNames` rewrites each `SourceFileCoverage` name in place and leaves the `TreeMap` keyed by the pre-rename name, so two entries that land on the same new name stay unmerged and `getAllSourceFiles()` stays in old-name order. The merger renames each `SF:` path before accumulating, so those records are summed and the report is sorted by the new path even when no later filter or manifest rebuilds the map. A following `filterOutMatchingSources` or `getOnlyTheseSources` does re-`add` by the current name, which merges and re-sorts; the merger merges on the new name in every case.
  - [P2] `filterOutMatchingSources` drops a path only when Java `String.matches` succeeds (full-string `java.util.regex`), and an invalid pattern throws `PatternSyntaxException`. The merger uses awk ERE wrapped as `^(regex)$`. An invalid pattern fails that awk stage and, with no `pipefail`, can come out as an empty report and exit 0.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/GcovJsonParser.java`
- Bazel lines: 131
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] Zel never gunzips or JSON-parses a GCC 9+ gcov intermediate document into per-source coverage. `lcov_merger.sh` only merges LCOV `SF`/`FN`/`FNDA`/`BRDA`/`DA` records from `*.dat` or `--reports_file`.
  - [P2] A `format_version` other than `"1"` is not logged. Bazel warns and still parses.
  - [P2] Function `name` to `start_line` (last name wins) and summed `execution_count` are not recorded. `demangled_name`, columns, and block counts are unused.
  - [P2] Line `line_number` and summed `count` are not recorded. A negative line number throws; `unexecuted_block` and the line's `function_name` are unused.
  - [P2] Branches are not mapped to block `"0"`, a 0-based index, a line-wide taken flag that is true when any branch on that line has `count > 0`, and that branch's own count. `fallthrough` and `throw` are unused.
  - [P2] Input must be a gzip member. Plain JSON fails, and a null document, missing `format_version`, or null `files`, `functions`, `lines`, or `branches` throws instead of producing an empty report. The decompressed bytes are decoded with the platform default charset.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/GcovParser.java`
- Bazel lines: 242
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] Zel never parses gcov intermediate text. Coverage merging only reads LCOV `*.dat` records (`SF`, `FN`, `FNDA`, `DA`, `BRDA`) and does not consume `*.gcov`.
  - [P2] `file:` does not open a source-file record (empty name rejected after closing the previous file). `version:` and `cwd:` are not ignored, and other lines are not dropped with a per-line warning plus one end-of-input malformed warning.
  - [P2] `function:` lines of 3 or 4 comma fields are not recorded (start line and execution count kept, end line ignored; non-numeric fields skipped).
  - [P2] `lcount:` lines of 2 or 3 fields are not recorded (line number and execution count; `has_unexecuted_block` ignored).
  - [P2] `branch:` values `notexec`, `nottaken`, and `taken` are not emitted as block `"0"` branches, numbered in input order and flushed in ascending line order, with `taken` as evaluated count 1, `nottaken` as evaluated count 0, and `notexec` as not evaluated.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/LcovMergerFlags.java`
- Bazel lines: 121
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P1] Bazel accepts both `--flag=value` and `--flag value`. Zel accepts only `--flag=value`, so a space-separated value is an unrecognized argument and exits 1.
  - [P2] Bazel ignores unknown options. Zel exits 1 on them. `--lcovfile_path` is an alias of `--reports_file` in Bazel and unrecognized in Zel.
  - [P2] `--legacy_branches` selects `BA:` lines instead of `BRDA:`. Zel ignores bare `--legacy_branches` and `--nolegacy_branches` and always prints `BRDA:`. `--legacy_branches=true` or `false` is valid in Bazel and a parse error in Zel.
  - [P2] `--parse_parallelism` is an integer (default 4); a non-integer throws. Zel ignores `--parse_parallelism=*` including non-integers, and `--parse_parallelism N` is rejected.
  - [P2] An empty `--coverage_dir=`, `--output_file=`, or `--source_file_manifest=` is present in Bazel. Zel treats those empty strings as unset, so the required-input or required-output check fails and a blank manifest is not applied.
  - [P2] When both `--coverage_dir` and `--reports_file` are set, Bazel logs a warning and still returns both values. Zel keeps both and does not log that warning.
  - [P2] If several requirements are missing, Bazel reports a missing coverage dir or reports file before a missing output file. Zel reports a missing `--output_file` first.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/LcovParser.java`
- Bazel lines: 427
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P2] A record is kept only after `end_of_record`. Lines before the first `SF:`, or between `end_of_record` and the next `SF:`, are ignored. A second `SF:` while a record is open is rejected, and an empty `SF:` is rejected. Zel switches source on every `SF:` (including an empty path), keeps data with no `end_of_record`, and still attaches lines after `end_of_record` to the previous source. Lines that appear before any `SF:` are stored under an empty path.
  - [P2] `BA:<line>,<taken>` is parsed: `0` is not evaluated, `1` is evaluated and not taken, `2` is evaluated and taken once. The block id is `0`, and the branch index increments while the line stays the same. It resets when the line changes or an `SF:` line is seen, including a rejected `SF:`. Any other taken value drops that line. Zel drops every `BA:` line.
  - [P2] `FN:` accepts two or three non-empty comma fields. The start line must be an integer, the optional end line is ignored, and the name is the last field. A repeated name keeps the later start line. Zel splits on the first comma, so `FN:2,3,func` is the function `3,func`, commas in the name are kept, the start line is not checked, and a repeated name keeps the first line.
  - [P2] `FNDA:` must be exactly two non-empty fields with a `long` count, or the line is dropped. A function with no accepted `FNDA:` has no execution count. Zel coerces the count with `+ 0` (a non-numeric count becomes 0), allows commas in the name, and emits count 0 for an `FN:` that had no `FNDA:`.
  - [P2] `DA:` must have two or three non-empty fields, an `int` line, and a `long` count. A third field is an ignored checksum. Duplicate lines sum. A negative line throws and aborts the parse. Zel accepts two or more fields, ignores extras, coerces bad numbers to 0, and keeps negative lines.
  - [P2] `BRDA:` must have exactly four non-empty fields. `taken` of `-` means not evaluated and count 0; any other value must be a `long` or the line is dropped. A negative line aborts the parse. Evaluated-ness is stored per line, so one taken branch makes every branch on that line evaluated, and a sibling `-` is reported as count 0. Zel accepts four or more fields, tracks evaluated-ness per branch, treats any non-`-` value as evaluated (non-numeric becomes 0), and leaves a `-` branch unevaluated when another branch on the same line was taken.
  - [P2] Line numbers must fit in a Java `int` and counts in a `long`, or that line is dropped. Zel stores awk doubles.
  - [P2] Inside a record, an unrecognized line, or a malformed `FNF`, `FNH`, `BRF`, `BRH`, `LH`, or `LF` value, is skipped with a warning. Those summary values are not stored. Zel ignores those lines with no warning. Well-formed summary lines do not change the coverage data in either implementation.
  - [P2] The stream is decoded as UTF-8, and line breaks may be CR, LF, or CRLF. Zel reads bytes in the awk locale, so a CRLF tracefile leaves a trailing CR on the source path.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/LcovPrinter.java`
- Bazel lines: 219
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P2] `--legacy_branches` is ignored. Bazel prints `BA:<line>,<taken>` (`0` never evaluated, `1` evaluated but not taken, `2` taken) instead of `BRDA`, and still prints `BRF`/`BRH`. Zel always prints `BRDA` with the count or `-`. Input `BA:` lines never become branch records, so those branches are missing.
  - [P2] Unevaluated `BRDA` (`-`) is per branch. Bazel's `evaluated()` flag is per source line, so if any branch on a line was evaluated, every branch on that line is printed as a number (`0` when the count is 0) rather than `-`.
  - [P2] An `FN` with no `FNDA` is printed as `FN` only and is left out of `FNF`/`FNH`. Zel adds `FNDA:0` and counts that function in `FNF`.
  - [P2] Bazel prints `FN:<start>,<name>` from the integer start line, dropping the optional end-line field. Zel keeps the text after the first comma, so `FN:007,foo` stays zero-padded and `FN:10,20,name` is reprinted with the end line in the name.
  - [P2] `FNDA`, `DA`, and `BRDA` counts are awk doubles, so values above 2^53 are rounded. Bazel prints the exact 64-bit count.
  - [P2] Source paths and function names are ordered with a C-locale byte sort. Bazel uses Java `String` order (`TreeMap`), so non-ASCII names can come out in a different order.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/LineCoverage.java`
- Bazel lines: 133
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P2] `addLine` throws `IllegalArgumentException` for a negative line and only accepts a signed 32-bit line number. The merger stores `a[1] + 0` for any numeric text, including negatives and values outside `int`, and negative lines then sort lexicographically on `%012d` (`-1` before `-2` before `-10`).
  - [P2] Execution counts are `long` and wrap on overflow. Sums and hit checks treat a line as executed only when the count is `> 0`, including a stored zero. The merger uses awk doubles: values above 2^53 are inexact, sums near `Long.MAX_VALUE` stay large and positive instead of wrapping, and a magnitude of at least 1e20 is printed in scientific notation. Non-decimal count text (`1.5`, `1abc`, `-`, empty) is coerced and kept.
  - [P3] Storage is a dense `long[]` indexed by line number. `addLine(Integer.MAX_VALUE)` does not grow the array (`line + 1` overflows) and throws `ArrayIndexOutOfBoundsException`; other very large indexes allocate every slot up to that line. The merger keeps a sparse map of lines that appear.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/Main.java`
- Bazel lines: 386
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P1] Bazel walks `--coverage_dir` for `.dat`, `.gcov`, `.gcov.json.gz`, and `.profdata`, and merges LCOV with gcov intermediate text and gcov JSON. Zel merges only regular `*.dat` files, in `LC_ALL=C` path order, and skips the output path.
  - [P1] A single `.profdata` and no other coverage is copied over the output (`REPLACE_EXISTING`). Other coverage plus a `.profdata` logs a warning and exits 0 without writing. More than one `.profdata` logs a severe error (the message says `.profadata`) and is treated as absent. Zel ignores `.profdata`.
  - [P2] With both `--reports_file` and `--coverage_dir`, Bazel warns and uses the list only for LCOV tracefiles; gcov, gcov JSON, and profdata still come from the directory. A blank or missing list entry makes `parseFiles` call `System.exit(1)`. Zel ignores the directory when a reports file is set, skips empty and non-file lines, and still exits 0.
  - [P2] `--sources_to_replace_file` is `key:value` on `:`, and only lines with exactly two fields are kept. Duplicate keys throw and the process exits 1. Renames update the printed name but the map stays keyed by the old path, so with no filter or manifest the order follows the old names and two sources renamed to one path stay separate records. Zel splits on the first space, last duplicate wins, and merges and sorts by the new path.
  - [P2] A missing or unreadable `--source_file_manifest` still filters (a read error yields an empty allow-list and an empty report). Zel applies the manifest only when the file exists. Both drop `.gcno` and `.em` lines.
  - [P2] `--legacy_branches` makes Bazel print `BA:` lines instead of `BRDA:`. Zel accepts the flag and still prints `BRDA:`.
  - [P2] Bazel accepts `--flag value`, the `--lcovfile_path` alias of `--reports_file`, and unknown options. Zel accepts only `--flag=value`, rejects unknown flags with exit 1, and has no `--lcovfile_path` alias. Required-flag failures differ in text (`output_file was not specified.` / `At least one of coverage_dir or reports_file should be specified.` versus Zel's `lcov_merger:` errors). If the empty output cannot be created, Bazel exits 1; Zel still exits 0.
  - [P2] An unreadable tracefile exits 1. Zel continues. Default `--parse_parallelism` is 4, so disagreeing `FN` start lines are last-merge-wins and can follow partition order. Zel keeps the first `FN` in sorted file order. An `FN` with no `FNDA` is omitted from Bazel's `FNDA`/`FNF`; Zel emits `FNDA:0` and counts it. Branch evaluated-ness is per line in Bazel (any taken branch on that line makes a sibling `-` print a count) and per branch in Zel. `BA:` input is parsed by Bazel and dropped by Zel. Bazel keeps a source only after `end_of_record`; Zel commits every `SF:`. `--filter_sources` is a full-match Java regex; Zel uses an awk ERE anchored the same way.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/Parser.java`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel `Parser.parse` reads one `InputStream` and returns `List<SourceFileCoverage>` or throws `IOException`, so `Main.parseFiles` can apply the same parser to every coverage file.
  - [P3] Zel has no parser with that contract. `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh` only folds LCOV text already on disk in awk, and gcov intermediate text and gcov JSON are not parsed through a shared stream parser.

### `tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/SourceFileCoverage.java`
- Bazel lines: 215
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P2] Function start lines are last-write-wins: `addFunctionLineNumber` replaces the line, and `merge` `putAll`s the second file over the first. Zel stores an `FN` line only when `(source, name)` is not already in `fnline`, so the first record wins within a file and across the merged trace.
  - [P2] `nrFunctionsFound` and `nrFunctionsHit` count only `functionsExecution`. A name added solely with `addFunctionLineNumber` has no execution entry, is not found or hit, and is not printed as `FNDA`. Zel's `fnseen` is the union of `FN` and `FNDA`; every seen function is emitted as `FNDA` (count `0` when no `FNDA` was present) and included in `FNF`/`FNH`.
  - [P2] `addBranch` combines the evaluated flag per source line. `getAllBranches` then reports every branch on that line as evaluated, so a branch added only as not evaluated is returned with count `0` instead of never-evaluated (the line-900 `"0","2"` case in `SourceFileCoverageTest`). Zel records `brev` per `(line, block, branch)` and prints `-` for a key that never had a numeric taken, even when another branch on the same line was taken.

### `tools/test/CoverageOutputGenerator/javatests/com/google/devtools/coverageoutputgenerator/BranchCoverageTest.java`
- Bazel lines: 265
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P3] `BranchCoverage.get` reads `evaluated` from a `BitSet` keyed only by line number. `addBranch` sets that bit when `evaluated` is true and leaves it set, so every branch on that line reads back true, including a branch added with `evaluated == false` and every branch `merge` copies from the other map. Zel stores taken-versus-`-` on the `(line, block, branch)` key. A `-` branch stays `-` when another branch on the same line has a numeric taken count. These fixtures never mix those flags on one line, so the asserted items still match Zel's per-branch flag.
  - [P3] `testExhaustedIteratorThrows` requires `hasNext()` to be false and `next()` to throw `NoSuchElementException` after the single entry is consumed. Zel's merger has no branch iterator.

### `tools/test/CoverageOutputGenerator/javatests/com/google/devtools/coverageoutputgenerator/CoverageTest.java`
- Bazel lines: 247
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P3] `filterOutMatchingSources(null, ...)`, `filterOutMatchingSources(coverage, null)`, `getOnlyTheseSources(null, ...)`, and `getOnlyTheseSources(coverage, null)` throw `IllegalArgumentException`. Omitting `--filter_sources` or `--source_file_manifest` keeps every source, and a manifest path that is not an existing file is ignored (`have_manifest=0`).
  - [P3] Filters are awk EREs anchored as `^(regex)$`, not Java `String.matches`. Java-only syntax does not select the same paths, a `)` in the pattern breaks the added group, and an invalid pattern makes `awk` fail. The script does not set `pipefail`, so that failure can still exit 0 with empty output instead of `PatternSyntaxException`.
  - [P3] `getOnlyTheseSources` keeps names by exact set membership. The merger drops manifest lines ending in `.gcno` or `.em` before that check, so a source with one of those names is removed even when it is listed.
  - [P3] Line and branch hits are Java `long` sums. The merger adds them as awk doubles, so totals above 2^53 are not exact.
  - [P3] Distinct sources are ordered by `TreeMap` (`String.compareTo`, UTF-16 code units). The merger sorts paths with `LC_ALL=C` byte order, which differs for some non-ASCII names. `src_1.foo` before `src_2.foo` matches.

### `tools/test/CoverageOutputGenerator/javatests/com/google/devtools/coverageoutputgenerator/GcovJsonParserTest.java`
- Bazel lines: 133
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel gunzips one gcov JSON document (`format_version` `"2"`) and asserts a single source file `baz.cc`.
  - [P3] Function execution is `main` -> 2 (`execution_count`); lines are 4->3, 5->3, 6->1, 7->3.
  - [P3] Line 5's two branches become items with block `"0"`, branch ids `"0"` and `"1"`, both taken, counts 1 and 2.
  - [P3] Zel's coverage merger only merges LCOV `*.dat` records and never gunzips or parses `.gcov.json.gz`, so this mapping is absent.

### `tools/test/CoverageOutputGenerator/javatests/com/google/devtools/coverageoutputgenerator/GcovParserTest.java`
- Bazel lines: 175
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel parser or test consumes gcov intermediate text. `"Invalid gcov file"` is expected to yield an empty source-file list.
  - [P3] Both fixtures (4-field `function:` / 3-field `lcount:` plus ignored `version:` and `cwd:`, and 3-field `function:` / 2-field `lcount:`) are expected to parse to one file `tmp.cpp`. Zel's merger only reads LCOV `SF`/`FN`/`FNDA`/`DA`/`BRDA` and does not accept these records.
  - [P3] Expected coverage is 5 functions found and 3 hit, 14 instrumented lines and 13 with a nonzero count (duplicate `lcount` executions summed), and 16 branches found and 8 hit.
  - [P3] Branches are recorded as block `"0"` with a per-line index: `taken` is evaluated with count 1, `nottaken` is evaluated with count 0, and `notexec` is not evaluated with count 0. That mapping is absent.

### `tools/test/CoverageOutputGenerator/javatests/com/google/devtools/coverageoutputgenerator/LcovMergerTestUtils.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No generator builds synthetic LCOV records `SF:{prefix}{i}.cc`, `FNF:0`, `FNH:0`, `BA:{line},2` for lines 1, 5, 9, ... through `numLines`, `BRF`/`BRH` as integer `numLines/4` (which can disagree with the `BA` count), `DA:{line},{line%2}` for every line, `LH` as integer `numLines/2` (short of the odd-line hit count when `numLines` is odd), `LF:{numLines}`, and `end_of_record`.
  - [P3] No helper always writes that text to a real `coverage0.dat` (UTF-8, platform line separator after every line, including when the requested file count is 0) and then adds `coverage{i}.dat` symlinks to that file for `i` in `1..numLcovFiles`. Zel coverage tests and the baseline writer emit fixed or empty `FNF:0`/`FNH:0`/`LH:0`/`LF:0` records and do not build this symlink corpus.

### `tools/test/CoverageOutputGenerator/javatests/com/google/devtools/coverageoutputgenerator/LcovParserTest.java`
- Bazel lines: 214
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P3] `BA:` lines are dropped. The test expects gcov `BA:line,taken` to become branches with block `"0"` and a per-line branch index: `0` is not evaluated with count 0, `1` is evaluated with count 0, and `2` is evaluated with count 1 (seven branches on lines 2, 4, and 7). The merger output for that tracefile keeps only the `DA` lines.
  - [P3] `FN:2,3,func` is not recorded as function `func` at start line 2. The first comma is the only split, so the name is `3,func`. Reprint is `FN:2,3,func` plus a synthesized `FNDA:0,3,func` and `FNF:1`. The Java parser stores only `{func -> 2}` and adds no execution count when there is no `FNDA`.

### `tools/test/CoverageOutputGenerator/javatests/com/google/devtools/coverageoutputgenerator/LcovPrinterTest.java`
- Bazel lines: 173
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P3] `testPrintBaLines` requires legacy-branch output (`print(..., true)`): `BA:<line>,2` if taken, `BA:<line>,1` if evaluated but not taken, `BA:<line>,0` if never evaluated (`BA:3,2`, `BA:3,1`, `BA:7,0`, `BA:7,0`), with `BRF`/`BRH` still printed. Zel accepts `--legacy_branches` and ignores it, and only writes `BRDA:` with the raw taken count or `-`.

### `tools/test/CoverageOutputGenerator/javatests/com/google/devtools/coverageoutputgenerator/LineCoverageTest.java`
- Bazel lines: 85
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P3] `addLine(-1, 1)` throws `IllegalArgumentException`. The merger accepts `DA:-1,1`, exits 0, and emits that line (before line 0), counting it in `LH` and `LF`.
  - [P3] After the last instrumented line, `iterator().next()` throws `NoSuchElementException`. Zel has no line iterator; it prints kept `DA:` records in numeric order and stops.

### `tools/test/CoverageOutputGenerator/javatests/com/google/devtools/coverageoutputgenerator/MainTest.java`
- Bazel lines: 133
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P3] `runWithArgs` is called with split arguments `--coverage_dir <dir>` and `--output_file <file>`. Zel accepts only `--flag=value`, so those calls are rejected and exit 1 instead of writing the report and exiting 0.
  - [P2] `generateLcovFiles` creates one real `coverage0.dat` and the rest as symlinks to it. `Files.walk` keeps every path ending in `.dat` and parsing follows the link, so each copy is merged. Zel's `find -type f -name '*.dat'` skips symlinks, so only the real file is merged.
  - [P3] Those tracefiles contain `BA:` records. Java parses them as branches and `LcovPrinter.print` (non-legacy) emits `BRDA`, `BRF`, and `BRH`. Zel never reads `BA:` lines, so that branch data is dropped.

### `tools/test/CoverageOutputGenerator/javatests/com/google/devtools/coverageoutputgenerator/SourceFileCoverageTest.java`
- Bazel lines: 209
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh`
- Verdict: partial
- Differences:
  - [P3] `testMismatchedBranchMerge` stores `evaluated` once per line and ORs it, so a branch that only appears unevaluated is still evaluated with count 0 when any branch on that line was evaluated (`900,"0","2"` -> true, 0 in both merge orders). The merger stores that flag per BRDA key, and a taken value of `-` leaves it unset, so the merged record is `BRDA:900,0,2,-`. The other asserted branches match, including line 800 counts 4 and 4 and line 900 counts 1 and 0.
  - [P3] `SourceFileCoverage.merge` copies the first file's function start lines and then `putAll`s the second file, so the second line number wins. The merger keeps the first `FN` line for each function name. These tests merge equal lines (`foo` at 3, `bar` at 10), so that assertion matches.
  - [P3] Function execution, line hits, and branch taken counts are summed as Java `long` values (wrap at 2^64). The merger adds them as awk numbers, so totals above 2^53 are inexact. The totals these tests assert (12, 8, 4, 3, 1, 10, and repeated 0+1+2=3) match.

### `tools/test/collect_coverage.sh`
- Bazel lines: 270
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/collect_coverage.sh`
- Verdict: partial
- Differences:
  - [P2] `LLVM_PROFILE_FILE` is `${COVERAGE_DIR}/%h-%p-%m.profraw`. Zel uses `${COVERAGE_DIR}/${BAZEL_LLVM_PROFILE_FILE:-%h-%p-%m.profraw}`, so a set `BAZEL_LLVM_PROFILE_FILE` changes the raw profile name. Unset, the pattern matches, including the `%c` suffix when `LLVM_PROFILE_CONTINUOUS_MODE` is set.
  - [P1] When `JAVA_RUNTIME_CLASSPATH_FOR_COVERAGE` is set, SingleJar source paths are prefixed with `$TEST_SRCDIR/`. Zel prefixes `$TEST_SRCDIR/$TEST_WORKSPACE/`, so the merged JaCoCo classpath jar is built from different paths.

### `tools/test/default_test_toolchain.bzl`
- Bazel lines: 27
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/test/default_test_toolchain.bzl`
- Verdict: partial
- Differences:
  - [P1] `visibility("private")` is not enforced. `visibility()` returns `None` and does not limit loads of `bool_flag` or `empty_toolchain` to the defining package.
  - [P2] Implicit `build_setting_default` and `help` on `bool_flag` stay configurable, so `select()` is accepted. Bazel marks both nonconfigurable.
  - [P1] Bool values accept only the case-sensitive tokens `true`, `1`, `yes`, `false`, `0`, and `no`. Bazel's boolean converter also accepts `t`, `y`, `f`, and `n` in any letter case, both on the command line and in `config_setting.flag_values`.

### `tools/test/empty_test.cc`
- Bazel lines: 15
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Non-Windows source for `//tools/test:tw_test`: `main` returns exit status 0 immediately, writes nothing, and ignores arguments.
  - [P3] Zel's embedded `tools/test` tree has no `tw_test` target and no program with that placeholder success behavior.

### `tools/test/empty_test_wrapper.cc`
- Bazel lines: 28
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel compiles this as the non-Windows `tw_lib` source. `main` ignores its arguments, writes `"<translation unit>: The C++ test wrapper is not used on this platform.\n"` to stderr, and returns 1.
  - [P3] Zel has no program that prints that message or exits 1. The identical `.cc` is only an unused resource; the embedded `tools/test` package does not build it, and the non-Windows `test_wrapper` is `dummy.sh`, which prints nothing and exits 0.

### `tools/test/generate-xml.sh`
- Bazel lines: 130
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`
- Verdict: partial
- Differences:
  - [P2] Zel never runs the script. When `test.xml` is missing it writes `synthesize_xml` in-process and ignores write errors. Bazel spawns `generate-xml.sh` on the `test.log` file, and a non-zero status from that spawn (including `sed` failure) is fatal and does not retry the test.
  - [P3] The script's `-` mode encodes stdin and exits 0, ignoring the other arguments. Zel has no such entry point.
  - [P2] The suite and case name come from `$TEST_BINARY` with one leading `./` and one leading `../` removed, unescaped. If `TEST_TOTAL_SHARDS` is set and non-zero, the name gains `_shard_${TEST_SHARD_INDEX+1}/${TEST_TOTAL_SHARDS}`. Zel uses the target name after the last `:`, escapes `&`, `<`, `>`, and `"` in the name, and never adds a shard suffix.
  - [P1] `duration` and `time` are the whole seconds passed as argument 3. Zel ignores `duration_ms` and always writes `duration="0" time="0"`.
  - [P2] A non-zero exit code yields `errors="1"` and `<error message="exited with error code $EXIT_CODE"></error>` with the code unescaped. Zel uses that text for a non-zero exit, but a timeout is `errors="1"` with message `timed out` (and the in-memory log drops lines that start with `Action timed out after `). A shard run that did not touch `TEST_SHARD_STATUS_FILE` is reported as exit code 1 even when the process exited 0.
  - [P1] The log is the file, passed through `encode_stream`: illegal XML/UTF-8 bytes become `?` (overlong sequences such as `\xc0\x80` are kept), and `]]>` becomes `]]>]]<![CDATA[>`. A missing file leaves the CDATA empty. The unquoted heredoc then expands `$`, backticks, and backslashes in that text. Zel embeds stdout plus stderr via `from_utf8_lossy` (invalid bytes become U+FFFD), strips one trailing `\n`, leaves other control bytes in place, and splits `]]>` as `]]>]]&gt;<![CDATA[`.

### `tools/test/test-setup.sh`
- Bazel lines: 377
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/resources/test-setup.sh`
- Verdict: partial
- Differences:
  - [P0] An unset or empty `HOME` is left unset. A relative `HOME` is prefixed with `$PWD`. Bazel replaces either case with `TEST_TMPDIR` and exports `HOME`.
  - [P2] The test and the coverage command are backgrounded directly, so the waited pid and process-group leader are the test itself. Bazel wraps them in a subshell, so that pid and group leader are the subshell and the test's parent is the subshell.
  - [P2] After `SIGTERM` (the timeout signal), Zel waits up to 15 seconds for the process group to exit before `SIGKILL`. Bazel `SIGKILL`s the group as soon as the waited child has exited.
  - [P2] Undeclared-output manifest sizes follow symlinks (`stat -L`). Bazel `stat` does not, so a symlink entry records the link size.
  - [P3] Manifest rows are written with `paste`, so `%` and backslashes stay literal. Bazel passes the path, size, and mime type through `printf` as the format string.
  - [P2] The output-directory prefix is stripped as a literal string, and leading or trailing whitespace in a path is kept. Bazel uses shell pattern removal and `read`, so glob characters in the directory can remain in the name and surrounding whitespace is stripped.
  - [P1] Sizes come from one batched `stat`. On BSD/macOS the `%N` name can be escaped, so the lookup misses and the size becomes `Could not stat <path>`. If the batch fails, every row gets that failure text. Bazel stats each file on its own.

### `tools/test/windows/tw.cc`
- Bazel lines: 2049
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`
- Verdict: partial
- Differences:
  - [P2] Zel runs `crates/zel-exec/resources/test-setup.sh` under bash. The wrapper attaches the child stdin to `NUL` and tees stdout and stderr to the parent stdout and to `XML_OUTPUT_FILE.log`.
  - [P2] The wrapper prints `Executing tests from <TEST_TARGET>` (or `unknown target`) and the dash marker before the rest of setup. The shell prints a `less` pager line, leaves an unset `TEST_TARGET` blank, and prints the dash marker only after `chdir`.
  - [P2] The wrapper exports `GUNIT_OUTPUT=xml:<XML_OUTPUT_FILE>`. The shell assigns that variable without exporting it, so the test process does not see it.
  - [P2] With `COVERAGE_DIR` set, the wrapper executes the coverage tool, passes the resolved test path as its first escaped argument, and moves `RUNFILES_DIR`, `JAVA_RUNFILES`, `PYTHON_RUNFILES`, and `RUNFILES_MANIFEST_FILE` under the `BAZEL_COVERAGE_INTERNAL_` prefix. Zel leaves those variables unprefixed, so `collect_coverage.sh` does not receive the wrapper runfiles. The wrapper also skips the runfiles-directory check in coverage mode; the shell fails if `RUNFILES_DIR` is missing.
  - [P2] The wrapper strips a leading `./` or `../`, prefixes `TEST_WORKSPACE` except for external paths, and resolves the binary with the C++ runfiles library when it is not in the tree or `RUNFILES_MANIFEST_ONLY=1`. The shell only strips `./`, looks up `$TEST_WORKSPACE/$EXE`, and greps the manifest. The shell also honors `TEST_SHORT_EXEC_PATH` (short `tN.exe` link) and, when `BUILD_EXECROOT` is set, `exec`s the test and skips archiving. The wrapper does neither.
  - [P2] A missing or relative `HOME` becomes the absolute mixed-style `TEST_TMPDIR` in the wrapper. The shell leaves `HOME` unset when unset, and joins a relative `HOME` onto the exec root. Relative path variables the wrapper absolutizes are re-exported with `/` separators. `USER` comes from `GetUserNameW` in the wrapper and from `whoami` in the shell.
  - [P2] The wrapper zips undeclared outputs only when both the directory and the zip path are set, using an uncompressed archive (directory mode `040750`, file mode `0100640`), registry MIME types, and forward-slash paths, and it rejects files larger than `INT_MAX`. The manifest is written only after the zip succeeds, and the archived files are then deleted. With zipping disabled it writes no manifest. Zel writes a sorted manifest whenever the directory and manifest path are set, using `file --mime-type` and `find -L`, then `zip -qr`, and deletes inputs only if the zip step runs. Zel also concatenates `*.pb` into `ANNOTATIONS.pb`. The wrapper concatenates only depth-0 `*.part` files, and still creates the annotations file when the directory is non-empty but has no `*.part` file.
  - [P2] `TestWrapperMain` does not write `test.xml`. `XmlWriterMain` embeds the log with illegal UTF-8 octets replaced by `?`, encodes `]]>` as `]]>]]<![CDATA[>`, names the case from `TEST_BINARY` plus `_shard_<index+1>/<total>` when sharded, and records duration as rounded seconds (half-up, capped at `INT_MAX`). Zel's fallback XML uses duration `0`, the label's target name with no shard suffix, a `Generated test.log` CDATA prefix, `]]>` replaced by `]]>]]&gt;<![CDATA[>`, no illegal-XML sanitizing, and the message `timed out` when the spawn times out. Zel has no `xml` writer CLI.
  - [P2] The wrapper waits with no timeout and returns the process exit code. The shell traps signals, kills the process group, and re-raises a status above 128. Zel also enforces the test timeout, forces exit code 142, and strips `Action timed out after` lines from the log.
  - [P2] Directory creation failure is ignored by the wrapper, which reports `ERROR(<file>:<line>)` on stdout. The shell aborts on `mkdir` or `cd` failure.

### `tools/test/windows/tw.h`
- Bazel lines: 207
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no Windows test-wrapper main. Bazel adds the current directory to `PATH`, resolves the test through runfiles (`RUNFILES_MANIFEST_ONLY` selects the runfiles tree or `Rlocation`), chdirs into the runfiles workspace, exports the Windows test environment (`TEST_SRCDIR`, `TEST_TMPDIR`, `HOME`, `USER`, gtest vars, shard status), rewrites `XML_OUTPUT_FILE` to a mixed path and sets `GUNIT_OUTPUT=xml:<path>`, and prefixes runfiles variables with `BAZEL_COVERAGE_INTERNAL_` when `COVERAGE_DIR` is set. Zel launches `test-setup.sh` from `zel-exec`.
  - [P2] Bazel unexports `TEST_UNDECLARED_OUTPUTS_ZIP`, `TEST_UNDECLARED_OUTPUTS_MANIFEST`, and `TEST_UNDECLARED_OUTPUTS_ANNOTATIONS` before the test process, leaving only the outputs directory and the annotations directory. Zel exports the zip, manifest, and annotations paths into the test environment.
  - [P2] Bazel tees the child stdout and stderr, on a background thread, to the test log and the wrapper stdout, with stdin on `NUL`. Zel waits for the spawn and concatenates captured stdout and stderr afterward.
  - [P2] Bazel's XML writer is a separate entry (`<log> <xml> <duration_seconds> <exit_code>`). The test-wrapper entry skips XML. The writer names the case from `TEST_BINARY` (leading `./` or `../` stripped), appends `_shard_{index+1}/{total}` when `TEST_TOTAL_SHARDS` is positive, records whole seconds rounded half-up and capped at `INT_MAX`, and emits `<error message="exited with error code N">` only for a nonzero exit. The log is CDATA with illegal UTF-8 octets replaced by `?` and `]]>` split as `]]>]]<![CDATA[>`. Zel synthesizes XML from the target name with `duration` and `time` always `0`, a "Generated test.log..." preamble, `]]>` split as `]]>]]&gt;<![CDATA[`, and a "timed out" error with exit code 142 on timeout.
  - [P2] With `--zip_undeclared_test_outputs`, Bazel walks the undeclared-output tree (negative depth is unlimited, zero lists only the root), rejects any file whose size does not fit in a signed 32-bit int, stores an uncompressed zip with CRCs (directory mode `040750`, file mode `0100640`, directory entries ending in `/`, paths in the ANSI code page with `/`), writes a manifest of files only as `path<TAB>size<TAB>mime` (registry Content Type, otherwise `application/octet-stream`), then deletes the archived tree. Zel sets `TEST_UNDECLARED_OUTPUTS_ZIP` and copies `test.outputs`; it does not build that zip or manifest.
  - [P2] Bazel concatenates only the immediate `*.part` files in the annotations directory into the annotations file. Zel leaves that file to whatever the test wrote.

### `tools/test/windows/tw_test.cc`
- Bazel lines: 768
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`
- Verdict: partial
- Differences:
  - [P3] Directory walk follows Windows junctions and honors a depth cap (0 = immediate children, 1 = one level deeper). Zel does not walk outputs; only `test-setup.sh` runs `find -L` on files, which does not list directories, follow junctions, or apply a depth limit.
  - [P3] Zip entries use forward slashes, a trailing `/` on directories, a stripped `\\?\` prefix, and a second copy of junction targets with the original bytes. Zel does not build that zip; the shell script may run `zip -qr` and then delete the sources.
  - [P3] The undeclared-output manifest is files only, `relative/path<TAB>size<TAB>mime`, with MIME from `HKCR\\<ext>\\Content Type` (`.ico` -> `image/x-icon`, `.bmp` -> `image/bmp`, otherwise `application/octet-stream`). Zel never writes that file; the shell script uses `stat` and `file --mime-type`.
  - [P3] Annotations are the concatenation of only direct `*.part` children (`Hello a` + `Hello c`, skipping `b.txt`, nested parts, and the `bar.part` directory). Zel does not concatenate them; the shell globs `*.part` and also concatenates `*.pb` into `ANNOTATIONS.pb`.
  - [P3] `Tee` copies each write to two pipes until the input handle closes. Zel appends captured stdout and then stderr after the spawn exits.
  - [P2] CDATA turns NUL and illegal UTF-8 octets into `?` and splits `]]>` as `]]>]]<![CDATA[>`. `synthesize_xml` only rewrites `]]>` to `]]>]]&gt;<![CDATA[` and embeds `String::from_utf8_lossy` (U+FFFD, NUL and XML-illegal controls kept). An empty `classname=""` on `<testcase>` does match.
  - [P3] The paged `IFStream` (`Get` / `Peek` across page boundaries, EOF as 256) has no counterpart; Zel reads the whole log at once.

### `tools/test/windows/xml_main.cc`
- Bazel lines: 21
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`
- Verdict: partial
- Differences:
  - [P2] This file is a Windows `wmain` that forwards `argc`/`argv` to `XmlWriterMain` and returns that status (0 after a successful write, 1 on usage, path, `TEST_BINARY`, or I/O failure). Zel never runs that binary. When `test.xml` is missing it calls `synthesize_xml` in-process and ignores the write result.
  - [P2] Bazel requires `<test_log> <xml_path> <duration_seconds> <exit_code>`, absolutizes the paths from the cwd, and overwrites the XML path. Zel takes the captured stdout and stderr, the target label, and the attempt result, and writes only if `test.xml` is absent.
  - [P2] The test name comes from `TEST_BINARY` (strip a leading `./` or `../`; if `TEST_TOTAL_SHARDS` is non-zero, append `_shard_{index+1}/{total}`) and is written as the active code page. Zel uses the label name after `:` with no shard suffix and XML-escapes `&`, `<`, `>`, and `"`.
  - [P2] `duration` and `time` are the parsed second count. Zel always writes `0` and drops `duration_ms`. A non-zero exit code becomes `<error message="exited with error code N"></error>`. Zel also treats a timeout as an error with message `timed out`.
  - [P2] The log is CDATA-escaped from the log file: illegal XML/UTF-8 sequences become `?`, and `]]>` becomes `]]>]]<![CDATA[>`. Zel lossily decodes the captured log, strips one trailing newline, only rewrites `]]>` to `]]>]]&gt;<![CDATA[`, and adds the `Generated test.log...` preamble and different indentation. The test log is left in place in both cases (`DeleteAfterwards::kDisabled`).

### `tools/upload_bazel_mirror.sh`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no tool that takes a bazelbuild repo name and version, checks that each is 3-40 characters of `[A-Za-z0-9_.-]`, and exits with a usage or validation error otherwise.
  - [P3] Nothing downloads `https://github.com/bazelbuild/<repo>/archive/<version>.tar.gz` and uploads it with no-clobber to `gs://bazel-mirror/github.com/bazelbuild/<repo>/archive/<version>.tar.gz`.
