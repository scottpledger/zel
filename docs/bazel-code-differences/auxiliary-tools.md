# Bazel vs zel code differences: auxiliary tools

Category rule: `src/tools/` standalone utilities such as remote, launcher, execution-log, workspace-log, singlejar, and bzlmod tools.

- Failure entries: 100
- Missing: 90
- Partial: 10
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `src/tools/bzlmod/blazel_utils.bzl`
- Bazel lines: 29
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`
- Verdict: partial
- Differences:
  - `get_canonical_repo_name` and `get_repo_root` both resolve through `Label` in the defining `.bzl` repo mapping. For an apparent name that mapping does not contain, Bazel builds a non-visible repo and `.workspace_name` / `.workspace_root` fail (`'…' is not allowed on invalid Label`, including the “did you mean” suffix). Zel rewrites that name to a visible repo whose name is `[unknown repo '…' requested from @@…]` and returns that string (and `external/` plus that string) instead of failing.
  - Zel skips that unknown-repo check for apparent names `bazel_tools` and `platforms`. If they are absent from the mapping, the helpers return those names and `external/bazel_tools` or `external/platforms`. Bazel still treats them as non-visible and the attribute access fails.
  - `get_repo_root` follows `Label.workspace_root`: empty for the main repo, `external/<canonical>` otherwise, and `../<canonical>` when `--experimental_sibling_repository_layout` is set. Zel always returns `external/<name>` for a non-main repo and ignores that flag.
  - Both attributes exist only when `--incompatible_enable_deprecated_label_apis` is true (Bazel default). Zel exposes `.workspace_name` and `.workspace_root` even when that flag is false, so the helpers still return values instead of failing.

### `src/tools/bzlmod/utils.bzl`
- Bazel lines: 178
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no helper that reads a lockfile and, for `registryFileHashes` URLs ending in `/source.json` whose repo name is in the required list, downloads that `source.json` (lockfile `sha256`, after deleting `./tempfile`) and emits `{integrity, url}` for `source.json`'s `url` plus each `patches` and `overlay` entry at `<source.json dir>/patches/<name>` and `<source.json dir>/overlay/<name>`. Repo names are `bazel_tools` and `platforms` unchanged and `name+` otherwise. The version path segment is ignored.
  - Zel has no scan of every `moduleExtensions` value's `generatedRepoSpecs` that keeps rules whose `ruleClassName`, or else `repoRuleId`, ends with `http_archive`, `http_file`, or `http_jar`, names them `{module_repo}+{extension}+{local_name}` (`@@…//` prefix before `//`, otherwise an empty module repo, extension after `%`), and records `sha256` and `integrity` (explicit `None` when absent) plus `urls[0]` if that key exists, otherwise `url`, failing if neither URL key exists.
  - Zel does not fail with `Could not find all required repos, missing: …` when a required repo is absent from those two scans, does not return the first-seen deduped `source.json` repo-name set, and does not return every `registryFileHashes` entry as `{sha256, url}` (empty when that map is missing). Direct indexing of `registryFileHashes` and `moduleExtensions` is also absent, so a lockfile missing those keys is not failed the same way.
  - Zel does not re-export `get_canonical_repo_name` or `get_repo_root` (apparent repo to `Label.workspace_name` / `Label.workspace_root`, adding `@` when missing).

### `src/tools/diskcache/Gc.java`
- Bazel lines: 125
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No standalone disk-cache garbage-collection program. Bazel parses `--disk_cache`, `--max_size` (byte size), and `--max_age` (duration), exits 1 when the cache path is missing, is not a directory, or both limits are non-positive, then runs collection on a pool of `max(4, available processors)` threads with size and/or age policy and prints collection stats before exiting 0.
  - Zel has no binary, command, or library entry that accepts those flags, validates the cache directory, applies that policy, or prints collection stats. The `--experimental_disk_cache_gc_*` flags are the idle-server policy from a different Bazel class and are not wired to a collector.

### `src/tools/execlog/src/main/java/com/google/devtools/build/execlog/ConverterOptions.java`
- Bazel lines: 123
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No execution-log converter option surface: `--input` and `--output` (default null, `type:path`) and `--sort` (boolean, default false, “sort the output in a deterministic order”).
  - No `binary`, `json`, or `compact` format token. Accepted names are the enum names lowercased with `Locale.US`, and the token is matched case-sensitively against those keys.
  - No `type:path` parse: split on the first `:` only; reject anything other than two parts with `Expected input to be in the form of <type>:<path>, where <type> is one of binary,json,compact.`; reject an unknown type with `Invalid type '<token>' specified; valid types: binary,json,compact`; reject an empty path with `Empty path specified, expected input to be in the form of <type>:<path>.`; otherwise keep the path as an NIO `Path` without requiring the file to exist.
  - No converter type description `type:path, where type is one of binary json compact`.

### `src/tools/execlog/src/main/java/com/google/devtools/build/execlog/ExecLogConverter.java`
- Bazel lines: 98
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No tool reads an existing execution log and writes it in another format. Bazel requires `--input` and `--output` as `<type>:<path>` (`binary`, `json`, or `compact`); a missing flag prints to stderr and exits 1, and an unreadable input path does the same.
  - Input `binary` is a length-delimited `SpawnExec` stream, `json` is a JSON `SpawnExec` stream, and `compact` is expanded by `SpawnLogReconstructor`. Output accepts only `binary` and `json`; `compact` throws `AssertionError` (`unsupported output format`).
  - Without `--sort` (default false), messages are copied in input order. With `--sort`, `StableSort.stableSort` reorders them so a producer precedes its consumers and otherwise by primary output path.

### `src/tools/execlog/src/main/java/com/google/devtools/build/execlog/ExecLogParser.java`
- Bazel lines: 291
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No tool sniffs an execution log (zstd magic `28 b5 2f fd` then compact reconstruction, `{` plus newline then JSON `SpawnExec`, otherwise length-delimited binary) and streams `SpawnExec` messages.
  - No `--restrict_to_runner` filter that keeps only entries whose runner string equals the flag.
  - No text dump of each `SpawnExec` followed by the dashed delimiter, to stdout or `--output_path`.
  - No two-file mode: the first log is emitted in order and becomes the golden keyed by the first listed output; the second log is reordered by that golden position, with unmatched actions appended in input order. Duplicate first outputs keep the later golden index.
  - No CLI checks for leftover arguments, a missing `--log_path`, more than two `--log_path` values, more `--output_path` values than logs, or anything other than exactly two `--output_path` values when two logs are given.

### `src/tools/execlog/src/main/java/com/google/devtools/build/execlog/ParserOptions.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No execution-log parser option set. `--log_path` is a repeatable list, default unset: one path is parsed as-is; a second path is accepted so its actions can be reordered to the first by matching the first output’s name, with unmatched actions appended.
  - `--output_path` is a repeatable list, default unset (stdout). When two `--log_path` values are given, the option contract requires exactly two output paths.
  - `--restrict_to_runner` is an optional string, default unset. When set, only executions whose runner equals that value are emitted.

### `src/tools/execlog/test/main/java/com/google/devtools/build/execlog/ExecLogParserTest.java`
- Bazel lines: 353
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No reader sniffs an execution log: zstd magic `28 b5 2f fd` is a compact `ExecLogEntry` stream reconstructed to `SpawnExec` (invocation dropped, spawn args become `commandArgs`); a file starting with `{\n` is newline-delimited JSON `SpawnExec`; anything else is length-delimited binary `SpawnExec`.
  - No runner filter. An exact `runner` match yields those spawns in order; a null filter yields every spawn; non-matches are skipped; end of stream is null.
  - No golden reorder. Spawns are matched on the first listed output, emitted in the golden file’s position order, with unmatched input spawns appended in input order. Golden entries with no input counterpart are omitted, and the original spawn (including command args) is kept rather than replaced by the golden stub.
  - Zel only writes JSON, binary, and compact execution logs (`/workspace/zel-cursor/crates/zel-events/src/exec_log.rs`) and round-trips a known compact frame. It does not detect a format, reconstruct `SpawnExec` from compact entries, filter by runner, or reorder against a golden log.

### `src/tools/launcher/bash_launcher.cc`
- Bazel lines: 81
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no runtime that launches a Windows shell binary from `bash_bin_path` and `bash_file_rlocationpath`. The only copy of this logic is the original C++ embedded as `@bazel_tools` source at `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/tools/launcher/bash_launcher.cc`, which is identical to Bazel and is not a Rust launcher.
  - Missing the rule that skips `Rlocation` when the bash path’s basename without extension is `bash` (so `bash` or `bash.exe` is taken from `PATH`), and otherwise runfiles-resolves the path, leaving an absolute path unchanged.
  - Missing the `PATH` update that, when that resolved file exists, appends its parent directory with `;` rather than prepending it, so a coreutils `link.exe` does not shadow the Visual Studio linker. When the file does not exist, Bazel substitutes `bash.exe` and relies on `PATH`; Zel does not.
  - Missing the `bash -c` invocation whose command is the runfiles-resolved script plus `argv[1..]` (`argv[0]` dropped), each token passed through `BashEscapeArg` (empty becomes `""`, spaces and quotes are double-quoted, backslashes are doubled). `zel_analysis::shell_escape` in `/workspace/zel-cursor/crates/zel-analysis/src/action.rs` is a separate Unix single-quote escaper for parameter files, not this launcher.

### `src/tools/launcher/java_launcher.cc`
- Bazel lines: 479
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Rust Windows `JavaBinaryLauncher`. The same-named file under `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/tools/launcher/java_launcher.cc` is an embedded copy of this source, not a port.
  - No parsing of leading launcher flags or `--wrapper_script_flag=` values (`--debug` / `--debug=<port>`, `--main_advice=`, `--main_advice_classpath=`, `--jvm_flag=`, space-split `--jvm_flags=`, `--singlejar`, `--print_javabin`, `--classpath_limit=`), and no fatal error on an invalid wrapper argument. Quoted flag values are unquoted.
  - No `JAVA_RUNFILES` assignment from the existing environment or the launcher runfiles path before exec. Test/run env injection of `JAVA_RUNFILES` elsewhere is not this launcher.
  - No rlocation of `java_bin_path` (with workspace name) and no early exit that prints that path when `--print_javabin` is set or `java_start_class` is `--print_javabin`.
  - No classpath build: rlocated `;`-separated `main_advice_classpath` plus launch-info `classpath`, or `<binary>_deploy.jar` when `--singlejar` (fatal if that jar is missing).
  - No spill when the classpath is longer than the limit (default 7000): a classpath jar for a missing or `<= 8` `java_version` (cross-drive directory junctions, percent-encoded `Class-Path`, `jar` `cvfm`, then delete the jar and junctions), or a UTF-8 `@` flagfile with backslashes turned into slashes for Java `> 8`.
  - No JVM argument assembly: JDWP (`DEFAULT_JVM_DEBUG_PORT` or 5005, `DEFAULT_JVM_DEBUG_SUSPEND` default `y`, `quiet=y` when `PERSISTENT_TEST_RUNNER=true`), space-split `JVM_FLAGS`, tab-split launch-info `jvm_flags`, `-Djava.io.tmpdir=` when `TEST_TMPDIR` exists, then command-line JVM flags, optional main-advice class, `java_start_class`, and the remaining program arguments. No launch of that `java` binary or return of its exit code.

### `src/tools/launcher/python_launcher.cc`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `PythonBinaryLauncher::Launch` is not implemented in Rust. The only related tree file, `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/tools/launcher/python_launcher.cc`, is an unmodified embedded copy of this C++ source and is not executed.
  - No code reads `python_bin_path`, skips `Rlocation` when the path without extension is `python`, otherwise runfile-resolves it, and falls back to `python.exe` when that path does not exist.
  - No code chooses the entry file from `python_file_short_path` (`Rlocation` without a workspace prefix) or, if that key is empty, from the launcher path with a `.zip` suffix only when `use_zip_file` is `1`.
  - No code replaces `argv[0]` with that entry path and launches the chosen interpreter with the remaining arguments.

### `src/tools/launcher/util/data_parser.cc`
- Bazel lines: 113
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not open a launcher binary, read the trailing native `int64` payload length, or parse the preceding NUL-terminated `key=value` records into a string-to-wide-string map.
  - It does not reject a zero-length trailer, a record with no `=`, an empty key, or a duplicate key.
  - It does not convert value bytes with the Windows C-string-to-wide conversion, and it does not open the binary through the Windows absolute-path helper.

### `src/tools/launcher/util/data_parser.h`
- Bazel lines: 50
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `LaunchDataParser::GetLaunchInfo` opens the launcher binary, reads a native `int64_t` byte count from the last 8 bytes, and parses the preceding NUL-separated `key=value` records into `unordered_map<string, wstring>` (values widened with `CstringToWstring`). A zero length, a line with no `=`, an empty key, or a duplicate key prints a launcher error and returns false. Zel has no reader of that trailer. The same header is only embedded as `@bazel_tools` source at `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/tools/launcher/util/data_parser.h`.

### `src/tools/launcher/util/data_parser_test.cc`
- Bazel lines: 191
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Rust implementation or test of the Windows launcher trailer parser these cases exercise. The only copy is the original C++ source embedded as `@bazel_tools` resource text and not executed.
  - No coverage that a binary ending in NUL-terminated `key=value` records plus a little-endian `int64` byte length parses to those pairs, including an empty value, and that a missing key is not present.
  - No coverage that a zero-length trailer fails with `LAUNCHER ERROR: No data appended, cannot launch anything!`.
  - No coverage that a repeated key fails with `LAUNCHER ERROR: Duplicated launch info key: foo`.
  - No coverage that an empty key fails with `LAUNCHER ERROR: Key is empty string in line: =bar2`.
  - No coverage that a record without `=` fails with `LAUNCHER ERROR: Cannot find equal symbol in line: foo2bar2`.
  - No coverage that those parse failures abort the process (`exit(-1)` under `ASSERT_DEATH`).

### `src/tools/launcher/util/launcher_util.h`
- Bazel lines: 109
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Windows launcher helpers: no `LAUNCHER ERROR:` printing, no `die` that switches stderr to UTF-8 and exits 1, and no `GetLastErrorString` of the form `(error: N):` plus a system message.
  - No 8.3-to-long path conversion, no `\\?\` absolute-path conversion that exits on failure, no case-insensitive `.exe` add/strip, and no Bash escaping that quotes only when the argument contains a space, turns `"` into `\"` and `\` into `\\`, and maps an empty argument to `""`.
  - No launcher file or directory existence checks, file delete, or directory delete that removes an empty directory or a junction without deleting the junction target. No Win32 environment get (false when missing or empty, 32767-character buffer) or set, and no `rand_s` alphanumeric string.
  - No lowercase Windows path normalization, basename or parent split on `\` or `/`, or `RelativeTo` that emits `..\` for two same-drive normalized paths and rejects mixed absolute/relative paths or different drives.
  - `shell_escape` in `/workspace/zel-cursor/crates/zel-analysis/src/action.rs` is param-file single-quote escaping, not this launcher escaper. The byte-identical header at `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/tools/launcher/util/launcher_util.h` is embedded `@bazel_tools` source, not a Rust implementation.

### `src/tools/launcher/util/launcher_util_test.cc`
- Bazel lines: 214
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Rust port of these Windows launcher checks. The same `.cc` file under `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/tools/launcher/util/launcher_util_test.cc` is embedded as `@bazel_tools` bytes and is not a reimplementation. `PathFragment` in `/workspace/zel-cursor/crates/zel-vfs/src/lib.rs` is a different path type (forward slashes, uppercased drive, no `..` climb).
  - `GetBinaryPathWithoutExtension` strips only a final `.exe` matched case-insensitively and keeps the rest of the spelling: `foo.exe` → `foo`, `foo.sh.exe` → `foo.sh`, `Foo.EXE` → `Foo`, `Foo.Sh.EXE` → `Foo.Sh`, and `foo.sh` stays `foo.sh`.
  - `GetBinaryPathWithExtension` appends `.exe` only when that suffix is absent, ignoring case: `foo` → `foo.exe`, `foo.sh` → `foo.sh.exe`. An existing suffix is kept as written (`foo.exe`, `Foo.EXE`, `foo.sh.exe`, `Foo.Sh.EXE`).
  - `BashEscapeArg` returns `""` for an empty argument, leaves `foo` unquoted, and adds double quotes only when the argument contains a space (`"foo bar"`, `"\"foo bar\""`, `"C:\\foo foo\\bar\\"`). `"` becomes `\"` and `\` becomes `\\` even when the result is not quoted (`foo\"bar`, `foo\\bar`, `C:\\foo\\bar\\`).
  - `DoesFilePathExist` is true for `TEST_TMPDIR` (or `TEMP` when that is unset) plus `/foo` after a one-byte NUL file is created there, and false for the missing sibling `/bar`. `DoesDirectoryPathExist` is true for `/dir1` after `CreateDirectoryW` and false for missing `/dir2`.
  - `SetEnv(L"foo", L"bar")` succeeds and `GetEnv(L"foo")` returns `bar`. `SetEnv(L"FOO", L"")` then makes `GetEnv(L"FOO")` false. Those calls are `SetEnvironmentVariableW` / `GetEnvironmentVariableW`: names are case-insensitive, and an empty value is not returned.
  - `NormalizePath` lowercases, turns `/` into `\`, drops a trailing separator, and collapses repeated separators: `C:\foo\bar\` and `c:/foo/bar/` and `c://foo//bar` become `c:\foo\bar`; `FoO\\bAr\` becomes `foo\bar`; `X\Y/Z\` becomes `x\y\z`. The drive-relative path `c:foo\bar` returns false.
  - `GetRandomStr` has length 0, 1, 10, or 32 as requested. A length-256 string uses only `A–Z`, `a–z`, and `0–9`. Two independent length-10 results must differ.
  - `RelativeTo` uses `\` and emits `..\` for unmatched base segments. Examples: `c:\foo\bar1` from `c:\foo\bar2` is `..\bar1`; `c:\foo\bar` from `c:\` is `foo\bar`; identical paths are empty; `c:\` from `c:\a\b\c` is `..\..\..\`; `foo\bar1` from `fo` is `..\foo\bar1`; empty from `foo\bar` is `..\..\`; `foo\bar` from empty is `foo\bar`. It returns false for mixed absolute and relative paths, a drive-relative path (`c:foo\bar1`), and different drives (`c:` versus `d:`).

### `src/tools/one_version/allowlist.cc`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no one-version allowlist filter. Bazel derives the package key as the class name up to the last `/`, and uses the whole class name when there is no `/`.
  - `Apply` copies every version and label, marking a label allowlisted only on an exact label-name match for that package. A version is new when it has at least one non-allowlisted label. The violation is kept only when more than one version is new, and a kept violation still includes fully allowlisted versions and labels, with CRC32 and jar path unchanged. Zero or one new version drops the violation.
  - `MapAllowlist` returns the label set stored for that package key. A missing key allowlists nothing.

### `src/tools/one_version/allowlist.h`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no one-version allowlist. Bazel’s `Allowlist` returns the allowlisted labels for a class name (`AllLabels`) and applies that allowlist to duplicate-class violations (`Apply`). `MapAllowlist` is built from a map of package name to a set of label names.
  - `Allowlist::Apply` keeps a violation only when more than one version still has a non-allowlisted label. Surviving violations still list every label, marked allowlisted or not. Lookup uses the package (text before the last `/` of the class name), not the full class name. A version whose labels are all allowlisted does not count, so allowlisting every label of all but one version drops the violation.
  - `java_tools` build rules name `java_tools/src/tools/one_version/allowlist.h`, but that source is not in the tree. The one-version enforcement flags only store an enforcement level and do not ignore allowlisted labels.

### `src/tools/one_version/allowlist_test.cc`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `Allowlisting` records `com/google/Foo` at crc32 1 from `//hello:foo` via `hello/libfoo.jar` and at crc32 2 from `//hello:bar` via `hello/libbar.jar` and `//hello:baz` via `hello/libbaz.jar`. The unfiltered report is the two-space class line `com/google/Foo has incompatible definitions in:`, then `crc32=1` and `crc32=2` blocks with each label marked `[new]` and a `via <jar>` line. Zel has no duplicate-class collector or this report.
  - A map allowlist entry `com/google` → `//hello:bar` matches the package prefix of `com/google/Foo`. The same report is kept, with only `//hello:bar` changed to `[allowlisted]`. Foo and baz stay `[new]`.
  - Allowlisting both `//hello:bar` and `//hello:foo` drops the violation even though `//hello:baz` is not allowlisted, because only one crc still has a non-allowlisted label. Allowlisting only `//hello:foo` also drops it. Zel does not implement that suppression.
  - Enforcement flags (`experimental_one_version_enforcement` and related) are stored in `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` and `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`. They do not apply a package-to-label allowlist or emit this report.

### `src/tools/one_version/duplicate_class_collector.cc`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never records a class name with a CRC32 and a label, groups labels that share a CRC, or reports a class only when more than one distinct CRC was seen.
  - It does not sort versions by CRC32, labels by name, and violations by class name, or emit the `has incompatible definitions in` / `crc32=` / `[allowlisted]` or `[new]` report, including the `via <jar>` line only when the jar is non-empty.

### `src/tools/one_version/duplicate_class_collector.h`
- Bazel lines: 97
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `Label` (name, jar, allowlisted flag defaulting to false), `Version` (CRC32 plus labels), or `Violation` (class name plus versions) for one-version classpath entries.
  - Zel has no collector that records a class name with a CRC32 and a label, returns the collected one-version violations, or formats them as an error report.
  - The one-version flags in `flag_tables.rs`, `build_options_table.rs`, and `fragments.rs` only store the enforcement setting. They do not collect duplicate class definitions.

### `src/tools/one_version/duplicate_class_collector_test.cc`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel collector or test records classpath class names with CRC32 and labels, so distinct classes are not shown to produce an empty violation set.
  - Same class name recorded twice with the same CRC32 and different labels is not treated as a violation; zel has no equivalent rule.
  - Differing CRC32s for one class are not reported. Bazel’s expected text sorts versions by CRC32 (1 before 2, not insertion order), tags each label `[new]`, and lists the jar on a `via` line: `com/google/Foo` with `//hello:foo` via `hello/libfoo.jar` (crc32=1) and `//hello:bar` via `hello/libbar.jar` (crc32=2).
  - One-version flag metadata exists in zel, but nothing implements this collection or report behavior.

### `src/tools/one_version/one_version.cc`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never records jar entries for one-version checking. Bazel keeps only names that end in `.class`, strips that suffix, and stores the central-directory CRC32 with the entry's label.
  - Those stripped names are dropped when they are exactly `module-info`, or when they start with `META-INF/versions/` and end with `/module-info`.
  - Android `R` and databinding `BR` classes are also dropped: the stripped name ends with `/R`, contains `/R$`, or ends with `/BR`. Root names such as `R`, `R$…`, and `BR` are still recorded.
  - A null jar entry aborts. `Report` returns the allowlist applied to the collector's violations.
  - Related Zel flags (`experimental_one_version_enforcement`, `one_version_enforcement_on_java_tests`) only expose configuration. They do not scan jars or apply this filter.

### `src/tools/one_version/one_version.h`
- Bazel lines: 49
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No `one_version::OneVersion` that takes exclusive ownership of an `Allowlist` and also holds a `DuplicateClassCollector`.
  - No `Add` that records a jar central-directory entry and label when the entry name is a class file.
  - No `Report` that returns the collected one-version violations.

### `src/tools/one_version/one_version_main.cc`
- Bazel lines: 122
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Rust program scans classpath jars for one-version violations. Related flags (`experimental_one_version_enforcement`, `one_version_enforcement_on_java_tests`) only store enforcement settings.
  - Missing CLI parsing of `--output`, `--succeed_on_found_violations`, `--allowlist`, and `--inputs`, including exit 1 and `error: bad command line argument` on an unknown token.
  - Missing allowlist load: an empty `--allowlist` is an empty map; otherwise each line must be `<package> <label>` (one space split), or the process exits 1 with `error: expected <package> <label>` or `error: unable to open allowlist file`.
  - Missing input handling: each `--inputs` entry must be `<jar path>,<label>`, or the process exits 1. Each jar is opened, every central-directory entry is added with label `(label, jar, allowlisted=false)`, and a jar that fails to open exits 1 with `error: unable to open`.
  - Missing the empty output file (opened, never written) and the violation path: non-empty results print `Found one definition violations on the runtime classpath:` plus the duplicate-class report to stderr and exit 1 unless `--succeed_on_found_violations` is set, in which case the exit is 0.

### `src/tools/remote/src/main/cpp/testonly_output_service/bazel_output_service_impl.cc`
- Bazel lines: 126
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No test-only Bazel Output Service server. `Clean`, `StartBuild`, `StageArtifacts`, `FinalizeArtifacts`, `FinalizeBuild`, and `BatchStat` each ignore the request, leave the response empty, and return gRPC `UNIMPLEMENTED` with an empty message.
  - No process that skips `argv[0]`, defaults to port 8080, and accepts only `--port=<digits>`. A non-zero parsed value is stored in a 16-bit port (values above 65535 wrap). A zero value, including `--port=`, `--port=0`, and `--port=000`, is rejected with `Not a valid port: <suffix>` and parsing stops. The `valid` flag is ignored, so a non-zero prefix of an otherwise illegal suffix is accepted (`--port=12x` is port 12). Any other argument yields `Unknown command line: <arg>` and stops. A later `--port=` replaces an earlier one only when no error has been set.
  - No insecure gRPC listen on `0.0.0.0:<port>`, no stderr line `Server listening on port %d...`, and no block until the server stops. A parse error is printed to stderr and the process exits 1. A failed bind is not turned into that exit path.
  - Zel only vendors `bazel_output_service.proto` and `bazel_output_service_rev2.proto` as resources and lists `--experimental_remote_output_service` in the flag catalog. Neither is a server, a CLI, or an RPC implementation.

### `src/tools/remote/src/main/cpp/testonly_output_service/bazel_output_service_impl.h`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Rust `BazelOutputService` server implements the six RPCs this header binds: `Clean`, `StartBuild`, `StageArtifacts`, `FinalizeArtifacts`, `FinalizeBuild`, and `BatchStat`.
  - `RunServer(int argc, char** argv)` has no counterpart, so there is no test-only output-service process that starts that gRPC service.
  - `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/main/protobuf/bazel_output_service.proto` and `bazel_output_service_rev2.proto` are schema copies only and do not host or serve the API.

### `src/tools/remote/src/main/cpp/testonly_output_service/main.cc`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no process entry point for the testonly Bazel output service. This file’s program behavior is to pass `argc` and `argv` through to `RunServer` and return that function’s status as the process exit code.
  - No zel binary starts that server. The `bazel_output_service` protos under `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/main/protobuf/` are embedded protocol resources only.

### `src/tools/remote/src/main/cpp/testonly_output_service/memory.cc`
- Bazel lines: 123
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no bump arena for the test-only output service: page-aligned reserve of a caller-chosen span (scratch arenas use the 1 GiB default), with only the arena header committed up front and the usable base immediately after that header.
  - `PushArena` is absent: 8-byte-aligned bump allocation, zero-fill of the requested size only, and on-demand commit of whole pages up to the reservation, aborting when the bump would pass the reserved end.
  - `PopArena` and temporary scopes are absent: rewind without decommit, a saved watermark plus a live-scope count, and `FreeArena` refusing to release the reservation while a temporary scope is still open.
  - Thread-local scratch arenas are absent: two slots per thread, lazy allocation, and selection of the first arena that is not in the caller’s conflict list, aborting when every slot conflicts.

### `src/tools/remote/src/main/cpp/testonly_output_service/memory.h`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no virtual-memory or stack-arena facility matching this header: `KiB`/`MiB`/`GiB`, page-size query, reserve (uncommitted) / commit / release, `Arena` with a default 1 GiB reservation, 8-byte-aligned zeroed `PushArena` / `PopArena` / `PushArray`, temporary-memory scopes, or the two thread-local scratch arenas selected to avoid caller conflicts (`GetScratchArena`, `BeginScratch`, `EndScratch`). The Bazel output-service protos in zel are the service schema, not this allocator.

### `src/tools/remote/src/main/cpp/testonly_output_service/memory_unix.cc`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Unix virtual-memory layer for the testonly output service.
  - `GetPageSize` returns `getpagesize()` with no extra rounding or caching. That call is absent.
  - `ReserveMemory` maps `size` bytes with `mmap(NULL, size, PROT_NONE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0)` and aborts if the result is `MAP_FAILED`. Zel has no inaccessible anonymous reservation.
  - `CommitMemory` makes a previously reserved range readable and writable with `mprotect(ptr, size, PROT_READ|PROT_WRITE)` and aborts on failure. Zel has no separate commit step.
  - `ReleaseMemory` unmaps the whole reserved range with `munmap` and aborts on failure. Zel has no matching release.

### `src/tools/remote/src/main/cpp/testonly_output_service/string.cc`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no test-only remote output service and no arena `Str8` helpers.
  - `ParseUInt32` is absent: digit-only decimal scan into `uint32` (wrapping `value * 10 + digit`), empty input stays valid `0`, and the first non-digit marks the parse invalid.
  - `PushStr8` is absent: arena copy of `len + 1` bytes, including the existing NUL.
  - `PushSubStr8` is absent: arena copy of `[begin, end)` (or `[begin, len)`), with `begin <= end <= len` asserted and a spare byte allocated after the copy.
  - `PushStr8F` is absent: `vsnprintf` into a 256-byte arena buffer, then either shrink the unused tail or free and retry at the exact size.

### `src/tools/remote/src/main/cpp/testonly_output_service/string.h`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `Str8` is a `uint8_t` pointer plus a byte length that excludes a required trailing NUL, so the buffer is at least `len + 1`. `IsEmptyStr8` is true exactly when `len == 0` and does not read the bytes. Zel has no NUL-terminated, length-prefixed string type for the test-only output service.
  - `Str8FromCStr` aliases the caller’s `char*` (the `const` is cast away) and sets `len` with `strlen`, so the first NUL ends the string and the buffer is not copied.
  - `StartsWithStr8` is true when `str.len >= prefix.len` and `memcmp` of `prefix.len` bytes is 0. An empty prefix matches. The compare is raw bytes, with no UTF-8 decoding and no case folding.
  - `ParsedUInt32` holds `valid` and a `uint32_t` that is meaningful only when `valid` is true. `ParseUInt32`, `PushStr8` (arena copy), `PushStr8F` (`printf`-style), and the two `PushSubStr8` overloads (`begin` through the end, or `[begin, end)`) are declared against the test-only `Arena` from `memory.h`. Zel does not implement that parser or those arena copies. The proto copies at `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/main/protobuf/bazel_output_service.proto` and `bazel_output_service_rev2.proto`, and the `--experimental_remote_output_service` text in `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`, do not provide these helpers. `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/main/cpp/util/strings.h` is the separate `blaze_util` string API.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/ActionCacheServer.java`
- Bazel lines: 122
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no remote-worker ActionCache gRPC service. `zel-remote` mirrors the REAPI client (`lib/remote`), not this worker server.
  - `GetActionResult` loads the on-disk action result (`inlineOutErr` false; integrity check depends on the worker cache). A miss or `CacheNotFoundException` is `NOT_FOUND` (`"Digest not found:"` plus the action digest). It then downloads and sets `contents` on only the first `output_files` entry whose path is in `inline_output_files` (the loop breaks after one match). Any other failure is `INTERNAL`.
  - `UpdateActionResult` requires both the Action blob and that Action’s Command blob to exist in the CAS, stores the result, and echoes the request `ActionResult`. A missing blob is `FAILED_PRECONDITION`; any other failure is `INTERNAL`.
  - `crates/zel-remote/src/fake_server.rs` is an in-memory client test double (hash-only map, no output inlining, updates gated by `update_enabled` as `PERMISSION_DENIED`, no Action/Command CAS check). `crates/zel-remote/src/disk_cache.rs` is the client disk cache, not this service.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/ByteStreamServer.java`
- Bazel lines: 302
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/fake_server.rs`
- Verdict: partial
- Differences:
  - Resource names: Bazel takes the last two `/` segments as hash and size (trailing slash ignored; no `blobs` or `uploads` required, so `blobs/md5/{hash}/{size}` and `uploads/{uuid}/blobs/{hash}/{size}` both work). Zel requires `blobs/...` on Read and `uploads/{uuid}/blobs/...` on Write, treats the segment immediately after `blobs` as the hash (so a digest-function segment such as `md5` fails), and ignores extra segments after the size.
  - Read of a size-0 digest always succeeds with an empty body and does not consult the cache or check the hash. Zel returns NOT_FOUND unless that hash was stored as an empty blob.
  - Read does not require the stored blob length to equal the resource-name size (the cache is keyed only by hash). Zel returns NOT_FOUND on a length mismatch. A negative `read_offset` is OUT_OF_RANGE and a negative `read_limit` is INVALID_ARGUMENT; Zel clamps a negative offset to 0 and treats a negative limit as “read to the end.”
  - Read responses are chunked at 16 KiB. Zel chunks at 64 KiB. With `--error_on_duplicate_downloads`, a second Read of the same digest in one invocation fails; Zel has no such check.
  - Write of a hash that is already in the CAS completes on the first request with `committed_size` equal to the declared size, without checking `write_offset`, `finish_write`, or bytes, and without replacing the stored blob. Zel always requires a full body and replaces the entry.
  - `finish_write` must be true on the request that makes the running total equal the declared size and false on every other request. A client half-close before that, or an empty Write stream, is FAILED_PRECONDITION. Zel only requires `finish_write` on the last message, ignores it on earlier messages, and returns INVALID_ARGUMENT if it is missing.
  - After a complete upload Bazel hashes with the worker digest function, commits the bytes under the computed digest, then returns INVALID_ARGUMENT if that digest does not match the name. Zel always checks SHA-256, commits only on match, and leaves the CAS unchanged on mismatch.
  - A later Write message with a different non-empty resource name is INVALID_ARGUMENT, and the name must parse on the first message. Zel keeps the first non-empty name, ignores later names, and allows the name to arrive after the first chunk. Validation errors carry `google.rpc.BadRequest` field violations; Zel returns a plain status string.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/CapabilitiesServer.java`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no remote-worker Capabilities service. `GetCapabilities` is only a client call in `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`. `/workspace/zel-cursor/crates/zel-remote/src/fake_server.rs` is a configurable test double, not this worker.
  - The worker ignores `GetCapabilitiesRequest`, including `instance_name`, and always finishes the unary call with one `ServerCapabilities`.
  - With `--legacy_api`, both low and high API versions are 2.0.0. Otherwise the window is low 2.0.0 and high 2.11.0.
  - Cache capabilities always advertise the worker digest function, absolute symlinks `DISALLOWED`, action-cache updates enabled, a 4 MiB batch limit, split-blob and splice-blob support, FastCDC 2020 (`avg_chunk_size_bytes` 512 KiB, `seed` 0), and RepMax CDC (`min_chunk_size_bytes` 256 KiB, `horizon_size_bytes` 2 MiB).
  - Execution capabilities are sent only when the worker was constructed with execution enabled, and then only with that same digest function and `exec_enabled` true.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/CasServer.java`
- Bazel lines: 240
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/fake_server.rs`
- Verdict: partial
- Differences:
  - `FindMissingBlobs` treats a digest as present when the CAS file `cas/<hash[:2]>/<hash>` exists, and a hit sets that file's mtime to now. Missing digests are echoed in request order, including size and `digest_function`. An `IOException` fails the RPC with `INTERNAL` and no missing list, after earlier hits have already been touched. A hash shorter than two characters throws `StringIndexOutOfBoundsException`. Zel looks up an in-memory map by hash only, returns the same order, and does not touch an mtime. `forget_after_next_find_missing` still reports that hash present, then deletes it.
  - `BatchUpdateBlobs` hashes `data` with the worker `DigestUtil` and uploads under the computed digest before the check. `Digest.equals` includes `digest_function`, and the computed digest leaves that field unset, so an explicit function (including SHA256 = 1) is a mismatch when the hash and size match. The bytes stay stored under the computed hash: an existing hash file is kept and its mtime refreshed, and a later `FindMissingBlobs` of that hash is present. A mismatch sets that item to `INVALID_ARGUMENT` with message `invalid argument(s): digest: Upload digest <request text> did not match data digest: <computed text>` and a `BadRequest` field violation on `digest`. A match sets status `OK`. In both cases the response digest is unset. An item exception sets `INTERNAL` with the exception message and the loop continues. `MAX_BATCH_SIZE_BYTES` (4194304) is not applied here. Zel always uses SHA-256. It inserts only when `sha256(data)` equals `digest.hash` and `data.len()` equals `size_bytes`, ignoring `digest_function`, and it replaces any previous bytes for that hash. A mismatch stores nothing and returns per-item code 3, message `digest mismatch`, echoing the request digest. A match echoes the digest with status code 0. If the sum of `data` lengths exceeds `max_batch_total_size_bytes` (default 4194304; 0 disables the check), the whole RPC fails with code 3 `batch too large` and writes nothing.
  - `GetTree` is a depth-first walk of `Directory.directories` that emits each digest once, so a shared subtree appears once and a cycle stops. A missing directory fails the RPC with `NOT_FOUND` and message `Digest not found:` plus that digest's protobuf text, and no directories are returned. A proto that is not a `Directory` fails with `INTERNAL` and the parser exception message. An interrupt fails with `CANCELLED` and message `Server operation was interrupted for ` plus that digest. `page_size` and `page_token` are ignored: one response with an empty `next_page_token`. With `--error_on_duplicate_downloads` (default false), a second download of the same digest for one tool invocation id fails `GetTree` and `SpliceBlob` with `INTERNAL` and message `Duplicate download of blob digest <hash>/<size> for invocation id <id>`. Zel emits a directory once per incoming edge and recurses until the stack overflows on a cycle. A missing hash is code 5 `missing directory <hash>`. A bad proto is code 3 `bad request: <decode error>`. The request `page_size` is ignored. `get_tree_page_size` (default 0, meaning the full list) and a numeric `page_token` choose the slice; `next_page_token` is the next index as a decimal string, or empty when the slice reaches the end. A `page_token` index past the directory list panics. There is no interrupt and no per-invocation download cap.
  - `SplitBlob` returns the chunk digests and chunking function stored by an earlier `SpliceBlob` in this process. A CAS blob with no in-memory mapping fails with `NOT_FOUND` and message `Digest not found:` plus the protobuf text of `blob_digest`. `SpliceBlob` requires every chunk hash file to exist (and refreshes those mtimes), concatenates the downloaded bytes, and hashes them with the worker digest function. The computed digest leaves `digest_function` unset, so a request that sets it fails even when the hash and size match. A mismatch fails the RPC with `INVALID_ARGUMENT`, message `invalid argument(s): blob_digest: Splice digest <blob text> did not match computed digest: <computed text>`, and a `BadRequest` field violation on `blob_digest`. The concatenation is not written to the CAS, so `FindMissingBlobs` of `blob_digest` stays missing. A missing chunk fails with `NOT_FOUND`. An interrupt fails with `CANCELLED`. Success returns the request `blob_digest`. Zel has no `SplitBlob` or `SpliceBlob` messages; both paths return code 12 and message `unknown CAS method`.
  - `BatchReadBlobs` is not overridden, so the gRPC base finishes it as `UNIMPLEMENTED` (code 12). Zel reads the same map: a hit returns the bytes and status code 0; a miss, or a hash queued with `evict_on_next_read`, returns per-item code 5, message `not found`, and empty data. The lookup ignores `size_bytes`.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/ExecutionServer.java`
- Bazel lines: 649
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/fake_server.rs`
- Verdict: partial
- Differences:
  - Bazel `execute` always runs the action. `skip_cache_lookup`, `instance_name`, and `Action.do_not_cache` are ignored. The operation name is a random UUID. The first `Operation` has only that name (`done` false, no metadata). Completion is a second `done` operation whose `response` is an `ExecuteResponse`. Zel names ops `operations/1`, `operations/2`, and so on, and emits `ExecuteOperationMetadata` stages `QUEUED` (2), `EXECUTING` (3) unless the action cache hits, then `COMPLETED` (4). A cache hit (`skip_cache_lookup` false and the hash is in the map) skips execution and sets `cached_result=true`.
  - `waitExecution` of an unknown name fails the stream with `NOT_FOUND` and message `Operation not found: {name}`. A successful or failed finish removes the name, so a later wait of a completed op is the same `NOT_FOUND`. Client cancel only removes the map entry. The subprocess keeps running. Zel `WaitExecution` returns `ops.last()` for a known name on every call and code 5 `unknown operation` otherwise. There is no cancel handler, and finished ops stay in the map.
  - The worker pool has `workerOptions.jobs` core and max threads, a 1-minute idle timeout, `allowCoreThreadTimeOut`, an unbounded queue (work is never rejected), and thread names `subprocess-handler-%d`. Zel runs `Execute` on the request handler. `exec_enabled == false` is gRPC code 12 `execution disabled` with no operation. No executor is code 12 `no executor` inside `ExecuteResponse`.
  - Bazel creates `workPath/build-{opName}`, downloads the input tree there, and creates `Command.working_directory` (parents included). Arguments, env names and values, output paths, and platform properties pass through `unicodeToInternal`. The directory is deleted afterward. `workerOptions.debug` keeps it and logs `Preserving work directory`. A delete failure is logged and ignored. Zel never writes a work directory. It only checks that action, command, directory, and file blobs are in the CAS. A `size_bytes == 0` blob is treated as present. Decode failures are code 3 `bad action`, `bad command`, or `bad directory`.
  - With a non-empty `output_paths`, only that list is prepared. A pre-existing directory is kept. A pre-existing non-directory throws `FileAlreadyExistsException` `Output file already exists: {path}`. Legacy `output_files` use that same message. A legacy `output_directories` path that exists and is not a directory throws `Non-directory exists at output directory path: {path}`. Parents are created. Those exceptions become an `ExecuteResponse` status from `internalErrorStatus` (a `StatusException` keeps its code; otherwise `INTERNAL` and `e.getMessage()`), and the stream still completes. Zel does not create or reject output paths.
  - The process is not run under a shell, and `Command` is built with timeout `Duration.ZERO`, so the action timeout does not kill it. A relative argv that contains a directory separator is absolutized against the working directory. An empty argv throws `IllegalArgumentException` `cannot run an empty command line`. `executeAsync` rethrows an `IOException` cause. Any other start failure is swallowed and the exit code is `-1` with no status. `AbnormalTerminationException` keeps `e.getResult()`. After exit, the deadline is `action.timeout` if set, otherwise 15 minutes. It is exceeded when `timeoutMillis > 0` and the wall time is greater, or when `terminationStatus().timedOut()` (which this zero timeout does not set). The status is `DEADLINE_EXCEEDED` with message `Command:\n{arguments}\nexceeded deadline of {timeoutMillis/1000.0} seconds.` The exit code is then `-1`, outputs are still uploaded, and the response carries both the result and that status. Zel has no deadline and does not spawn the command.
  - The environment is `LocalEnvProvider.forCurrentOs(System.getenv())` plus `BAZEL_REMOTE_PLATFORM` set to `name=value` pairs joined by commas in platform-property order (empty string when there are no properties). POSIX drops `TMPDIR` and sets it from the worker's `TMPDIR`, or `/tmp` when that is null or empty (the `/tmp` fallback passed by this server is not used). Windows drops `TMP` and `TEMP`, sets both from client `TMP`, else `TEMP`, else that fallback, and replaces `/` with `\`. Darwin rewrites through `XcodeLocalEnvProvider` using the runfile `xcode-locator`. A runfiles failure at construction throws `IllegalStateException`. Zel does not rewrite the environment or set `BAZEL_REMOTE_PLATFORM`.
  - A single `container-image` platform property must start with `docker://`. The rest is the image name (`docker://ubuntu` is accepted). The error text says images must be `docker://gcr.io/{IMAGE_NAME}`, but `gcr.io` is not checked. A second property is `INVALID_ARGUMENT` on field `platform`: `Multiple entries for container-image in action.Platform`. The argv is `docker run`, then on non-Windows (`os.name` starts with `Windows`) `-u {uid}` when `id -u` returns a number within 30 ms. Failure, timeout, or a non-numeric uid logs a warning, returns `-1`, and omits `-u`. Then `-v {workdir}:{workdir}-docker`, `-w {workdir}-docker`, one `-e key=value` per env entry in `HashMap` order, the image, and the arguments. The docker process itself inherits the worker environment (`environment` null). Otherwise, if `sandboxPath` is set, the argv is that binary, `-N` when `sandboxingBlockNetwork`, `-w` for each `sandboxingWritablePaths` entry, `-e` for each `sandboxingTmpfsDirs` entry, `--`, then the arguments, with the rewritten environment. Otherwise the arguments run directly with that environment. Zel does not wrap with docker or `linux-sandbox`.
  - Outputs are uploaded with `preserveExecutableBit` false, so every output file is `is_executable` true. Existing stdout and stderr files (`build-{id}/stdout-{uuid}` and `stderr-{uuid}`) are set as `stdout_digest` and `stderr_digest`. `execution_metadata` worker and execution start timestamps are the start instant, and the completed timestamps are start plus the wall time, only when the wall time is at least 1 ms. Exit code 0 uploads that `ActionResult` to the action cache after the blobs, including when `do_not_cache` is set. A non-zero exit does not. Zel stores the executor's `ActionResult` only when `do_not_cache` is false and `exit_code == 0`, and it does not upload stdout, stderr, or output files.
  - After the cache write, Bazel inlines at most one output file: the first `output_files` entry whose path is in `inline_output_files`. The bytes come from another CAS download. A download `ExecutionException` leaves `contents` empty. Later matches are not tried. The cached action result has empty `contents`. An `ExecException` from the upload becomes `FAILED_PRECONDITION` with `e.getMessage()` when no deadline status was already set. The result is then exit code only if the upload produced nothing. A missing blob from `downloadBlob` or `downloadTree` is `FAILED_PRECONDITION`, message `Missing Blob: {digest}`, with a `PreconditionFailure` violation type `MISSING`, subject `blobs/{hash}/{size}`, and an empty description. That `StatusException` is delivered as the `ExecuteResponse` status (the stream still completes), and both `Work failed: build-request-id: … command-id: … action-id: …` and `Work failed: {opName}` are logged. Zel's missing-blob status is code 9, message `missing {what}`, and the violation description is `{what}` (`action`, `command`, `input directory`, or `input file`). It does not inline outputs.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/FetchServer.java`
- Bazel lines: 332
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Rust service implements `build.bazel.remote.asset.v1.Fetch`. `zel-remote`’s fake server covers REAPI (CAS, ByteStream, Action Cache, Execution) only, and there is no Remote Asset proto in the tree.
  - `FetchBlob` is absent: at least one URI, qualifier parsing (`bazel.canonical_id`, `checksum.sri`, `http_header:`, `http_header_url:<index>:<name>`, unknown names rejected), in-memory `(url, canonical_id)` cache gated by `oldest_content_accepted`, HTTP GET with a 30s timeout when the request timeout is zero, URL-specific headers overriding global headers, SRI verification, digest, and upload into the on-disk CAS.
  - Download failure is returned as a completed `FetchBlobResponse` whose status is `DEADLINE_EXCEEDED`, `NOT_FOUND`, `ABORTED` (checksum mismatch), or `UNKNOWN`, using the first URI and no blob digest. Upload or interrupt failures are an `INTERNAL` RPC error. Neither path exists.
  - A cache hit returns the first request URI with the cached digest, not the URI that matched. That lookup is absent.
  - `FetchDirectory` returns `UNIMPLEMENTED` (“FetchDirectory is not implemented”). There is no equivalent RPC.
  - `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` is a local repository downloader, not this remote-worker Fetch server.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/OnDiskBlobStoreCache.java`
- Bazel lines: 170
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no remote-worker on-disk blob store. `OnDiskBlobStoreCache` is the worker `CombinedCache`: remote client null, `DiskCacheClient` opened with action-result integrity checking forced on, no symlink template, no chunking. `/workspace/zel-cursor/crates/zel-remote/src/disk_cache.rs` is the client `--disk_cache` store, and `/workspace/zel-cursor/crates/zel-remote/src/fake_server.rs` keeps CAS and the action cache in memory. Neither is this worker cache.
  - `getRemoteCacheCapabilities` advertises action-cache updates enabled and `SymlinkAbsolutePathStrategy.ALLOWED`, and leaves every other capability field at the proto default (no digest-function list, no batch limit). The fake server advertises digest functions (SHA-256 by default), a configurable `update_enabled` (default true), and `max_batch_total_size_bytes` (default 4 MiB), and leaves `symlink_absolute_path_strategy` at 0.
  - `refresh` touches the CAS entry mtime and returns whether the blob exists, propagating I/O errors other than a missing file. There is no worker entry point that does that. The client disk cache refreshes mtime on its own reads.
  - `downloadActionResult` follows the integrity-checking disk lookup only when `--action_cache_integrity_check` is true (default). When it is false, a missing `ac/` file returns null. A present file is parsed with an empty extension registry and returned with no mtime update and no check that referenced CAS blobs exist. `inlineOutErr` and `inlineOutputFiles` are ignored on that path. Zel has no such worker flag. The client disk cache’s `check_ac_integrity` flag still refreshes the action-result mtime after a successful decode, and a decode failure is a miss rather than a parse error.
  - `downloadTree` materializes a CAS `Directory` under `rootLocation`: create the directory, then files (download blob, set the executable bit), then symlinks, then recurse into subdirectories. Child names go through `unicodeToInternal`. A repeated path after that conversion throws `IOException` (`Duplicate child '%s' in directory %s`) across files, symlinks, and directories. Symlink targets go through `PathFragment.create`, which normalizes `.` and `..` and does not keep a raw target. The fake server only walks directories to see that blobs exist. `spawn_runner.rs` writes output `Tree` entries and local inputs. It does not do this CAS directory walk, duplicate-child check, or `PathFragment` symlink normalization.
  - With `--error_on_duplicate_downloads` (default false), `downloadBlob` counts downloads per digest and tool-invocation id and, on the second download, fails with `Duplicate download of blob digest <hash>/<size> for invocation id <id>` before the real read. The count includes cache misses, is never evicted, and is not updated when the flag is off. Different invocation ids do not share a count. Zel does not track or reject duplicate blob downloads.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/OnDiskHttpCacheServerHandler.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No HTTP REST cache handler maps `/ac/` and `/cas/` request URIs onto an on-disk action cache and CAS.
  - A cache read of a missing path does not return null (not found) while other I/O errors and any other URI prefix fail the request.
  - A cache write does not build a digest from the URI hash tail and body length, then store those bytes in the matching AC or CAS store.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/RemoteWorker.java`
- Bazel lines: 426
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No standalone remote-worker process: no `RemoteWorkerOptions` CLI, single-line logging with `--debug` at FINE and Netty forced to SEVERE, or process lifetime that starts the servers and blocks until the gRPC server terminates before shutting down the retry pool and Netty event loops.
  - No worker gRPC server on `--listen_port` with max inbound size `Integer.MAX_VALUE`, hosting Action Cache, ByteStream, CAS, Capabilities, and Remote Asset Fetch, plus Execution only when `--work_path` is set (otherwise execution is omitted and only cache requests are served). The default work directory is `/tmp/remote-worker`.
  - No `--unavailable` interceptor that closes every RPC except Capabilities with `UNAVAILABLE`, and no Bearer `Authorization` interceptor that requires the token to equal `--expected_authorization_token` or closes the call with `PERMISSION_DENIED`.
  - No optional OpenSSL TLS (`--tls_certificate` / `--tls_private_key`, client certs required when `--tls_ca_certificate` is set) and no optional HTTP cache server on `--http_listen_port` (port 0 leaves it off).
  - No `--cas_path` check that the path is absolute, no creation of that directory, and no digest function taken from the `bazel.DigestFunction` property (default SHA-256) with a Windows versus Unix filesystem.
  - No pid file containing the current process id, and no shutdown hook that deletes it.
  - No Linux-only sandbox bootstrap: absolute `--work_path` required, bundled `linux-sandbox` extracted and made executable, then a self-check that runs `true` inside it, exiting 1 on a non-Linux host or on any failure.
  - `zel-remote`'s in-process `FakeReapiServer` is a client test double (in-memory REAPI over HTTP/2). It does not implement this worker.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/RemoteWorkerOptions.java`
- Bazel lines: 274
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no remote-worker options type. `crates/zel-remote/src/options.rs` parses client `--remote_*` flags, and `FakeReapiServer` is an in-process test server. Neither accepts this worker flag set.
  - Missing flags and defaults: `--listen_port` 8080, `--work_path` and `--cas_path` unset, `--debug` false, `--legacy_api` false, `--pid_file` unset, `--sandboxing` false, repeatable `--sandboxing_writable_path` and `--sandboxing_tmpfs_dir`, `--sandboxing_block_network` false, `--http_listen_port` 0, unset `--tls_certificate`, `--tls_private_key`, and `--tls_ca_certificate`, unset `--expected_authorization_token`, `--unavailable` false, `--error_on_duplicate_downloads` false, and `--action_cache_integrity_check` true.
  - Path options go through `PathFragmentConverter`, which runs `unicodeToInternal` and then `PathFragment.create`. There is no worker-flag path converter.
  - `--jobs` defaults to `auto` (host CPU count) and also accepts `HOST_CPUS`, `HOST_RAM`, and `[-|*]<float>` expressions. Values below 1 fail with `Value '(%d)' must be at least %d.`. Values above 16384 are clamped to 16384 after the warning `Flag remoteWorker "jobs" ('%d') was set too high. This is a result of passing large values to --jobs. Using '%d' jobs`. Zel's `--jobs` converter is the client flag: cap 5000, and it clamps with no that warning.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/StatusUtils.java`
- Bazel lines: 118
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No remote-worker helper builds these `google.rpc.Status` values or the gRPC status exceptions wrapped around them.
  - `internalErrorStatus` returns an embedded rpc status from the throwable when `StatusProto.fromThrowable` finds one, and otherwise `INTERNAL` with the exception message. It does not map a plain throwable to `UNKNOWN`.
  - `notFoundStatus` is `NOT_FOUND` with message `Digest not found:` concatenated with the digest's protobuf text (no space after the colon).
  - `interruptedStatus` is `CANCELLED` with message `Server operation was interrupted for ` plus that digest text.
  - `invalidArgumentStatus` is `INVALID_ARGUMENT` with message `invalid argument(s): {field}: {desc}` and a packed `google.rpc.BadRequest` containing one field violation.
  - `outOfRangeError` is `OUT_OF_RANGE` with message `out of range: {field}: {desc}` and the same `BadRequest` detail.
  - `preconditionStatus` is `FAILED_PRECONDITION` with only the exception message and no details.
  - `missingBlobStatus` is `FAILED_PRECONDITION` with message `Missing Blob: ` plus the digest text, and a `PreconditionFailure` violation of type `MISSING` whose subject is `blobs/{hash}/{size_bytes}` and whose description is empty.
  - `/workspace/zel-cursor/crates/zel-remote/src/fake_server.rs` is a client test double. Its nearby errors (`blob not found`, `offset out of range`, `missing {what}` with a `MISSING` violation whose description is the blob role) do not use these codes, messages, or details.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/http/AbstractHttpCacheServerHandler.java`
- Bazel lines: 148
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No HTTP/1.1 REST cache server accepts `FullHttpRequest`s and dispatches `GET` to a cache read and `PUT` to a cache write.
  - URI check `^/?(.*/)?(ac/|cas/)([a-f0-9]{64})$` is absent. Bazel returns 400 for a non-match, 404 when a `GET` read returns null, 500 on read/write `IOException` (logged at severe), 405 for any method other than `GET` or `PUT`, and 400 when the request decoder failed.
  - Success responses are absent: `GET` is HTTP/1.1 200 with the raw bytes, `Content-Length`, and `Content-Type: application/octet-stream`; `PUT` is HTTP/1.1 204 with an empty body.
  - Error bodies (`<reason phrase>\r\n`, `Content-Type: text/plain; charset=UTF-8`, `Content-Length`) and closing the channel when the request is not keep-alive are absent.
  - A second decoder-failure check inside `PUT` that returns 500 is also absent. `channelRead0` already returns 400 for the same failure before `handlePut` runs.
  - `/workspace/zel-cursor/crates/zel-remote/src/disk_cache.rs` is a local `ac/`+`cas/` client, and `fake_server.rs` is an in-process REAPI gRPC server. Neither serves this HTTP cache.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/http/HttpCacheServerInitializer.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no HTTP cache accept path that, for each new socket, installs an HTTP server codec, aggregates the message into one full request with a 100 MiB cap (`100 * 1024 * 1024`), then passes it to a cache handler.
  - The Bazel initializer keeps one `AbstractHttpCacheServerHandler` and adds that same instance to every channel pipeline. Zel never shares a handler that way because it does not start this HTTP cache server.

### `src/tools/remote/src/main/java/com/google/devtools/build/remote/worker/http/InMemoryHttpCacheServerHandler.java`
- Bazel lines: 44
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel is an in-memory HTTP REST cache: the parent handler serves GET and PUT for `ac/` and `cas/` URIs from this map. Zel rejects `--remote_cache=http://...` in `/workspace/zel-cursor/crates/zel-remote/src/options.rs` and has no HTTP cache server. `/workspace/zel-cursor/crates/zel-remote/src/fake_server.rs` is an in-process REAPI gRPC server, not this protocol.
  - Reads return the bytes stored under the raw request URI, or null when the key is absent (the parent turns null into HTTP 404). Zel has no URI-keyed cache lookup.
  - Writes are `putIfAbsent`: the first bytes for a URI stay, and a later write of the same URI is ignored. Zel's in-memory CAS (`put_blob` in `/workspace/zel-cursor/crates/zel-remote/src/fake_server.rs`) keys by content hash and replaces any previous value for that hash.
  - Construction rejects a null map. Zel has no equivalent guard.

### `src/tools/remote/src/test/java/com/google/devtools/build/remote/worker/http/AbstractHttpCacheServerHandlerTest.java`
- Bazel lines: 107
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No remote-worker HTTP cache handler validates request URIs as optional leading `/`, optional prefix ending in `/`, then `ac/` or `cas/`, then exactly 64 lowercase hex digits.
  - Valid cases are untested and unimplemented: `http` and `https` hosts (domain, IPv4, localhost), scheme-less `host:port/...`, and bare `ac/<hash>` or `cas/<hash>` paths all return true.
  - Invalid cases are untested and unimplemented: `ac_` / `cas_` instead of a slash, a digest shorter than 64 hex digits, and a digest containing non-hex characters all return false.
  - Zel only parses a client `--remote_cache=http://` endpoint into host, port, and prefix, then refuses the HTTP/1.1 REST cache protocol. That does not accept or reject `ac`/`cas` digest paths.

### `src/tools/simple_catter/simple_catter.cc`
- Bazel lines: 75
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no tool that concatenates exactly two inputs into one binary output (`simple_catter <input1> <input2> <output>`), including the stderr usage, open, and write failures and exit code 1. Bazel uses this only from `bazel_binary` (`ConcatClientAndZip`); Zel has no matching packaging action.

### `src/tools/singlejar/combiners.cc`
- Bazel lines: 286
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar combiner. `java_resource_jar_action` only spawns the toolchain `single_jar` binary. Nothing in Zel implements the combining behavior below.
  - `Concatenator::Merge` accepts only stored and deflated entries (anything else exits with code 2). Deflated entries are inflated with one reused inflater. When newline insertion is on, a `\n` is inserted before the next entry if the buffer is non-empty and does not already end in `\n`.
  - `Concatenator::OutputEntry` returns null when there is no buffer or the local-header allocation fails. Otherwise it emits one local header: version 20, flag 0, time 00:00:01, date 2010-01-01, filename, and a zlib CRC. `compress` uses `CompressOut` (deflate only when the compressed size is smaller; empty input stays stored). Otherwise the payload is stored. Uncompressed size `>= 0xFFFFFFFF` adds a Zip64 extra field (uncompressed size `0xFFFFFFFF`, 16-byte payload, original size in attribute 0). The compressed-size field is `0xFFFFFFFF` only when the compressed size also needs Zip64; attribute 1 is still written with the full compressed size.
  - `NullCombiner::Merge` always succeeds and drops the entry. `OutputEntry` always returns null.
  - `XmlCombiner::Merge` builds one concatenator with newline insertion off, then writes `<tag>\n` once. Each input is inflated the same way as `Concatenator`. A leading exact start tag is removed. Trailing whitespace is stripped only when it sits after a matching end tag; otherwise it is kept. `OutputEntry` returns null if nothing was merged; otherwise it appends `</tag>\n` and emits that concatenator entry.
  - `PropertyCombiner::Merge` always returns false and does not read the entry.
  - `ManifestCombiner::Merge` always returns true and ignores the input bytes, so manifest bodies from inputs are not concatenated. `AppendLine` swallows a line containing `Multi-Release: ` and sets the flag only when `true` or `false` occurs at or after that prefix. A line containing `Add-Exports: ` or `Add-Opens: ` is not copied; the remainder after the first `key_length` bytes is split on whitespace and appended to those lists (the key match is a substring search, not an anchored prefix). Any other line is copied, and `\r\n` is added when it does not already end in `\n`. An empty line reaches that last-byte check.
  - `EnableMultiRelease` forces the flag on. `AddExports` and `AddOpens` append, preserving duplicates until output. `OutputEntry` writes `Multi-Release: true\r\n` only when the flag is set, then each module-flag list sorted and deduplicated as `Key: ` plus values separated by `\r\n `, then a blank `\r\n`, then the concatenator entry.

### `src/tools/singlejar/combiners.h`
- Bazel lines: 176
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar combiner types. `java_resource_jar_action` in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` binary.
  - `Combiner::Merge` folds one central-directory/local-header entry into the combiner. `OutputEntry(compress)` returns a caller-freed buffer of a local header plus payload: a copy of the held bytes when `compress` is false, otherwise compressed only when the compressed form is smaller.
  - `Concatenator` concatenates input entries and appended byte sequences under `filename`. `insert_newlines` defaults to true. `Append` lazily allocates a `TransientBytes` buffer. The C-string overload stops at the first NUL; the `std::string` overload keeps embedded NULs.
  - `NullCombiner` accepts later entries and writes nothing, so a directory entry already emitted is not emitted again.
  - `XmlCombiner` builds one XML entry whose tags are exactly `"<" + xml_tag + ">"` and `"</" + xml_tag + ">"`, with no check that `xml_tag` is a legal name.
  - `PropertyCombiner` is a `Concatenator` (so newline insertion stays on) that only appends `key=value\n`. It does not deduplicate keys, and its `Merge` override refuses existing entries.
  - `ManifestCombiner` starts a `Concatenator` with newline insertion off and `multi_release` false, and keeps separate `add_exports` and `add_opens` lists. `AppendLine` and `OutputEntry` are responsible for manifest lines, `Multi-Release`, and those module flags.

### `src/tools/singlejar/combiners_test.cc`
- Bazel lines: 369
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Concatenator, NullCombiner, XmlCombiner, PropertyCombiner, or ManifestCombiner. Resource jars only spawn the toolchain `single_jar` binary.
  - Concatenator joins selected zip entries with newlines (`<tag1>Contents1</tag1>\n<tag2>Contents2</tag2>`). `OutputEntry(true)` is a version-20 deflate local header named `concat` with no extra fields and compressed size at most the uncompressed size. `OutputEntry(false)` is stored, with equal sizes and the raw bytes.
  - Concatenator accepts a 5,000,000,000-byte append (5,000 chunks of 1,000,000 `0xFA` bytes) and reports that uncompressed size on a version-20 deflate local header.
  - NullCombiner `Merge(null, null)` succeeds, and both compressed and stored `OutputEntry` return null.
  - XmlCombiner wraps the merged bodies in `<toplevel>\n` … `</toplevel>\n` under the configured name (`combined.xml` / `combined2.xml`), with the same version-20 deflate versus stored local-header rules and no extra fields.
  - PropertyCombiner `AddProperty` emits `name=value\nname_str=value_str\n`. `Merge` returns false. Compressed and stored outputs match that text under the name `properties`.
  - ManifestCombiner always stores, even when compression is requested. `Multi-Release` from an input jar is dropped unless `AppendLine("Multi-Release: true")` is also used, which de-duplicates to `Multi-Release: true\r\n\r\n`. Jar-only `Multi-Release`, or `AppendLine("Multi-Release: false")`, yields `\r\n`. The entry name is `META-INF/MANIFEST.MF`, version 20, with no extra fields.

### `src/tools/singlejar/data/log4j2_plugins_gen.py`
- Bazel lines: 204
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no tool that writes a Log4j2 plugin cache: big-endian `u32` category count, then each category as a big-endian `u16` UTF-8 length plus bytes, a big-endian `u32` entry count, and each entry as key, class name, and name in that string form followed by two big-endian booleans (`printable`, `defer`). The entry's category is kept only in memory and is not written.
  - Zel has no loader that fills a category-to-key map from that format and prints a collision warning before overwriting an existing key.
  - Zel has no packer that puts the cache in a `ZIP_DEFLATED` jar at `META-INF/org/apache/logging/log4j/core/config/plugins/Log4j2Plugins.dat`.
  - Zel has no `--data_dir` (default `src/tools/singlejar/data`) / `--dump` CLI. Dump mode prints each category and `PluginEntry` and exits without writing. The default run writes `log4j2_plugins_set_1.jar` and `log4j2_plugins_set_2.jar` (cat1/key1, cat1/key2, cat2/key3, then cat1/key11, cat1/key12, cat3/key13) through a deleted `temp.dat`, then writes the merged `log4j2_plugins_set_result.dat` (cat1 keys key1, key11, key12, key2; cat2/key3; cat3/key13) in insertion order.

### `src/tools/singlejar/desugar_checking.cc`
- Bazel lines: 156
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No combiner reads `META-INF/desugar_deps`, accepting only stored or deflated entries and exiting with status 2 on any other method, a protobuf parse failure, or trailing bytes.
  - No first-wins accumulation of `assume_present` (`target` binary name plus `.class` to origin), `missing_interface` (target to origin), non-empty interface supertype lists, or interfaces with `num_default_methods > 0`.
  - No output-time check that each needed `.class` is already in the jar, or that each missing interface neither declares nor inherits default methods, including cycle-safe caching that treats a loop as not having default methods.
  - No failure that exits with status 2 (the production path) with the neverlink message or the “needed on the classpath for desugaring” message. The entry is also never written into the output jar.

### `src/tools/singlejar/desugar_checking.h`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar `Combiner` that reads `META-INF/desugar_deps` and checks Java 8 desugaring of default and static interface methods across a binary.
  - No check that every dependency the desugar step assumed is present in the output, using a caller-supplied known-member predicate (the neverlink classpath guard).
  - No check that interfaces missing at desugar time neither declare nor inherit default methods, using desugar metadata instead of opening `.class` files, including a cache of that result by bytecode interface name.
  - The public constructor always fails the merge on those errors (`fail_on_error` fixed true) and takes a verbose flag; `Merge` accepts a central-directory and local header, and `OutputEntry` runs the checks and emits no zip entry.
  - `crates/zel-bazel-tools/resources/src/main/protobuf/desugar_deps.proto` only keeps the `DesugarDepsInfo` schema. Nothing parses those entries or runs the checks. `experimental_check_desugar_deps` is only an option flag.

### `src/tools/singlejar/desugar_checking_test.cc`
- Bazel lines: 102
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `HasDefaultMethods` reports true for an interface recorded with default methods, false for an unknown interface, and true when a default method is reached through `extended_interfaces`, including a cycle (`d`↔`e` that also reaches `a`). The walk caches a false entry before recursing so the cycle does not crash, and `error_` stays false.
  - `OutputEntry(compress=true)` returns null and does not emit a jar entry. It asks the known-member predicate whether `a$$CC.class` is present, and it caches `HasDefaultMethods` as false for missing interface `b` and for extended interfaces `c`, `d`, and `e` when none of them have default methods. `error_` stays false.
  - With `fail_on_error` false, a needed companion that the predicate says is absent (`a$$CC.class` required by origin `b`) makes `OutputEntry` return null and set `error_`.
  - With `fail_on_error` false, a missing interface `a` that extends an interface `b` recorded as having default methods (and that also lists itself) makes `OutputEntry` return null and set `error_`, even when the known-member predicate always returns true.
  - Zel has no singlejar desugar-deps checker and no tests of these checks. `DesugarDepsInfo` is only a proto schema, and `experimental_check_desugar_deps` is only a flag.

### `src/tools/singlejar/diag.h`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar diagnostic wrappers. On Apple, Linux, FreeBSD, and OpenBSD, `diag_err` / `diag_errx` / `diag_warn` / `diag_warnx` are `err` / `errx` / `warn` / `warnx`: stderr text is `<program>: <printf message>` plus `: <strerror(errno)>` for `err` and `warn`, then a newline. `diag_err` and `diag_errx` `exit` with the given status. `diag_warn` and `diag_warnx` return.
  - On Windows those four macros do not call `err.h`. `diag_err` and `diag_errx` are the same: `fprintf` `ERROR: <message>\n` to stderr and `ExitProcess(exit_value)`, with no program name and no `errno` text. `diag_warn` and `diag_warnx` are the same `WARNING: <message>\n` and return. The format argument must be a string literal. Any other platform fails the compile with `Unknown platform`.
  - Nothing under `/workspace/zel-cursor` defines these macros or prints singlejar’s `err`/`warn` lines. `java_resource_jar_action` in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` binary.

### `src/tools/singlejar/input_jar.cc`
- Bazel lines: 173
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`
- Verdict: partial
- Differences:
  - `Open(path)` memory-maps the file and a second open exits via `errx` (status 1). `Open(path, data, length)` requires a non-empty path (empty path exits the same way), maps the caller’s buffer, and does not check that the instance is already open. A failed path open warns and returns false. Zel has no `InputJar`. `parse_zip` takes one buffer and returns `Result`. `java_native.rs` spawns the toolchain `single_jar` binary.
  - A path open rejects a file shorter than 22 bytes before the scan. The memory open does not. `find_zip_eocd` returns none for every buffer shorter than 22, and `parse_zip` then errors with `Zip file '<name>' is malformed. It does not contain an end of central directory record.`
  - Bazel walks back at most 65536 + 22 bytes and keeps the first `PK\x05\x06`, ignoring the comment-length field. Zel scans the whole buffer and keeps a signature only when the 16-bit comment length ends exactly at EOF. A spurious signature inside the comment is a Bazel hit and a Zel miss. A comment longer than 65536 bytes is a Bazel miss and a Zel hit.
  - For a non-ZIP64 directory, Bazel places the first header at `ECD - cen_size` and sets `preamble_size_` to that address minus the recorded offset, so an unadjusted SFX prefix is added to later local-header offsets. A non-`0xFFFFFFFF` offset outside the map or past the ECD, and a non-`0xFFFFFFFF` size larger than the ECD offset, are warnings and a false return. Zel uses `eocd - cd_size` only when that value is non-negative and its low 32 bits equal the recorded offset. Any other preamble is read from the recorded offset, and Zel stores no preamble.
  - `cen_size == 0` is an empty archive: the cursor is the ECD, the central-header signature is not checked, and ZIP64 is not consulted. The preamble is still `offset(ECD) - cen_offset32`. Zel yields no entries when the entry count is 0 or the size-based span is 0. A zero size with a non-zero count still reads that many headers from the recorded offset.
  - If the 20 bytes before the ECD are a ZIP64 locator, Bazel expects a fixed 56-byte ECD64 immediately before that locator. It ignores the locator offset and `remaining_size`. A missing ECD64 signature warns, unmaps, and returns false. The directory start is `ECD64 - cen_size64`, and the preamble is that position minus `cen_offset64`. A `0xFFFFFFFF` size or offset with no locator returns false without unmapping, so the next path `Open` on that instance exits because the fd is still open. Zel tries ZIP64 only when the entry count is `0xFFFF` or the offset is `0xFFFFFFFF` and a locator is present, reads the ECD64 at the locator’s absolute offset (wrong when a preamble was not adjusted), replaces the count and offset but leaves the 32-bit size, and on a bad record keeps the Zip32 values.
  - After a non-empty directory is located, Bazel requires the first record’s signature to be `PK\x01\x02`. Zel reports `Malformed Central Directory File Header; does not start with 02014b50` only when it reads a header.
  - `Close` clears the path and returns true. It unmaps only a file-backed mapping. Zel has no close step.
  - Vendored ijar `FindZipCentralDirectory` in `/workspace/zel-cursor/crates/zel-bazel-tools/resources/third_party/ijar/zip.cc` is a different reader. It requires the comment to reach EOF, caps the scan at 65535 + 22 bytes, rejects multi-disk archives, and does apply a preamble (`central directory - recorded offset`).

### `src/tools/singlejar/input_jar.h`
- Bazel lines: 115
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar `InputJar`. Nothing memory-maps an input jar and walks its central directory, returning each central-directory record together with a local-header pointer.
  - `NextEntry` aborts the process if `Open` was never called. It returns null when the cursor is not a central-directory signature (`0x02014b50`), which is also how an empty archive ends. It then steps forward by the record size (46 bytes plus the name, extra field, and comment) and aborts if that next address is outside the mapping, including when the current record ends exactly at the mapped end. The diagnostic includes the current record’s mapped offset and those three lengths. The local header itself is not bounds-checked.
  - `LocalHeader` is the mapped address of the central-directory local-header offset plus `preamble_size_` (bytes before the zip proper, so an unadjusted self-extractor prefix still resolves). `CentralDirectoryRecordOffset` and `LocalHeaderOffset` are offsets in that mapping, and `mapped_start` is byte 0, used to copy raw entry bytes. On non-Windows, `fd()` returns the mapped file descriptor. The destructor calls `Close`. Opening from a path or from an existing buffer is only declared here.
  - The other zip reader, `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`parse_zip` and `zip_entry_bytes`), extracts repository archives the way Java `ZipReader` does: it returns an error on a bad directory, stops by entry count or directory size, and reads each local header at the stored offset with no preamble added. `java_native` only schedules an external singlejar action.

### `src/tools/singlejar/input_jar_bad_jar_test.cc`
- Bazel lines: 36
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `NotAJar` requires `InputJar::Open` to return false for a 1000-byte non-zip (`jar.jar` created by `AllocateFile`). The map is long enough for an end-of-central-directory record, the scan of the last 64KiB finds no ECD signature, and the mapping is closed. Zel has no singlejar input-jar opener.
  - `EmptyFile` requires `InputJar::Open` to return false for a zero-length file because the map is shorter than an ECD record, without faulting in `MappedFile`. That empty-file guard is absent.

### `src/tools/singlejar/input_jar_empty_jar_test.cc`
- Bazel lines: 81
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`
- Verdict: partial
- Differences:
  - `EmptyZipFile` requires `InputJar::Open` of the 22-byte `data/empty.zip` to succeed and `NextEntry` to yield nothing (`cen_size == 0` leaves the cursor on the end-of-central-directory record, which is not a central-directory header). `parse_zip` returns an empty entry list for that record. `extract` then fails it with `Prefix "" was given, but not found in the archive. We could not find any directory in this archive (maybe there is no need for strip_prefix?)` when the strip prefix is empty and `strip_components` is 0.
  - `InputJarPreambledTest.Empty` concatenates a 100-byte `ftruncate` file in front of that zip and does not run `zip -A`. Open must still succeed and yield no entries, and the empty-archive branch sets `preamble_size_` to `offset(EOCD) - cen_offset` (100). `parse_zip` also yields no entries, because the recorded entry count is 0, so it never reads the central directory at the unadjusted offset 0. It does not record a preamble size, and `extract` still rejects the empty list as above.
  - Both cases locate the directory by the first `0x06054b50` in the last 65536+22 bytes, without requiring the comment length to consume the tail. `find_zip_eocd` requires `offset + 22 + comment_len == file_len` and scans the whole buffer. Both accept this fixture (comment length 0, record at EOF, with or without the 100-byte prefix).

### `src/tools/singlejar/input_jar_preambled_test.cc`
- Bazel lines: 120
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`
- Verdict: partial
- Differences:
  - `Small` truncates a 100-byte prefix and concatenates it in front of `libtest1.jar` with no `zip -A`. `InputJar` places the central directory at the end-of-central-directory record minus the 32-bit directory size, sets the preamble to that physical offset minus the recorded offset, and reads each local header at the recorded offset plus the preamble. Each entry must carry central-directory signature `0x02014b50` and local-header signature `0x04034b50`, the two file names must match, and when general-purpose bit 3 is clear the compressed and uncompressed sizes must match, taking Zip64 extra attributes when a 32-bit size is `0xFFFFFFFF`. `parse_zip` uses `eocd - cd_size` only when its low 32 bits equal the recorded offset. A 100-byte prefix fails that check, so the directory is read at the pre-preamble offset, and `zip_entry_bytes` seeks the recorded local-header offset.
  - `SmallAdjusted` runs `zip -A`, so the recorded offsets already include the preamble and the low-32 check can match. Zel checks a local-header signature, the compression method, and the central-directory uncompressed size only when an entry’s bytes are read. It never compares the local-header file name to the central-directory name and never compares the two size fields, so bit 3 does not change that check. Nothing runs `zip -A`.
  - `Huge` truncates a sparse file to `0x10000000F` bytes, stores it with `zip -0m` (Zip64), then concatenates `OutputFilePath("exe")`, which this test never creates (a 100-byte `exe` exists only if `Small` or `SmallAdjusted` already ran in the same `TEST_TMPDIR`). `InputJar` reads the Zip64 locator and a fixed-size end-of-central-directory record from the bytes immediately before the 32-bit end record, derives the preamble from the physical directory start minus `ECD64.cen_offset`, and adds that preamble to local-header offsets. Zel enters the Zip64 path only when the entry count is `0xFFFF` or the directory offset is `0xFFFFFFFF` and the locator signature sits 20 bytes before the end record, then uses the locator’s absolute `u64` at locator+8. An unadjusted preamble makes that address miss signature `0x06064b50`, and the directory offset stays `0xFFFFFFFF`. `extract` also loads the whole archive with `fs::read`. `detect_kind` rejects the `out.jwp` names used by `Small` and `SmallAdjusted` before `parse_zip` runs.

### `src/tools/singlejar/input_jar_random_jars_test.cc`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `ScanAllJars` requires `third_party/bazel/src/tools/singlejar/jars_to_test` to exist, then opens every regular file and every symlink whose `stat` target is a regular file. Broken symlinks are skipped after `perror`. Other entries are skipped. The walk is not recursive.
  - Each path is opened with `InputJar::Open` (failure fails the test). Every central-directory entry must have a valid CDH, a non-null local header with a valid signature, and the same filename as the local header. Names that do not end in `/` are counted as files. The jar is closed, and `name: files, entries` is printed to stderr.
  - The test fails unless at least one jar was processed. Zel has no `InputJar` scan of that corpus and no check that local-header names match central-directory names.

### `src/tools/singlejar/input_jar_scan_entries_test.h`
- Bazel lines: 317
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar `InputJar` scan suite. `parse_zip` in `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` extracts repository archives like Java `ZipReader`, and `java_native` only schedules an external singlejar action.
  - `OpenClose` builds a compressed jar of `res1` (123 bytes) and `res2` (456 bytes), requires `Open` to succeed with `fd() >= 0`, and requires `Close` to leave `fd() < 0`. Zel never exposes a jar file descriptor.
  - `SmogCheck` requires each central-directory record to have a local header with the local signature, the same file name, and, unless general-purpose bit 3 is set, the same compressed and uncompressed sizes. Zel does not cross-check the local header against the directory and ignores bit 3.
  - `Basic` requires those two entries at those uncompressed sizes and skips names ending in `/`. `BasicInMemory` requires the same scan when `Open` is given an already mapped buffer plus a diagnostic path. Zel has no mapped-buffer open.
  - `HugeUncompressed` requires a compressed entry `4GB+1file` whose uncompressed size is `0x100000001`. `TestZip64` requires a stored `4GB-1file` with both sizes `0xFFFFFFFF`, and stored `4GB+1file`, `empty`, and `res1` whose local-header offsets are greater than `0x100000001` (`empty` also requires a local compressed size of 0). Zel does not scan a >4GB jar or read the local header’s compressed size.
  - `LotsOfEntries` stores 256 directories of 256 one-byte files and requires at least 256×257 records, 256 central-directory names ending in `/`, and 256×256 files, allowing JDK `jar` to add `META-INF/`. On non-Apple non-Windows it also requires `unzip -v jar.jar | tail` to exit 0. Zel has no >64K entry scan and does not run `unzip`.

### `src/tools/singlejar/input_jar_scan_jartool_test.cc`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `JartoolCreator::SetUpTestCase` resolves the JDK `jar` binary once through Bazel runfiles (`JAR_TOOL_PATH`). Zel never locates or invokes that tool.
  - `Jar(compress, output_jar, paths...)` shells out to `jar`. An existing output is updated with `-uf` when `compress` is true and `-u0f` when it is false. A new output is created with `-cf` or `-c0f`. Member paths are appended space-separated, so the shell expands globs such as `dir*`. The call returns the `system()` status. Zel has no create-versus-update or deflate-versus-store `jar` driver.
  - The file instantiates the `InputJarScanEntries` suite as `Jartool`, so open/close, basic compressed entries, a >4GB entry, Zip64 sizes and local-header offsets, >64K entries, and an in-memory open all run against archives this JDK `jar` invocation produced. Zel has no `InputJar` scanner and no test that checks those cases on JDK `jar` output.

### `src/tools/singlejar/input_jar_scan_ziptool_test.cc`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `ZiptoolCreator::Jar` shells out through `system()`: a new compressed archive is `zip -qr <out> <paths>`, and a new stored archive is `zip -qr0 <out> <paths>`. Paths are space-joined and null-terminated. If the output already exists, the command is replaced with `u <out> …` or `u0 <out> …`, which does not invoke `zip`. Setup and teardown are empty, so `zip` must already be on `PATH`.
  - That creator is registered as the `Jartool` instantiation of `InputJarScanEntries`, so the zip-built jars are the ones opened, scanned entry-by-entry, and checked for central-directory versus local-header name and size, file descriptor open/close, a deflated entry of uncompressed size `0x100000001`, stored Zip64 entries and local-header offsets past `0x100000001`, at least `256×257` entries from a shell glob `dir*`, and the same basic scan from a mapped file image.
  - Zel has no InputJar and no test that builds archives with Info-ZIP `zip` and walks them that way. The only system-`zip` use is archive extraction (`zip -q -r -y` of a small tree, then content, mode, and symlink checks).

### `src/tools/singlejar/log4j2_plugin_dat_combiner.cc`
- Bazel lines: 198
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Log4j2 plugin-cache combiner: nothing inflates a stored or deflated entry, parses the big-endian cache (category count, category name, plugin count, then key, class name, name, printable, defer), or writes that layout back.
  - Merge semantics are absent: categories and plugin keys are not merged with first-wins `map` insert, duplicate `category.key` does not abort when `no_duplicates` is set, and any other compression method does not abort.
  - Output is absent: a non-empty merged cache is not rewritten in sorted category and key order through the concatenator, and an empty cache does not suppress the output entry.

### `src/tools/singlejar/log4j2_plugin_dat_combiner.h`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `PluginEntry` is a public record of a Log4j2 plugin cache entry: `key`, `className`, `name`, `printable`, `defer`, and `category`, all set by the constructor. Zel has no such entry type.
  - `Log4J2PluginDatCombiner` is a `Combiner`. Its constructor stores `filename` and `no_duplicates` and creates a `Concatenator` for that filename with newline insertion off (`Concatenator(filename_, false)`), so the output entry is raw bytes. Zel never constructs this combiner.
  - The combiner holds an optional inflater and `categories_`, an ordered map from category name to an ordered map from plugin key to `PluginEntry`. `Merge(const CDH *, const LH *)` and `OutputEntry(bool compress)` are the only operations (bodies are in the `.cc`). Zel has no plugin-cache merge state. `java_resource_jar_action` only spawns the toolchain `single_jar` binary.

### `src/tools/singlejar/log4j2_plugin_dat_combiner_test.cc`
- Bazel lines: 145
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Log4j2 plugin-cache combiner and no test that merges `Log4j2Plugins.dat` from `log4j2_plugins_set_1.jar` and `log4j2_plugins_set_2.jar` (duplicates allowed) into one deflated local-file entry named `META-INF/org/apache/logging/log4j/core/config/plugins/Log4j2Plugins.dat` with version 20, deflate method, compressed size at most the uncompressed size, and inflated bytes equal to `log4j2_plugins_set_result.dat`.
  - Zel has no test that, with duplicates forbidden, merging that same plugins-cache entry from `log4j2_plugins_set_1.jar` a second time aborts with a message containing `present in multiple jars`.

### `src/tools/singlejar/mapped_file.cc`
- Bazel lines: 21
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar `MappedFile`. Java resource jars are spawned through the toolchain `single_jar` executable, and the Rust tree never maps an input file read-only into a pointer span.
  - This file compiles the Windows mapper when `_WIN32` is set and the POSIX mapper otherwise. The POSIX body is limited to 64-bit Linux, macOS, FreeBSD, and OpenBSD. The Windows body requires 64-bit Windows.
  - `Open` on an already-open instance aborts. Other open failures warn and return false. POSIX uses `open(O_RDONLY)`, `fstat`, and `mmap(PROT_READ, MAP_PRIVATE)`. A directory is reported as a directory. An empty file is mapped as one byte while the reported span length stays 0, and `Close` unmaps that 0-length span. Windows converts the path with `AsAbsoluteWindowsPath` and opens it with `CreateFileW(GENERIC_READ, FILE_SHARE_READ, OPEN_EXISTING)`. An empty file stays unmapped, the file handle remains open, and `Open` returns true. A non-empty file uses `CreateFileMapping(PAGE_READONLY)` and `MapViewOfFile(FILE_MAP_READ|FILE_MAP_COPY)`.
  - `Close` does nothing when the instance is not open. When it is open, POSIX `munmap`s the reported span and closes the fd. Windows unmaps a non-null view, closes a valid mapping handle, then closes the file handle.

### `src/tools/singlejar/mapped_file.h`
- Bazel lines: 84
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no read-only mapped-file type: no `Open` that `mmap`s (or Windows-maps) a path, fatal-errors if already open, returns false on open/map failure, and maps an empty file to a 1-byte private mapping with size 0 on POSIX or a null range that stays open on Windows.
  - No `MapExisting` that adopts an existing `[start, end)` range and always returns true without taking a file descriptor.
  - No `Close` or destructor that unmaps and closes the descriptor or Windows handles only when open.
  - No `mapped`, `start`, `end`, `address`, `offset`, or `size` over the mapped range, and no non-Windows `fd()`.

### `src/tools/singlejar/mapped_file_posix.inc`
- Bazel lines: 75
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar `MappedFile` open/close. `java_native.rs` only spawns the toolchain `single_jar` binary. Nothing maps an input path read-only.
  - This implementation is compiled only for 64-bit Linux, macOS, FreeBSD, or OpenBSD.
  - `Open` aborts with `diag_errx(1, ...)` when `fd >= 0`. Otherwise it `open`s the path `O_RDONLY`; failure warns and returns false.
  - `fstat` or `mmap` failure warns (`%s is a directory` when `S_ISDIR`, otherwise an `mmap` warning), closes the descriptor, sets `fd` to `-1`, and returns false. A non-empty file is `mmap`’d with `PROT_READ` and `MAP_PRIVATE` for `st_size` bytes. An empty file is mapped as 1 byte, but `mapped_end_` stays at `mapped_start_`, so the logical length is 0.
  - `Close` `munmap`s `mapped_end_ - mapped_start_` bytes (0 for an empty file, not the 1 byte that was mapped) and closes the descriptor. `is_open` is `fd >= 0`.

### `src/tools/singlejar/mapped_file_windows.inc`
- Bazel lines: 132
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar `MappedFile` Windows backend. Nothing opens a path, maps the whole file as a read-only byte range, and keeps the file handle open until `Close`.
  - This file builds only for 64-bit Windows. A second `Open` on an already-open instance exits through `diag_errx(1)`. The path is converted with `AsAbsoluteWindowsPath`; failure warns and returns false. The file is opened with `CreateFileW` (`GENERIC_READ`, `FILE_SHARE_READ`, `OPEN_EXISTING`); failure warns and returns false.
  - A zero-length file is a successful open with a null mapped range and no mapping object, because `CreateFileMapping` rejects length 0. The file handle stays open, so `is_open()` is true and `Close` only closes that handle.
  - A non-empty file is mapped with `CreateFileMapping` (`PAGE_READONLY`) and `MapViewOfFile` (`FILE_MAP_READ | FILE_MAP_COPY`, offset 0, full size). Either failure warns, closes the handles already taken, and returns false. Success sets the mapped end to the start plus the file size. `GetFileSizeEx` failure is not checked.
  - `Close` unmaps only when the view pointer is non-null, closes the mapping handle when it is not `INVALID_HANDLE_VALUE`, closes the file, and clears both pointers.
  - `/workspace/zel-cursor/crates/zel-bazel-tools/resources/third_party/ijar/mapped_file_windows.cc` is ijar `MappedInputFile`, not this type: it aborts on open, map, and close errors, does not special-case an empty file, and maps with `FILE_MAP_READ` only. Resource jars in Zel only spawn the toolchain `single_jar` binary.

### `src/tools/singlejar/options.cc`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar command-line parser. `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` binary for resource jars, always passing `--normalize`, `--dont_change_compression`, `--exclude_build_data`, `--no_strip_module_info`, and `--output`, plus optional `--sources`, `--resources`, and `--classpath_resources`.
  - `ParseCommandLine` walks an `ArgTokenStream`, so a token beginning with `@` is expanded as a parameter file before matching. Any token `ParseToken` does not consume exits 1 with `Bad command line argument %s`. Parsing then always calls `PostValidateOptions`.
  - Exact boolean flags set the field true and take no value: `--exclude_build_data`, `--compression` (`force_compression`), `--dont_change_compression` (`preserve_compression`), `--normalize` (`normalize_timestamps`), `--add_missing_directories`, `--no_duplicates`, `--verbose`, `--warn_duplicate_resources`, `--check_desugar_deps`, `--multi_release`, `--no_strip_module_info`. A token such as `--verbose=true` is an unrecognized argument.
  - These flags each require one following token, and a later occurrence overwrites the field: `--output` (`output_jar`), `--main_class`, `--java_launcher`, `--cds_archive`, `--jdk_lib_modules`, `--build_target`, `--hermetic_java_home`, `--output_jar_creator`. A missing token exits 1 with `%s requires argument`. The next token is stored as the value when it starts with `-`.
  - `--build_info_file` and `--extra_build_info` each require one following token and append it to `build_info_files` or `build_info_lines`, so repeated flags accumulate.
  - These flags append every following token that does not start with `-`, and a flag with no such tokens appends nothing: `--deploy_manifest_lines`, `--resources`, `--classpath_resources`, `--include_prefixes`, `--nocompress_suffixes`, `--add_exports`, `--add_opens`. `--exclude_zip_entries` inserts those tokens into a set, so duplicate names collapse.
  - `--sources` appends those same non-option tokens as pairs split on the first comma. A token with no comma is `(token, "")`. `path,suffix` is `(path, suffix)`, and any later commas stay in the suffix.
  - `PostValidateOptions` exits 1 when `output_jar` is empty, with `Use --output <output_jar> to specify the output file name`, and when both `--compression` and `--dont_change_compression` are set, with `--compression and --dont_change_compression are mutually exclusive`.

### `src/tools/singlejar/options.h`
- Bazel lines: 97
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar `Options` object and does not parse a singlejar command line. `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` binary.
  - A new `Options` sets `output_jar_creator` to `"singlejar"` and `build_target` to empty, and sets these booleans to false: `exclude_build_data`, `force_compression`, `normalize_timestamps`, `add_missing_directories`, `no_duplicates`, `no_duplicate_classes`, `preserve_compression`, `verbose`, `warn_duplicate_resources`, `check_desugar_deps`, `multi_release`, `no_strip_module_info`. The other strings start empty. `manifest_lines`, `resources`, `classpath_resources`, `build_info_files`, `build_info_lines`, `include_prefixes`, `nocompress_suffixes`, `add_exports`, and `add_opens` start empty. `input_jars` is an empty vector of string pairs. `exclude_zip_entries` is an empty set. `no_duplicate_classes` has no setter on this type and stays false unless other code assigns it.
  - `ParseCommandLine` fills those fields from argv. Protected virtual `ParseToken` consumes one notional flag, which may take many following tokens, and returns whether that flag was recognized and fully consumed. Protected virtual `PostValidateOptions` runs after the command line is consumed and `exit(1)`s on an invalid combination; the header's example is a missing `--output_jar`. Both hooks are overridable. Nothing in this tree subclasses `Options`.
  - The resource-jar spawn always passes `--normalize`, `--dont_change_compression`, `--exclude_build_data`, `--no_strip_module_info`, and `--output`. It may also pass `--sources`, `--resources` (as `execpath:respath` when those paths differ), and `--classpath_resources`. It does not reject a missing output name, does not treat excluded zip entries as a set, and does not store the rest of this option state.

### `src/tools/singlejar/options_test.cc`
- Bazel lines: 186
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `--exclude_build_data`, `--compression`, `--normalize`, and `--no_duplicates` set `exclude_build_data`, `force_compression`, `normalize_timestamps`, and `no_duplicates`. `--output` and `--hermetic_java_home` store the following token (`output_jar`, `hermetic_java_home`). `preserve_compression`, `verbose`, `warn_duplicate_resources`, `check_desugar_deps`, and `multi_release` stay false.
  - `--dont_change_compression`, `--verbose`, `--warn_duplicate_resources`, `--check_desugar_deps`, and `--multi_release` set `preserve_compression`, `verbose`, `warn_duplicate_resources`, `check_desugar_deps`, and `multi_release`. The four booleans from the first case stay false, and `hermetic_java_home` stays `""`.
  - `--output`, `--main_class`, `--java_launcher`, `--cds_archive`, and `--jdk_lib_modules` each store one string. Repeated `--build_info_file` and `--extra_build_info` append in order (`build_file1`, `build_file2` and `extra_build_line1`, `extra_build_line2`).
  - Repeated `--sources` appends jar names in order (`jar1`, `jar2`, `jar3`) as the first of each input-jar pair. `--resources`, `--classpath_resources`, `--include_prefixes`, `--nocompress_suffixes`, `--add_exports`, and `--add_opens` keep order. `--exclude_zip_entries` is a set containing `file1`, `file2`, and `file3`.
  - A multi-value flag with no tokens before the next `--` flag adds nothing. After `--output output_file --sources --resources --classpath_resources --sources --include_prefixes prefix1 --resources`, input jars, resources, and classpath resources are empty and include prefixes is only `prefix1`.
  - `--output_jar_creator` followed by `CustomCreatedBy 123.456` replaces the creator string. With only `--output output_file`, the creator is `singlejar`.
  - Zel has no singlejar options parser and no tests of this parsing. `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` `java_resource_jar_action` only spawns the toolchain `single_jar` with `--normalize`, `--dont_change_compression`, `--exclude_build_data`, `--no_strip_module_info`, `--output`, and optional `--sources`, `--resources`, and `--classpath_resources`.

### `src/tools/singlejar/output_huge_jar_test.cc`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `EntryAbove4G` builds a `0x100000010`-byte launcher (`TEST_TMPDIR`/`AllocateFile`), runs singlejar with `--java_launcher` and `--sources` `io_bazel/src/tools/singlejar/libtest1.jar`, requires `OutputJar::Doit` to return 0, and requires `zip -Tv` to accept the archive so the first copied entry sits past 4GiB (Zip64 local-header offset and end records).
  - Zel has no singlejar `OutputJar`. `java_resource_jar_action` in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` binary and never passes `--java_launcher`.

### `src/tools/singlejar/output_jar.cc`
- Bazel lines: 1167
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar `OutputJar`. `java_resource_jar_action` in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` binary. `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` only reads and extracts archives. Nothing in Zel implements the writer below.
  - `Doit` runs once. A second call exits 1. Unless `--exclude_build_data`, `build-data.properties` is a combiner; otherwise that name is copied from the first input that has it. The manifest gets `Manifest-Version: 1.0` and `Created-By: ` plus `output_jar_creator`. `build.target` is `build_target`, or the output path when that is empty. Verbose mode prints `combined_file_name`, optional `main_class` and `java_launcher_file`, the source count, and the manifest-line count. `Open` failure exits 1.
  - A non-empty launcher is copied in front of the zip with mode-0777 output, in 128KB `pread` chunks from offset 0 (Windows: `ReadFile`). A short or failed copy exits 1. `main.class` and `Main-Class: ` are added only when `main_class` is set. `--cds_archive` is zero-padded to the OS page size (`sysconf(_SC_PAGESIZE)`, or `dwPageSize` on Windows); the manifest gains `Jsa-Offset: %ld` (50-byte buffer) and `build-data.properties` gains `cds.archive` set to that path. `--jdk_lib_modules` is padded the same way and adds `JDK-Lib-Modules-Offset` and `JDK-Lib-Modules-Size`. The recorded offset is the aligned position computed before the pad write.
  - `--multi_release` enables multi-release. `AddExports` and `AddOpens` are applied. A non-empty hermetic Java home appends `Hermetic-Java-Home: `. Each non-empty `--manifest_line` is appended. Each build-info line is stored with an added `\n`. Each build-info file is mapped or the process exits 1, then copied in `\n`-terminated slices with no property parsing. `--classpath_resources` uses the text after the last `/`. `--resources` splits on the last `:`, rejects a colon at index 0, and on Windows does not split a volume separator at index 1 of an absolute path.
  - A classpath path that is already in the member map is skipped after a warning when `--warn_duplicate_resources` is set. Otherwise another entry of that name is still queued; the map keeps the first combiner. A path that maps is stored under the resource name. A directory does not map: an empty `name/` concatenator is queued (its output entry is null, so no zip bytes are written) and `name` without the extra slash is recorded on the null combiner, which drops a later jar entry of that exact name. Anything else exits 1.
  - The output is created truncated and writable, executable (`0777`) off Windows. Windows uses `CreateFileW` with read sharing and binary mode. Failure warns and `Open` returns false. I/O is fully buffered at 128KB. `Position` is a cached offset and exits if the file is not open.
  - Zip order is the launcher and page-aligned preambles, then `META-INF/` (stored, version 2.0, extra field `FE CA 00 00`), the manifest, `build-data.properties` unless excluded, classpath entries, each input jar, then at close the `META-INF/services/` combiners, extra combiners, `META-INF/spring.handlers`, `META-INF/spring.schemas`, and `protobuf.meta`. Synthesized entries are compressed when either `--compression` or `--dont_change_compression` is set. Classpath names ending in a `--nocompress_suffixes` entry stay stored, and missing parent directories are written even without `--add_missing_directories`. Combiners flushed at close use only `--compression`, so `--dont_change_compression` leaves them stored. `WriteEntry(null)` writes nothing.
  - `AddJar` exits on an input that will not open or close. An empty central-directory name exits 1. Names ending in `.SF`, `.RSA`, or `.DSA` are dropped everywhere, not only under `META-INF`. Unless `--no_strip_module_info`, `module-info.class` and `META-INF/versions/.../module-info.class` are dropped. A non-empty `--include_prefixes` list keeps a name only when one prefix matches; `--exclude_zip_entries` then drops an exact name. A file under `META-INF/services/` is concatenated. Every other kept entry calls `ExtraHandler`, which is empty in this file. With `--check_desugar_deps`, a remaining name starting with `j$/` exits 1.
  - The first plain file is kept and remembers its jar index. A later plain duplicate exits 1 under `--no_duplicates`, or under `--no_duplicate_classes` when the name ends in `.class`, citing both jars. Otherwise it is skipped and counted. Directory duplicates go to the null combiner and are dropped. `--add_missing_directories` inserts missing parents before the entry. A file is compressed when `--compression` is set, or when `--dont_change_compression` is set and the input was already compressed, unless a nocompress suffix matches. A method change is rewritten through a temporary concatenator; merge failure exits 1. Otherwise the local header, payload, and data descriptor (when the local header has no sizes, including a Zip64 descriptor) are copied. Copy failure exits 1.
  - `--normalize` sets copied timestamps to 2010-01-01 00:00:00, or 00:00:02 for `.class`, and strips a Unix-time extra field from the local header and central directory only when the date, time, or that field differs. Central-directory copies keep the input version, flags, comment, and attributes, rewrite the local-header offset, and add or drop one Zip64 attribute when the output offset crosses the 4GiB boundary. The offset is the last Zip64 attribute.
  - `WriteEntry` sets the same normalized timestamps, or otherwise the local time rounded up to an even second in DOS format. Its central-directory record is version 2.0, flag 0, comment and external attributes empty. Non-Zip64 extras are copied. If the uncompressed size, compressed size, or offset needs Zip64, all three 64-bit fields are written and the 32-bit fields are `0xFFFFFFFF`. Directory records are stored and empty. A null-buffer write is ignored.
  - The central directory grows in 1,000,000-byte steps; allocation failure exits 1. Zip64 end records are written when the data offset or directory size is at least `0xFFFFFFFF` or the entry count is at least `0xFFFF`: Zip64 end record version `0x031E` and extract version 45, a one-disk locator, then a classic end record with `0xFFFF` counts and `0xFFFFFFFF` sizes. Otherwise a 32-bit end record is used. The end records are not included in the recorded directory size. A short directory write or `fclose` failure exits 1. Closing an already closed jar succeeds. The destructor warns if `Close` was not called. Verbose close prints the entry count and, when non-zero, the skipped-duplicate count.

### `src/tools/singlejar/output_jar.h`
- Bazel lines: 141
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar `OutputJar`. `java_resource_jar_action` in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` binary.
  - `Doit` is the one-shot write of the archive. `ExtraCombiner` registers a combiner for one entry name, and `OutputJar` owns that combiner and deletes it in its virtual destructor. `ExtraHandler` is the subclass hook for one input central-directory record, with the input jar path and an optional aux label. `path` is `options_->output_jar`. `NewEntry` is true only when that exact name is absent from `known_members_`.
  - `begins_with` and `ends_with` compare a byte span to a C string: the length is `strlen` of the pattern, a short span is false, and the span itself need not be NUL-terminated. An empty pattern matches any span.
  - The writer keeps a `FILE` plus `outpos_`, entry and duplicate counts, and a growable central-directory buffer. `WriteMetaInf` is the first output entry. Directory entries carry extra fields. Each input central-directory header is copied into that buffer with a local-header position, a normalized time, and a timestamp-fix flag. `ClasspathResource` binds a resource name to a path. Page-aligned append returns the aligned offset and size, and the other append records those as manifest attributes plus an optional build property. `CopyAppendData` copies `count` bytes from an fd offset. `WriteBytes` succeeds only when the full count is written.
  - `known_members_` maps each name to an `EntryInfo`: a combiner pointer and an input-jar index that defaults to `-1`. Built-in slots are two concatenators, a third concatenator, a manifest combiner, a property combiner, and a null combiner, plus vectors of service-handler concatenators, classpath-resource concatenators, and extra combiners. Opening, adding jars, and closing are only declared here.

### `src/tools/singlejar/output_jar_shell_test.sh`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `test_new_entries` runs `singlejar --output` with no inputs, unzips `build-data.properties`, and requires that file to be readable. The jar always writes that entry (unless `--exclude_build_data`) with central-directory version 20 and `external_attributes` 0 so unzip does not apply a Unix mode. Zel writes no jar. `java_resource_jar_action` in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` and passes `--exclude_build_data`. Zel's `build-data.properties` is only the server version stamp.
  - `test_empty_resource_file` (Bazel issue 6820) requires `singlejar --output out.jar --resources <zero-byte file>` to exit 0. The path is the entry name unless the last `:` splits `PATH:NAME` (a leading `:` is an error; a Windows drive colon is not a split). A zero-length file is accepted (POSIX maps one byte and reports size 0; Windows returns a null map) and stored as an empty entry. Zel has no writer and only appends `--resources` arguments (`path` or `path:name`) onto that toolchain spawn.

### `src/tools/singlejar/output_jar_simple_test.cc`
- Bazel lines: 1126
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not build an output jar. `java_resource_jar_action` in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` with `--normalize`, `--dont_change_compression`, `--exclude_build_data`, `--no_strip_module_info`, `--output`, and optional `--sources`, `--resources`, and `--classpath_resources`.
  - An empty run (`--output`, `--build_target //some/target`) still returns 0 and passes `zip` verification. Each central-directory record has a local header with the same name, matching DOS time and date, and matching compressed and uncompressed sizes unless the central record omits sizes. Timestamps are within five minutes of now, rounded to an even second. The first entry is `META-INF/` with the four-byte extra `FE CA 00 00` on both headers. `META-INF/MANIFEST.MF` is exactly `Manifest-Version: 1.0\r\nCreated-By: singlejar\r\n\r\n`. `build-data.properties` contains `build.target=//some/target`.
  - `--sources` of `libtest1.jar` and `libtest2.jar` keeps at least four non-directory entries, with local and central names and sizes agreeing.
  - `--java_launcher` copies the launcher in front of the jar. The first central-directory local-header offset equals the launcher’s byte size.
  - With a launcher, `--cds_archive` puts `Jsa-Offset: <page size>` in the manifest and `\ncds.archive=<path>\n` in `build-data.properties`. `--jdk_lib_modules` puts `JDK-Lib-Modules-Offset: <page size>` and `JDK-Lib-Modules-Size: <file size>`. Together, the CDS bytes sit at offset `pagesize` and the modules bytes at `pagesize * 2`.
  - `--main_class com.google.my.Main` adds `Main-Class: com.google.my.Main\r\n`. `--output_jar_creator "SingleJarTestValue 123.456"` replaces `Created-By`. `--deploy_manifest_lines property1: foo property2: bar` appends those two lines before the blank line.
  - `--extra_build_info property1=value1` and `property2=value2` appear as `\nproperty1=value1\n` and `\nproperty2=value2\n`. `--build_info_file` text is copied through as well (`property11=value11`, `property12=value12`, `property21=value21`, `property22=value22`, plus `property=value`).
  - `--resources a:res1 b:res1 c` stores `res1` as the concatenation of `a` and `b`, and stores `c` under its full path. With `--exclude_build_data`, `the/resources/res1` and `the/resources2/res2` also insert parent directories, and the entry order is `META-INF/`, `META-INF/MANIFEST.MF`, `the/`, `the/resources/`, `the/resources/res1`, `the/resources2/`, `the/resources2/res2`. A directory mapped to `the/dir` yields `META-INF/`, `META-INF/MANIFEST.MF`, `the/`, `the/dir/`.
  - `--classpath_resources` stores the file under its basename. With `--warn_duplicate_resources`, a repeated classpath path and two resources mapped to `foo` each keep the first body (`line1\nline2\n` and `resline1\nresline2\n`).
  - A registered combiner concatenates `tools/singlejar/data/extra_file1` from `libdata1.jar` and `libdata2.jar`. An `ExtraHandler` does the same for every non-directory entry under `tools/singlejar/data/`.
  - `--include_prefixes tools/singlejar/data` on `libtest1.jar` plus `libdata1.jar` keeps only `META-INF/`, `META-INF/MANIFEST.MF`, `build-data.properties`, `tools/singlejar/data/`, `extra_file1`, and `extra_file2`. Adding `--exclude_zip_entries tools/singlejar/data/extra_file1` drops `extra_file1`.
  - `--normalize` sets DOS date `15393` (2010-01-01) on every entry, time `1` (00:00:02) for names ending in `.class` and time `0` otherwise, and removes Unix-time extra fields from both headers. Inputs include a `java_library` jar, a `jar` archive, a `zip` archive, a resource, and a classpath resource.
  - `--add_missing_directories` with `--normalize` inserts one each of `a/`, `a/b/`, `a/c/`, and `c/` for `a/b/jar_testinput.txt`, `a/c/Foo.class`, `c/Foo.class`, and `c/Bar.class`, with the same timestamps and no Unix-time extras.
  - `META-INF/services/<name>`, `META-INF/spring.handlers`, and `META-INF/spring.schemas` are concatenated per name: `spi.DateProvider` is both implementations, `spi.TimeProvider` is only the first, and the spring files are `schema1\nschema2\n` and `handler1\nhandler2\n`.
  - With no compression flag, every entry is stored. `--compression` deflates files and stores directories. `--dont_change_compression` deflates created entries and `java_library` entries, stores directories, and leaves the stored `stored.jar` entry `src/tools/singlejar/output_jar.cc` stored. `--nocompress_suffixes .foo .h` stores names ending in `.foo` or `.h` and deflates `.cc` and `.bar`.
  - `--exclude_build_data` omits `build-data.properties` when no input has it, and copies an input entry (`build: foo`) when one does.
  - `--multi_release` adds `Multi-Release: true\r\n`. The same flag with `--deploy_manifest_lines Multi-Release: false` leaves neither `Multi-Release` line. `--hermetic_java_home foo/bar/java_home` adds `Hermetic-Java-Home: foo/bar/java_home\r\n`.
  - `--add_exports` and `--add_opens` merge with deploy lines: `Add-Exports: bar/com.export` then ` foo/com.export`, and the same shape for `Add-Opens`. A repeated `--add_exports foo/export` is written once. `Add-Exports: foo/export bar/export foo/export` is split, sorted, and deduped to `bar/export` then ` foo/export`.

### `src/tools/singlejar/port.h`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar portability layer. Bazel makes `off64_t` 8 bytes (`off_t` on Apple, `__int64` on Windows, `int64_t` on OpenBSD, the system type elsewhere) and fails the build if it is not.
  - On Windows, Bazel turns on POSIX CRT names, sets `F_OK` to 0 for `access` existence checks, maps `stat`/`fstat` to `_stat64`/`_fstat64` so `st_size` is 64-bit on Win64, and typedefs `ssize_t` to `ptrdiff_t`.
  - Bazel’s Windows `localtime_r` fills the caller’s `tm` through `localtime_s` and returns that pointer, or null on failure. Zel only calls Unix `localtime_r` for client, event, and exec log clocks, and those paths fall back to UTC hour/minute/second fields.

### `src/tools/singlejar/singlejar_local_main.cc`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - On Windows the process entry is `wmain` and wide arguments are converted with `blaze_util::WArgsToCArgs` before parsing; elsewhere it is `main`. Zel has no singlejar process entry point.
  - Options are parsed from the argument vector with the program name skipped. The process status is the return value of `OutputJar::Doit`.
  - `META-INF/desugar_deps` is always consumed and left out of the output. With `--check_desugar_deps`, `Java8DesugarDepsChecker` runs (`verbose` from the options; a name counts as already present when `OutputJar::NewEntry` is false). Without that flag, a `NullCombiner` drops the entry. Zel never installs either combiner. `experimental_check_desugar_deps` is only a Java build option and defaults to true.
  - `META-INF/org/apache/logging/log4j/core/config/plugins/Log4j2Plugins.dat` is always merged by `Log4J2PluginDatCombiner`, with duplicate handling taken from `options.no_duplicates`.
  - `reference.conf` is always merged by a `Concatenator` with newline insertion enabled.

### `src/tools/singlejar/singlejar_main.cc`
- Bazel lines: 46
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar process entry. On Windows Bazel uses `wmain` and converts wide argv with `blaze_util::WArgsToCArgs`; elsewhere it uses `main`. Both parse `argv[1..]`, then return `OutputJar::Doit`.
  - When `--check_desugar_deps` is set, Bazel exits with status 1 before writing a jar. `diag_errx` prints `<program>: <this file>:<line>: Desugar checking not currently supported in Bazel.` The file and line are the call site in `singlejar_main.cc`.
  - Otherwise Bazel installs a `NullCombiner` on `META-INF/desugar_deps` (those entries are discarded), a `Log4J2PluginDatCombiner` on `META-INF/org/apache/logging/log4j/core/config/plugins/Log4j2Plugins.dat` with `options.no_duplicates`, and a `Concatenator` on `reference.conf`, then builds the output jar.
  - `java_resource_jar_action` in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` binary (`--normalize`, `--dont_change_compression`, `--exclude_build_data`, `--no_strip_module_info`, `--output`, and optional `--sources` / `--resources` / `--classpath_resources`). It does not implement this entry, the desugar-deps fatal exit, or those extra combiners.

### `src/tools/singlejar/test_util.cc`
- Bazel lines: 144
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `AllocateFile` creates or truncates a mode-`0777` file and extends it to `size` zero bytes with `ftruncate`. A size of 0 only creates an empty file. An open failure returns false. A truncate failure closes the descriptor, restores `errno`, and returns false. A close failure returns false. Windows uses `_chsize_s`. Zel has no such helper.
  - `RunCommand` joins a null-terminated argument list with single spaces and no quoting, prints `Arguments: …` to stderr, and returns `system()`.
  - `LsZip` runs `unzip -v <name>` through `RunCommand` except on Apple, where it does nothing.
  - `OutputFilePath` joins the relative path onto `TEST_TMPDIR`, or `.` when that variable is unset.
  - `VerifyZip` runs `zip -Tv <path>` through `system()` with no quoting and returns that status.
  - `GetEntryContents` runs `unzip -p <zip> <entry>` through `popen` (`rb` on Windows so CR/LF is kept), appends `fgets` chunks (a NUL ends the chunk), and returns the text only when the stream reaches EOF with no error and `pclose` is 0. Otherwise it records a gtest failure and returns an empty string.
  - `CreateTextFile` writes under `OutputFilePath`, creates the parent directory with mode `0777`, and returns the path. A write failure is a gtest failure and an empty string.

### `src/tools/singlejar/test_util.h`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar test helpers. `AllocateFile` creates a file of exactly `size` bytes that read as zeroes: POSIX `open` with `O_CREAT|O_RDWR|O_TRUNC` and mode `0777`, then `ftruncate` when `size` is nonzero, returning false on open, truncate, or close failure (`perror` of the path on open). Windows uses `_sopen` (`_O_RDWR|_O_CREAT|_O_BINARY`) and `_chsize_s`, which keeps existing bytes below `size` and zero-fills only the extension.
  - `RunCommand` joins a `nullptr`-terminated vararg list with single spaces and no quoting, prints `Arguments: <line>` to stderr, and returns the raw `system` status. `LsZip` runs `unzip -v <zip>` that way except on Apple, where it does nothing. `VerifyZip` runs `zip -Tv <path>` through `system` with no quoting and returns that status (0 is success); `zip` writes its diagnostics to stdout and stderr.
  - `GetEntryContents` runs `unzip -p <zip> <entry>` with `popen` (`"rb"` on Windows so CR/LF is kept, `"r"` elsewhere). A failed `popen` records a gtest failure `Command <cmd> failed.` and returns an empty string. It appends `fgets` chunks of 1023 bytes, so a NUL inside a chunk drops the rest of that chunk. It returns the text only when the stream hits EOF with no error and `pclose` is 0; otherwise it records `Command <cmd> failed on close.` and returns an empty string.
  - `OutputFilePath` joins `relpath` onto `TEST_TMPDIR`, or onto `.` when that variable is unset. `CreateTextFile` writes under that path after creating parent directories with mode `0777`, stores the C string with mode `0644`, and returns the path. A write failure records `Cannot write <path>` and returns an empty string.
  - `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` extracts repository archives in process and does not shell out to `unzip -v`, `unzip -p`, or `zip -Tv`. `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` binary.

### `src/tools/singlejar/token_stream.h`
- Bazel lines: 324
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar `ArgTokenStream`. `java_resource_jar_action` in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only builds a spawn (shell `@%s` param file via `shell_escape` in `/workspace/zel-cursor/crates/zel-analysis/src/action.rs`) and runs the toolchain `single_jar`.
  - argv: each argument is one token. A leading `@` opens the rest of the argument as a file (`fopen`, or on Windows `AsAbsoluteWindowsPath` plus `_wfopen`). Open failure and a Windows path-conversion failure exit 1 through `diag_err` (`<program>: <path>: <strerror>`, or the path error text). Tokens read from the file are not scanned for `@`, so `@@` is a file named `@`. An empty or whitespace-only file is skipped and the following argv entries continue.
  - File tokens split on C `isspace`. `'` and `"` quote, and adjacent quoted and unquoted pieces concatenate (`hi'x'` is `hix`). Inside `'`, a backslash is literal (`'\jkl'` is `\jkl`). Inside `"`, `\"` is `"` and any other `\x` is the two characters `\` and `x` (`"\xyz"` is `\xyz`, `"\"0"` is `"0"`). `\` plus newline is deleted everywhere, including inside quotes (`cont\` plus newline plus `inue` is `continue`). `\` at EOF exits 1 with `Expected character after \, got EOF in <file>`. A missing closer exits 1 with `No closing <quote> in <file>`.
  - `MatchAndSet` is an exact token match. The bool form sets the flag and consumes the option. The string form takes the next token as the value, including a token that starts with `-`, and exits 1 with `<option> requires argument` when none remains. The vector and set forms take following tokens until one whose first character is `-` or the input ends (a set inserts). The pair form splits each of those tokens on the first comma into `(before, after)`, or `(token, "")` when there is no comma. Zero values is success. `AtEnd` becomes true only after `next` steps past the last argv slot.
  - `/workspace/zel-cursor/crates/zel-options/src/rc.rs` (`tokenize_rc_line`), `/workspace/zel-cursor/crates/zel-options/src/converters_ext.rs` (`run_under`), and `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs` (`expand_argument`) tokenize rc lines, shell prefixes, or worker flag files by different rules.

### `src/tools/singlejar/token_stream_test.cc`
- Bazel lines: 195
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar `ArgTokenStream`. `java_resource_jar_action` in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` binary.
  - A non-matching flag check returns false and leaves the current token in place. A match sets the flag and advances. After `-foo` then `-bar`, the stream is at end. `token()` is the current argument.
  - `@file` is spliced between the surrounding arguments and yields `-cmd1`, `foo`, `bar`, `abcd`, `efg`, `hix`, `\jkl`, `\xyz`, `"0`, `continue`, `x`. Quotes are removed and adjacent pieces join. A single-quoted backslash stays. In double quotes only `\"` drops the backslash. A backslash-newline is deleted, so `cont\` plus `inue` is `continue`.
  - `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` `expand_param_file_args` treats `@path` as one token per non-empty trimmed line, so `cont\` and `inue x` stay separate and quotes are not stripped.
  - On Windows the same tokens are read from an `@` file under three nested directories named 251 `a`s plus `.aaa`, 251 `b`s plus `.bbb`, and a file of 251 `c`s plus `.ccc`, opened with the absolute Windows path.
  - A string option takes exactly the next token (`--arg1` → `optval1`, `--arg3` → `optval3`). A failed match does not consume. A boolean `--arg2` between them is set.
  - A repeated option collects every following token until one that starts with `-` (`--arg1` → `value11`, `value12`; `--arg2` → `value21`, `value22`). A failed match does not consume or append.
  - A comma-pair option splits each following token on the first comma (`optval1`/`optsuff1`, `optval2`/`optsuff2`, `optvalnosuff`/empty). A failed `--foo` match does not consume.

### `src/tools/singlejar/transient_bytes.h`
- Bazel lines: 330
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar entry buffer. `java_resource_jar_action` in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar` binary.
  - `Append` copies raw bytes. The C-string overload stops at the first NUL.
  - `ReadEntryContents` uses the central-directory uncompressed size when the local header has no sizes, otherwise the local-header size, and appends that many payload bytes.
  - `DecompressEntryContents` inflates in input chunks of at most 4GiB−1. Inflate errors, and `Z_OK` while output space remains, abort with exit 2. Except on Windows, the inflated length must match the declared uncompressed size. The inflater is then reset.
  - `CompressOut` sets a CRC-32 of the uncompressed bytes. Empty input yields checksum 0, zero bytes written, and `Z_NO_COMPRESSION`. Otherwise it deflates only while the compressed length stays strictly below the original size (and below 4GiB−1 per call); if deflate is no smaller it copies the bytes uncompressed. `Z_STREAM_END` with data remaining, and other deflate errors, abort with exit 2.
  - `last_byte` aborts with exit 1 on an empty buffer or an empty last block.

### `src/tools/singlejar/transient_bytes_test.cc`
- Bazel lines: 345
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no chunked byte buffer that appends a string repeatedly and streams the concatenation back with a matching length.
  - Zel does not read stored zip entries, including an entry larger than 4GiB, and stream those bytes back unchanged.
  - Zel does not inflate deflated zip entries (511 bytes, larger than 4GiB, and 1KiB, each byte `offset & 255`) into that buffer and check the uncompressed size and contents.
  - Zel does not recompress that buffer, keeping deflate only when it is smaller, recording the zlib CRC-32, and otherwise storing the original bytes.
  - Compressing the single byte `a` is not implemented: stored method, one byte written, the next output byte left unchanged, and CRC-32 `0xE8B7BE43`.
  - Compressing an empty buffer is not implemented: stored method, zero bytes written, the output buffer left unchanged, and CRC-32 0.
  - Copying `a` out with that same CRC-32, without altering the following output byte, is not implemented.

### `src/tools/singlejar/zip64_huge_entry_test.sh`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `test_hugezipentry` writes a 4294967296-byte zero file (`dd bs=4M count=1024`), packs it with the JDK `jar -cf` as the entry `file`, runs `singlejar --output` / `--sources`, and requires `jar tvf`’s size column to equal that value. Zel has no singlejar and no such check. `java_resource_jar_action` only spawns the toolchain `single_jar` binary.
  - The JDK jar deflates the entry, and singlejar’s default is to store it, so the input is inflated and rewritten. A size `>= 0xFFFFFFFF` gets a Zip64 local extra (32-bit sizes `0xFFFFFFFF`, 64-bit original and compressed sizes). The central-directory record then writes all three Zip64 attributes (uncompressed size, compressed size, local-header offset) and sets those 32-bit fields to `0xFFFFFFFF`. `Close` writes a Zip64 end record (version-made-by `0x031E`, version-needed 45), a locator, and a classic end record of `0xFFFF` / `0xFFFFFFFF` once the central-directory offset is past 4GiB. Nothing in Zel writes that entry or those trailers.
  - Vendored ijar reads extra id `0x0001` into 64-bit sizes, stores entry sizes as `u4`, and caps estimated output at the `uint32` maximum. Its Zip64 end record is only for an entry count, directory size, or offset past the 16/32-bit limits, with version-needed 0, and it writes no per-entry Zip64 extra, so `jar tvf` cannot report 4294967296.
  - `archive.rs` only parses repository zips. It does not run `singlejar --sources` or emit a Zip64 archive of a 4GiB entry.

### `src/tools/singlejar/zip64_test.sh`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `test_65Kentries` is the only case. It writes a 256-byte zero file, splits that into 256 one-byte files in each of 256 directories, packs the tree with `jar -cf`, and runs `singlejar --output <out> --sources <in>`. `jar -tf` must then print strictly more than 65536 lines, or the test exits 1 with `Expected 65536 entries, got <n>`. Zel has no singlejar and does not run this check. Resource jars only spawn the toolchain `single_jar` binary.
  - Passing requires reading a Zip64 input and writing a Zip64 output the JDK `jar` tool can list. Nothing in Zel merges `--sources` jars or writes a Zip64 end-of-central-directory when the entry count is at least 65535. `parse_zip` in `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` only reads a Zip64 locator when the classic entry count is `0xFFFF` or the central-directory offset is `0xFFFFFFFF`.

### `src/tools/singlejar/zip_headers.h`
- Bazel lines: 662
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`
- Verdict: partial
- Differences:
  - Zel never writes zip records. This header stores little-endian signatures and fields for a local header (`0x04034b50`), a central-directory header (`0x02014b50`), an end-of-central-directory record (`0x06054b50`), a Zip64 end record (`0x06064b50`), a Zip64 locator (`0x07064b50`), and an optional data-descriptor signature (`0x08074b50`). A 32-bit size or offset is replaced by `0xFFFFFFFF` when the real value is `>= 0xFFFFFFFF`, and the 64-bit value goes in a Zip64 extra field.
  - For a central-directory Zip64 extra field (tag 1), both readers take uncompressed size, then compressed size, then local-header offset, and only for fields whose 32-bit value is `0xFFFFFFFF`. Bazel keeps the first tag-1 field and, if that field is missing, leaves the value at `0xFFFFFFFF`. On a local header the compressed size is always attribute 1 and the uncompressed size attribute 0. Zel keeps scanning, so a later tag-1 field replaces an earlier one, and a payload shorter than the 8-byte value returns `Truncated zip file`. Zel does not resolve those sizes from the local header.
  - `LH::in_zip_size` is the compressed size when the compression method is nonzero and the uncompressed size when the method is stored. Zel skips `30 + name length + extra length`, then inflates methods 8 and 9 or copies bytes, and the only length it uses is the central-directory uncompressed size. General-purpose bit 3 (sizes live in a data descriptor) is ignored.
  - The Unix-time extra field `0x5455` is absent in Zel. Bazel reads a flags byte (bit 0 modification, bit 1 access, bit 2 creation) and `(payload_size - 1) / 4` little-endian Unix timestamps. A payload shorter than one byte makes that count wrap.
  - Zel has no data-descriptor length. Bazel counts 8 bytes when the first 32-bit word is `0x08074b50` and 4 otherwise (so a CRC equal to that signature is treated as a signature), then 8 or 4 for the compressed size and 8 or 4 for the original size.
  - `ECD::ecd64_offset` always reads the 20 bytes immediately before the end-of-central-directory record and returns the locator’s 64-bit offset when its signature is `0x07064b50`, otherwise `0xFFFFFFFFFFFFFFFF`. Zel consults that locator only when the 16-bit entry count is `0xFFFF` or the 32-bit directory offset is `0xFFFFFFFF`, and only accepts it when an ECD64 signature is at the locator offset. It then replaces the entry count and directory offset from ECD64 offsets 32 and 48, leaves the 32-bit directory size in place (ECD64 offset 40), and does not read the locator disk number or total disk count.
  - Names are raw bytes of the recorded length. `file_name_is` also requires `strlen` to match, so an embedded NUL does not match. Zel decodes central-directory names with lossy UTF-8.
  - Extra-field search advances while the cursor is still below the end and will read a tag and length without a full 4-byte header remaining. Zel stops when fewer than 4 bytes remain. On write, a central-directory extra-field update skips the copy when the source is already the header’s extra-field address. A local-header update always copies.

### `src/tools/singlejar/zip_headers_test.cc`
- Bazel lines: 318
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`
- Verdict: partial
- Differences:
  - A local header whose 32-bit sizes are `0xFFFFFFFF` returns Zip64 attribute 0 as the uncompressed size (`5000000000`) and attribute 1 as the compressed size (`3000000`). With method 8, `in_zip_size` is that compressed size. `zip_entry_bytes` never reads a local Zip64 extra; it uses the central-directory uncompressed size and starts the payload at 30 plus the name and extra lengths. `build_zip` writes both sizes as `u32` and a zero extra length.
  - Unix-time extra `0x5455` is interpreted from the payload, not only the flag bits. A 9-byte local payload with flags `0x03` is two timestamps, with modification and access set and creation clear. A 5-byte central payload is one timestamp, `0x57910A85`. Nothing in Zel reads tag `0x5455`.
  - `ecd64_offset` is the locator’s little-endian `u64` whenever the 20 bytes immediately before the end-of-central-directory record have signature `0x07064b50`, including when the record’s counts and offsets are not sentinels (the test still reads `9876543210`). With no locator signature the value is `0xFFFFFFFFFFFFFFFF`. The two-byte comment `0xCA 0xFE` is stored and readable. `parse_zip` follows a locator only when the entry count is `0xFFFF` or the directory offset is `0xFFFFFFFF`, and it uses the comment length only to accept the record.
  - The Zip64 locator round-trips disk `123456` and total disks `213456`. The Zip64 end record round-trips remaining size 44, versions 56 and 754, disks 75123 and 87654, this-disk entries `9000000000`, total entries `8000000000`, directory size `19000000000`, and offset `11000000000`. `parse_zip` keeps only total entries at Zip64 offset 32 and the directory offset at offset 48, leaves the directory size as the 32-bit end record, and ignores both locator disk fields.
  - Zero-length filename or extra writes leave the next byte as the fill `0xFB`, and a Zip64 extra of payload 16 leaves the byte at `size()` unchanged. Zel allocates a new buffer and does not keep that in-place record layout or the unused local and central scalar fields (version, DOS time and date, CRC, disk, internal attributes, and the central comment length).

### `src/tools/singlejar/zlib_interface.h`
- Bazel lines: 106
- Zel counterpart: `/workspace/zel-cursor/crates/zel-util/src/deflate.rs`
- Verdict: partial
- Differences:
  - Bazel keeps a zlib stream. `Inflate` always uses `Z_SYNC_FLUSH` and returns the zlib code (`Z_OK` when the output buffer fills, `Z_STREAM_END`, `Z_BUF_ERROR`). `Deflate` takes the caller’s flush (`Z_NO_FLUSH` / `Z_FINISH`) and can continue one stream across calls, including leftover `avail_in`. Zel `deflate` / `inflate` run a whole buffer to one `BFINAL` block and return either the bytes or an error string.
  - Bazel compresses with `deflateInit2(Z_DEFAULT_COMPRESSION, Z_DEFLATED, -MAX_WBITS, 8, Z_DEFAULT_STRATEGY)` (zlib level 6, raw 15-bit window). Zel uses a custom LZ77 (chain length 64) and one fixed-Huffman block, so the compressed bytes differ.
  - `DataToInflate` replaces `next_in` / `avail_in` in place (`uint32`, so one feed is at most 4GiB−1). Callers observe `next_in`, `total_in`, `available_out`, `total_out`, and `z_stream.msg`, and `reset` is `inflateReset`. Zel returns the full output and a consumed count only after a finished stream; a truncated or non-final prefix is an error instead of partial output plus `Z_OK` / `Z_BUF_ERROR`.
  - `inflateInit2` or `deflateInit2` returning anything other than `Z_OK` calls `diag_errx(2)` and exits. Zel has no init-failure exit.
  - `Deflater` is a public `z_stream`: the caller sets `next_out` / `avail_out` and reads `avail_in`, `total_out`, and `msg`. `Deflate` only replaces the input pointer and length. Zel owns both buffers inside the function.

### `src/tools/singlejar/zlib_interface_test.cc`
- Bazel lines: 94
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No incremental raw-deflate session: finishing the 12-byte `{1,2,3,4}` pattern is expected to return stream-end.
  - No output-buffer exhaustion: a 2-byte window returns OK with no output space left, then finishing the leftover input returns stream-end.
  - No input split: the first 4 bytes with no flush return OK, and the remaining 8 with finish return stream-end.
  - No one-shot inflate of that stream that checks the output length is 12 and the bytes match the input.
  - No chunked inflate: the first 3 bytes return OK, the remainder returns stream-end, and the joined output matches the input.
  - Zel only has one-shot whole-buffer deflate and inflate, so it never resumes a compressor across a full output buffer or reports OK versus stream-end on partial calls.

### `src/tools/starlark/java/com/google/devtools/starlark/common/DocstringUtils.java`
- Bazel lines: 495
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not parse a trimmed docstring into a summary, a long description, parameter docs, a returns section, and a deprecation section.
  - Zel does not parse `Args:` or `Arguments:` parameter lines (`name (attributes): description`), hanging continuation lines, or empty parameter descriptions.
  - Zel does not parse indented `Returns:` and `Deprecated:` bodies, including the two-space indent check and empty-section errors.
  - Zel does not report a missing blank line after the summary, duplicate or out-of-order sections, body text after special sections, an inline `Returns: ` line, or a nonstandard deprecation (`deprecated:` or `DEPRECATED`).
  - Zel does not classify a docstring as single-line when the long description, parameters, and returns text are all empty.

### `src/tools/workspacelog/src/main/java/com/google/devtools/build/workspacelog/WorkspaceLogParser.java`
- Bazel lines: 112
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no CLI that parses a workspace-rules log. Bazel requires `--log_path`, rejects leftover arguments with `Unexpected options: …` and exit 1, and exits 1 with `--log_path needs to be specified.` when the path is null or empty.
  - Bazel reads length-delimited `WorkspaceEvent` protos and drops every event whose `context` is in the repeated `--exclude_rule` set (a null list excludes nothing). Reading stops when `InputStream.available()` is 0.
  - Each kept event is printed as protobuf text, then the fixed delimiter line of 57 dashes, to stdout or to `--output_path` (the file is truncated). The writer is UTF-8 and auto-flushed.
  - The only related zel text is the unused flag help for `experimental_workspace_rules_log_file`. Nothing writes or parses those protos.

### `src/tools/workspacelog/src/main/java/com/google/devtools/build/workspacelog/WorkspaceLogParserOptions.java`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no workspace-log parser options. Bazel defines `--log_path` (string, default null: path of the workspace rules log to parse), `--output_path` (string, default null: output file, documented as stdout when left empty), and repeatable `--exclude_rule` (list of strings, default null, `allowMultiple`: rules to filter out).
  - All three are logging-category options with effect tag UNKNOWN. Zel only has `--experimental_workspace_rules_log_file`, which writes delimited WorkspaceEvent protos and does not parse a log or filter rules.

### `src/tools/workspacelog/test/main/java/com/google/devtools/build/workspacelog/WorkspaceLogParserTest.java`
- Bazel lines: 127
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `ExcludingLogParser` over a delimited `WorkspaceEvent` stream, so none of these `getNext` cases exist.
  - An empty stream returns null both when the exclusion set is null and when it is non-empty.
  - An event is skipped when its `context` is in the exclusion set (match anywhere in the set), and the next call is null if nothing remains.
  - An event whose `context` is not excluded is returned once, equal to the original event (`context` and `location`), and the following call is null.
  - A mixed sequence yields only non-excluded events in input order, then null.
