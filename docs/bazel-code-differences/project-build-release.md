# Bazel vs zel code differences: project build release

Category rule: Repository-root build metadata and `scripts/` (bootstrap, CI, packaging, release, completion, and documentation tooling).

- Failure entries: 41
- Missing: 27
- Partial: 14
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `combine_distfiles.py`
- Bazel lines: 50
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No port of `main` (lines 28–46). It writes `os.path.join(os.getcwd(), sys.argv[1])` as a `zipfile.ZIP_DEFLATED` archive of `sorted(sys.argv[2:])`. `.tar` inputs go through `src.create_embedded_tools_lib.copy_tar_to_zip` (lines 24, 41–42) and `.zip` inputs through `copy_zip_to_zip` (lines 25, 43–44), both with `_normalize` stripping a leading `./` (lines 37–38). Any other suffix raises `Exception("unknown archive type \"%s\"" % input_file)` (line 46). Searches under `/workspace/zel-cursor/crates` (including `zel-bazel-tools`) for `combine_distfiles`, `copy_tar_to_zip`, `copy_zip_to_zip`, `create_embedded_tools`, and `unknown archive type` found no counterpart.
  - Python `zipfile` deflate and entry ordering are the CPython implementation. Zel has no equivalent writer, so timestamp, compression-level, and extra-field behavior cannot match.

### `combine_distfiles_to_tar.sh`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The script is absent from zel. It builds one tar of every member of the input archives (lines 19–54).
  - Output path is `${PWD}/$1` (line 25). Inputs are `${PWD}/$i` (line 35). Zel has no equivalent CLI.
  - `*.zip` is unpacked with `unzip -q` and `*.tar` with `tar xf` (lines 36–40). Other suffixes leave `UNPACK` unset and fail under `set -u`. Zel does not shell out to `unzip` or `tar` for this pack step.
  - Temp dir is `mktemp -d ${TMPDIR:-/tmp}/bazel.XXXXXXXX` with `trap rm -fr` (lines 28–30). A second `mktemp` holds the file list (`bazel-distfile-files.XXXXXXXX`, lines 50–51).
  - Only regular files are archived: `find . -type f | sort` (line 52). Directories, symlinks, and empty dirs are dropped. Order is byte-wise `sort` of the `find` paths.
  - Ownership is forced to 0. GNU tar gets `--group=0 --owner=0`; Darwin (`uname -s` = Darwin) gets `--gid=0 --uid=0` (lines 43–46). Reproducibility depends on the inputs already having canonical timestamps (comment lines 20–23); this script does not rewrite mtimes.
  - No zel code references `combine_distfiles_to_tar` or `bazel-distfile-files`.

### `compile.sh`
- Bazel lines: 84
- Zel counterpart: `scripts/package_zel.sh` (partial; it packages a Rust binary, it does not bootstrap Bazel)
- Verdict: partial
- Differences:
  - Bazel lines 1, 23, 36, 46, and 70 are a bash (`errexit` only) two-stage Java bootstrap (`scripts/bootstrap/buildenv.sh`, `scripts/bootstrap/compile.sh`, then `bazel_build`). Zel `scripts/package_zel.sh` lines 11 and 120–131 is `set -euo pipefail` plus one `cargo build --bin zel`. No `javac`, `jar`, `java`, `protoc`, `GRPC_JAVA_PLUGIN`, `unzip`, `xcodebuild`, or `xcrun`/`clang`.
  - Bazel lines 39 and 44–47 build from scratch by sourcing `scripts/bootstrap/compile.sh` unless `$BAZEL` is already executable, then lines 63–74 rebuild with that binary. Zel has no `$BAZEL` skip and no self-host stage (`package_zel.sh` lines 128–131).
  - Bazel lines 38 and 70–81 `mkdir output`, build `src:bazel_nojdk${EXE_EXT}`, require the file to exist, and `cp -f` it to `output/bazel${EXE_EXT}` (a distinct no-embedded-JDK binary). Zel `package_zel.sh` lines 17 and 139–154 writes `dist/release/{zel,bazel,bazel_nojdk}-<version>-<os>-<arch>` as three copies of the same binary. There is no separate no-JDK build.
  - Bazel lines 52–56 set an unset `EMBED_LABEL` to `$(get_last_version) (@${git_sha1:-non-git})`. An empty `EMBED_LABEL` stays empty (`${EMBED_LABEL-x}`). `scripts/bootstrap/bootstrap.sh` lines 27–29 pass `--stamp --embed_label`. Zel `package_zel.sh` lines 80–89 and 131 use `--version`, else `ZEL_EMBED_LABEL`, else `BAZEL_EMBED_LABEL`, else `.bazelversion`, else `0.0.1`, baked in as compile-time `ZEL_EMBED_LABEL`. Runtime fallback is `BAZEL_COMPAT_VERSION` `"9.3.2"` (`crates/zel-util/src/lib.rs` line 26, `crates/zel-server/src/lib.rs` lines 52–63). No git SHA, `non-git`, `MODULE.bazel` parse, or `--stamp`.
  - Bazel line 30 defaults `VERBOSE` to `no`, so `buildenv.sh` `run` prints command output only on failure. Zel `cargo build` always streams compiler output. `package_zel.sh` has no `VERBOSE`.
  - Bazel lines 32–34 `unset MSYS_NO_PATHCONV` and `MSYS2_ARG_CONV_EXCL`. Zel does not.
  - Bazel lines 58–61, on darwin when `xcodebuild -showsdks` shows `iphonesimulator`, append `--define IPHONE_SDK=1` to `EXTRA_BAZEL_ARGS`. Zel has no counterpart.
  - Bazel lines 68–74 build with `--action_env=PATH --host_platform=@platforms//host --platforms=@platforms//host`, plus `scripts/bootstrap/bootstrap.sh` lines 23 and 32–58 (`--bazelrc=/dev/null`, `--spawn_strategy=standalone`, `--compilation_mode=opt`, Java language/runtime default 21, `--lockfile_mode=update`, `--enable_bzlmod`, worker/`javacopt`, repository caches). Zel `package_zel.sh` lines 19 and 120–126 pass only `--bin zel`, `--release` unless `--debug`/`--profile`, and optional `--target`. No JDK pin and no Bazel rc isolation.
  - Bazel line 36 sources `buildenv.sh`, which requires a JDK (`JAVA_HOME`, default `JAVA_VERSION=21`, failure text `Could not find JAVA_HOME, please ensure a JDK (version 21+) is installed.`). The scratch stage also assembles an `@bazel_tools` install archive and a `libblaze.jar` run with `java -jar`. Zel’s build does not need a JDK. Embedded tools are `include_bytes!` tables from `resources/` and `builtins/` in `crates/zel-bazel-tools/build.rs` lines 1–39.
  - Bazel failures use `fail` (`buildenv.sh` lines 65–72): blank line, `ERROR: …` on stderr, exit status of the failed command or 1. Messages include `Could not build Bazel`, `Could not find freshly built Bazel binary at '…'`, and `Could not copy '…'`. A phase file is dumped on exit. Zel `package_zel.sh` lines 73–75 print `Unknown option: $1` and exit 1. A missing binary is a `cargo`/`cp` error under `set -u`/`pipefail`, not those strings.
  - Bazel lines 45 and 65–67 and 83–84 print `Building Bazel from scratch`, `Building Bazel with Bazel`, a `.` phase log, then `Build successful! Binary is here: ${BAZEL}`. Zel prints `+++ Building zel (version=…)` and `+++ Packaged Zel & Bazelisk release artifacts` and writes `.sha256` sidecars. Bazel `compile.sh` writes no checksums.
  - Bazel line 25 `cd`s to the script directory (repo root) and line 38 creates `output/`. Zel builds from the workspace root into `dist/release` (overridable with `--output-dir`). Bazel `compile.sh` accepts no CLI flags.
  - Windows: Bazel `EXE_EXT=.exe` comes from `buildenv.sh` when `uname` is `msys`/`mingw`/`cygwin`, and the target label itself is `src:bazel_nojdk.exe`. That script also rewrites `TMPDIR` with `cygpath`. The scratch stage builds `windows_jni.dll`. Zel adds `.exe` when the detected or `--os` value is `windows`, and accepts `--os`, `--arch`, and `--target`. It does not build JNI.

### `distdir.bzl`
- Bazel lines: 100
- Zel counterpart: none (nearest related code is `crates/zel-bzlmod/src/download.rs`, which only checks a distdir during fetch)
- Verdict: missing
- Differences:
  - Lines 37–99 define the `repo_cache_tar` repository rule (loaded by Bazel’s `extensions.bzl` as `bootstrap_repo_cache` / `bazel_tools_repo_cache`). This file does not implement `--distdir` lookup itself. Zel has no rule or embedded `.bzl` with this name. `download.rs` distdir basename search (around lines 991–1011 and 1378–1397) and cache layout `content_addressable/<algo>/<hex>/file` only consult a distdir while fetching. They do not build this cache tarball.
  - Lines 16 and 45–47 load and call `parse_http_artifacts` / `parse_registry_files` from `//src/tools/bzlmod:utils.bzl`. `repos` selects lockfile `http_archive` / `http_file` / `http_jar` artifacts. `module_files` is forwarded, but `parse_registry_files` ignores it and returns every `registryFileHashes` entry. Zel has neither parser.
  - Lines 49–78 prefetch those artifacts into a repository-cache tree. `"integrity" in artifact` and `"sha256" in artifact` test key presence, so a present key whose value is `None` still enters the branch. A repeated integrity reuses `integrity_to_sha256` and `continue`s (57–59). Otherwise it `ctx.delete("./tempfile")` (61–63, so a `--experimental_repository_cache_hardlinks` hardlink can be replaced) and `ctx.download`s to `./tempfile` with `integrity` to learn `sha256` (64), then downloads again to `content_addressable/sha256/%s/file` (73–74). A repeated sha256 `continue`s (70–71). If neither key is present it `fail`s with `Could not find integrity or sha256 hash for artifact %s` (78). Zel has no equivalent prefetch, dedup, double download, hardlink delete, or that error string.
  - Lines 18–34 and 80–87 always write `README.md` (one bullet per first-seen URL and SHA256, in `http_artifacts + registry_files` order) and a `BUILD` that loads `@rules_pkg//pkg:tar.bzl` `pkg_tar`, with public `filegroup` `files` and public `pkg_tar` `archives` (`strip_prefix = "external/" + ctx.attr.name`, `package_dir = dirname`). Zel’s `pkg_tar` (`crates/zel-bazel-tools/resources/tools/build_defs/pkg/pkg.bzl`) is not driven by a generator like this.
  - Lines 90–99: attrs are `lockfile` (default `Label("//:MODULE.bazel.lock")`), `dirname` (default `"repository_cache"`), `repos` (`string_list`), and `module_files` (`label_list`). The `repository_rule` sets no `local`, `environ`, or `doc`. Zel does not expose this rule or these defaults.
  - This file never indexes or slices strings, so Starlark code-point indexing versus Java UTF-16 does not apply. `integrity_to_sha256` and `seen_sha256` are only keyed, not iterated. README lines and BUILD `srcs` follow the list order of `http_artifacts + registry_files` (line 53), which is the helpers’ JSON-object key order.

### `extensions.bzl`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Line 56 defines one extension, `bazel_build_deps` (`module_extension(implementation = _bazel_build_deps)` only). No `tag_classes`, `doc`, `environ`, `os_dependent`, `arch_dependent`, or `facts_version`. Zel’s generic `module_extension()` is in `crates/zel-bzlmod/src/rt.rs`. It does not define `bazel_build_deps`. This file has no tags.
  - Line 28 `embedded_jdk_repositories()` registers `openjdk_linux_vanilla`, `openjdk_linux_aarch64_vanilla`, `openjdk_linux_ppc64le_vanilla`, `openjdk_linux_riscv64_vanilla`, `openjdk_linux_s390x_vanilla`, `openjdk_macos_x86_64_vanilla`, `openjdk_macos_aarch64_vanilla`, `openjdk_win_vanilla`, `openjdk_win_arm64_vanilla`, `openjdk_win_arm64_jmods`. Absent in zel.
  - Line 29 `debian_deps()` registers `debian_java_deps`, `debian_cc_deps`, `debian_proto_deps`, `debian_bin_deps`. Absent in zel.
  - Lines 30–39 `repo_cache_tar(name = "bootstrap_repo_cache", repos = DIST_ARCHIVE_REPOS, dirname = "derived/repository_cache", module_files = ["//:MODULE.bazel", "//third_party/remoteapis:MODULE.bazel", "//src:MODULE.tools"])`. `lockfile` stays the rule default `//:MODULE.bazel.lock` (`dirname` default is `repository_cache`). Absent in zel. Zel’s `RepositoryCache` is a download cache, not this tar repo.
  - Line 27 `ctx.path(Label("//:MODULE.bazel"))` is only a dependency edge so `bootstrap_repo_cache` reruns when `MODULE.bazel` changes. Zel has `module_ctx.path` and does not use it for this repo.
  - Lines 40–41 `bazel_tools_repo_cache`: `repos` from `parse_bazel_module_repos` of `//src/test/tools/bzlmod:MODULE.bazel.lock`, `lockfile` set to that lockfile, `dirname` and `module_files` left default. Absent in zel.
  - Lines 42–53 `graalvm_repository`: `graalvm_ce` (`distribution = "ce"`, `java_version = "21"`, `version = "21.0.2"`) and `graalvm_oracle` (`distribution = "oracle"`, same Java version). Absent in zel. `rules_graalvm` 0.11.1 is not in either tree. `bazel/MODULE.bazel` `use_repo` consumes `graalvm_ce_toolchains` and `graalvm_oracle`, not the name `graalvm_ce`.
  - Line 54 returns `ctx.extension_metadata(reproducible = True)`. Zel can record that flag. Nothing here calls it for this extension’s counterpart, because the extension is absent.
  - This file has no `fail()`, no string indexing, and no dict iteration. Dedupe order for `bazel_tools_repo_cache` is `dict` insertion order inside `parse_bazel_module_repos` (`src/tools/bzlmod/utils.bzl`), not in this file.
  - `crates/zel-bazel-tools/resources/tools/test/extensions.bzl` is Bazel’s `tools/test/extensions.bzl` (`remote_coverage_tools`), not this file.

### `scripts/bash_completion_test.sh`
- Bazel lines: 812
- Zel counterpart: `none (the completer script is copied; this test is not)`
- Verdict: missing
- Differences:
  - The script drives tab completion through an interactive `bash --norc -i`, not by calling the completer alone. `COMMAND_ALIASES` defaults to `bazel`. Each alias is redefined as `echo <name>'`, so every expected line is prefixed with that name (`bazel build `, `bazel query //video/streamer2:task_lib `). The sourced script is `io_bazel/scripts/bazel-complete.bash`, and the unit helpers call `_bazel__expand_rules_in_package`, `_bazel__expand_package_name`, `_bazel__expand_target_pattern`, and `_bazel__complete_pattern`. There is no `zel` name.
  - Subcommand words: `bui` → `build `, `hel` → `help `, `shut` → `shutdown `. Startup: `--hos` → `--host_jvm_`, `--host_jvm_a` → `--host_jvm_args=`. Build and run flags: `--keep_g` → `--keep_going `, `--expe` → `--experimental_`, `--test_stra` → `--test_strategy=`. `help --cros` stays unexpanded. Query: `--out` → `--output` (no trailing space) plus labels `task_lib`. Tristate: `--nocache_test_result` → `--nocache_test_results `.
  - Packages and targets under `video/streamer2`: directories gain a trailing `/` until a `BUILD` file exists, then the slash is dropped (`testing`, `stuff`). Absolute `//video/...` still completes from a subdirectory. A relative `video/...` from inside `video/` does not. Unique labels finish with a space (`:task_lib `, `:with_special+_,=-.@~chars `, `:stuff `, `:checks/thingy `, `:foo `, `dash:mia-bid-multiplier-mixer-module ` under `LC_ALL=en_US.UTF-8`). An ambiguous prefix stays open (`:token_bucket` with no space). Commented-out rules are not completed. `test` completes only `token_bucket_test`. A single test match does not add `:all`, and several matches do (`:all `). `run` completes only `token_bucket_binary`, `JavaBinary`, `pybin`, and `AnotherJavaBinary`. `BAZEL_COMPLETION_ALLOW_TESTS_FOR_RUN=true` or `1` also offers tests. `false`, `0`, `junk`, and empty stay binaries-only. After `run :target --`, completion switches to filenames (`video/`, `BUILD `, `names/`), including `--arg=`.
  - `help` completes `query`, `build`, `shutdown`, `startup_options`. `info` completes `committed-heap-size`, the `install_` prefix, and `--show_make_env`. Direct completer checks also expect `info-key` `install_base`, command `clean`, alternation `{clean,add}` / `command|{abc,def}`, `label` versus `label-test` versus `label-bin`, and a `path` listing of `BUILD`, `names/`, `stuff/`, and `testing/` (`stuff/` omitted when `PACKAGE_PATH_PREFIX` is set).
  - Exit codes in this file: `1` if runfiles or `testenv.sh` cannot be sourced. `cd video || exit` propagates `cd`'s status. `assert_equals` returns `0` on an exact string match and `1` after `fail` on a mismatch. `assert_expansion_error_not_contains` requires stderr for `test video/streamer2:match` not to match `syntax error`. `run_suite` exits `0` when every case passes and `1` otherwise. `test_workspace_boundary` and `test_complete_root_package` return immediately unless the alias list contains the word `bazel`. Those cases expect `WORKSPACE`, `WORKSPACE.bazel`, `MODULE.bazel`, or `REPO.bazel` to make `//s` complete to `//some/`, and a root `BUILD` to make `//` complete to `//:`.
  - Zel has no `scripts/bash_completion_test.sh` and no shell test of these expansions. `crates/zel-cmdline` has no completion code. The bash script Bazel generates is copied to `crates/zel-server/resources/help/bazel-complete-header.bash` and `bazel-complete-template.bash` (45 and 810 lines) and is printed by `help completion bash`. It still sets `BAZEL=bazel` and `IBAZEL=ibazel` and runs `complete -F _bazel__complete` only for those names, so a `zel` argv is not completed. Variables stay `BAZEL_COMMAND_*`. `crates/zel-client` only checks `help completion`: stdout starts with `BAZEL_COMMAND_LIST="`, the bash form contains `BAZEL_COMMAND_BUILD_FLAGS="`, hidden commands such as `config` are listed, and `help completion zsh` exits `2`.

### `scripts/bazel-dev.sh`
- Bazel lines: 71
- Zel counterpart: `scripts/zel_dev.sh`
- Verdict: partial
- Differences:
  - Neither script sets `TEST_TMPDIR`, `JAVA_HOME`, or a bazelrc/`BAZELRC` variable. `bazel-dev.sh` only reads `BAZEL_REPO` (assigned, never used), `BAZEL_DIR` (default: repo root, two directories above the script), and `BAZEL_BINARY` (default: `which bazel`).
  - Bazel rebuilds `$BAZEL_DIR/bazel-bin/src/bazel-dev` with `$BAZEL_BINARY build //src:bazel-dev` when that binary is not executable or when the current directory is not `BAZEL_DIR`, then `exec`s it with the original arguments unchanged. Zel does not build: it uses `$ZEL_BIN` if set, otherwise the first executable among `target/release/zel`, `target/debug/zel`, `/tmp/zel-target-fix-cmds4/release/zel`, and `/tmp/zel-target-fix-cmds4/debug/zel`, and otherwise the `zel` on `PATH`.
  - Unless `--registry` is already on the command line, `zel_dev.sh` appends `--registry=file:///tmp/zel-registry` after creating that directory with `scripts/make_vendored_registry.py`. Running the `zel` binary directly skips that registry injection and the fallback path search.

### `scripts/bazel-bisect.sh`
- Bazel lines: 101
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no first-party git-bisect helper. The only hit, `third_party/bcr/protobuf/33.4/upb/wire/decode_fast/bisect.sh`, is a vendored upb fasttable numeric bisect, not a Bazel/zel source bisect.
  - Usage: `bazel-bisect.sh GOOD_COMMIT BAD_COMMIT [<bazel arguments>...]`. It clones `https://github.com/bazelbuild/bazel` (override `BAZEL_REPO`) into `$HOME/os-bazel-bisect` (override `BAZEL_DIR`), `git fetch --tags`, `git checkout master`, then `git bisect start`, `git bisect good`, `git bisect bad`, `git bisect run /tmp/bisect.sh`, and `git bisect reset`. The outer script exits with the `git bisect run` status.
  - Each step builds with `"$BAZEL_BINARY" build //src:bazel` (`BAZEL_BINARY` defaults to `$(which bazel)`), copies `bazel-bin/src/bazel` to `/tmp/bazel.bisect`, optionally runs `"$TMP_BIN" clean` when `BAZEL_BISECT_WITH_CLEAN=y`, then runs `"$TMP_BIN" $BAZEL_ARGUMENTS` in the original cwd, then `"$TMP_BIN" shutdown`. There is no hardcoded test target and no `--version` flag.
  - Exit convention matches `git bisect run`: the user command’s status is preserved (`0` = good, `1`–`124` and `126`–`127` = bad, `125` = skip, `128`–`255` = abort). A failed `//src:bazel` bootstrap exits `1` (counted bad, not skipped). `clean` and `shutdown` failures are ignored (`|| true`).
  - Hardcoded Bazel names: binary `bazel`, target `//src:bazel`, output `bazel-bin/src/bazel`, temp `/tmp/bazel.bisect`, repo `bazelbuild/bazel`, branch `master`. A zel port would need `zel`, target `//crates:zel` (`rust_binary` in `crates/BUILD.bazel`), and output `bazel-bin/crates/zel`. The script never passes a version flag. It execs the freshly built binary directly, so `USE_BAZEL_VERSION` / `.bazelversion` (and zel’s `USE_ZEL_VERSION`) do not apply inside the loop.

### `repositories.bzl`
- Bazel lines: 205
- Zel counterpart: none (versions checked against `third_party/bcr`, `MODULE.bazel`, and `crates/zel-bzlmod`)
- Verdict: partial
- Differences:
  - Lines 26–72 name distdir repos only (no URL, integrity, version, `strip_prefix`, or `patches`). Zel has no copy of that list. Versions below are the Bazel 9.3.2 lockfile `source.json` selections for those names. No `local_repository` in this file. No `patches` on any fetch. List order is not a zel difference.
  - Same version vendored as an extracted tree (no `source.json`, so archive integrity and `strip_prefix` are not recorded): `abseil-cpp` 20250814.1 (line 28), `apple_support` 2.5.2 (29), `bazel_features` 1.42.1 (30), `bazel_skylib` 1.8.2 (31), `platforms` 1.0.0 (42), `protobuf` 33.4 (43), `rules_cc` 0.2.17 (47), `rules_java` 9.1.0 (51), `rules_kotlin` 1.9.6 (53), `rules_license` 1.0.0 (54), `rules_proto` 7.1.0 (57), `rules_python` 1.7.0 (58), `rules_shell` 0.6.1 (59).
  - Different version: `re2` 2025-11-05.bcr.1 vs zel 2024-07-02.bcr.1 (45); `rules_apple` 4.3.1 vs 4.1.0 (46); `rules_fuzzing` 0.6.0 omitted, `bcr_modules` has only 0.5.2 (48); `rules_jvm_external` 6.6 vs 6.7 (52); `rules_pkg` 1.1.0 vs 1.0.1 (56); `rules_swift` 3.3.0 vs 3.1.2 (60); `stardoc` 0.8.0 vs 0.7.2 (61); `zlib` 1.3.1.bcr.7 vs 1.3.1.bcr.5 (64).
  - Omitted (no tree under `third_party`): `blake3` 1.8.2 (32), `c-ares` 1.34.5.bcr.2 (33), `envoy_api` 0.0.0-20251105-4a2b9a3 (34), `googleapis` 0.0.0-20250604-de157ca3 (35), `googleapis-grpc-java` 1.0.0 (36), `googleapis-java` 1.0.0 (37), `googleapis-rules-registry` 1.0.0 (38), `grpc` 1.76.0.bcr.1 (39), `grpc-java` 1.71.0 (40), `opencensus-cpp` 0.0.0-20230502-50eb5de.bcr.2 (41), `protoc-gen-validate` 1.2.1.bcr.1 (44), `rules_go` 0.59.0 (49), `rules_graalvm` 0.11.1 (50), `rules_perl` 0.2.4 (55), `with_cfg.bzl` 0.13.0 (62), `xds` 0.0.0-20240423-555b57e (63), `zstd-jni` 1.5.6-9 (65).
  - Lines 82–149: ten `http_file` JDKs omitted (no matching URL or integrity in zel). No `strip_prefix` or `patches`. `openjdk_linux_vanilla` `sha256-8XUtAFG2yiM2Jd2ywYyRcO2+VcXuZRW/79jqAZfuHCA=` zulu25.32.17 linux_x64 (82–87); aarch64 `sha256-9ylaux3ssvbg6p92CnCRfy1HNW20Nm1hXHq5sBw8aGY=` (88–93); ppc64le Temurin 25.0.2+10 `sha256-smK3NbIVFzADdm2jZYjV9xfc6toChttBtDn5P7KtpGg=` (94–99); riscv64 `sha256-FoEZ5PujUPTms8qSRQorkKhQK4miNaBEFemt+fXTFk4=` (100–105); s390x `sha256-FeXLytzz1DYjwxuCUGPNwoF7nxuoQLUdxu9w5dM8hOM=` (106–111); macOS x64 `sha256-/iI0bBkpIMW4SyP0qiTj3rsLuU/EKAFvfCssd5DIJ4A=` (112–117); macOS aarch64 `sha256-U3rHT6HKLE3Y9gY93t4BOK5KiW8Si/4QtCin3MSqkp8=` (118–123); Windows x64 `sha256-kLy7y+L7euxDzo/S76Ak/MSPALvc16drWPkS1rl+bVY=` (124–129); Windows ARM64 vanilla Temurin 25.0.3+1-ea `sha256-zhWbTuPBCc1RNXrPhCZBQsHdsOGHjSBGbqxQsBU0cVQ=` (130–139); Windows ARM64 jmods `sha256-2rjwZCoUIYD7L9nLwLJindsYPkDMvpI4km5a9UlxFtg=` (144–149).
  - Lines 152–157: `http_file` `async_profiler` v4.1 jar omitted, integrity `sha256-d2VI3O7jJpa197ArxjzAixnb/nTciR6X/j4p4H+qeMw=`.
  - Lines 170–179: `http_archive` `async_profiler_linux_arm64` omitted. integrity `sha256-0Mucl8OAZytiXAblo+1XjpkPRnTGqui1JJ9YTEyaxQ4=`, `strip_prefix` `async-profiler-4.1-linux-arm64/lib`.
  - Lines 181–190: `http_archive` `async_profiler_linux_x64` omitted. integrity `sha256-OxOjigBj9pcNmFo3ndrtkbzzfiOaHqRh0J6s9inz3eE=`, `strip_prefix` `async-profiler-4.1-linux-x64/lib`.
  - Lines 192–201: `http_archive` `async_profiler_macos` omitted. integrity `sha256-xfsFjiEiguk4SiYDGgURn183UMdVsrX7bQimtngD6tA=`, `strip_prefix` `async-profiler-4.1-macos/lib`.
  - `MODULE.bazel` does not declare these fetches. Its own pins for overlapping names are `bazel_skylib` 1.9.0, `platforms` 1.1.0, `rules_shell` 0.8.0, `apple_support` 2.8.4, `rules_cc` 0.2.25. `rules_license` 1.0.0 matches. Those versions are not the vendored `third_party/bcr` trees.

### `scripts/bazel_lockfile_merge_test.sh`
- Bazel lines: 469
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The test drives `scripts/bazel-lockfile-merge.jq` as a git-merge-style driver (`jq -s` on base, left, right, then overwrite left). Zel has no merge driver under `crates/zel-bzlmod` or `scripts`. `crates/zel-bzlmod/src/lockfile.rs` only reads and writes a version-28 JSON lockfile.
  - Nested maps are a shallow union. Values are uniquified in input order (base first), then last unique value wins, so a side that still equals the base does not clobber the other side. The synthetic case keeps left's `repo1: new_args` over right's `old_args`, takes right's changed `bzlTransitiveDigest` when left omits that extension, and unions yanked versions and per-factor extension entries (`aarch64` and `amd64`).
  - The merger never emits a conflict message and is expected to exit 0 in every case, including version skew. The harness only fails the test on a non-zero `jq`/`sh` status (`merge failed`) or a `diff` mismatch (`output differs`).
  - Only inputs whose `lockFileVersion` equals the maximum are merged. A lower version (the v9 right file) is dropped and the higher-version file is kept byte-for-byte. If the max-version set cannot be merged (the v7/v8/v9 case, which lacks the v10+ maps), the script returns the first file unchanged, still with exit 0.
  - `facts` are merged the same way, but per extension only entries at the highest `factsVersions` value survive (left's version 1 `old_key` is dropped; right's version 2 `new_key` is kept). The output `factsVersions` map keeps only non-zero versions. Empty `facts` and `factsVersions` objects are preserved. If no input has those keys, they are omitted.
  - Top-level key order is fixed: `lockFileVersion`, `registryFileHashes`, `selectedYankedVersions`, `moduleExtensions`, then `facts`, then `factsVersions`. Nested keys are sorted lexicographically after splitting on `:`, so `//:rbe_extensions.bzl%…` precedes `//python/extensions:python.bzl` which precedes `…/private:internal_deps.bzl`, and `os:linux,arch:aarch64` precedes `os:linux,arch:amd64`. Fact ids and keys sort the same way (`bar`, `baz`, `foo`; `cbor`, `json`, `xml`).
  - The lockfile shape is jq-pretty JSON only. The identity merge must match `src/test/tools/bzlmod/MODULE.bazel.lock` byte for byte. The test never uses a Starlark lockfile.
  - Zel's only merge-related behavior is on a failed parse: if the text contains `<<<<<<<` or `>>>>>>>`, the error adds "This looks like a merge conflict. See https://bazel.build/external/lockfile#merge-conflicts for advice." That does not merge. Its writer uses `BTreeMap` string order (not split on `:`) and always emits `facts` and `factsVersions`.

### `scripts/bootstrap/bootstrap.sh`
- Bazel lines: 86
- Zel counterpart: `scripts/package_zel.sh` (no `scripts/bootstrap/` in the zel tree)
- Verdict: partial
- Differences:
  - Lines 17–21 and 62–82: sourced Bazel driver (`BAZEL`, `BAZELRC`, `run_bazel_jar` or `${BAZEL} … build`). Zel is a standalone `cargo build` packager (`package_zel.sh` lines 11 and 120–131).
  - Lines 23–30: non-empty `EMBED_LABEL` becomes `--stamp --embed_label`. Zel stamps with env `ZEL_EMBED_LABEL` / `BAZEL_EMBED_LABEL` into `cargo` (`package_zel.sh` lines 80–90 and 131), read by `option_env!` in `crates/zel-server/src/lib.rs` lines 52–63. No `--stamp`.
  - Line 25: `SOURCE_DATE_EPOCH` is defaulted and unused. Absent from `package_zel.sh`.
  - Line 32 and lines 50–55, 68, 76: JDK 21 (`JAVA_VERSION`) for runtime, language, and tool javac, plus `--javacopt=-proc:full` and `--javacopt="-g"`. Zel has no JDK pin. The toolchain is rustc/cargo.
  - Lines 36–40: `--spawn_strategy=standalone`, `--nojava_header_compilation`, `--strategy=Javac=worker --worker_quit_after_build`, `--experimental_java_classpath=off`, `--compilation_mode=opt`. Zel does not set spawn or Javac strategy here. Release vs dev is `cargo --release` (`package_zel.sh` lines 19, 65–67, 121–123).
  - Lines 41–49 and 56–57: Bazel repo/bzlmod bootstrap (`repository_cache`, empty `repo_contents_cache`, HTTP canonical-id repo env, `//scripts/bootstrap:all` and `@rules_python` extra toolchains, `--enable_bzlmod`, lockfile update, Maven `--override_repository`, `--define=protobuf_allow_msvc=true`). None of this is in `package_zel.sh`. Line 60 copies `BUILD.bootstrap` (Java 21 `local_jdk` toolchain) over `scripts/bootstrap/BUILD`. Zel has no such tree.
  - Protoc: none in this file. Java stub generation and the “specify PROTOC / download a dist archive” failure live in `scripts/bootstrap/compile.sh`, which the root `compile.sh` sources before this file. `package_zel.sh` never calls protoc.
  - Embedded tools: line 17 only re-invokes Bazel to build them. Root `compile.sh` lines 70–80 build `src:bazel_nojdk`. `package_zel.sh` lines 147–154 copy one Rust binary to `zel-…`, `bazel-…`, and `bazel_nojdk-…` (same bytes; the nojdk name is not a JDK-stripped Java install base).
  - This script has no exit messages of its own. Line 85 swallows `info bazel-bin` failure and prints `bazel-bin`. Zel exits 1 on an unknown option (`package_zel.sh` lines 74–75).
  - This script assumes a Java Bazel (`libblaze.jar` via `run_bazel_jar`, or `$BAZEL`) and a JDK 21 host/tool javac worker. Zel replaces that stage with `cargo build`. The version label is a Rust env var, not Bazel stamping.

### `scripts/bootstrap/buildenv.sh`
- Bazel lines: 376
- Zel counterpart: `scripts/package_zel.sh` (version constant `crates/zel-util/src/lib.rs`)
- Verdict: partial
- Differences:
  - Lines 35–49 and 82–116 detect `msys*|mingw*|cygwin*` (remap `PLATFORM` to `windows`, `PATHSEP=";"`), plus `linux`, `darwin`, `freebsd`, and `openbsd`. Zel lines 92–102 only map `linux*`, `darwin*`, and `msys*|mingw*|cygwin*|windows*` to a Bazelisk OS name, and pass any other `uname` through. Zel lines 104–113 also map arch (`x86_64`/`amd64`, `aarch64`/`arm64`). Bazel does not.
  - Line 80 defaults `JAVA_VERSION` to 21. `JAVA_HOME` defaults: Linux line 88 (`readlink` of `javac`), FreeBSD line 93 (`/usr/local/openjdk11`), OpenBSD line 98 (`/usr/local/jdk-11`), Darwin lines 102–104 (`/usr/libexec/java_home -v ${JAVA_VERSION}+`), Windows lines 113–115 (latest `C:/Program Files/Java/jdk*`). `get_java_version` is lines 297–311. Zel packaging has no `JAVA_HOME` or `JAVA_VERSION`.
  - Lines 45–48 set `TMPDIR` from `TMPDIR`/`TMP`/`TEMP` on Windows. Lines 54–58 use `${BAZEL_WRKDIR}/tmp` when set. Lines 124–125 rewrite it with `cygpath -ml` (default `cygpath -m` of Temp). Lines 173–183 make `bazel_XXXXXXXX` under `TMPDIR`. Zel packaging does not touch `TMPDIR` or `cygpath`.
  - `EXE_EXT`: Bazel lines 118–121 is `.exe` only after the Windows remap. Zel lines 115–118 is the same `.exe` when `BAZELISK_OS` is `windows`, and it is appended to `zel-`, `bazel-`, and `bazel_nojdk-` artifact names (lines 147–149).
  - `fail()` (lines 65–73) keeps `$?` unless it is 0 (then 1), prints a blank line, then `ERROR: $*` on stderr, and exits with that code. Zel has no `fail()`. Unknown options are `Unknown option: $1` and `exit 1` (lines 74–75). Bazel is `set -o errexit` (line 19). Zel is `set -euo pipefail` (line 11).
  - Phase file: Bazel lines 185–193, 223–231, 237–250 (`${OUTPUT_DIR}/phase`, dot progress, dump on exit, leaf/`INFO` steps). Zel progress is the `+++ Building` / `+++ Packaged` echoes (lines 128, 176–179).
  - `git_sha1` (lines 253–257) is `git rev-parse --short HEAD` when `git` is executable and `.git` exists. `git_date` is lines 259–263. `get_last_version` (lines 267–282) takes the first `version = "..."` in `MODULE.bazel` (else `unknown`) and, when a commit date exists, appends today's `date +%Y-%m-%d` (the commit date is overwritten). Zel lines 80–89 set the package version from `--version`, else `ZEL_EMBED_LABEL`, else `BAZEL_EMBED_LABEL`, else the first line of `.bazelversion`, else `0.0.1`, and stamp the binary with `ZEL_EMBED_LABEL` (line 131). `BAZEL_COMPAT_VERSION` is the fixed string `"9.3.2"` (`zel-util` `lib.rs` line 26), not a git SHA or `MODULE.bazel` plus date.

### `scripts/bootstrap/compile.sh`
- Bazel lines: 424
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Lines 19–30 build the javac classpath from `derived/jars` or `/usr/share` jars listed in `tools/distributions/${DISTRIBUTION}/${DISTRIBUTION}_java.BUILD`, plus sorted `derived/maven` jars, dropping `JavaBuilder` and `netty-tcnative`. Zel has no jar classpath and no JDK. `scripts/package_zel.sh` lines 120–132 run `cargo build --bin zel`.
  - Lines 26 and 204–247 compile `PROTO_FILES` with `protoc --java_out` and `GRPC_JAVA_PLUGIN` (`--grpc_out`) into `${OUTPUT_DIR}/src`, or copy `derived/src/java` when it already exists. Zel embeds `.proto` text with `include_str!` in `crates/zel-proto/src/embedded.rs` and uses hand-written wire types in `crates/zel-proto/src/lib.rs`. It does not run protoc or a gRPC Java plugin to generate sources.
  - Lines 32–43 and 91–149 run javac on `src/main/java`, singlejar, Starlark Java, runfiles, and generated sources, excluding test and docgen files. Flags are `-source`/`-target` `$JAVA_VERSION`, UTF-8, `--add-exports` for `jdk.internal.misc` and `jdk.internal.vm`, `-proc:full` (AutoValue/AutoService), and optional `BAZEL_JAVAC_OPTS`. Zel has no javac step.
  - Lines 64–67 and 181–223 fail when javac is older than `$JAVA_VERSION`, or when `PROTOC` / `GRPC_JAVA_PLUGIN` are missing, and print `HOW_TO_BOOTSTRAP` (dist archive or `bazel build //src:bazel`). Zel has no equivalent bootstrap error text.
  - Lines 71–88 and 151–154 unzip every library jar into the class directory and, on Windows, reject zip entries that would exceed MAX_PATH 259. Zel does not unpack jars.
  - Lines 157–179, 324–326, and 366 write deploy jar `libblaze.jar` with `Main-Class: com.google.devtools.build.lib.bazel.Bazel`, keep only `META-INF/services`, and copy the jar into the install base. Zel’s server is the cargo binary, not a jar.
  - Lines 257–321 assemble `@bazel_tools` in the install base by linking `src/`, `third_party/`, and `tools/**` `BUILD.tools` into `embedded_tools`. Zel bakes that tree into the binary with `include_bytes!` from `crates/zel-bazel-tools/build.rs` (`FILES` / `BUILTINS`) and materializes it at runtime.
  - Lines 332–363 build `windows_jni.dll` only on Windows via `src/main/native/windows/build_windows_jni.sh` and set `-Djava.library.path`. Other platforms skip JNI. `crates/zel-ffi-bridge/src/native.rs` is `#![cfg(unix)]` and documents Windows as not implemented. There is no `windows_jni.dll`.
  - Lines 368–374 compile `xcode-locator` with `xcrun clang` on Darwin and otherwise copy `xcode_locator_stub.sh`. `package_zel.sh` does not do this.
  - Lines 382–424 launch the first server with `java -jar libblaze.jar` (`--batch`, `--install_base`, `--output_base`, ISO-8859-1, `--add-opens`, client env). Zel does not start a JVM.
  - Packaging only: `scripts/package_zel.sh` lines 120–154 copy that one cargo binary to `zel-…`, `bazel-…`, and `bazel_nojdk-…`. That corresponds to copying `libblaze.jar` into the archive (lines 328–366), not to the Java bootstrap. `bazel_nojdk` is the same binary again. Zel never ships a JDK.

### `scripts/ci/build_status_command.sh`
- Bazel lines: 40
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel prints only non-`STABLE_` keys, so they are volatile: `RELEASE_GIT_HASH`, `RELEASE_COMMIT_URL`, optional `RELEASE_BUILT_BY` (`BUILT_BY`), optional `RELEASE_BUILD_LOG` (`BUILD_LOG`), and, when `get_full_release_name` is non-empty, `RELEASE_NAME` and `RELEASE_NOTES` (notes with newlines replaced by form feeds). Zel has no `RELEASE_*` keys.
  - Git source is `git rev-parse --short HEAD`, and the commit URL is `${GIT_REPOSITORY_URL:-https://github.com/bazelbuild/bazel}/commit/${short}`. Zel’s only status script, `crates/zel-bazel-tools/resources/tools/buildstamp/get_workspace_status`, uses full `git rev-parse HEAD` and `git diff-index` (`Clean`/`Modified`) as `BUILD_SCM_REVISION` and `BUILD_SCM_STATUS`. This Bazel script has no dirty-tree key.
  - No clock or date key. The release name comes from the git branch `release-X.Y.Z[rcN]` or a matching tag, which may include `-pre.YYYYMMDD`. Zel’s `--workspace_status_command` handler in `write_workspace_status_files` (`crates/zel-server/src/lib.rs`) always writes volatile `BUILD_TIMESTAMP` from `SystemTime::now()` unix seconds, and does not write `FORMATTED_DATE` or read `SOURCE_DATE_EPOCH`.
  - Zel’s stability split matches Bazel’s rule for command output: `STABLE_*` or `BUILD_EMBED_LABEL` / `BUILD_HOST` / `BUILD_USER` go to `stable-status.txt`. Every other key, including these `RELEASE_*` keys, goes to `volatile-status.txt`. Zel also always writes those three stable keys itself. It runs the flag via `/bin/sh -c` and does not ship or invoke this CI script.

### `scripts/ci/build.sh`
- Bazel lines: 497
- Zel counterpart: `.github/workflows/release.yml` (packaging via `scripts/package_zel.sh`; CI itself is `.github/workflows/ci.yml`)
- Verdict: partial
- Differences:
  - This file is a sourced deploy library (`set -eu`), not a build driver. It loads `scripts/release/common.sh` and `relnotes.sh`. The header names `bazel_build` and `bazel_release`, but this file defines neither and runs no `bazel build` or `bazel test` and no target pattern.
  - Required tools: `gcloud`, `gh`, `debsign`, `reprepro`, `gpg`, `pandoc` (each missing tool exits 1). The `ssmtp` check is commented out. `APT_GPG_KEY_ID` comes from Secret Manager secret `release-key-gpg-id` in project `bazel-public`.
  - Platforms and artifacts consumed (produced earlier by `package_build`): GitHub repo `bazelbuild/bazel` for non-rolling releases; GCS `gs://bazel` for every release, including rolling; APT `gs://bazel-apt` for non-rolling only (`amd64` plus `source`, component `jdk1.8`). APT inputs are `bazel_<label>-linux-x86_64.deb`, `bazel_<label>.dsc`, and `bazel_<label>.tar.gz`, plus a rebuilt `bazel-<version>-versioned-package-amd64.deb` whose binary is `bazel-<version>`.
  - Release flags: RC tags use the full name, `--prerelease`, and `--latest=false`, and APT codename `testing` with GCS path `<name>/rc<N>`. A final release uses the release name, APT `stable`, and GCS `<name>/release`. `--latest=true` only when that name sorts at least as high as the current GitHub latest. A failed latest lookup warns and forces `--latest=false`. `--target` is the release branch. GitHub notes are the release page. `.dsc` and `.tar.gz` (and their `.sha256`/`.sig`) are removed before the GitHub upload. Rolling releases skip APT and GitHub and upload to `<track>/rolling/<full name>`, and a final rolling release rewrites `gs://bazel/rolling.html`.
  - Signing: every artifact gets a `.sha256` and a `gpg --detach-sign` with the APT key. `digest-algo sha256` is appended to `~/.gnupg/gpg.conf`. APT republishes merged `Packages`, `Sources.gz`, `Release`, `InRelease`, and `Release.gpg`.
  - Failure handling: `set -eu`. `deploy_release` exits 1 if the artifact directory is missing or the release name is empty. The GitHub latest-tag lookup is non-fatal. A notes tempfile is removed by a subshell `EXIT` trap. `generate_email` returns 0 with "No emails for rolling releases" and is not called by `deploy_release`. No retries.
  - Zel has no `scripts/ci/`. CI is `.github/workflows/ci.yml`: rustfmt and clippy (`-D warnings`) on `ubuntu-latest`; `cargo test --workspace --locked` on `ubuntu-latest`, `macos-latest`, and `windows-latest` (`fail-fast: false`); Unix-only `scripts/zel_compat_test.sh`; Unix-only `zel build //crates:zel` twice; Ubuntu `bazel test //crates/...` with repository and disk caches. That does not publish installers.
  - Zel release packaging is `scripts/package_zel.sh` (`set -euo pipefail`), invoked by `release.yml` for linux `x86_64` and `arm64`, darwin `x86_64` and `arm64`, and windows `x86_64`. It `cargo build --bin zel` and copies that one binary to `zel-<version>-<os>-<arch>`, `bazel-<version>-<os>-<arch>`, and `bazel_nojdk-<version>-<os>-<arch>`, each with `.sha256` when `sha256sum` or `shasum` exists. No `.sig`, deb, dsc, tarball, APT, GCS, GPG, RC, rolling, or latest logic. Publish is `gh release create` with `--generate-notes`, or `gh release upload --clobber` if the tag already exists. `upload-artifact` uses `if-no-files-found: error`.
  - Zel CI does not build the same artifacts. It tests `//crates:zel` and the Cargo workspace, and the release job publishes three renamed copies of the `zel` binary per platform, not Bazel’s signed linux-x86_64 installer set or the GCS and APT trees.

### `scripts/ci/ci.sh`
- Bazel lines: 80
- Zel counterpart: `.github/workflows/ci.yml`
- Verdict: partial
- Differences:
  - Bazel is a local affected-target script (`set -eu`): `COMMIT_RANGE` defaults to `git merge-base` of `origin/HEAD` and `HEAD`, then each `git diff --name-only` path is passed to `bazel query "$file"` with no `--keep_going`.
  - It then runs `bazel query --keep_going --noshow_progress 'kind(.*_binary, rdeps(//..., set(...)))'` and `bazel build` only if that output is non-empty, then the same flags for `kind(test, rdeps(//..., set(...))) except attr('tags', 'manual', //...)` and `bazel test` only if non-empty. A non-zero query still aborts under `set -e`. No platform matrix, cache, or format gate.
  - Zel has no `scripts/ci/`. CI is GitHub Actions on push to `main`, `master`, and `release-*`, plus `pull_request` and `workflow_dispatch`.
  - The `lint` job (ubuntu, Rust 1.85) runs `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --locked -- -D warnings`.
  - The `test` job matrix is ubuntu, macOS, and Windows with `fail-fast: false`, and runs `cargo test --workspace --locked`. Unix only also runs `./scripts/zel_compat_test.sh` and `./target/debug/zel --output_base=/tmp/zel-ci-selfhost build //crates:zel` twice.
  - The `bazel-rules-rs` job (ubuntu only) runs `bazel test --repository_cache=~/.cache/bazel-repo --disk_cache=~/.cache/bazel-disk //crates/...`. That is the full `//crates/...` graph, not changed-file `rdeps`, and it does not use `--keep_going`, a `.*_binary` build query, or a `manual` tag exclusion. A failed step fails that job. The other OS jobs keep running.

### `scripts/docs/clr_converter_test.py`
- Bazel lines: 232
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel test converts command-line-reference HTML to MDX. `crates/zel-options/tests/converter_parity_test.rs` checks flag-value parsing (`--jobs=auto` and similar), not this docs converter.
  - `test_full_conversion` reads `scripts/docs/testdata/clr_input.html` and `clr_expected.mdx` via `TEST_SRCDIR`/`TEST_WORKSPACE` with `open(..., "rt", encoding="utf-8")` and `assertEqual`s the full `str` result. Text mode uses universal newlines. The compare is Unicode code points, not bytes. The test source is ASCII.
  - Substring checks: `path="--[no]batch"`, `type="boolean"`, `default="false"`; `path="--jobs"`, `type="an integer"`, `default="auto"`; `type="void"`, `Expands to:`, `` `--verbose` ``, and no `default=`; `May be used multiple times` and no `default=`; substring `deprecated`; `Short form: `-j``; `Inherits all options from [common](#common) and [test](#test)` and `{#build}`; `&lcub;0,1,2&rcub;`; no `("auto"`, yes `&quot;auto&quot;`; on the first line containing `<ParamField`, no `*]<float` and yes `&lt;float&gt;` (if that line is absent the test still passes); `## Commands`, `` [`build`](#build) ``, `Build targets.`; `### Option Effect Tags`, `<span id="effect_tag_FOO">`foo`</span>`, `Foo description.`
  - Inline fixtures are ASCII triple-quoted `str`s. Entities such as `&lt;` and `&nbsp;` stay literal. Python does not decode them. `split("\\n")` splits only on `\\n`.
  - `unittest.main()` exits 0 on success and 1 on failure. No test asserts an exit code or a raised exception.

### `scripts/docs/create_release_docs.py`
- Bazel lines: 217
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no release-doc packager (`scripts/docs/` is absent). No tree walk, archive extract, or versioned zip. HTML escaping in `crates/zel-server/src/commands/help.rs` and `markdown.rs` is help and rendered markdown, not this pipeline.
  - Version is an opaque required string. It becomes the directory `versions/<version>` and is passed into `rewriter.rewrite_links` with no semver or date parse. `os.path.join` drops the parent if `version` is absolute. `/` and `..` add path segments. Nothing checks the string.
  - This file does not HTML- or Markdown-escape. Non-generated text goes through `mdx_fixes.apply`, then `rewriter`, which splices `version` into `href`/`src`, YAML paths, and `{% dynamic setvar version "…" %}` via `str.format` / f-strings. A quote in `version` breaks that attribute. `mdx_fixes` leaves HTML inside `<pre>` escaped (`TODO: Replace escaped HTML`) and turns `&lcub;#…&rcub;` back into `{#…}`. Generated paths containing `/reference/be/`, `command-line-reference`, or `/rules/lib/` skip `mdx_fixes`.
  - No date handling. No `datetime`, stamp, or timezone. Dates inside the archives are copied through unchanged.
  - Text reads use `open(..., "rt", encoding="utf-8")`: strict UTF-8, and universal newlines turn CRLF into LF before the zip write. `ZipFile.writestr(str)` then writes UTF-8 with the UTF-8 flag. Non-rewritten members are copied as raw bytes, so CRLF survives only there. Rust `read_to_string` does not translate newlines. Linux non-UTF-8 paths become lone surrogates via `os.fsdecode` and still round-trip. Rust `OsStr` to `String` fails or replaces them.
  - `re.sub` replacement templates are built with `version` inside them, so a backslash in `version` is a group reference or `re.error`. Rust regex replacement uses `$n`, not `\\n`.
  - `html.escape` is never called. A Rust `html_escape` would also disagree on `'` versus `"` if one were added later.
  - Suffix check is `splitext` against `.tar` and `.zip` only, so `.tar.gz` is rejected. The bad-extension path is `_ARCHIVE_FUNCTIONS.keys.join(...)`, which raises `AttributeError` on the unbound method instead of `ValueError`. `extractall` allows zip-slip `..`. `tempfile.mkdtemp()` is never deleted.

### `scripts/docs/clr_converter.py`
- Bazel lines: 692
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not convert command-line-reference HTML to MDX. There is no `scripts/docs/clr_converter.py`, `docs2mdx.py`, or `ParamField` writer. `crates/zel-server/src/commands/help.rs` `everything_as_html` only emits the HTML this script consumes.
  - Encoding: `HTMLParser()` keeps `convert_charrefs=True`, so `handle_entityref` and `handle_charref` never run. References are decoded, then `html.escape` re-encodes text (`& < > "`, and `'` as `&#x27;`). Prefix text maps `&#x27;` back to `'`, but `convert()` never emits `prefix_html`. `html.unescape` (full HTML5 named entities, so `&nbsp;` becomes U+00A0) runs on category blurbs, value types, defaults, `<pre><code>` bodies, and table cells.
  - Tags are not XML-safe. `_rebuild_tag` inserts attribute values in double quotes with no escaping. `None` is a bare attribute. End tags are `</tag>`. A `<td id>` becomes an unclosed `<span id="…">` with the id copied raw, and tag-table rendering looks that span up.
  - MDX escaping: `{`/`}` become `&lcub;`/`&rcub;`. Outside inline backticks and fences, `$` becomes `&#36;` and `<>` become `&lt;`/`&gt;`. Fence splitting is a pattern that stops at a backtick inside the fence, so a fence that contains a backtick is escaped as text. JSX attributes escape `& " < > { }` and not `'`. Help text is a regex subset (`a`, `code`, `em`, `strong`, `br`, lists, `p`). Other tags are dropped. One erratum rewrites a mismatched backtick quote to a closing backtick. Expansion flags and table cells are brace-escaped only, and expansion captures are not unescaped.
  - Zel `markdown.rs` emits `<pre><code class="language-…">` when a fence has an info string, and bare `<pre><code>` otherwise. This script only rewrites exact `<pre><code>…</code></pre>`, so language-tagged samples are stripped and then `$ < > { }` are MDX-escaped, while untagged fences stay literal. Zel `html_escape` writes `'` as `&#39;` and expansion flags with `&lt;` and `&nbsp;`. Markdown help leaves `'` raw. Decoded apostrophes match after `html.unescape`, but an expansion `<` stays `&lt;` inside backticks. Zel never emits `&lcub;`, `&rcub;`, or `&#36;`. `crates/zel-query/src/format.rs` `xml_escape_attr` also escapes newlines and other controls to numeric character references and does not escape `{`, `}`, `$`, or `'`. Nothing in zel uses it for this conversion.

### `scripts/docs/docs2mdx.py`
- Bazel lines: 433
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no HTML or Markdown to MDX converter. This file walks `.html`, `.md`, and `.mdx`, runs markdownify (`AcornSafeMarkdownConverter`, ATX headings), then `mdx_fixes.apply` and flag-anchor insertion. The closest zel code (`crates/zel-server/src/commands/help.rs`) only builds `flag--` help ids.
  - HTML entities are written, not decoded. Outside `pre` and `code`, `{`, `}`, `$`, `<`, and `>` become `&lcub;`, `&rcub;`, `&#36;`, `&lt;`, and `&gt;`. The table-cell path escapes only `{`, `}`, and `$`, and only in even backtick segments. `&` is never escaped, and replacement strings do not contain those keys, so there is no double-encoding. Dict iteration order is insertion order.
  - Headings: `<h1>`–`<h6>` with a quoted `id` become `#`–`######` plus ` {#id}` when the same line has no non-whitespace prefix. A non-empty prefix still becomes an ATX heading, but the anchor is dropped. Unquoted ids are left as HTML. The inner markup is kept. After conversion, the first line matching a flag link gets a preceding `<a id="…"></a>` and a blank line. Repeat ids are skipped. The match is `^`-anchored and is not stripped first.
  - Code: there is no fenced-code writer. `<pre>` and `<code>` tags are each replaced by one backtick, then even segments are treated as prose. An odd or unpaired tag flips that pairing. Inline `<code>` not inside `<pre>` is collapsed with `str.split()` and `" ".join` before markdownify. Table cells then strip each line and join with a space, so preformatted text becomes one inline-code line.
  - The whole pipeline is Python 3 `str` (Unicode code points). There is no code-point index or slice, so Rust `char` versus byte indexing does not change these replacements. `open(..., "rt"/"wt")` uses the locale encoding with strict errors and universal newlines (`\r\n` and lone `\r` become `\n` on read; `\n` becomes `os.linesep` on write). `read_to_string` is UTF-8 only and keeps `\r`. Later splits are on `"\n"` only, not `splitlines()`. `str.strip()` and no-arg `str.split()` use Unicode whitespace. `re` is Unicode, so `(\w)\\_(\w)` treats non-ASCII letters as word characters.

### `scripts/docs/docs2mdx_test.py`
- Bazel lines: 233
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel test or `scripts/docs` port. The only hit is a path string in a patch-corpus fixture.
  - `Docs2MdxTest` calls `_transform`. Inside `<pre><code>` and a Markdown `<pre>`, braces stay literal (`{"define": "species=excelsior"}`) and `&lcub;` / `&rcub;` are absent. The fenced body is `result.split("```")[1]`, and that slice must not contain `&lt;`. Prose keeps the entities: `&lt;`, `&lcub;`, and `&rcub;` for `x < y` and `{braces}`.
  - `Docs2MdxTableCellTest` calls `_html2md` with three named cases. A cell list must contain `<ul>` and must not contain `* Targets listed`. A nested table must contain `<table>` and must not contain `| Size | RAM`. A `<pre><code>` URL must contain the inline code `` `https://example.com/file.tar.gz` `` and must not contain a fenced block.
  - `Docs2MdxHeadingAnchorTest` checks `_transform` headings. Single- and double-quoted `id` values, an `h2` with `class` plus interior newlines, and an `h3` must keep `{#…}` anchors (`{#common-attributes}`, `{#typical-attributes}`, `{#cc_binary}`, `{#package_args}`). `<h2>Rules</h2>` stays `## Rules` and must not contain `{#`.
  - `Docs2MdxFlagAnchorTest` checks definition-list flags. `build-flag--define`, `flag--test_string`, and `flag--expanded_a` each produce `<a id="…"></a>` and `](#…)`. A repeated `build-flag--define` id is inserted once (`count == 1`). Flag text uses `&lt;` / `&gt;`, but those entities are not asserted.
  - Exit is `unittest.main()` only under `__main__`: success is process status 0, and any failure is unittest’s non-zero status. Importing the module does not run or exit. There is no custom status.
  - Fixtures are in-source ASCII literals, not files. Table and flag strings include the newline after `"""` and a trailing newline. The heading cases concatenate adjacent literals, with the space already in the text. `extra_attributes` is one literal with interior newlines. The expected `## cc_binary {#cc_binary}` assumes the transformer drops those newlines. The test never calls `splitlines` or rewrites newlines. The only index is `split("```")[1]`, which requires a fenced block in the first slot. f-strings need Python 3.6 or newer. The `__future__` imports do not change this.

### `scripts/docs/get_workspace_status.sh`
- Bazel lines: 23
- Zel counterpart: `crates/zel-bazel-tools/resources/tools/buildstamp/get_workspace_status` (consumed by `write_workspace_status_files` in `crates/zel-server/src/lib.rs`)
- Verdict: partial
- Differences:
  - Bazel prints one volatile key, `BUILD_SCM_REVISION` (no `STABLE_` prefix, no stable/volatile split). The value is `get_release_name` from `scripts/release/common.sh` (release branch `release-X.X.X[-pre.YYYYMMDD.N]rcY` or tag `X.X.X[-pre.YYYYMMDD.N]`). If that is empty it prints `UNSAFE_` plus `git rev-parse --abbrev-ref HEAD`.
  - Bazel git is only that branch command, and only on the empty-name path. No `git rev-parse HEAD`, no `git diff-index`, no failure exit.
  - Zel's stamp script prints volatile `BUILD_SCM_REVISION` as `git rev-parse HEAD` and exits 1 if that fails, plus volatile `BUILD_SCM_STATUS` (`Clean` if `git diff-index --quiet HEAD --` is 0, otherwise `Modified`). No release name and no `UNSAFE_` branch fallback.
  - `write_workspace_status_files` always writes stable `BUILD_EMBED_LABEL`, `BUILD_HOST`, `BUILD_USER` and volatile `BUILD_TIMESTAMP` (unix seconds from `SystemTime::now`). Command stdout is stable only for `STABLE_*`, `BUILD_EMBED_LABEL`, `BUILD_HOST`, and `BUILD_USER`. Every other key, including both `BUILD_SCM_*` keys, is volatile. It does not run git itself.
  - Neither the Bazel script, the zel stamp script, nor `write_workspace_status_files` reads `SOURCE_DATE_EPOCH` or formats a date. `BUILD_TIMESTAMP` is wall-clock seconds. `SOURCE_DATE_EPOCH` is used only in `crates/zel-server/build.rs` for `ZEL_BUILD_TIMESTAMP`, outside this writer.

### `scripts/docs/mdx_fixes.py`
- Bazel lines: 213
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Entities: Bazel restores only the heading-anchor shape ` &lcub;#…&rcub;` to ` {#…}` (`[^&]+` is every code point except `&`), and emits entities for `<inc(\\d+)>` → `&lt;incN&gt;` and for a brace-wrapped backtick pair → `&lcub;…&rcub;`. Python 3 `re` `\\d` is Unicode decimal digits, so non-ASCII digits match `<inc…>`. No other entity decode. `<pre>` contents are left escaped (explicit TODO). Zel `markdown.rs` `escape` / `render_to_html` and `help.rs` `html_escape` only encode on the way to HTML (`& < > "`, plus `&#39;` in help). They never decode `&lcub;`/`&rcub;`, and `{`/`}` are not escaped. Help hardcodes `&lt;`/`&gt;` around flag type placeholders only.
  - Braces: Bazel deletes kramdown `{: .class}` and `{# disableFinding(…) #}`, rewrites `{:#id}` to `{#id}`, turns `{# … #}` into `{/* … */}`, unwraps `{{ '<tag>' }}` / `{{ "</tag>" }}`, and flips `title:` quotes to `"` when the title contains `'`. `\\s`/`\\S` are Unicode whitespace and its complement. `^`/`$` (`MULTILINE`) break only on `\\n`, so U+2028 stays inside the match. `[\\S+]` is a class (Unicode non-whitespace or a literal `+`), not "`\\S` then `+`". `\\w` in the double-brace tag is Unicode word characters, so a non-ASCII tag name matches. `{# … #}` cannot span a newline (no `DOTALL`). Zel does not rewrite these. `{`/`}` stay text. Inline HTML is recognized only when the tag starts with an ASCII letter, then escaped.
  - Code: Bazel turns a `<pre>…</pre>` block into a fence via `str.strip()` (all `str.isspace()` code points, including non-ASCII spaces) and fixes a mismatched backtick-quote span to matching backticks (`[^>]` is any code point except `>`). It also drops a blank-line-plus-`</p>` immediately before a fence. Zel does the reverse for CommonMark: fences and indented code become `<pre><code>` with `& < > "` escaped, and it does not repair mismatched backtick/quote spans or orphan `</p>`.
  - Headings: If no `title:` line exists, Bazel copies the first `^# (.+)$` into `title: '…'` (`count=1`; `.+` is code points on that `\\n`-terminated line, closing hashes included). It also rewrites a broken `---\\n\\n## title: …` block into YAML front matter, and restores escaped `{#anchor}` suffixes. Zel parses ATX (`#`–`######`, optional closing hashes) and setext headings to `<hN>` and does not build YAML titles or MDX anchors. `str.rstrip`/`lstrip` in the surrounding whitespace and `<style>` cleanup are Unicode `isspace()`, and line splits are only on `\\n`.

### `scripts/docs/rewriter.py`
- Bazel lines: 144
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel `scripts/docs` tree and no port of `can_rewrite` / `rewrite_links`. `zel-bzlmod` `UrlRewriter` only rewrites download URLs.
  - Link rewrite splices `/versions/{version}` after an optional `https://bazel.build` and before a root-relative path. HTML: `(href|src)\\s*=\\s*["']` plus optional base URL, replacement group 1 + `/versions/{version}/`. Markdown links and images: optional `!`, `[...](`, optional base URL, `(/...)`, replacement `\\1/versions/{version}\\3)`.
  - Metadata splice: HTML exact replace of `value="/_book.yaml"`. Markdown `^(Book: )(/.+)$` (`MULTILINE`) becomes `Book: /versions/{version}…`. YAML keys `path`, `book_path`, `image_path`, `include` via a callback on groups 1, 4, and 5. Ignored YAML paths: `/`, `/_project.yaml`, `/versions/`, `/versions/_toc.yaml`.
  - Header splice (`.html` and `.md` only): the literal `{% include "_buttons.html" %}` is prefixed with `{% dynamic setvar version "…" %}` and `{% dynamic setvar original_path "/{stem}" %}`. `{{`/`}}` are f-string brace escapes. Version and path are not HTML- or Markdown-escaped. `_buttons.html` is skipped. `.mdx` gets only Markdown and HTML link fixes. `.yaml` gets YAML paths plus HTML fixes.
  - Python `re` replacements use `\\1` and `\\3`. The Rust `regex` crate uses `$1` and `$3`, and `$` in the replacement must be escaped. `.` in `bazel.build` is unescaped in the pattern. Rust `.` also does not match `\\n` by default. `re.MULTILINE` is Rust `(?m)`. Python raw `r"[\\"']"` keeps the backslash, so the class is `\\`, `"`, or `'`. Rust raw strings do not treat `\\"` as an escaped quote. The triple-quoted header template contains real newlines, not `\\n` escapes.

### `scripts/docs/rewriter_test.py`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `scripts/docs` rewriter test. Unrelated URL rewriting in `zel-bzlmod` is not this suite.
  - `CanRewriteTest.testCanRewrite` uses `assertEqual` on `rewriter.can_rewrite`. True: `/file/doc.md`, `/path/_book.yaml`, `http://www.bazel.build/foo.html`. False: `/dir/test.txt`, `/images/aspects.svg`. These are literal paths, not regexes. The extension comes from `os.path.splitext`, so the `http://…foo.html` case is true because the suffix is `.html`.
  - `RewriteLinksTest.testRewrite` golden-compares full rewritten text, not regexes, for `_book.yaml`, `doc.md`, `markdown_with_html.md`, `site.html`, and `yaml_with_html.yaml`. Paths are `$TEST_SRCDIR/$TEST_WORKSPACE/scripts/docs/testdata/{input,expected_output}/<basename>`.
  - The version string is the entire `testdata/input/VERSION` body, passed through with no `strip`. On disk it is exactly `6.6.6` (5 bytes, no trailing newline), so `.format(version)` does not inject a newline. A newline in that file would be inserted into every substitution.
  - Reads use `open(path, "rt", encoding="utf-8")` (strict UTF-8, universal newlines). Equality is on `str` after `\r\n` and `\r` become `\n`. Invalid UTF-8 raises. A BOM is kept as a character (`utf-8`, not `utf-8-sig`).
  - The test file has no regex. Golden files depend on `rewriter.py`: HTML `(href|src)\s*=\s*["'](https://bazel.build)?/` ; markdown `!?[.*?]((https://bazel.build)?)(/.*?)\)` with non-greedy `.*?` and the first `)` ; `^(Book: )(/.+)$` with `re.MULTILINE` and `.` not crossing newlines ; YAML `(((book_|image_)?path|include): ['"]?)(/.*?)(['"]?)$` with exact ignore-list matches for `/`, `/_project.yaml`, `/versions/`, `/versions/_toc.yaml`. Also exact `str.replace` for `value="/_book.yaml"` and `{% include "_buttons.html" %}`. `.format(version)` runs before `re.sub`, so `\` or `{` `}` in a version would change the replacement. `6.6.6` does not.
  - `if __name__ == "__main__": unittest.main()` exits 0 on success and non-zero via `SystemExit` on failure. Importing the module does not run tests.

### `scripts/generate_bash_completion.sh`
- Bazel lines: 77
- Zel counterpart: `crates/zel-server/src/commands/help.rs` (`help completion` / `help completion bash`); templates `crates/zel-server/resources/help/bazel-complete-header.bash` and `bazel-complete-template.bash` (no generator script)
- Verdict: partial
- Differences:
  - The shell script does not list commands or flags. It writes a temp `WORKSPACE`, then runs `"${bazel}" --batch --output_user_root=… [--server_javabase=…] help completion` (no `bash` argument), so only the variable block is appended. Optional `--prepend` and `--append` files are concatenated around that, then moved to `--output`. Zel has no `--bazel` / `--output` / `--javabase` / `--prepend` / `--append` packager. `cmd_help` emits the variables itself, and `help completion bash` also prints the header and template.
  - Those two zel bash files are byte-identical to `scripts/bazel-complete-header.bash` and `bazel-complete-template.bash`. Completion is registered for `${BAZEL:-bazel}` and `${IBAZEL:-ibazel}` (`complete -F _bazel__complete`), and the variables stay `BAZEL_*`. Nothing completes the name `zel`. Zel’s default `product_name()` is also `bazel`. Info keys such as `{product}-bin` change only if that product name is not `bazel`.
  - Command list: Bazel `ImmutableSortedMap` of every registered command (`String.compareTo`, hidden commands included), space-joined into `BAZEL_COMMAND_LIST`. Zel sorts `COMMANDS` with `sort_by_key(|c| c.name)` the same way. Per-command names become `BAZEL_COMMAND_<UPPER_UNDERSCORE>_FLAGS`, plus `_ARGUMENT` when the command’s completion string is non-empty.
  - Flag lists: both drop undocumented options and sort by option name (`OptionDefinition.BY_OPTION_NAME` vs `name.cmp`). Neither uses a locale collator. Enum text: Bazel lowercases the joined constants with `Locale.ENGLISH`. Zel `OptionType::Enum` prints `spec.values` as stored. Custom converters with `enum_values` use Unicode `to_lowercase()`, not `Locale.ENGLISH`.
  - Startup options: Bazel filters only `documentationCategory != UNDOCUMENTED`. Zel drops `is_internal()` first, then undocumented. Zel also forces `allow_multiple` options (except boolean, tri-state, and void) to `--flag=`, and treats `DiskCacheConverter`, `ExecutionLogFileConverter`, and converter names containing `PathFragment` as `=path`. Bazel keys only off the Java field type (`boolean`, `TriState`, enum, `Label`, `PathFragment`, `Void`, otherwise `=`).
  - `BAZEL_INFO_KEYS` is hardwired declaration order on both sides, not sorted.

### `scripts/generate_fish_completion.py`
- Bazel lines: 327
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel generator. This file (target `//scripts:fish_completion`) is the only fish-completion writer. Zel has bash resources at `crates/zel-server/resources/help/bazel-complete-header.bash` and `bazel-complete-template.bash`, and `cmd_help` in `crates/zel-server/src/commands/help.rs`. The only other fish files are unrelated Swift ArgumentParser sources under `third_party/`.
  - The command name is hardcoded `bazel`. Required flags are `--bazel` (binary used only to run help) and `--output`. The emitted script uses `complete -c bazel`, `BAZEL_COMMAND_LIST`, `__bazel_seen_subcommand`, and label queries `bazel query -k '...'`. It never uses the product name. Zel’s binary is `zel`. `product_name()` still defaults to `bazel` in help text.
  - Flag descriptions are the parenthetical type text from default medium `help startup_options` and `help <cmd>`, matched by a regex for `--name (desc)`. That text becomes fish `-d`. A description starting with `a boolean` emits `--name` and `--noname` and omits `-r`. Every other option gets `-r`. An abbreviation (`--name [-x] (...)`) fails the regex, so those flags are skipped. Subcommands come from the `Available commands:` block. The description must end in `.`. Param kinds come from `help completion` (no shell): `BAZEL_COMMAND_<SUB>_ARGUMENT` (`_` normalized to `-`) and `BAZEL_INFO_KEYS`. `label` / `label-bin` / `label-test` become query substitutions. `command|{...}` splits on commas. `info-key` splits on whitespace. Zel’s medium help uses the same `(type; default: "...")` shape, and `zel help completion` prints those `BAZEL_*` variables, but nothing turns them into fish.
  - Quoting: names and descriptions are double-quoted and joined with spaces. `_escape` only replaces `"` with `\\"`. `$`, `\\`, backticks, and newlines are left as-is. Guards are `-n "__fish_seen_subcommand_from <cmd>"` or `-n "not __bazel_seen_subcommand"`. Params are one double-quoted `-fa` token. A label query inside it is `(bazel query -k 'QUERY' 2>/dev/null)`.
  - Python text: `__future__` imports for `absolute_import`, `division`, and `print_function`. `subprocess.check_output(..., universal_newlines=True)` decodes help stdout with the locale preferred encoding (no `encoding=`). `open(FLAGS.output, mode='w')` writes locale-encoded text and translates `\\n`. `--output_user_root` is a `tempfile.TemporaryDirectory`. Regexes are raw strings with `re.MULTILINE` / `re.DOTALL`.
  - `zel help completion fish` does not print a fish script. Stderr is `ERROR: The completion command only supports 'bash' as an argument, got 'fish'` and the status is command-line error (exit 2), the same rejection Bazel uses for any shell other than `bash`. `zel help completion` prints only the bash variable block. `zel help completion bash` wraps that block in the bash header and template.

### `scripts/kokoro/release/build_linux.sh`
- Bazel lines: 62
- Zel counterpart: `.github/workflows/release.yml` and `scripts/package_zel.sh`
- Verdict: partial
- Differences:
  - Bazel runs this Kokoro script with `set -e` and `set -x`. `RELEASE_NAME` defaults to `unknown`. `uname -m` picks Bazelisk v1.7.2: `aarch64` downloads `bazelisk-linux-arm64`, and every other architecture downloads `bazelisk-linux-amd64` into `/tmp/tool/bazelisk`. Zel releases from GitHub Actions on tags `v*.*.*` or `*.*.*`, or on `workflow_dispatch`, and takes the version from the tag with a leading `v` removed or from the `version` input.
  - This script builds on the current Linux machine and names artifacts with raw `uname -m` (`x86_64` or `aarch64`). Only `x86_64` copies the Debian source package and the dist zip into `artifacts/`. Zel’s package matrix builds linux `x86_64` and linux `arm64`, and also darwin `x86_64`, darwin `arm64`, and windows `x86_64`, with `fail-fast: false`.
  - Bazel bootstraps `//src:bazel` with `--sandbox_tmpfs_path=/tmp`, copies `bazel-bin/src/bazel` to `output/bazel`, then that binary builds `-c opt --stamp --sandbox_tmpfs_path=/tmp --embed_label "${RELEASE_NAME}" --workspace_status_command=scripts/ci/build_status_command.sh` for `src/bazel`, `scripts/packages/with-jdk/install.sh`, `scripts/packages/debian/bazel-debian.deb`, `scripts/packages/debian/bazel.dsc`, `scripts/packages/debian/bazel.tar.gz`, and `bazel-distfile.zip`. Staged names are `bazel-<release>-linux-<arch>`, `bazel-<release>-installer-linux-<arch>.sh`, `bazel_<release>-linux-<arch>.deb`, and on `x86_64` also `bazel_<release>.dsc`, `bazel_<release>.tar.gz`, and `bazel-<release>-dist.zip`. Zel runs `cargo build --bin zel` (`--release` unless `--debug` or `--dev`) and copies that single binary to `zel-<version>-<os>-<arch>`, `bazel-<version>-<os>-<arch>`, and `bazel_nojdk-<version>-<os>-<arch>` (`.exe` on Windows).
  - This script has no signature and no checksum. Zel writes a `.sha256` sidecar with `sha256sum` or `shasum -a 256`, and the script still exits successfully when neither tool exists. Neither path GPG-signs or code-signs the binaries.
  - Bazel stops on the first failed command and traces each command. It has no `pipefail`, no `wget` retry, and no extra artifact check beyond a failing `cp`. The Debian dsc, tar.gz, and dist zip are built on every architecture, including `aarch64`, and copied only for `x86_64`. Zel uses `set -euo pipefail`, exits 1 on an unknown flag, and if the version is omitted walks `ZEL_EMBED_LABEL`, `BAZEL_EMBED_LABEL`, `.bazelversion`, then `0.0.1`. The workflow fails the upload when `dist/release/*` is empty (`if-no-files-found: error`) and runs `publish` only after every matrix package job succeeds. Publish uploads with `gh release upload --clobber` when the tag already exists, or creates the release with generated notes. The Kokoro script ends once `artifacts/` is filled and does not publish.

### `scripts/kokoro/release/build_macos.sh`
- Bazel lines: 48
- Zel counterpart: `.github/workflows/release.yml` and `scripts/package_zel.sh`
- Verdict: partial
- Differences:
  - Bazel (Kokoro): `RELEASE_NAME` defaults to `unknown`. It downloads Bazelisk v1.7.2 `bazelisk-darwin-amd64` to `/tmp/tool/bazelisk`, runs `sudo xcode-select -s /Applications/Xcode_10.3.app/Contents/Developer`, bootstraps `"${BAZELISK}" build //src:bazel`, and copies `bazel-bin/src/bazel` to `output/bazel`. Zel: GitHub Actions on `macos-14` runs `./scripts/package_zel.sh` with Cargo (`cargo build --bin zel --release`, optional `--target`). No Bazelisk bootstrap, no `xcode-select`, no pinned Xcode.
  - Bazel rebuilds with `output/bazel build -c opt --stamp --embed_label "${RELEASE_NAME}" --workspace_status_command=scripts/ci/build_status_command.sh --define IPHONE_SDK=1` for `src/bazel`, `scripts/packages/with-jdk/install.sh`, and `scripts/packages/dmg/bazel.dmg`. Zel does not stamp, set `IPHONE_SDK`, or build those package targets.
  - Signing: Bazel does not run `codesign`, `productsign`, or notarization. The disk image is copied as `artifacts/bazel-${RELEASE_NAME}-darwin-x86_64-unsigned.dmg`. Zel also has no signing or notarization step.
  - Bazel artifacts (x86_64 only): `artifacts/bazel-${RELEASE_NAME}-darwin-x86_64`, `artifacts/bazel-${RELEASE_NAME}-installer-darwin-x86_64.sh`, `artifacts/bazel-${RELEASE_NAME}-darwin-x86_64-unsigned.dmg`. No checksums. Zel writes the same binary three times plus `.sha256` files: `zel-<version>-darwin-<arch>`, `bazel-<version>-darwin-<arch>`, `bazel_nojdk-<version>-darwin-<arch>` for both `x86_64` (`x86_64-apple-darwin`) and `arm64` (`aarch64-apple-darwin`). `bazel` and `bazel_nojdk` are identical copies, not a with-JDK installer versus a no-JDK binary. No installer script and no DMG.
  - Zel adds a tag/`workflow_dispatch` release job that uploads those files with `gh release create` or `gh release upload`. The Bazel script only fills a local `artifacts/` directory.

### `scripts/packages/bazel.sh`
- Bazel lines: 224
- Zel counterpart: `scripts/zel.sh` (client reimplementation: `crates/zel-client/src/bazelisk.rs`)
- Verdict: partial
- Differences:
  - This script never locates an install base or a JDK. It only picks a sibling binary and `exec`s it. Install base and JDK belong to the real client after that `exec`. Zel’s shell wrapper is the same. Zel’s client (`crates/zel-client/src/lib.rs`, `resolve_install_base` in `crates/zel-options/src/startup.rs`) sets the install base to `<output_user_root>/install/<md5 of the running binary>` unless `--install_base` is set, and only checks `--server_javabase` as `<path>/bin/java`. It does not unpack embedded tools or search `JAVA_HOME` / a bundled JDK.
  - Real binary: directory of this script (`readlink -f`, or a Darwin symlink walk). Version from `$USE_BAZEL_VERSION`, else the first line of `.bazelversion` next to `WORKSPACE`, `WORKSPACE.bazel`, or `MODULE.bazel`, else executable `bazel-real` (version string `real`), else the newest `bazel-[0-9]*-<uname -s>-<uname -m>` then `bazel-[0-9]*` (`find | sort -V | tail -1`). The `bazel-` prefix is stripped and the OS/arch suffix is left on, so the hit is the unsuffixed candidate `bazel-<version>`. Last resort: `bazel-real --version` must match `^bazel <version>`.
  - If `<workspace>/${BAZEL_WRAPPER:-tools/bazel}` is an executable file, it exports `BAZEL_REAL` and `exec -a "$0"` that wrapper even when the real binary is missing. An empty workspace makes that path `/tools/bazel`.
  - Final handoff is bash `exec -a "$0"` (process replace, argv0 stays the wrapper). Bash only. No `.exe`, no `cmd`, no arch rename (`aarch64` stays `aarch64`).
  - `scripts/zel.sh` also accepts `$USE_ZEL_VERSION` and `.zelversion` first, strips a leading `v`, and treats `MODULE.zel`, `REPO.bazel`, and `WORKSPACE.zel` as workspace markers. It searches `zel-*` before `bazel-*`, strips the OS/arch suffix, probes `zel-real` then `bazel-real` with awk field 2 (not `grep '^bazel '`), and treats version `real` as a match. It exports `ZEL_REAL` as well and tries `tools/zel` then `tools/bazel`, and it skips wrappers when there is no workspace. Apt/curl error text still names Bazel and `bazel-<ver>-<os>-<arch>`.
  - The Rust client (`plan_invocation` / `execute_external_action`) can stay in-process when the version matches, or download a release. It normalizes OS to `linux`/`darwin`/`windows` and `aarch64` to `arm64`, and appends `.exe` on Windows. It spawns `std::process::Command` (a child, argv0 is the target path) and sets `ZEL_SKIP_WRAPPER`. On non-Unix it treats a wrapper as executable without a mode check. `scripts/package_zel.sh` copies one binary to `zel-`, `bazel-`, and `bazel_nojdk-` names (plus `.exe` on Windows) and does not install this shell wrapper, a `*-real` link, or a JDK.

### `scripts/packages/debian/generate_changelog.py`
- Bazel lines: 44
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel reads a status file and writes a Debian changelog: a line starting with `RELEASE_NAME ` supplies the version (`bazel (<version>) unstable; urgency=low`, body `Bumped Bazel version to <version>.`). If that line is absent it uses `bazel (0.1.0~HEAD)` and body `Development version`.
  - The maintainer trailer is fixed: `The Bazel Authors <bazel-discuss@googlegroups.com>`. The date is `datetime.now()` formatted `%a, %d %b %Y %H:%M:%S +0100` (local clock, timezone hardcoded `+0100`). Files are opened in text mode (`"r"` / `"w"`) with no explicit encoding.
  - Zel has no `scripts/packages/debian/` tree and no generator. Vendored `rules_pkg` can build a `.deb` if a caller supplies a changelog, but zel itself does not emit a Debian changelog or a `.deb`.

### `scripts/packages/dmg/create_bazel_dmg.sh`
- Bazel lines: 95
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel builds a macOS installer disk image. Zel does not produce one. There is no `scripts/packages/` tree and no `hdiutil` create script.
  - Flags: required `--bazel_binary` and `--output`. Optional `--bazel_icon`, `--ds_store`, and `--background`. Unknown flags warn and continue.
  - Staging: `mktemp -d -t bazel-dmg-XXXX` with an EXIT trap that deletes it. The binary is copied to `bazel`, and `bin` is a symlink to `/usr/local/bin`.
  - Optional icon: `chmod +w`, `Rez -append` the `.rsrc` onto the binary, `SetFile -a C`, then `chmod 0755`.
  - Optional layout: copy `.DS_Store` into the staging dir. If a background is also given, copy it under `.background/`.
  - Image: `hdiutil create` with `-ov -volname "bazel-install" -format UDZO -fs HFS+ -srcfolder` the staging dir. This script does not sign the image (`codesign` / `productsign` are absent).
  - Zel’s only `.dmg` code is third-party consumption: `third_party/bcr/apple_support/2.5.2/tools/http_dmg/http_dmg.bzl` downloads and extracts a disk image with 7z. It does not create one.

### `scripts/packages/package_info_generator.sh`
- Bazel lines: 78
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `scripts/packages/` tree and no Rust writer for the `generate-package-info` output `README.md`.
  - The script does not emit name, version, arch, or dependency metadata. It prints a Markdown README from stamped `*status*.txt` keys.
  - Empty `RELEASE_NAME`: `# Binary package at HEAD (@$git_hash)` plus a newline. Non-empty: `# ${release_notes}`, a blank line, then `## Build information`.
  - Optional bullets are three spaces, hyphen, space: `[Built by ${built_by}](${build_log})`, or `Built by ${built_by}`, or `[Build log](${build_log})`. The last line is always ` - [Commit](${url})`.
  - `url` is only the default `https://github.com/bazelbuild/bazel/commit/${git_hash}`. `RELEASE_COMMIT_URL` is stored in `commit_url` and never read, so a port that uses that key would not match.
  - Values are `cut -d ' ' -f 2-` with `tr '\\f' '\\n'`, so a form feed becomes a real newline inside the header. `echo` supplies the trailing newline on every line. `cat $i` is unquoted.

### `scripts/packages/self_extract_binary.bzl`
- Bazel lines: 97
- Zel counterpart: `scripts/package_zel.sh` (packaging only; not a self-extracting installer)
- Verdict: partial
- Differences:
  - Bazel `self_extract_binary` builds an executable whose header is the mandatory single-file `launcher` and whose footer is a zip. The installer launcher (`scripts/packages/template_bin.sh`, stamped to `launcher_bin.sh`) starts with `#!/usr/bin/env bash`. The `.bzl` does not insert a shebang. `executable = True` only marks the output executable.
  - Compression is Info-ZIP: stage files, `touch -t 198001010000.00` every entry, then `zip -rq` (recursive, quiet, default deflate). `cat` concatenates the launcher and the zip, then `zip -qA` rewrites offsets so the zip stays valid after the header.
  - Archive paths: `resources` are copied to `${tmpdir}/<short_path>` (parent dirs created). `flatten_resources` are copied as the basename only. `empty_files` are zero-byte files at the given relative path. The zip is `*` from that staging directory. The launcher extracts selected names with `unzip -q "${BASH_SOURCE[0]}" … -d "${base}/bin"` (`base` defaults to `/usr/local/lib/bazel`).
  - Embedded shell is a list of ordinary double-quoted strings joined by `"\\n".join`. `%` is Starlark formatting. `${tmpdir}` and `$()` are not Starlark substitutions and pass through to the shell. Adjacent literals are concatenated with `+` (`%` binds tighter than `+`). `\\n` and `\\"` are real escapes. There is no triple-quoted script body. The docstring says `flatten_ressources`. The attribute is `flatten_resources`.
  - Zel does not build this installer. `package_zel.sh` (`#!/usr/bin/env bash`) runs `cargo build` and copies the same native binary to `zel-<version>-<os>-<arch>`, `bazel-…`, and `bazel_nojdk-…` plus SHA-256 files. No launcher header, zip payload, `zip -A`, or `unzip` extract step.

### `scripts/packages/template_bin.sh`
- Bazel lines: 192
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel shell installer. `%release_info%` is substituted at package time and printed. `%prefix%` in the default `--bin` and `--base` paths is replaced at runtime. Flags: `--prefix` (default `/usr/local`), `--bin`, `--base`, `--user` (`$HOME/bin`, `$HOME/.bazel`), `--skip-uncompress`.
  - Requires `unzip`. Extracts `bazel`, `bazel-real`, `bazel-complete.bash`, `_bazel`, and `bazel.fish` from the script itself (`BASH_SOURCE[0]`, shell plus zip) into the base `bin`, then symlinks `${bin}/bazel`. If uncompress is on and the user is not root, it runs `"${bin}/bazel" help` and exits 0. It does not replace the installer process.
  - JDK check only when `$0` matches `*without-jdk*`: it looks up `JAVA_HOME` (Linux `javac`, FreeBSD OpenJDK 8, OpenBSD JDK 1.8, macOS `java_home -v ${JAVA_VERSION}+`) and requires `javac`. A with-JDK build skips that check.
  - Zel has no self-extracting installer and no bash `bazel` / `bazel-real` pair. `scripts/package_zel.sh` copies one native `zel` binary to `zel-*`, `bazel-*`, and `bazel_nojdk-*` (the nojdk name is an alias). The client is Rust (`crates/zel-client/src/main.rs`: `execute_cli_with_exec` / `perform_exec_request`), with no unzip, prefix install, JDK probe, or shell-completion install.

### `scripts/release/common.sh`
- Bazel lines: 124
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `scripts/release/common.sh` and no `get_release_name` (or the related helpers). Zel does not use these release-name rules.
  - Bazel branch: `git symbolic-ref --short HEAD`, or on detached HEAD `git branch --remote --contains | cut -d "/" -f2`. Tag: `git describe --tag`.
  - A release candidate is the suffix after `rc` on the branch (`(?<=rc)([0-9]|\\.)*$`). A non-match is emptied with `|| true` and does not fail the script.
  - The release name is `release-X.X.X` or `release-X.X.X-pre.YYYYMMDD.N` (optional extra `.M`) with an optional `rc…` tail, or the same string as a tag with no `release-` prefix and no `rc`. The pre-release date is exactly eight digits.
  - Rolling is that name ending in `X.Y.Z-pre.YYYYMMDD.N` or `…N.M` (prints `1` or `0`). LTS is the leading `X.Y.Z`. The full name appends `rc` plus the candidate when one is present.
  - `get_release_branch` writes `Not a release branch: …` to stderr and `exit 1` when the name or the candidate is empty. The file starts with `set -eu`.
  - Zel’s `.github/workflows/release.yml` takes a version from a workflow input or from a tag matching `v*.*.*` or `*.*.*`, then strips an optional leading `v`. No `release-*rc*` branch grammar, no `-pre.YYYYMMDD` rolling names, and no exit when the current branch is not a release branch.

### `scripts/release/release.sh`
- Bazel lines: 322
- Zel counterpart: `.github/workflows/release.yml`
- Verdict: partial
- Differences:
  - Branch: Bazel `create` force-fetches `git@github.com:bazelbuild/bazel`, checks out `release-<name>rc<N>` (default rc 1) from a baseline, and cherry-picks extra commits, opening an interactive shell on conflict. `release` then requires that RC branch to match an existing `release-<tag>` branch. Zel never creates or updates a release branch. It only checks out the workflow commit.
  - Tag: Bazel adds a lightweight `git tag <name>` (no `-a`, message, or tagger date) after a changelog commit, and only after an interactive y/N. Zel does not run `git tag`. It starts from an existing tag matching `v*.*.*` or `*.*.*`, or from `workflow_dispatch`. `gh release create` can create a GitHub tag at the default-branch tip when that version is not already a tag, because no `--target` is set.
  - Notes: Bazel prepends `generate_release_message` onto `CHANGELOG.md` taken from `master`, commits it with `--no-verify` as `Bazel Release System <noreply@google.com>`, and can three-way-merge notes on temporary branches. It then copies that file onto `master`. Zel does not edit a changelog. A new GitHub release uses `--generate-notes` and the title `Zel <tag>`. If the release already exists, it only re-uploads assets with `--clobber` and leaves the notes unchanged.
  - Push: Bazel force-pushes (`-f`, `+ref`, `push-justification=b/303672453`) the release branch and tag to the GitHub SSH remote, and force-pushes `+master` to `https://bazel.googlesource.com/bazel` after `git pull --rebase`. `abandon` deletes local and remote `release-<tag>rc*` branches. The release path does not delete branches, despite its comment. Zel pushes no git refs. It uploads packaged binaries with `gh` and `GITHUB_TOKEN`.
  - Git and date: Bazel exits on a dirty tree or when not run at the repo root. Commits set `--author` but not `--date`, so author and committer times are the local clock. The lightweight tag has no tagger date. `EDITOR` defaults to `vi` and is unused in this file. There is no `date` command. Zel sets no git identity, commit, rebase, or date. `actions/checkout@v4` keeps the default shallow checkout.

### `scripts/release/relnotes.py`
- Bazel lines: 245
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no release-notes generator. Nothing parses `RELNOTES`, walks `base..HEAD`, or prints categorized notes. The only nearby file is vendored `third_party/bcr/rules_pkg/1.0.1/pkg/releasing/print_rel_notes.py`, a package changelog template, not this script.
  - Git stdout is `check_output(...).decode("utf-8").strip().split("\\n")`: strict UTF-8 (invalid bytes raise), then Unicode `strip` before the split. Empty output becomes `[""]` because `"".split("\\n")` is `['']`. They special-case that. Rust `str::lines()` on empty input yields no lines, so that check would not match. `lines()` also drops a trailing newline. This code splits only on `\\n` after an end trim.
  - Patch releases (`version` not ending in `.0`) use the commit subject. Otherwise notes come only from `RELNOTES` / `RELNOTES[INC]` / `RELNOTES[NEW]` bodies. Subjects are the first line with a leading `[X.Y.Z]` removed. The version regex is `\\d+\\.\\d+\\.\\d` (last component is one digit) plus a single optional `\\s`, so `[1.2.3] foo` becomes `foo` and `[1.2.10] foo` stays unchanged. `\\s?` is one whitespace character, not a run.
  - `RELNOTES` sets an in-note flag and keeps the text after the prefix. `INC` is prefixed with the only markdown this script emits, `**[Incompatible]** `. `NEW` is recognized and then discarded with the tag. A remainder that lowercases and `rstrip(".")`s to `n/a`, `na`, or `none` returns `None` for the whole commit. Later non-empty lines stay in the note until a blank line, `PiperOrigin-RevId:`, or `Fixes`/`Closes #N`. Lines are joined with single spaces. There is no HTML escape and no markdown escape of `*`, `_`, `<`, or `&`.
  - An issue id is taken only from the last whitespace-separated subject token matching `\\(\\#[0-9]+\\)$` (`[0-9]`, not `\\d`), then appended as `(#123)`. Category lookup strips `(` / `#` / `)` and reads GitHub labels via `gcloud` secret `github-trusted-token` and `requests`. The first label whose name contains `team-` wins. `team-` is removed with a global replace. Missing labels become `General`. Categories are sorted with ordinary Unicode code-point order.
  - Rollback commits whose subject matches `^Automated rollback of commit ([hex])` are skipped, and the captured id is skipped too. The capture is compared to full `rev-list` hashes, so an abbreviated hash in the message never matches. Notes identical to ones reachable from the same merge-base to the previous tag are dropped. Order is reverse-chronological, then reversed to chronological. A non-patch whose notes are all empty (including no commits: `all([])` is true) is retried in patch mode because `is_patch == 0` is true when `is_patch` is `False` (`bool` is an `int`). Rust `bool` is not an integer.
  - External authors are `%aN` from `%aN|%aE` whose full line does not `endswith("@google.com")` (literal dot), plus `Co-authored-by` trailers that fail `re.search("@google.com", ...)` (the dot matches any character). Co-author text drops `Co-authored-by: ` and `<...>` (`.` does not cross newlines), then collapses whitespace. Names are sorted with `str.casefold`, which Rust `to_lowercase` does not implement (`ß`, `İ`, final sigma).
  - Other Rust mismatches: Python `re.match` is start-anchored and `\\d` / `\\s` are Unicode unless `re.ASCII`, while `[0-9]` and `[A-Fa-f]` are ASCII, and this file mixes them. `re.sub(r"rc\\d", ...)` turns `7.0.0rc10` into `7.0.00`. A `Match` group is `None` when `[INC|NEW]` is absent. Slicing `line[len(match):]` is by code point. A byte index is safe here only because the prefix is ASCII. `tag.sort()` is lexicographic, not semver. `print("+", note)` inserts a space. An empty subject makes `split()[-1]` raise `IndexError`.

### `scripts/release/relnotes.sh`
- Bazel lines: 321
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `scripts/release/` tree and no `relnotes.sh` or `relnotes.py`. It does not generate Bazel-style release notes. Vendored changelog helpers under `third_party/bcr/` are dependency packaging, not a zel release-notes generator.
  - Non-rolling releases (`is_rolling_release` prints `0`) call `python3 ${RELNOTES_SCRIPT_DIR}/relnotes.py` with no arguments. The shell does not pass a range, baseline, or tags. Python reads `HEAD`: a `release-*` branch name (with `rcN` stripped) or `git describe --tags`. It lists `git tag --sort=refname`, drops tags containing `pre`, appends the current version if it is not already a tag, and takes the previous sorted tag as the last release. The range is `git merge-base HEAD <that tag>` through `HEAD`, minus notes that also appear from that merge-base to the previous tag. The shell prints that stdout unchanged, optionally wrapped by a delimiter. The unquoted script path is word-split only if the directory contains IFS or glob characters.
  - Rolling releases (`…-pre.YYYYMMDD.N`, function prints `1`) stay in bash. The revision baseline is `git merge-base master <branch>` (default `HEAD`). Cherry-picks are commits on `baseline..branch` whose `git patch-id` is missing from `baseline..master`. A match is replaced by the master hash. That list is only printed as “Cherry picks” via `__create_revision_information`. `create_release_notes` ignores the baseline and cherry-pick arguments it is given.
  - Note selection reads `Baseline: <hash>` from `git show master:CHANGELOG.md`, plus following ` + <hash>:` lines until a blank line. `echo $BASELINE $CHERRY_PICKS` and `__release_notes ${last_release}` word-split that into separate words, then only the first word is kept. The git range is `${baseline}..` (through `HEAD`). Changelog cherry-picks are not passed into `__get_release_notes_commits`, so they are not excluded. Rollbacks matching `^Rollback of commit <hash>.$` are excluded. `RELNOTES` / `RELNOTES[NEW]` / `RELNOTES[INC]` lines are kept unless the text is `none`, `n/a`, `no`, or `.`.
  - Notes are stored with `eval "RELNOTES_${kind}+=(\"\${relnote_text}\")"`. A `"`, `\`, backtick, or `$` in the text is parsed again and can change or drop the note. Bash `=~` anchors to the whole string, so a multi-line `RELNOTES` body fails the match and is dropped. `fold -s -w 66` wraps on spaces and strips trailing spaces. Command substitution strips trailing newlines. `__trim_empty_lines` swaps newlines for form feeds, so a form feed in a note is corrupted, and lines starting with `#` are removed before the notes are written back.

### `scripts/testenv.sh`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel does not assign `PATH`, `TMPDIR`, or a bazel binary. It loads Bazel bash runfiles v3 (`RUNFILES_DIR`, `RUNFILES_MANIFEST_FILE`, `$0.runfiles`, `$0.runfiles_manifest`, `$0.exe.runfiles_manifest`), then sources `io_bazel/src/test/shell/unittest.bash` through `rlocation`.
  - `set_up` makes `$TEST_TMPDIR/workspace`, `cd`s there, and touches `WORKSPACE`. `tear_down` deletes `$TEST_TMPDIR/workspace/*`. `TEST_TMPDIR` is assumed to already exist.
  - Zel has no `scripts/testenv.sh`. No zel test script sources that helper, initializes runfiles v3, loads `unittest.bash`, or creates `$TEST_TMPDIR/workspace`.
  - Zel harness scripts use their own temps (`mktemp` under `${TMPDIR:-/tmp}`, `ZEL_HARNESS_DIR`, `LOCKCASES_DIR`) and a separate bazel path (`BAZEL_BIN` or `/tmp/bazel-9.3.2`), not this setup. `crates/zel-bazel-tools/resources/tools/build_defs/pkg/testenv.sh` matches `tools/build_defs/pkg/testenv.sh`, not this file.

### `scripts/url-for-sheriff.sh`
- Bazel lines: 34
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel prints one GitHub issues search URL for `bazelbuild/bazel`. There is no commit range. The query is open issues, excluding `type: documentation`, `Under investigation`, and every repo label whose name contains `category:`.
  - Those category names come from `https://api.github.com/repos/bazelbuild/bazel/labels`. In each name, `:` becomes `%3A` and `+` becomes `%2B`, then the script appends the literal fragment `%20-label%3A"<encoded>"`. Spaces inside a category name are left raw.
  - The fixed query uses `%3A` for `:` and `%20` for spaces, keeps the label quotes as raw `"`, and leaves `utf8=✓` as a raw check mark: `https://github.com/bazelbuild/bazel/issues?utf8=✓&q=is%3Aopen%20-label%3A"type%3A%20documentation"%20-label%3A"Under investigation"` plus the dynamic exclusions.
  - Zel has no sheriff script and no CI-link helper. The only hit is the filename listed in a patch-corpus fixture.
