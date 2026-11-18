# Bazel vs zel code differences: native client platform

Category rule: `src/main/cpp/`, `src/main/native/`, `src/main/tools/`, main protos, and repository-root files under `src/` that bootstrap or package Bazel.

- Failure entries: 128
- Missing: 52
- Partial: 76
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `src/CheckSunJnuEncoding.java`
- Bazel lines: 24
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The Java main requires `sun.jnu.encoding` to be exactly `UTF-8` (`"UTF-8".equals(...)`, case-sensitive). On any other value, including a missing property, it writes `ERROR: sun.jnu.encoding is not UTF-8: ` plus that value (`null` if unset) to stderr and `System.exit(1)`. A match prints nothing and the JVM exits 0. `src/upload_all_java_tools.sh` runs this before the java-tools build.
  - [P3] Zel is a native Rust binary with no JVM. Nothing reads `sun.jnu.encoding` or exits 1 when the locale is not UTF-8.
  - [P2] `crates/zel-vfs/src/lib.rs` stores every path in `PathFragment` as a Rust `String` (valid UTF-8 only; unpaired surrogates cannot be represented). `LocalFileSystem` passes that string to `std::path::Path` and does not transcode with the locale or platform code page. `readdir` (`file_name().to_string_lossy()`) and `read_symbolic_link` (`read_link` then `to_string_lossy()`) replace non-UTF-8 bytes with U+FFFD, so those names do not round-trip.
  - [P2] `crates/zel-client/src/main.rs` takes `std::env::args()` (invalid UTF-8 arguments panic on Unix) and turns the cwd into a string with `to_string_lossy()`.
  - [P3] A non-UTF-8 locale diverges from this check: Java fails with exit 1, while Zel keeps using UTF-8 path strings and still exits 0. `crates/zel-server/src/commands/info.rs` only prints a synthetic `sun.jnu.encoding` from `LC_ALL` / `LC_CTYPE` / `LANG` for `character-encoding`; it does not enforce UTF-8.

### `src/bazel_binary.bzl`
- Bazel lines: 69
- Zel counterpart: `/workspace/zel-cursor/crates/BUILD.bazel` (`rust_binary` `zel`) and `/workspace/zel-cursor/scripts/package_zel.sh`
- Verdict: partial
- Differences:
  - [P3] The rule concatenates a `client` with a `package_zip` using `//src/tools/simple_catter:simple_catter`, writes `<out basename>.unaligned`, then runs `//src/java_tools/singlejar/java/com/google/devtools/build/zip:adjust_sfx` to produce the mandatory `out`. Zel compiles one Rust binary and copies it. There is no client/zip concatenation and no SFX adjustment.
  - [P3] The embedded JDK is not chosen in this file. Callers in `/workspace/bazel/src/BUILD` pass distinct zips: `package-zip_jdk_allmodules`, `package-zip_jdk_minimal`, `package-zip_nojdk`, and `package-zip_notools`, built from a platform `select` of vanilla OpenJDK (`embedded_jdk_vanilla`) minimized to `minimal_jdk.zip` or `allmodules_jdk.zip`. `package_zel.sh` writes `bazel-…` and `bazel_nojdk-…` as identical copies of `zel`. There is no embedded JDK payload.
  - [P3] The launcher is the separate input `client` (`//src/main/cpp:client` at the call site), prepended as the self-extracting stub. Zel’s launcher is `rust_binary` `zel` (`crates/zel-client/src/main.rs`). Windows `.exe` in Bazel is a later copy of that same stub (`bazel.exe`, `bazel-dev.exe`, `bazel_notools.exe`, `bazel_nojdk.exe`). Zel’s `copy_file` `zel.exe` is unconditional, and `package_zel.sh` adds `.exe` only when the OS is Windows.
  - [P3] This rule does not stamp. `package_zip` only has `dev_build` (fast compression for the `-dev` / all-modules zip). Zel stamps at `cargo build` time with `ZEL_EMBED_LABEL`, falling back to `BAZEL_EMBED_LABEL`, `.bazelversion`, or `0.0.1`, and writes a `.sha256` next to each artifact.
  - [P3] Bazel output names from the rule are `bazel`, `bazel-dev`, `bazel_jdk_allmodules`, `bazel_jdk_minimal`, `bazel_notools`, and `bazel_nojdk`. Zel release names are `zel-<version>-<os>-<arch>[.exe]`, `bazel-<version>-<os>-<arch>[.exe]`, and `bazel_nojdk-<version>-<os>-<arch>[.exe]` (os/arch from `uname` or `--os`/`--arch`: linux/darwin/windows, x86_64/arm64). Missing relative to Bazel: `bazel-dev`, `bazel_jdk_allmodules`, `bazel_jdk_minimal`, `bazel_notools`.
  - [P3] No `select()` in this file. The JDK and tool `select`s are in `/workspace/bazel/src/BUILD`. Zel’s `rust_binary` `zel` and `package_zel.sh` implement none of them.
  - [P3] Transition Zel does not implement: `_cat_binary` and `_adjust_sfx` use `cfg = "exec"`. There is no custom Starlark `transition()`.

### `src/combine_derived_java_srcs.sh`
- Bazel lines: 44
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel extracts each source jar with `$JAVABASE/bin/jar xf` into a temp tree at `derived/src/java`, then packs only regular files.
  - [P3] The archive is Info-ZIP `zip -qDX@`: quiet, no directory entries (`-D`), extra fields stripped (`-X`), paths read from stdin (`-@`) in `find . -type f | sort` order.
  - [P3] Every path under the temp tree is `touch -t 198001010000.00` before zipping, so entry mtimes are 1980-01-01 00:00:00.
  - [P3] No charset conversion and no `-UN`/`-UTF8`. `jar xf` writes entry names as stored, and `zip` records those path bytes as-is.
  - [P3] Zel has no generated-Java or srcjar-merge step. The counterpart gap is hand-written Rust protobuf codecs in `/workspace/zel-cursor/crates/zel-proto/src/lib.rs`.

### `src/create_embedded_tools.py`
- Bazel lines: 149
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/src/lib.rs` (with `/workspace/zel-cursor/crates/zel-bazel-tools/build.rs` and `/workspace/zel-cursor/scripts/sync_bazel_tools.sh`)
- Verdict: partial
- Differences:
  - [P3] Language: Bazel builds the archive in Python with `zipfile.ZipFile` (`ZIP_DEFLATED`, `ZipInfo`, `writestr`). Zel compiles every file in with Rust `include_bytes!` and `materialize` writes those bytes with `std::fs::write`. There is no zip.
  - [P1] Paths: Bazel’s `output_paths` rewrites `MODULE.tools` to `MODULE.bazel`, `BUILD.tools` / `BUILD.repo` to `BUILD`, `*.bzl.tools` by dropping `.tools`, `tools/platforms/*` to `platforms/<basename>`, launcher / def_parser / zipper binaries and `xcode-locator` onto fixed tool paths, OpenJDK and `minimal_jdk` archives to `jdk.tar.gz` / `jdk.zip`, and strips a leading `bazel-out/<config>/bin/`. A duplicate output path is a `ValueError`. A `BUILD` is skipped when `BUILD.tools` is also in the argsfile. Zel’s sync does `MODULE.tools` to `MODULE.bazel` and `BUILD.tools` / `BUILD.repo` to `BUILD` (plain `BUILD` skipped when either sibling exists), then copies extra trees (`src/conditions`, `cpp/util`, native `BUILD`, launcher sources, def_parser, protobuf, a fixed ijar list) and rewrites `tools/zip/BUILD` to `//third_party/ijar:zipper`. It does not apply the binary, JDK, `platforms/`, or `bazel-out` rewrites. `build.rs` keeps repo-relative paths (`\` to `/`), sorted. `make_bazel_tools` then writes `BUILD.bazel` and deletes the materialized root `BUILD`.
  - [P2] Timestamps: every Bazel `ZipInfo`, including the synthetic `WORKSPACE` and JDK members, is `(1980, 1, 1, 0, 0, 0)`. Zel stores no archive timestamp; `materialize` leaves the write’s filesystem mtime.
  - [P1] Modes and `WORKSPACE`: Bazel sets `external_attr` to `0o755<<16` when `is_executable`, otherwise `0o644<<16`, and always adds `WORKSPACE` (`0o644`) containing `workspace(name = "bazel_tools")\n`. Zel does not copy the source mode and does not emit that `WORKSPACE`.
  - [P3] Compression: Bazel deflates the whole zip. Zel keeps the bytes uncompressed in the binary and on disk.
  - [P2] Exclusions: this Python file only drops a `BUILD` shadowed by `BUILD.tools`, and for `jdk.tar.gz` / `jdk.zip` it extracts members under `jdk/` (first path component renamed) instead of storing the archive. Zel’s sync `find` drops `intellij`, `distributions`, coverage javatests, `mirror_external_deps.py`, `upload_bazel_mirror.sh`, `*.md.tmpl`, `*_test` / `*_test.*` / `*_tests.*`, and `.DS_Store`; `cpp/util` is `*.cc` / `*.h` / `BUILD` outside `testing` and `*_test.cc`; native is a top-level `BUILD` only; ijar is an explicit file list; builtins are `*.bzl` and `BUILD`. `build.rs` then embeds every remaining file under `resources/` and `builtins/`. There is no JDK extract into `jdk/`.
  - [P1] Platform filters: the Python script has no OS branch; host binaries are included only when the argsfile contains them (`launcher.exe`, `launcher_maker.exe`, `def_parser.exe`, `zipper` / `zipper.exe`, `xcode-locator`). Zel embeds those tools’ sources on every build (`tools/osx/xcode_locator.m` and the stub, launcher sources) and does not package the prebuilt host executables or a `platforms/` tree. The install base (`resolve_install_base`, `install_base_key`) is a digest-named directory, and `@bazel_tools` is written under the output base or fetch dir (`external/bazel_tools`), while some commands still look for `install_base/embedded_tools/jdk`.

### `src/create_embedded_tools_lib.py`
- Bazel lines: 94
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/src/lib.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel copies tar and zip members into a zip in memory. Zel never builds that archive: `build.rs` `include_bytes!`s `resources/`, and `materialize` writes plain files with `std::fs::write`.
  - [P2] Zip entries: regular files get `0o755<<16` or `0o644<<16`; symlinks get `0o120000<<16` with the link target as the payload. Directories, hard links, and special files are dropped. Zel walks only regular files, creates parent directories on disk, and stores no symlink or directory entries.
  - [P3] Every `ZipInfo` timestamp is forced to `(1980, 1, 1, 0, 0, 0)`. Zel sets no archive timestamp; the written files take the process mtime.
  - [P3] Tar members are always `ZIP_DEFLATED`. Zip members keep the source `compress_type`. Zel does not deflate the embedded-tools payload.
  - [P3] Path rewriting here is only the optional `process_filename` callback (the JDK `jdk/` prefix lives in `create_embedded_tools.py`). Zel rewrites names in `/workspace/zel-cursor/scripts/sync_bazel_tools.sh` (`BUILD.tools` / `BUILD.repo` to `BUILD`, `MODULE.tools` to `MODULE.bazel`) and does not unpack a JDK tarball or zip under `jdk/`.
  - [P3] This file has no name exclusions. Zel’s sync script drops `intellij`, `distributions`, CoverageOutputGenerator javatests, `*_test*`, `*.md.tmpl`, and a few scripts, and it vendors extra trees (`src/conditions`, cpp/util, launcher, def_parser, protobuf, ijar).
  - [P1] Executable bits: any of `S_IXUSR|S_IXGRP|S_IXOTH` becomes `0755`, otherwise `0644` (exact modes such as `0700` are not kept). Zel’s materialize does not chmod; `exec_phase.rs` notes that and later forces `0755` only for `collect_cc_coverage`.
  - [P3] No platform filter in this file. Zel’s sync also copies Unix and Windows sources together (`mapped_file_unix.cc`, `mapped_file_windows.cc`, `tools/launcher`, `tools/test/windows`).
  - [P3] Python `zipfile.ZipFile` / `ZipInfo.external_attr` has no Rust writer counterpart. Zel’s zip code in `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` only reads and extracts archives.

### `src/create_java_tools_release.sh`
- Bazel lines: 165
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The script does not compile. It promotes already-built `java_tools` zips from `gs://bazel-mirror/bazel_java_tools` (`gcloud storage cp`), refusing to overwrite an existing object.
  - [P3] Flags: `--java_tools_version`, `--rc`, `--release` (`true` promotes an RC to a release; `false` creates an RC and requires `--commit_hash`).
  - [P3] Platforms: `linux`, `linux_aarch64`, `windows`, `darwin_x86_64`, `darwin_arm64`, plus a platform-agnostic `java_tools` zip and a matching sources zip.
  - [P3] RC objects: `release_candidates/java/v${version}/java_tools_${platform}-v${version}-rc${rc}.zip` (and `.../sources/java_tools-v${version}-rc${rc}.zip`), copied from `tmp/build/${commit_hash}/java/` and `tmp/sources/${commit_hash}/java/`.
  - [P3] Release objects: `releases/java/v${version}/java_tools_${platform}-v${version}.zip` and `releases/java/v${version}/sources/java_tools-v${version}.zip`.
  - [P3] URLs printed as JSON (`version`, `release`, per-artifact `sha`): mirror `https://mirror.bazel.build/bazel_java_tools/...`; on a real release also GitHub `https://github.com/bazelbuild/java_tools/releases/download/java_v${version}/...`. RC JSON has only the mirror URL.
  - [P3] Zel has no java-tools release or equivalent packaging. Zel is a Rust binary. Java builds consume upstream `remote_java_tools` zips. `/workspace/zel-cursor/.github/workflows/release.yml` and `/workspace/zel-cursor/scripts/package_zel.sh` publish the `zel` binary (linux, darwin, windows) to a GitHub Release, not `java_tools` zips to GCS, mirror.bazel.build, or bazelbuild/java_tools.

### `src/embedded_tools.bzl`
- Bazel lines: 30
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools` (`src/lib.rs`, `build.rs`; seeded by `/workspace/zel-cursor/scripts/sync_bazel_tools.sh`)
- Verdict: partial
- Differences:
  - [P3] Bazel’s only rule here, `srcsfile`, writes a newline-joined list of configured `ctx.files.srcs` paths (`f.path`) to a mandatory `attr.output`. `//src/BUILD` runs that three times (`_jdk_allmodules`, `_jdk_minimal`, `_nojdk`) and feeds the params file to `create_embedded_tools` to produce `embedded_tools*.zip`. Zel has no `srcsfile` and no params file.
  - [P2] The file sets those rules list are configuration-specialized `filegroup`s: `JAVA_TOOLS`, `BUILD.tools` / `MODULE.tools`, conditions, C++ util, native, protobuf, launcher sources, GPL, def_parser, ijar zipper plus sources, Abseil, and `//tools:embedded_tools_srcs`, plus a JDK archive. Zel’s sync copies `//tools` (with test/distribution exclusions), `BUILD.tools`→`BUILD`, conditions, a subset of `cpp/util`, native `BUILD` only, launcher, def_parser, protobuf, a fixed ijar file list, and `builtins_bzl`. It omits java_tools, the zipper binary, Abseil, GPL, and any JDK archive, and rewrites `tools/zip/BUILD` to compile zipper from the ijar sources.
  - [P3] Platform `select()`s never run for this payload. Bazel adds `//tools/osx:xcode_locator.m` only for `//src/conditions:darwin`, and `//src/tools/launcher`, `:launcher_maker`, and `//third_party/def_parser` only for `//src/conditions:windows`. `embedded_jdk_vanilla` selects an OpenJDK repo for `darwin_x86_64`, `darwin_arm64`, `windows_x86_64`, `windows_arm64`, `linux_aarch64`, `linux_ppc64le`, `linux_s390x`, `linux_riscv64`, or Linux by default. Zel embeds one host-agnostic tree.
  - [P2] Archives: Bazel builds three zips, then `package_zip` (sibling rule) may omit the zip for `_notools`. `create_embedded_tools.py` rewrites paths (`BUILD.tools`→`BUILD`, `platforms/`, launcher/zipper/xcode binaries), extracts the JDK archive under `jdk/`, writes `WORKSPACE` with `workspace(name = "bazel_tools")`, and stores 1980-01-01 timestamps with `0755`/`0644`. Zel `include_bytes!`s the tree and `materialize`s it to `output_base/external/bazel_tools`. The client only writes `install_base/install_base_key`. There is no embedded JDK and no `WORKSPACE` in `resources/`.
  - [P3] Select/transition behavior this packaging needs and Zel does not apply here: `ctx.files` is the post-`select` file set of the binary’s target configuration (specialization and ambiguous-match handling exist in `/workspace/zel-cursor/crates/zel-analysis/src/select.rs` but are not used to build these targets). The zipper in `package_zip` is `cfg = "exec"`, and `create_embedded_tools` runs as a genrule tool in the exec configuration. Zel does not configure or exec-transition a zipper or Python tool to assemble this archive, and it does not split one rule into the JDK / no-JDK variants.

### `src/java_opt_binary.bzl`
- Bazel lines: 24
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel wraps `@rules_java` `java_binary` with `with_cfg(...).set("compilation_mode", "opt")`, exporting the transitioned rule `java_opt_binary` and the raw rule `_java_opt_binary`. The only transition is compilation mode `opt` (benchmarks). It adds no JVM flags and no outputs of its own; those stay on the wrapped `java_binary`.
  - [P1] Zel is a Rust binary. It has no `java_opt_binary`, no `with_cfg` opt wrapper, and no Java launcher. Native `java_binary` / `java_test` are not implemented in `analyze_rule` (`/workspace/zel-cursor/crates/zel-rules/src/lib.rs`). `--java_launcher` and `--host_java_launcher` are option-table entries only. Zel does track `compilation_mode` (including an exec config hardcoded to `opt`), which is not this rule.

### `src/main/cpp/archive_utils.cc`
- Bazel lines: 333
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs` and `/workspace/zel-cursor/crates/zel-client/src/lib.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel opens the client binary as a zip and scans until a named member. Open, read, and missing-member failures die. Directory members are skipped. Zel never opens the binary as a zip.
  - [P2] `DetermineArchiveContents` sets `install_md5` to the raw bytes of the zip member `install_base_key` and fills the file list only through that member. `GetServerJarPath` returns the first listing and dies if it is empty, without checking that the name is a jar. Zel’s `install_md5` is an MD5 of the whole executable. `ZEL_INSTALL_MD5_OVERRIDE` wins with no length or hex check. A side cache at `<temp>/zel-install-md5-<USER>/<md5(path|size|mtime)>` is reused when the file is exactly 32 characters; errors are ignored. A failed read falls back to MD5 of the build label.
  - [P2] If Bazel’s install base exists and is not a directory, it is removed. Each archive-relative path is checked with `IsUntampered`. A mismatch of `install_base_key` logs a different product version and does not die; the tree is then removed. Removal failure dies. Zel never walks extracted files, never compares an on-disk key before reuse, and never deletes a bad install.
  - [P2] Bazel installs into a sibling temp directory, then renames it onto the install base. `kRenameDirectoryFailureNotEmpty` means another install won and is not rechecked. Other rename failures retry once a second, up to 120 attempts, then die. Zel creates the final directory in place.
  - [P2] `ExtractArchiveOrDie` requires the output directory to already exist and dies with `INTERNAL_ERROR` if it does not. A key that differs after extraction dies, quoting old and new MD5. Zel writes only `install_base_key` and discards both `create_dir_all` and `fs::write` results. Directories are `0o777` masked by umask and the key file is `0o666` masked by umask. There is no chmod and no executable bit.
  - [P3] `BlessFiles` sets every extracted file’s mtime to the distant future and dies on failure, then syncs each file and each parent up to the install root. Zel does not change mtimes or fsync the install tree.
  - [P2] Bazel’s install concurrency is the temp directory plus atomic rename. Zel, when `--lock_install_base` is on, takes a shared `flock` on `<parent>/<install_basename>.lock` for the whole daemon session. Open or lock failure is `LOCAL_ENVIRONMENTAL_ERROR`. If the path has no parent, the lock is skipped.
  - [P3] `ExtractBuildLabel` reads the whole `build-label.txt` member and dies if it is absent. Zel’s build label is a compiled-in string.
  - [P0] A live server is stale in Zel only when `read_link(<output_base>/install)` succeeds and the link text differs. A missing symlink does not force a restart. Bazel decides sameness from `install_base_key` contents.

### `src/main/cpp/archive_utils.h`
- Bazel lines: 111
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel declares the client installer in this header. Zel has no `archive_utils.h`. Under `src/main/cpp` it only vendors `util/*`.
  - [P3] `DetermineArchiveContents` lists the embedded binary archive and returns the install MD5 key. Zel has no function that walks an embedded archive that way.
  - [P3] `RestartReason` (`NO_RESTART`, `NO_DAEMON`, `NEW_VERSION`, `NEW_OPTIONS`, `PID_FILE_BUT_NO_SERVER`, `SERVER_VANISHED`, `SERVER_UNRESPONSIVE`) is defined here. Zel has no matching enum.
  - [P3] `LoggingInfo` stores `binary_path`, `start_time_ms`, and `restart_reason`, and only the first `SetRestartReasonIfNotSet` sticks.
  - [P3] `ExtractData` skips unpacking when the install base already exists and calls `ExtractArchiveOrDie` plus `BlessFiles`. Zel only derives the install base from `binary_install_md5()` and writes `install_base_key`.
  - [P3] `BlessFiles` sets distant-future mtimes. `ExtractBuildLabel` reads the version string out of the archive. `GetServerJarPath` picks the server jar from the listing. Zel’s server is native and its label is compiled in.
  - [P3] `archive.rs` is a repository decompressor. Its public surface shares no signatures with this header.

### `src/main/cpp/bazel_startup_options.cc`
- Bazel lines: 111
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/startup.rs`
- Verdict: partial
- Differences:
  - [P2] Product name is the constructor constant `"Bazel"`. The parent lowercases it only for the output directory (`_bazel_<user>`). The client then passes `--product_name=Bazel` to the server. Zel’s `product_name` defaults to lowercase `"bazel"` and is a parsed startup flag. `server_args` still hardcodes `--product_name=Bazel`, and the output path is always `_bazel_<user>`, so a non-default product name does not change the directory.
  - [P2] `lock_install_base` is fixed true and is not user-settable. Zel’s flag-table default is false. The client locks only because a missing flag is treated as true, and `server_args` does not add `--lock_install_base` unless the flag was actually parsed.
  - [P2] `GetDefaultOutputRoot()` is `GetCacheDir()`, then `<that>/_<product>_<user>`. `TEST_TMPDIR` replaces the cache dir when that variable exists, including when it is empty. Zel skips an empty `TEST_TMPDIR`, never consults the passwd database, and if `USER` is unset substitutes `"bazel"` instead of failing.
  - [P2] Linux cache dir matches `$XDG_CACHE_HOME/bazel` or `$HOME/.cache/bazel`, but Bazel falls back to `getpwuid()`’s home before `/tmp`. Zel goes straight to `/tmp/_bazel_<user>` when `HOME` is missing.
  - [P2] macOS prefers `XDG_CACHE_HOME`, then `~/Library/Caches`, then the passwd home, then `/var/tmp`. Zel’s Darwin branch ignores `XDG_CACHE_HOME` and the passwd entry.
  - [P1] Windows returns the home directory, so the output user root is `<home>\_bazel_<user>`. Zel uses `%LOCALAPPDATA%\bazel\_bazel_<user>`, then `%USERPROFILE%\AppData\Local\bazel\_bazel_<user>`, then `C:/temp/_bazel_<user>`.
  - [P2] BSD uses `XDG_CACHE_HOME` or the passwd `~/.cache`, not `$HOME`. Zel treats BSD like Linux and uses `$HOME/.cache`.
  - [P1] `--home_rc`, `--system_rc`, and `--workspace_rc` are illegal inside an rc file. Zel accepts them from rc files. Only `--bazelrc` is rejected.
  - [P2] Bazel warns that an explicit `--home_rc`, `--system_rc`, or `--workspace_rc` is ignored when `--ignore_all_rc_files` is set and the flag is still true. Zel warns only for a non-empty `--bazelrc` list.
  - [P2] A single `user_bazelrc_` keeps the last `--bazelrc`. Zel stores every value and warns if that vector is non-empty, including when the last value would have been empty.

### `src/main/cpp/bazel_startup_options.h`
- Bazel lines: 52
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/startup.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel declares `BazelStartupOptions : StartupOptions`. Zel has one `StartupOptions` struct that also holds the generic client fields. There is no Bazel-only subclass.
  - [P1] Bazel keeps private `use_system_rc`, `use_workspace_rc`, and `use_home_rc` and registers them with `RegisterNullaryStartupFlagNoRc`, so an rc file is rejected. Zel’s public fields also default true, but they are ordinary startup booleans and are applied from a `startup` section.
  - [P1] Bazel stores one `user_bazelrc_` string. `GetAllUnaryOptionValues(..., "/dev/null")` drops every later `--bazelrc`. Zel stores a `Vec` and skips a `/dev/null` entry without stopping later ones.
  - [P2] With `--ignore_all_rc_files`, Bazel warns for a non-empty user rc and for an explicit home, system, or workspace rc whose bool is still true. Zel warns only when `bazelrc_files` is non-empty.
  - [P3] `GetRcFileBaseName()` returns `".bazelrc"`. Zel discovery hardcodes `.bazelrc`. The comment in `rc.rs` mentions `.zelrc`; the loader does not add it.
  - [P2] The constructor is `StartupOptions("Bazel", lock_install_base=true)`, and the C++ client never registers `lock_install_base` as a flag. Zel’s `product_name` defaults to `"bazel"`. `lock_install_base` is a parsed flag (table default `"false"`, client fallback `true`), so `--nolock_install_base` can turn the lock off.

### `src/main/cpp/blaze.cc`
- Bazel lines: 2226
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/lib.rs`, `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs`, `/workspace/zel-cursor/crates/zel-client/src/main.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel `Main` initializes UTF-8 stdio, handles a bare `leaf` argument by printing the basil-leaf art, and on exactly `argv[1] == "--version"` prints the product and build label and exits before the workspace lookup. It warns and exits 36 if started from a desktop with no args, raises rlimits, and on Windows exports `BAZEL_SH`. Outside a workspace, only `canonicalize-flags`, `dump`, `help`, `license`, `shutdown`, and `version` are allowed (exit 2 otherwise, including `info`), and `batch` is forced on. It then extracts the install archive and creates a mode-0777 output base, canonicalizes it, and excludes it from backup. Zel has no `leaf` path, no desktop check, no rlimit setup, and no archive extraction. It first runs Bazelisk. Its `--version` fast path is after `split_command_line` and prints a different product line. Workspace markers add `MODULE.zel` and `WORKSPACE.zel`. An empty command inside a workspace is rewritten to `help`. An unknown command is not rejected in the client, and `batch` is not forced. `prepare_directories` does not canonicalize the output base or mark it no-backup. There is no JVM: no `--add-opens`, `-Dfile.encoding=ISO-8859-1`, JDWP port 5005, or `LD_PRELOAD` scrubbing.
  - [P0] Bazel takes a blocking shared `flock` on `<install_base>.lock` when `--lock_install_base` is set, then an exclusive lock on `<output_base>/lock`. `--noblock_for_lock` fails immediately with exit 9. Wait time is sent as `--command_wait_time`. Both locks are released only after the `Run` RPC has started. Batch mode keeps them across `exec` of the JVM. Zel takes the same two locks only on the daemon path (500 ms poll, prints the lock-file owner, same exit 9). It writes `pid` / `owner=client` / `cwd` into the lock file, records no wait duration, drops the output-base lock before `run_command`, and holds the install lock until the client returns. `--batch` takes neither lock.
  - [P0] Bazel connects by reading `server/server_info.rawproto`, requiring a localhost address (`127.0.0.1:`, `[::1]:`, or `[0:0:0:0:0:0:0:1]:`), verifying the pid still belongs to this output base, and Ping-ing with cookies. Startup retries every 100 ms up to 120 s, prints a “still trying” line every 10 s, and on death dumps `jvm.out` and exits 37. A running server is killed when `output_base/install` is missing or not the current install base, or when non-volatile startup args differ. Vector length mismatch alone forces a restart, so `--max_idle_secs` still restarts the server. Kill is a `Run("shutdown")` RPC, then up to 60 s, then `SIGKILL`. `shutdown` with nothing to connect to returns 0. If the server cwd moved or contains `" (deleted)"`, it is restarted. `exec-server` replaces this process with the JVM and refuses `--batch`. Zel discovers the server with a 3 s Ping, treats a missing `install` symlink as fresh, and never creates that symlink. Restart ignores vector length, so `--max_idle_secs` does not restart. Kill is `SIGTERM`, then `SIGKILL` after 5 s. There is no pid-to-output-base check, no IPv6 address check, and no `exec-server`. A startup timeout exits 36, not 37. Batch mode does not kill an existing daemon.
  - [P1] Bazel `Communicate` sends `--startup_time`, optional `--command_wait_time` / `--extract_data_time` / `--restart_reason`, and `--binary_path`. Each startup option is appended twice, the second time empty. A bad cookie exits 37; a broken pipe cancels and exits 36. `SIGINT`/`SIGWINCH` go through a pipe. Three interrupts are `SIGKILL` of the server. An abrupt RPC checks the custom exit-code file, then the OOM file (exit 33), else 37. Zel puts `--client_cwd` only on build/test/run/coverage/query/cquery/aquery, and `--isatty`/`--terminal_columns` only on build/test/run/coverage. It does not send startup-time, restart-reason, or binary-path args. The initial cookie is the literal `batch-cookie`. `SIGINT`/`SIGTERM` share one counter; the third signal `SIGKILL`s the server and `exit(1)`. Abrupt disconnect always exits 37, with no OOM or custom exit-code file. After `termination_expected` it waits 10 s and does not kill. `perform_exec_request` substitutes `PathToReplace` and writes `--script_path`. The client also writes `--failure_detail_out` itself.

### `src/main/cpp/blaze.h`
- Bazel lines: 40
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/lib.rs`
- Verdict: partial
- Differences:
  - [P3] Zel has no `blaze.h` and no `namespace blaze`.
  - [P2] `PrintVersionInfo` is not a separate function. `execute_cli_with_exec` prints the version only when the parsed command is `version` and the args contain `--startup_version`.
  - [P3] `blaze::Main(argc, argv, WorkspaceLayout*, OptionProcessor*, StartupInterceptor*, CommandExtensionAdder*, uint64_t start_time)` is replaced by `execute_cli` / `execute_cli_with_exec(...) -> i32`. Those collaborators and `start_time` are not parameters.
  - [P3] `RunRequest.command_extensions` is always an empty `Vec`. There is no `CommandExtensionAdder` and no `StartupInterceptor`.
  - [P3] `main.rs` may then `perform_exec_request`. That exec handoff is not part of the `blaze::Main` declaration.

### `src/main/cpp/blaze_util.cc`
- Bazel lines: 295
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/startup.rs`, `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs`
- Verdict: partial
- Differences:
  - [P3] Option matching is raw bytes. `var_strprefix` stores `strlen` in an `int`, so a prefix longer than `INT_MAX` makes the compare length huge. An embedded NUL ends the key. There is no UTF-8 decode and no case folding.
  - [P2] `GetUnaryOption` accepts only `key` (value is the next argv) or `key=value`. A final `--key` with no value records the flag token itself as the value. `GetAllUnaryOptionValues` stops at a bare `--` and at the first value equal to `ignore_after_value`. Zel only accepts names in the option tables and does not treat a missing final value as the flag string.
  - [P2] `GetNullaryOption` dies with `BAD_ARGV` on `key=<anything>`. `SearchNullaryOption` last-wins and does not skip values of earlier unary flags. Zel maps `--no<bool>` to `"false"` only for known boolean options. Unknown `--no…` stays an unknown option.
  - [P2] `IsArg` is true for every string that starts with `"-"` except `--help`, `-help`, and `-h`. `-help` is therefore the command. Zel treats `-help` as a startup token and fails it as an unknown option.
  - [P1] `AbsolutePathFromFlag`: empty string is the cwd; a leading `~` is home plus the remainder with no username lookup. `~foo` becomes `HOME/foo`. If `HOME` is unset, `JoinPath` returns the suffix. Zel does not expand `~`. `--output_base=~/x` stays the relative fragment `~/x`.
  - [P1] `AwaitServerProcessTermination` polls every 100 ms. On Linux that is pid plus `/proc` start time. A backwards monotonic clock becomes a huge elapsed value and the wait fails immediately. `wait_seconds * 1000` wraps mod 2^32 before the `uint64` compare. `SIGINT`, `SIGTERM`, and `SIGQUIT` abort the wait, and a false return is treated as “still alive” and then killed. Grace is 60 s after shutdown and 10 s after a forced kill. Zel polls every 20 ms for a fixed 10 s, does not install those handlers, and `kill_server` is SIGTERM, 5 s, SIGKILL, 5 s. Zel does not write `server.starttime`.
  - [P2] `IsRunningWithinTest` is true when `TEST_TMPDIR` is set, including empty, and Bazel then defaults `max_idle_secs` to 15. Zel uses `TEST_TMPDIR` only when it is non-empty and keeps `max_idle_secs` at `3 * 3600`.
  - [P2] `GetOOMFilePath` is `output_base/blaze_oomed`. `GetAbruptExitFilePath` is `output_base/exit_code_to_use_on_abrupt_exit`. Neither name exists in Zel.
  - [P2] `WithEnvVars` restores previous values on drop and walks keys in lexicographic order. Zel keeps only pairs where both sides are valid UTF-8, so non-UTF-8 variables disappear.

### `src/main/cpp/blaze_util.h`
- Bazel lines: 170
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs` and `/workspace/zel-cursor/crates/zel-options/src/startup.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel exports one `blaze` header. Zel splits the same jobs across the client daemon helper and the options parser. There is no `blaze_util` module.
  - [P3] `kServerPidFile` is `"server.pid.txt"`. Zel uses that filename inline, with no shared constant.
  - [P1] `GetAllUnaryOptionValues(..., "--bazelrc", "/dev/null")` records `/dev/null` and then stops, so later `--bazelrc` values are dropped. Zel skips only the `/dev/null` entry and still loads later `--bazelrc` files.
  - [P1] `AbsolutePathFromFlag` treats `""` as the cwd and a leading `~` as home. Zel stores the raw startup path. `AbsolutePathFragmentConverter` accepts only strings that already start with `/`.
  - [P2] `AwaitServerProcessTermination` checks every 100 ms and logs at 5 s, 10 s, and 30 s. Zel polls every 20 ms for a fixed 10 s and does not log.
  - [P2] `kPostShutdownGracePeriodSeconds` is 60 and `kPostKillGracePeriodSeconds` is 10. Zel waits 10 s on shutdown and 5 s after SIGTERM and again after SIGKILL.
  - [P2] `SetDebugLog` switches the Abseil stderr threshold. Zel `--client_debug` only prints `INFO: connected to server pid=...`.
  - [P2] `IsRunningWithinTest` is true when `TEST_TMPDIR` is set, and Bazel then defaults `max_idle_secs` to 15. Zel keeps `max_idle_secs` at `3 * 3600`.
  - [P2] `GetOOMFilePath` and `GetAbruptExitFilePath` do not exist in Zel. An abrupt end is `RunOutcome.abrupt` from a closed gRPC stream.
  - [P3] `DurationMillis` returns 0 and logs when `end < start`. Zel has no matching duration type.

### `src/main/cpp/blaze_util_bsd.cc`
- Bazel lines: 248
- Zel counterpart: `none`
- Verdict: partial
- Differences:
  - [P2] Bazel builds this file only for FreeBSD and OpenBSD. Zel has no BSD process/path translation unit. `Os::FreeBsd` and `Os::OpenBsd` only name the host for rc config.
  - [P2] `GetCacheDir` uses `$XDG_CACHE_HOME/bazel`, or `getpwuid_r` `pw_dir/.cache/bazel`, or `/tmp`. It does not read `$HOME`. Zel uses `$TEST_TMPDIR`, then `$XDG_CACHE_HOME`, then `$HOME/.cache`, then `/tmp/_bazel_<user>`. The user is `$USER` or the literal `bazel`, with no passwd lookup.
  - [P2] `WarnFilesystemType` calls `statfs` and warns when `f_fstypename` is `nfs`. Zel never checks the output base filesystem type.
  - [P2] `GetSelfPath` on FreeBSD uses libprocstat and aborts if the process count is not 1. On OpenBSD it trusts an absolute `argv[0]`, prefixes a relative one with the cwd, otherwise searches `PATH`, and aborts with `BAD_ARGV`. Zel uses `current_exe()`. If that fails, the install digest falls back to the build label.
  - [P2] `GetProcessCWD` is implemented only on FreeBSD. Other BSDs return null, so Bazel skips the “server cwd moved” restart. Zel never reads another process’s cwd.
  - [P3] `GetSystemJavabase` accepts `$JAVA_HOME` only when `bin/javac` is executable, otherwise `/usr/local/openjdk8` on FreeBSD and `/usr/local/jdk-17` on OpenBSD. Zel checks `--server_javabase` for an executable `bin/java`, not `javac`.
  - [P2] `GetMillisecondsMonotonic` aborts on `clock_gettime` failure. Zel’s `JavaClock` is wall-clock `SystemTime` and turns a clock error into zero.
  - [P2] `VerifyServerProcess` is only `killpg(pid, 0) == 0`, so a reused pid can be treated as the server. Zel probes with `kill(pid, 0)`, treats `EPERM` as alive, and rejects `pid <= 0`.
  - [P2] `GetExplicitSystemLimit` always returns -1, so soft `RLIMIT_NOFILE` and `RLIMIT_NPROC` are raised to the hard limit with no sysctl cap. Zel only implements `--unlimit_coredumps`.

### `src/main/cpp/blaze_util_darwin.cc`
- Bazel lines: 307
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/startup.rs`, `/workspace/zel-cursor/crates/zel-server/src/sys.rs`, `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs`
- Verdict: partial
- Differences:
  - [P2] `GetCacheDir` returns `$XDG_CACHE_HOME/bazel`, otherwise `$HOME/Library/Caches/bazel`, otherwise the passwd home, otherwise `/var/tmp`. Zel never reads `XDG_CACHE_HOME` or the password database. `$USER` missing becomes the literal user `bazel`. Bazel’s `GetUserName` dies unless `getpwuid` succeeds.
  - [P2] `WarnFilesystemType` warns when `kCFURLVolumeIsLocalKey` is false. Zel never inspects whether the output base is on a non-local volume.
  - [P2] `GetSelfPath` ignores `argv0` and dies if `proc_pidpath` returns 0. Zel’s install digest falls back to hashing the build label instead of aborting.
  - [P2] `GetMillisecondsMonotonic` is `CLOCK_UPTIME_RAW` and a zero reading aborts. Zel’s shutdown wait is `Instant` plus a 20 ms sleep, 5 s for SIGTERM and another 5 s for SIGKILL, with no fatal clock failure.
  - [P3] `SetScheduling` is an empty Darwin stub. Zel’s batch and ionice setters are also no-ops on macOS.
  - [P1] `GetProcessCWD` uses `proc_pidinfo`. Bazel kills the server when that path differs from the workspace or contains `" (deleted)"`. Zel never reads another process’s cwd.
  - [P3] `IsSharedLibrary` is the `.dylib` suffix only, used to build the server JVM’s `java.library.path`. Zel has no JVM and no install-archive `.dylib` scan.
  - [P3] `GetSystemJavabase` accepts `$JAVA_HOME` only when `bin/javac` is executable, then runs `/usr/libexec/java_home -v 1.8+`. Zel does not consult `JAVA_HOME` or that helper.
  - [P2] `ConfigureDaemonProcess` sets a QoS class on the `daemonize` spawn. The C++ parser accepts only `utility` and `background`; `default` is `BAD_ARGV`. Zel maps `user-interactive`, `user-initiated`, `default`, `utility`, and `background` and ignores the return code. `macos_qos_class` is client-only and is not put on the daemon command line, so the setter never runs for a normal server.
  - [P2] `VerifyServerProcess` is `killpg(pid, 0) == 0`. `EPERM` counts as dead. Zel’s `process_alive` is `kill(pid, 0)`, treats `EPERM` as alive, and rejects `pid <= 0`.
  - [P2] `ExcludePathFromBackup` sets `kCFURLIsExcludedFromBackupKey`. Zel never sets the Time Machine exclusion attribute.
  - [P1] `GetExplicitSystemLimit` maps `RLIMIT_NOFILE` to `kern.maxfilesperproc` and `RLIMIT_NPROC` to `kern.maxprocperuid`. Zel’s macOS `sysctl` path only reads memory counters. Soft `NOFILE` and `NPROC` are never raised.

### `src/main/cpp/blaze_util_linux.cc`
- Bazel lines: 235
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/startup.rs`, `/workspace/zel-cursor/crates/zel-server/src/sys.rs`, `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs`
- Verdict: partial
- Differences:
  - [P2] `GetCacheDir` returns `$XDG_CACHE_HOME/bazel`, else `$HOME/.cache/bazel`, else the passwd home plus `.cache/bazel`, else `/tmp/bazel`. The caller appends `_bazel_$USER` from `getpwuid` when `$USER` is empty. Zel never reads the passwd database. With both `HOME` and `USER` unset, Zel uses `/tmp/_bazel_bazel`. Bazel’s no-home fallback is `/tmp/bazel/_bazel_$USER`. The suffix is hardcoded `_bazel_`, so a non-default product name still lands under `_bazel_`.
  - [P2] `WarnFilesystemType` warns when `f_type == NFS_SUPER_MAGIC` (`0x6969`). Zel never emits that NFS warning.
  - [P2] `GetMillisecondsMonotonic` aborts on `clock_gettime` failure. Zel uses `Instant` and continues if a clock read fails. It does not record a client-startup duration from this clock.
  - [P2] `SetScheduling` runs in the client before the daemon is spawned, so both change, and a failure aborts. Zel applies the same calls only inside the daemon and ignores their return values. The I/O syscall is issued only on x86_64 (`251`) and aarch64 (`30`); Bazel uses `SYS_ioprio_set` on every Linux architecture.
  - [P1] `GetProcessCWD` is `readlink("/proc/<pid>/cwd")`. A readable cwd that differs from the workspace, or that contains `" (deleted)"`, kills the server. Zel never reads `/proc/<pid>/cwd`.
  - [P3] `IsSharedLibrary` is a `.so` suffix used to build `-Djava.library.path`. Zel does not scan an install tree for shared libraries.
  - [P3] `GetSystemJavabase` uses `$JAVA_HOME` only when `bin/javac` is executable, otherwise `Which("javac")` and two `dirname`s. Zel checks `--server_javabase` for an executable `bin/java` and never passes `--default_system_javabase`.
  - [P1] Server identity is PID plus `/proc/<pid>/stat` field 22. A missing `server.starttime` still counts as a match. Zel writes only `server.pid.txt` and never writes `server.starttime`. Liveness is `kill(pid, 0)`, and `EPERM` counts as alive. A recycled PID can be signaled. A comm field that contains spaces shifts Bazel’s field index.
  - [P2] `ConfigureDaemonProcess` returns 0. Zel starts the daemon with `process_group(0)` and has no socketpair liveness pipe.
  - [P1] `GetExplicitSystemLimit` returns `-1`, so each soft rlimit is lifted to the hard rlimit. Zel’s `--unlimit_coredumps` sets `RLIMIT_CORE` to `RLIM_INFINITY` and ignores the hard limit and `setrlimit`’s result.

### `src/main/cpp/blaze_util_platform.h`
- Bazel lines: 322
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs`, `/workspace/zel-cursor/crates/zel-server/src/sys.rs`
- Verdict: partial
- Differences:
  - [P0] `AcquireLock` is a shared or exclusive lock on a caller path. It uses `fcntl` open-file-description locks with a POSIX fallback, sets `O_CLOEXEC` unless `batch_mode` is set so the lock drops on `exec`, recreates the file if `st_nlink` is 0, writes `pid`, `owner=client`, `cwd`, and `tty` for an exclusive lock, busy-waits 500 ms, and dies on I/O errors. Zel splits this into an exclusive `flock` on `<output_base>/lock` and a blocking shared `flock` on `<install_base>.lock`. The output-base record has no `tty` line. The lock is dropped before `run_command`, and `--batch` never takes it. A missing parent skips the install lock.
  - [P2] `ExecuteDaemon` `posix_spawn`s an embedded `daemonize` helper with an explicit env map, a pid file plus a legacy pid symlink, Linux cgroup flags, and a liveness socketpair. Zel `Command::spawn`s the same binary as `--internal_server_daemon`, inherits the environment, always appends to `jvm.out`, sets `process_group(0)`, and `mem::forget`s the child. There is no pid symlink, liveness socket, or cgroup.
  - [P3] `ExecuteServerJvm` and `ExecuteRunRequest` do not return: they `execv`. Zel `--batch` dispatches inside the client process. `perform_exec_request` substitutes `PathToReplace` and uses `Command::exec` on Unix. There is no in-process JVM exec.
  - [P1] `VerifyServerProcess` binds the pid to `server/server.starttime` so a recycled pid is rejected. Zel’s `process_alive` is `kill(pid, 0)`, and `EPERM` counts as alive.
  - [P1] `KillServerProcess` is `killpg(pid, SIGKILL)` and dies if the group survives the grace period. Zel `SIGTERM`s that pid, waits 5 s, then `SIGKILL`s, and deletes status files even if the process is still alive. The third-signal path exits 1. The comment says exit 8.
  - [P1] `SignalHandler` covers `SIGINT` (third signal kills the server and `_exit(1)`), `SIGTERM` (cancel only), `SIGPIPE` (re-raised so the parent sees `WTERMSIG`), `SIGQUIT` (forwarded to the server), and `SIGWINCH`. Zel installs only `SIGINT` and `SIGTERM`, counts both toward the third-signal kill, and polls every 25 ms. Rust ignores `SIGPIPE`. Columns are sampled once.
  - [P2] `GetSelfPath(argv0)` is `std::env::current_exe()` with no `argv0` or `PATH` fallback. Failure is `LOCAL_ENVIRONMENTAL_ERROR` instead of a fatal abort.
  - [P2] `CreateSecureDirectory` creates mode `0755`, requires the effective uid, strips group and other write, and excludes the path from backup. Zel creates the user root at `0755` and then chmods it to `mode & ~umask` (a `0700` directory becomes `0755`) and the output base at `0777` only when it is absent. A directory owned by someone else is reported as `mkdir` `EACCES`.
  - [P2] `embedded_binaries::Dumper` has no client equivalent. The client writes `install_base_key` only. `Which`, `IsEmacsTerminal`, `WarnIfStartedFromDesktop`, `SetupStdStreams`, and `DetectBashAndExportBazelSh` are absent. `--isatty` is raw `isatty(stderr)`. Bazel requires both stdout and stderr to be terminals and rejects `TERM` empty, `dumb`, `emacs`, `xterm-mono`, `symbolics`, and `9term`.
  - [P1] `UnlimitResources` raises soft `RLIMIT_NOFILE` and `RLIMIT_NPROC` to the hard limit. Zel only implements `--unlimit_coredumps`, which sets both core limits to `RLIM_INFINITY` and ignores failure.

### `src/main/cpp/blaze_util_posix.cc`
- Bazel lines: 904
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs`, `/workspace/zel-cursor/crates/zel-server/src/sys.rs`, `/workspace/zel-cursor/crates/zel-util/src/terminal.rs`
- Verdict: partial
- Differences:
  - [P2] The server is not started with the embedded `daemonize` helper, `posix_spawn`, a `socketpair` liveness pipe, or `ConfigureDaemonProcess`. Zel spawns itself as `--internal_server_daemon` (`process_group(0)`, stdin `/dev/null`, stdout and stderr appended to `jvm.out`) and treats the process as alive with `kill(pid, 0)` plus a gRPC ping. A zombie still counts as alive. Spawn failures are exit 36. A child that exits during startup is exit 37.
  - [P2] Linux `--experimental_cgroup_parent` and `--experimental_run_in_user_cgroup` are not applied. `SCHED_BATCH`, ionice, and macOS QoS run inside the daemon after it is up, not as spawn attributes.
  - [P2] `bazel run` uses `Command::exec()` (`execvp`) and reports exit 36 with `ERROR: Cannot execute …`. Bazel uses `execv` and dies with internal error 37. The server JVM is never exec'd.
  - [P2] The daemon inherits the client environment. Bazel builds a `SET`/`UNSET` env array and passes that to `posix_spawn`.
  - [P1] Stale-server kill is `kill(pid, SIGTERM)`, wait 5 s, then `SIGKILL`, then delete status files even if the process is still alive. Bazel `KillServerProcess` uses `killpg(pid, SIGKILL)` and aborts if the group does not die. The third interrupt `SIGKILL`s only that pid, so grandchildren in the server process group survive.
  - [P1] Signal handlers are installed only for the duration of a `Run` call, and only for `SIGINT` and `SIGTERM`. Bazel unblocks every signal for the whole client. Zel counts `SIGTERM` toward the third-signal kill. Rust ignores `SIGPIPE`. `SIGQUIT` is not forwarded. `SIGWINCH` is not observed.
  - [P2] `--isatty` is raw `isatty(stderr)`. Bazel `IsStandardTerminal` requires both stdout and stderr to be ttys and rejects `TERM` empty, `dumb`, `emacs`, `xterm-mono`, `symbolics`, and `9term`, with an `eterm-color` exception. `IsEmacsTerminal` (`EMACS=t` or `INSIDE_EMACS`) is absent, so `--emacs` is never set from those variables. Bazel ioctls stdout and accepts a parsed `$COLUMNS` including 0. Zel ioctls stderr, skips `ws_col == 0`, then `$COLUMNS` only if it is greater than 0, else 80.
  - [P1] Output-base locking is exclusive `flock`, retried every 500 ms, with exit 9 on `--noblock_for_lock`. Missing versus Bazel: open-file-description locks, shared `F_RDLCK`, the `O_CLOEXEC`-unless-batch choice, mode `0644` (Rust uses `0666` minus umask), the `nlink > 0` deleted-file check, the `tty=` owner line, a returned wait duration, and fatal errors for an unexpected lock errno. Any `try_lock` failure is treated as “lock held.” `--batch` does not take the lock. Bazel keeps the lock fd across the batch exec.
  - [P2] The install-base lock is a blocking shared `flock` with no owner record and no `--noblock_for_lock`. If the parent path is missing, Zel skips the lock.
  - [P2] `CreateSecureDirectory` (mkdir `0755`, owner must be the euid, strip group and other write, exclude from backup) is not implemented. `prepare_directories` creates the output user root at `0755` and chmods it to `mode & ~umask`, so a `0700` directory becomes `0755`, and creates the output base at `0777` only when it is absent.
  - [P2] `Which` (split `PATH` on `:`, empty component means `.`, require `X_OK` and a regular file) is absent. `--server_javabase` only checks `<javabase>/bin/java` for a non-directory with some execute bit. An unset `USER` becomes the literal `bazel` with no `getpwuid` and no fatal error.
  - [P1] `SetupStdStreams` is absent: stdout and stderr stay Rust-buffered, and closed fds 0, 1, and 2 are not reopened onto `/dev/null`. `UnlimitResources` is absent. `--unlimit_coredumps` sets both core limits to `RLIM_INFINITY` and ignores failure. Bazel raises the soft core limit to the hard limit.

### `src/main/cpp/blaze_util_windows.cc`
- Bazel lines: 1479
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs`, `/workspace/zel-cursor/crates/zel-vfs/src/lib.rs`
- Verdict: partial
- Differences:
  - [P1] Zel’s client and `sys.rs` are POSIX (`std::os::unix`, `kill(2)`, `flock`). `process_alive`, `send_signal`, and `install_handler` are no-ops off Unix, so a Windows build cannot see, signal, or stop a server.
  - [P2] `CreateCommandLine` runs the exe through `AsShortWindowsPath` (8.3) and quotes it into a 32768-`WCHAR` buffer before `CreateProcessW`. `ExecuteProgram` strips a `\\?\` prefix. Zel passes the path to `std::process::Command` with no 8.3 conversion, no `\\?\` handling, and no command-line length check.
  - [P1] `SymlinkDirectories` is `CreateJunction`: an absolute NTFS mount-point reparse (`IO_REPARSE_TAG_MOUNT_POINT`, `\??\` substitute name). It is not a symlink. Zel’s symlink forest is `#[cfg(unix)] symlink(2)`. Off Unix, creation returns `Unsupported`. There is no reparse-point API.
  - [P2] The persistent server is not in a job (`DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`). Synchronous launches go through `WaitableProcess`, which creates a job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` so grandchildren die. Zel’s daemon uses Unix `setpgid` only. There is no kill-on-close of the process tree.
  - [P2] Bazel records `GetProcessTimes` creation `FILETIME` in `server/server.starttime` and `VerifyServerProcess` requires that value plus `STILL_ACTIVE`. Zel writes `server.pid.txt` and checks it with `kill(pid, 0)`. PID reuse is not detected. Kill is `SIGTERM` then `SIGKILL`.
  - [P1] `AcquireLock` is `LockFileEx` of one byte on a handle opened `CREATE_ALWAYS` with `FILE_SHARE_READ|WRITE|DELETE`, matching the JVM `FileChannel`. Locks are mandatory, so the holder is not written into the file. A `DeletePending` file is recreated. Zel’s output-base lock is advisory `flock`, then it truncates and writes `pid`, `owner`, and `cwd`. That module does not compile as Windows code.
  - [P2] `GetHashedBaseDir` keeps 40 bits of MD5 as 8 lowercase base-32 characters (`abcdefghijklmnopqrstuvwxyz234567`) because of the 259-character `MAX_PATH`. Zel always uses a 32-character hex MD5. Bazel’s output user root is `GetCacheDir()` plus `_bazel_<user>`: `HOME`, else `USERPROFILE`, else `SHGetKnownFolderPath(FOLDERID_Profile)`. Zel’s Windows root is `%LOCALAPPDATA%\bazel\_bazel_{user}`, and the user is only `$USER` or the literal `bazel`.
  - [P2] `SetConsoleCtrlHandler` cancels on Ctrl+C, Ctrl+Break, and `CTRL_CLOSE_EVENT`. The third Ctrl+C terminates the server and `_exit(1)`. `SetupStdStreams` opens `NUL` if a standard handle is missing and turns on virtual-terminal processing. Zel’s cancel path is POSIX `SIGINT`/`SIGTERM`. Off Unix, tty detection is false and handlers are not installed.
  - [P2] `GetSystemJavabase` requires `JAVA_HOME\bin\javac.exe` and the JVM binary is `bin/java.exe`. Zel looks for `bin/java`. The path-list separator is `;`. Shared libraries are `.dll` and `.pyd`. `BAZEL_SH` is the env var, else the MSYS2 uninstall key, else `SearchPathA`. `FindSystemWideBlazerc` returns empty. `UnlimitResources` and `UnlimitCoredumps` return true without doing anything.

### `src/main/cpp/command_extension_adder.h`
- Bazel lines: 34
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel defines `blaze::CommandExtensionAdder` with a pure virtual `MaybeAddCommandExtensions(RunRequest&)` that mutates a `RunRequest` before the client sends it. `blaze.cc` calls it from the run and shutdown paths when the pointer is non-null.
  - [P3] Zel has no equivalent trait. `zel-client` always sets `RunRequest.command_extensions` to `Vec::new()`, and the server only fills extensions later for `info`.

### `src/main/cpp/generate_jvm_module_options.sh`
- Bazel lines: 75
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel generates `jvm_module_options.h` from a deploy jar: it joins `META-INF/MANIFEST.MF` continuation lines, reads `Add-Exports` and `Add-Opens`, and emits `"--add-exports"` / `"--add-opens"` pairs with `=ALL-UNNAMED`. `startup_options.cc` appends those flags to the server JVM command.
  - [P3] Zel’s server is the native binary. `start_server` spawns `spec.exe` with `--internal_server_daemon` and writes stdout/stderr to `jvm.out`. It never reads a jar manifest and never passes `--add-exports` or `--add-opens`.
  - [P3] The only `--add-opens=…=ALL-UNNAMED` strings in the Zel tree are four hardcoded flags on a `java` argv in `scripts/bzlmod_harness/patch_corpus.py`. That is a harness invocation, not server launch.

### `src/main/cpp/get_self_path_linux.cc`
- Bazel lines: 45
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/lib.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel calls `readlink("/proc/self/exe", buffer, PATH_MAX)`, treats `bytes == PATH_MAX` as truncation (`errno = ENAMETOOLONG`), writes an explicit NUL, and returns that byte string. `argv0` is unused. Failure is `BAZEL_DIE` with `INTERNAL_ERROR`.
  - [P3] Zel has no `get_self_path_linux.cc`. Linux `std::env::current_exe` is the same `/proc/self/exe` readlink, but the standard library grows the buffer, so a target longer than `PATH_MAX` is kept.
  - [P2] `readlink` does not walk extra symlinks. A deleted binary keeps the `" (deleted)"` suffix. `bazelisk.rs` then `canonicalize`s and falls back to the raw path if that fails.
  - [P2] Bazel stores the raw symlink bytes with no UTF-8 check. `bazelisk.rs` uses `to_string_lossy()`, so non-UTF-8 bytes become U+FFFD. `binary_install_md5` uses `exe.display()` in the cache key, which is the same lossy encoding. The daemon `ServerSpec.exe` stays a `PathBuf`.
  - [P2] On failure, `run_with_daemon` prints `ERROR: cannot determine own path` and returns `LOCAL_ENVIRONMENTAL_ERROR`. `binary_install_md5` hashes the build label instead. None of these abort the process the way `BAZEL_DIE` does.

### `src/main/cpp/main.cc`
- Bazel lines: 59
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/main.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel forwards the full `argc`/`argv`, including `argv[0]`, into `blaze::Main`. Zel passes `std::env::args().skip(1)`, so the program name never reaches `execute_cli_with_exec`.
  - [P2] On `_WIN32`, Bazel uses `wmain(int, wchar_t**)`, converts each argument with `WstringToCstring`, and appends a null terminator. Zel has one entry. Unicode arguments come from the Rust runtime, with no `wmain`.
  - [P3] Bazel records `GetMillisecondsMonotonic()` before any other work and passes that `start_time` into `blaze::Main`. Zel’s main records no start time.
  - [P3] Bazel heap-allocates `WorkspaceLayout`, `BazelStartupOptions`, and `OptionProcessor`. The processor is leaked on purpose. Zel builds a cwd, a filesystem, and a pending exec slot. Those objects are not created in `main`.
  - [P3] Bazel passes null `startup_interceptor` and `command_extension_adder`. Zel has no such parameters.
  - [P2] Zel resolves the working directory in `main` with `current_dir()` and, on failure, substitutes `"/"`. Bazel’s entry does not compute the cwd. `GetCanonicalCwd()` later does not fall back to `/`.
  - [P3] If the CLI returns 0 and a pending exec is set, Zel flushes both streams (flush errors ignored) and replaces the exit code with `perform_exec_request`. A non-zero CLI code drops any pending exec. Bazel’s `main.cc` has no exec-request step.
  - [P3] Bazel returns the code from `main`/`wmain`, so local destructors run. Zel ends with `std::process::exit`, which skips `Drop` of the locals still in `main`.

### `src/main/cpp/option_processor-internal.h`
- Bazel lines: 64
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/rc.rs`
- Verdict: partial
- Differences:
  - [P3] The header declares seven helpers (`DedupeBlazercPaths`, `WarnAboutDuplicateRcFiles`, `GetOldRcPaths`, `FindLegacyUserBazelrc`, `FindSystemWideRc`, `FindRcAlongsideBinary`, `ParseErrorToExitCode`). Zel folds discovery into `discover_and_load_rc_files` and has no counterparts for the legacy-path or duplicate-warning functions.
  - [P2] `DedupeBlazercPaths` keeps the first path per `MakeCanonical` form and drops paths that do not resolve. Zel dedupes `PathFragment` strings in a `HashSet` and does not collapse symlinks to the same file before parsing.
  - [P2] `WarnAboutDuplicateRcFiles` warns when an import was already read, or when one top-level rc imports the same file more than once. Zel never emits those warnings.
  - [P3] The legacy-path helpers feed the “no longer being read” warning. Zel only prints that warning when `--noworkspace_rc` and `%workspace%/.bazelrc` exists. There is no binary-adjacent rc.
  - [P2] `FindSystemWideRc` absolutizes and expands env vars in the configured system rc path and returns it only if readable. Zel always queues a fixed `/etc/bazel.bazelrc` (or `C:/ProgramData/bazel.bazelrc` on Windows) and skips it when missing.
  - [P2] `ParseErrorToExitCode` maps `UNREADABLE_FILE` to internal error 37 and `INVALID_FORMAT` / `IMPORT_LOOP` to bad-argv 2. Zel maps only messages that start with `Unexpected error reading config file` to 37; import loops, bad format, and an unreadable `--bazelrc` all become command-line error 2.

### `src/main/cpp/option_processor.cc`
- Bazel lines: 766
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/startup.rs`, `/workspace/zel-cursor/crates/zel-options/src/rc.rs`
- Verdict: partial
- Differences:
  - [P1] `SplitCommandLine` treats `args[0]` as the binary. `IsArg` stops the scan on `--help`, `-help`, and `-h`, and those strings become the command with every later argument kept. Zel starts at the first user argument, rewrites only `--help` and `-h` to the command `help`, and drops the rest. `-help` is reported as an unknown startup option. A missing command stays empty. Zel later substitutes `help` and still expands rc options. An empty command token is rejected before rc files are read. Zel checks that only after rc loading. Unknown-option text during the scan always uses the product name `bazel`.
  - [P2] A nullary flag with `=` fails for every nullary flag, including `--block_for_lock`. Zel allows `--block_for_lock=<value>`. Zel converts values during the split and defers invalid values. Bazel classifies flags here and converts them later, after rc files are read, rc startup flags first and command-line flags last.
  - [P1] Rc search order is system, then workspace `.bazelrc`, then `$HOME/.bazelrc`, then `BAZELRC`, then `--bazelrc`, unless `--ignore_all_rc_files`. Zel follows that order and does not load `.zelrc`. Bazel’s system path is `/etc/bazel.bazelrc` or `%ProgramData%/bazel.bazelrc` after env-var expansion. Zel uses `/etc/bazel.bazelrc` or the fixed path `C:/ProgramData/bazel.bazelrc`. Home is `GetHomeDir()` (Windows profile included). Zel uses `HOME` only. `BAZELRC` and `--bazelrc` go through `AbsolutePathFromFlag` (`~` expands, an empty entry means the cwd, no trim). Zel trims, joins relative paths to the cwd, and does not expand `~`.
  - [P1] `GetAllUnaryOptionValues(..., "/dev/null")` records `/dev/null` and ignores every `--bazelrc` after it. Zel skips the `/dev/null` entry and still reads later files. An explicit missing rc file fails with `Error: Unable to read .bazelrc file '<path>'.` (`BAD_ARGV`). Zel uses that sentence without the `Error:` prefix. Implicit paths are dropped by canonical dedupe. Zel dedupes the path string and skips missing implicit files, so a workspace that is the same directory as home, and symlinks to the same file, are loaded twice.
  - [P1] `WarnAboutDuplicateRcFiles` warns when a file is imported twice or was already read. Zel silently skips a path it has already visited, so a diamond import contributes its options once.
  - [P0] `ParseStartupOptions` takes every `startup` option, then every `startup:<os>` option (`linux`, `macos`, `windows`, `freebsd`, `openbsd`), then the command line. Space-separated unary startup options in an rc file use the next startup token as the value. `--invocation_policy` may appear only once. `--ignore_all_rc_files`, `--system_rc`, `--home_rc`, `--workspace_rc`, and `--bazelrc` are illegal inside an rc file. Zel applies only the plain `startup` section, and it does so after the command line at a lower priority. A space-separated rc startup flag is rewritten into `Unknown startup option`. A second `--invocation_policy` overwrites the first. Only `--bazelrc` is rejected in rc text. The other rc-forbidden flags are accepted and applied too late to change which files were opened.
  - [P3] The legacy-rc warning covers the master bazelrc (workspace rc, `<binary>.<basename>rc`, system rc) plus the old single user rc. Zel emits that warning text only when `--noworkspace_rc` is set and the workspace `.bazelrc` exists.
  - [P1] `GetBlazercOptions` prepends client `common` options `--isatty`, `--terminal_columns`, and `--emacs` when `EMACS=t` or `INSIDE_EMACS` is set, then every rc option except the exact section `startup` (`startup:linux` is still forwarded). The env args send `--rc_source`, `--default_override=<index>:<command>=<option>`, then `GetProcessedEnv()` and `--client_cwd` for every real command. Zel expands rc options in the client, does not emit `--rc_source` or `--default_override`, never sends `--emacs`, and adds `--isatty`, `--terminal_columns`, and `--client_cwd` only for a subset of commands. `--client_env` is omitted for `help`, `version`, and `shutdown`, and the variables are sorted.
  - [P2] `PrintStartupOptionsProvenanceMessage` groups consecutive startup options by source and joins them with a space. Zel’s failure path matches that. With `--announce_rc`, the same options are joined with a comma.

### `src/main/cpp/option_processor.h`
- Bazel lines: 215
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/startup.rs`, `/workspace/zel-cursor/crates/zel-options/src/rc.rs`
- Verdict: partial
- Differences:
  - [P3] There is no `blaze::OptionProcessor`, `CommandLine`, or `BlazercOption`. Zel splits the work into `split_command_line`, `discover_and_load_rc_files`, and the client launcher.
  - [P2] `SplitCommandLine` takes `argv` including the binary path, returns rewritten startup strings (`--bar blah` becomes `--bar=blah`), and rejects an empty command and an empty `argv`. Zel drops the binary path, turns a missing command into `""` (the client later rewrites that to `help`), and special-cases `--help`, `-h`, and a lone `--version` inside the splitter. Unknown-startup errors often hardcode the product name `"bazel"`.
  - [P3] `ParseOptions` is a once-only method returning an exit code. Zel has no equivalent. `GetCommandArguments` (rc and env args, then explicit args) and `GetExplicitCommandArguments` are not separate accessors.
  - [P3] The constructors inject `WorkspaceLayout` and an optional system rc path. Zel hardcodes `/etc/bazel.bazelrc` (Windows: `C:/ProgramData/bazel.bazelrc`). There is no `SetBuildLabel` or `GetLowercaseProductName`.
  - [P1] `GetRcFiles` keeps the legacy master and binary-adjacent rc warning and canonical-path dedupe. Zel dedupes by path string and only warns when `--noworkspace_rc` leaves a workspace `.bazelrc` unread. `--bazelrc=/dev/null` does not stop later `--bazelrc` values.
  - [P0] `ParseStartupOptions` also applies `startup:<os>` (`linux`, `macos`, `windows`, `freebsd`, `openbsd`). Zel applies only the `"startup"` section.
  - [P1] `GetBlazercAndEnvCommandArgs` emits `--rc_source`, `--default_override=<index>:<command>=<option>` (including `--isatty`, `--terminal_columns`, and `--emacs`), then every `GetProcessedEnv()` entry as `--client_env`, then `--client_cwd`, for any non-empty command. Zel expands rc options itself, never emits `--rc_source` or `--default_override`, adds `--client_cwd` only for build, test, run, coverage, and the query family, adds sorted UTF-8 `--client_env` only outside help, version, and shutdown, and adds `--isatty` and `--terminal_columns` only for build, test, run, and coverage.

### `src/main/cpp/option_processor_unix.cc`
- Bazel lines: 54
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/lib.rs`
- Verdict: partial
- Differences:
  - [P3] This translation unit only implements `GetProcessedEnv`. Unix rc discovery and the home directory are not here.
  - [P2] Zel loads the user rc from `std::env::var("HOME")`. That returns `Err` when `HOME` is not Unicode, so a non-UTF-8 home skips `$HOME/.bazelrc`. Bazel keeps the raw `HOME` bytes and joins `.bazelrc` whenever the string is non-empty and `--home_rc` is on.
  - [P3] Both sides use only `.bazelrc` for that home file. The comment on `discover_and_load_rc_files` also names `.zelrc`. The body does not add it.
  - [P2] Unix `GetProcessedEnv` copies each `environ` entry as opaque bytes, in the original order, including entries without `=`. On `__APPLE__` it then `confstr`s `_CS_DARWIN_USER_TEMP_DIR` and `_CS_DARWIN_USER_CACHE_DIR` and appends `DARWIN_USER_TEMP_DIR=` and `DARWIN_USER_CACHE_DIR=` when the call returns a positive length.
  - [P1] Zel’s client env is `std::env::vars_os()` filtered with `OsString::into_string().ok()`, so any non-UTF-8 name or value is dropped, then the pairs are sorted and sent as `--client_env=NAME=VALUE`. Nothing calls `confstr`. The sandbox only reads those Darwin variables if they are already in `client_env`.

### `src/main/cpp/option_processor_windows.cc`
- Bazel lines: 102
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/lib.rs`, `/workspace/zel-cursor/crates/zel-options/src/rc.rs`
- Verdict: partial
- Differences:
  - [P1] This file only builds the client environment. Zel loads the home rc from a caller-supplied directory, and the client passes only `std::env::var("HOME")`. It does not consult `USERPROFILE` or `SHGetKnownFolderPath(FOLDERID_Profile)`. `PathFragment` rewrites `\` to `/` and uppercases a drive letter. The Windows system rc is the literal `C:/ProgramData/bazel.bazelrc`.
  - [P2] Bazel reads `GetEnvironmentStringsW`, then converts with `WideCharToMultiByte(CP_UTF8)`. A failed conversion aborts the client. Zel uses `vars_os()` and drops any name or value that is not Unicode. Neither side uses the ANSI code page.
  - [P2] Only `TMP` is rewritten with `ConvertPath` (`AsWindowsPath` plus ASCII lowercasing) so `c:/foo` is not parsed as a Unix path list. A leading `/` fails that conversion and aborts. `PATH` is renamed to `PATH` and its value is left unchanged. Zel has no Cygwin branch and does not rewrite `TMP`.
  - [P1] For `PATH`, `SYSTEMROOT`, `SYSTEMDRIVE`, `TEMP`, `TEMPDIR`, and `TMP`, only the name is uppercased. The value stays the UTF-8 of the wide string, including backslashes and a `\\?\` prefix. Zel keeps the original name case.
  - [P2] Names that start with `=` and any character outside `[A-Za-z0-9_()]` are dropped. Zel keeps any Unicode name that converts, including `.`, `-`, and non-ASCII. Zel also sorts the pairs. This file keeps `GetEnvironmentStringsW` order.

### `src/main/cpp/rc_file.cc`
- Bazel lines: 335
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/rc.rs`
- Verdict: partial
- Differences:
  - [P1] Imports that do not start with `%workspace%/` are opened exactly as written. Zel resolves a relative import against the importing file’s directory, so `import base.bazelrc` from `sub/nested.bazelrc` loads `sub/base.bazelrc`. Bazel reads the literal path `somefile`.
  - [P2] The workspace prefix is the exact string `%workspace%/`. `%workspace%` with no slash is an ordinary path. Zel treats every token that starts with `%workspace%` as workspace-relative and strips a leading `%workspace%` plus any following slashes, so `%workspace%foo` is rewritten.
  - [P2] A missing `%workspace%/` target is `INVALID_FORMAT` for `import` and is skipped for `try-import`. Zel uses that checkout message only when a workspace exists. A missing plain path is `UNREADABLE_FILE` with errno text. Zel pre-checks `exists` and synthesizes `(error: 2): No such file or directory`.
  - [P1] Import cycles are exact string matches on the path pushed onto the stack, not the canonical path. The same file under two spellings is parsed again, so diamond imports duplicate options. Zel normalizes via `PathFragment`, and a `visited` set skips any path already loaded. Zel also fails at `MAX_IMPORT_DEPTH` (100). Bazel has no depth cap in this file.
  - [P1] `try-import` ignores only `UNREADABLE_FILE`. A readable file that is malformed or closes a loop still fails. Zel drops only errors whose message contains `Unexpected error reading config file`.
  - [P2] Operators are `<`, `<=`, `>`, `>=`, `==`, `!=`, and `~`, and the condition must fully match. If the running label is not a `SemVer`, the directive is skipped. Zel allows spaces after the operator. If `build_label` does not parse, it returns success and still imports. `parse_str` always passes `"7.0.0"`.
  - [P1] `~8` is `>=8.0.0` and `<9.0.0`. `~8.1` and `~8.1.4` are `>=` that version and `<` the next minor. Pre-release is compared per semver. Zel pads missing minor and patch with zeros, strips a leading `v` and a `-suffix`, and ignores pre-release ordering. Its `~` check is only “same major and greater or equal,” so `~7.1.0` also matches `7.9.0`. `7.0.0+build` parses in Bazel. Zel fails the patch field and, for the running version, treats the condition as true.
  - [P2] File bytes are an unvalidated `std::string`. Zel decodes with `String::from_utf8_lossy` before tokenization, so illegal UTF-8 becomes U+FFFD and an import path can change.
  - [P2] Bazel’s strip is ASCII space. Token breaks are only space, newline, tab, and CR. Zel’s `trim` and `char::is_whitespace` also split on vertical tab, form feed, and Unicode spaces, so a line of NBSP is empty in Zel and a one-word command in Bazel.
  - [P1] Bazel applies `\` even inside single quotes, so `'foo\'bar'` is one token `foo'bar`. Zel disables `\` inside single quotes, so that line becomes `foo\` and `bar`.
  - [P1] Any other first word is an rc command. There is no command whitelist in this file. Zel warns and skips unknown commands, and rejects `--bazelrc` while parsing the rc.

### `src/main/cpp/rc_file.h`
- Bazel lines: 116
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/rc.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel exposes `RcOption` (`option` plus `source_index` into `canonical_source_paths()`) and `RcFile::OptionMap` (`flat_hash_map<string, vector<RcOption>>`). Zel’s `BazelrcConfig` stores `(option, source_path)` pairs in a `BTreeMap` and a separate `loaded_rc_files` path list.
  - [P3] `RcFile::Parse` returns `unique_ptr<RcFile>` and writes `ParseError` (`NONE`, `UNREADABLE_FILE`, `INVALID_FORMAT`, `IMPORT_LOOP`) plus `error_text`. Zel returns `Result` and mutates `BazelrcConfig` in place.
  - [P3] Both `Parse` overloads take `WorkspaceLayout*`, `workspace`, and injectable read and canonicalize functions. Zel takes `Option<&PathFragment>` and `&dyn FileSystem`, and canonicalizes with `std::fs::canonicalize`, falling back to the given path.
  - [P2] The versioned `Parse` takes `build_label` and `optional<SemVer>`. Zel’s `parse_file` takes `build_label: &str`, and `parse_str` hardcodes `"7.0.0"`.
  - [P2] `BazelVersionMatchesCondition` returns `optional<bool>` (`nullopt` on failure). Zel’s `eval_bazel_version_condition` treats an unparseable current version as a match and also accepts `~`.

### `src/main/cpp/sem_ver.cc`
- Bazel lines: 152
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/rc.rs` (`parse_semver`, `eval_bazel_version_condition`)
- Verdict: partial
- Differences:
  - [P2] Bazel parses with the semver.org regex into `int` major, minor, and patch plus prerelease and build strings. A core component that does not fit in `int` fails the match. Zel keeps only `(u32, u32, u32)` and drops the first `-` suffix, so `1.2.3-rc.1` compares equal to `1.2.3`. `+build` is not stripped: `1.2.3+build` fails `u32` parsing, and an unparseable current version makes the rc condition succeed.
  - [P2] Zel accepts a shorter core by padding minor and patch with 0, ignores components past the third, strips leading `v` characters, and accepts leading zeros (`"01"` parses as 1). Bazel requires exactly three components, no `v` prefix, and no leading zeros (`0|[1-9]\d*`).
  - [P3] Bazel rejects core values above `INT_MAX` (2147483647). Zel rejects only above `u32::MAX`, so 2147483648 through 4294967295 parse in Zel and not in Bazel. `NextMajorVersion` and `NextMinorVersion` are unchecked `int` increments, so `INT_MAX` wraps. Zel has no next-version helpers.
  - [P2] Prerelease comparison follows the Go rules: a release outranks a prerelease, a numeric id sorts below a non-numeric id, and a longer id list sorts higher when the shared prefix is equal. An all-digit id that overflows `int` is treated as non-numeric, so `"10000000000"` sorts before `"9999999999"`. Zel never applies those rules.
  - [P3] Build metadata is stored and ignored by `Compare`. Zel never stores it.
  - [P1] The C++ caller treats `~8` as the next major and `~8.1` or `~8.1.4` as the next minor. Zel treats every `~` as the same major and `>=` the target, so `~7.1.0` matches through `7.*`, not only `< 7.2.0`.
  - [P3] `/workspace/zel-cursor/crates/zel-bzlmod/src/version.rs` is a separate module-version parser. A numeric segment that does not fit in `u64` is a parse error. The empty version sorts above every other version.

### `src/main/cpp/sem_ver.h`
- Bazel lines: 77
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/rc.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel declares `blaze::SemVer` with `major_`, `minor_`, `patch_`, `prerelease_`, and `buildmetadata_`. Zel’s `parse_semver` returns `Option<(u32, u32, u32)>` and discards the suffix at the first `-`.
  - [P2] `SemVer::Parse` requires a full semver.org string and returns `nullopt` otherwise. Zel trims, strips a leading `v`, accepts one or two numeric components (missing minor and patch become `0`), and does not check leading zeros or build metadata.
  - [P2] Bazel orders prerelease identifiers and ignores build metadata. Zel compares the three-tuple only, so `1.0.0-rc1` and `1.0.0` compare equal.
  - [P1] `NextMajorVersion` (`8.3.4` to `9.0.0`) and `NextMinorVersion` (`8.3.4` to `8.4.0`) bound `~` ranges. Zel’s `~` operator is only the same major and `>=` the target, so `~8.1.0` is not bounded at `8.2.0`.
  - [P3] Component type is `int` in Bazel and `u32` in Zel.

### `src/main/cpp/server_process_info.cc`
- Bazel lines: 37
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs`
- Verdict: partial
- Differences:
  - [P2] This file does not take a lock and does not write `<output_base>/lock`. Zel’s server lock is an exclusive `flock` on that path. On success it truncates the file and writes `pid=<client pid>\nowner=client\ncwd=<cwd>\n`. The wait loop is 500 ms. `--noblock_for_lock` exits with the lock-held code. Zel omits Bazel’s `tty=` line.
  - [P3] The only pid here is the in-memory `server_pid_`, set to `-1` as the “no server” sentinel. Nothing is written to disk. `BlazeServer::Connect` later overwrites it from `server_info.rawproto`. Zel’s lock records the client’s process id and never uses `-1`. The daemon pid is a separate `server/server.pid.txt`.
  - [P3] There is no port field. Zel’s lock also stores no port. The bound address is written later to `server/command_port` and `server_info.rawproto`.
  - [P2] `GetJvmOutFile` picks `--server_jvm_out` when it is non-empty, otherwise `<output_base>/server/jvm.out`. The append flag is true only for the custom path, so the default log is not opened append. Zel uses the same path rule, but `start_server` always opens that file with `append(true)`, including the default `server/jvm.out`.
  - [P2] On a startup crash Bazel uses the append flag to either point at the custom log or dump the default `jvm.out`. Zel always prints the log contents and also prints `output_base`.

### `src/main/cpp/server_process_info.h`
- Bazel lines: 51
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel defines `blaze::ServerProcessInfo` with `(output_base, server_jvm_out)` and three public fields. Zel has no such type. The log path is `ServerSpec::jvm_out()`. The pid lives on `ServerInfo` (`pid: i32`) and again as `BlazeRuntime.server_pid: u32`.
  - [P3] `jvm_log_file_` is a stored path. Zel recomputes a `PathBuf`: a non-empty `--server_jvm_out`, otherwise `<output_base>/server/jvm.out`.
  - [P2] `jvm_log_file_append_` is true only when `server_jvm_out` is non-empty, so the default log is not opened with `O_APPEND`. Zel always opens the log with `append(true)` and has no append flag.
  - [P3] `server_pid_` is a mutable `pid_t`, initialized to `-1`. Zel does not keep that sentinel. The pid is the spawned child id, and it is `0` only via `ServerInfo`’s `Default`.
  - [P3] The header pulls in `<sys/types.h>` and a Windows `pid_t` alias. Zel’s client path is Unix-only.

### `src/main/cpp/startup_interceptor.h`
- Bazel lines: 40
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel defines a pure virtual `blaze::StartupInterceptor` with `MaybeReroute(StartupOptions const*, std::string_view build_label)` for rerouting an invocation to another backend from startup options and the build label.
  - [P3] Zel has no `StartupInterceptor` type. `bazelisk.rs` only resolves and launches a versioned binary, and `main.rs` runs the CLI directly.

### `src/main/cpp/startup_options.cc`
- Bazel lines: 705
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/startup.rs`
- Verdict: partial
- Differences:
  - [P3] Shared defaults line up for the flags this file registers and Zel stores: `batch` false, `block_for_lock` true, `connect_timeout_secs` 30, `local_startup_timeout_secs` 120, `io_nice_level` -1, `max_idle_secs` 10800 outside tests, `command_port` 0, `write_command_log` true, `windows_enable_symlinks` false. The five numeric range checks use the same error text (`io_nice_level` ≤ 7, `max_idle_secs` ≥ 0, `connect_timeout_secs` 1–3600, `local_startup_timeout_secs` ≥ 1, `command_port` 0–65535).
  - [P2] `max_idle_secs` drops to 15 when `TEST_TMPDIR` is set, including when that variable is empty. Zel’s default is always 10800. `TEST_TMPDIR` only changes Zel’s output-root path.
  - [P2] `macos_qos_class` starts as `QOS_CLASS_UNSPECIFIED` and accepts only `utility` and `background` on every platform. Zel’s default is the string `default`, and any string is accepted. The Zel help text also lists `user-interactive` and `user-initiated`.
  - [P2] `host_jvm_debug` is a nullary bool, so `--nohost_jvm_debug` clears it. Zel declares it `Void` with an expansion to `--host_jvm_args=-agentlib:jdwp=transport=dt_socket,server=y,address=5005`, and that expansion is never applied. `--nohost_jvm_debug` is an unknown startup option because the flag is not boolean syntax.
  - [P3] `host_jvm_profile` is registered as unary and has no parse branch, so it dies later as an unknown startup option. Zel’s startup map does not contain it, so the split fails immediately.
  - [P2] A second `--invocation_policy` is a hard error. Zel overwrites the string when a later equal-or-higher priority value arrives.
  - [P2] Every nullary with `=` is rejected, including `--block_for_lock=false`. Zel exempts `block_for_lock` and then converts the value, so `--block_for_lock=false` succeeds. Other nullaries such as `--batch=1` still fail.
  - [P2] `--ignore_all_rc_files` is illegal inside an rc file. Zel applies startup options from rc files with no such ban.
  - [P1] `output_base`, `install_base`, `output_user_root`, `server_jvm_out`, `failure_detail_out`, and `server_javabase` go through `AbsolutePathFromFlag`: empty becomes the cwd, a leading `~` is joined to the home directory, and every other value is made absolute. Zel stores the raw string. An empty `--output_base=` stays empty and suppresses the hashed default. A relative path stays relative.
  - [P1] `UpdateConfiguration` builds `output_user_root` as `<root>/_<lowercase product>_<user>`. Linux matches Zel (`$XDG_CACHE_HOME/bazel` or `$HOME/.cache/bazel`, else `/tmp`). macOS Bazel honors `XDG_CACHE_HOME`, otherwise `$HOME/Library/Caches/bazel`. Zel’s Darwin branch ignores `XDG_CACHE_HOME` and uses `/var/tmp/_bazel_<user>` when home is missing. Windows Bazel uses the home directory itself and aborts when that is empty. Zel uses `%LOCALAPPDATA%\bazel\_bazel_<user>`, else `%USERPROFILE%\AppData\Local\bazel\_bazel_<user>`, else `C:/temp/_bazel_<user>`. Zel always hardcodes the `bazel` product segment.
  - [P2] Server mode requires `--install_base` and `--output_base` (`exec-server requires …`). Otherwise the install base is `<output_user_root>/install/<md5>` and the output base is `<output_user_root>/<md5(workspace)>`. An empty `failure_detail_out` becomes `<output_base>/failure_detail.rawproto`. Zel always synthesizes those paths. `write_failure_detail` writes only when the flag is present and non-empty.
  - [P3] `--experimental_cgroup_parent` is accepted everywhere and stored only on Linux. Zel always keeps the string. `--experimental_run_in_user_cgroup` is registered only on Linux. Zel registers it on every OS.
  - [P3] On Windows, a detected bash root is pushed into `host_jvm_args` as `-Dbazel.windows_unix_root=…` before any user flag. Zel’s `host_jvm_args` starts empty.
  - [P3] Javabase selection is explicit `--server_javabase`, then `install_base/embedded_tools/jdk` when its java binary is executable, then a fatal error if `--noautodetect_server_javabase` is set, otherwise `JAVA_HOME` or `javac` on `PATH`. Sanity checks require an executable `java` plus `jre/lib/rt.jar`, `lib/rt.jar`, or `bin/java` / `bin/java.exe`. Explicit failures are `BAD_ARGV`. A missing system JDK is `LOCAL_ENVIRONMENTAL_ERROR`. Zel only checks an explicit `server_javabase` for an executable `bin/java` and returns `COMMAND_LINE_ERROR`. It does not search an embedded JDK and does not implement `--noautodetect_server_javabase`.
  - [P3] `AddJVMArguments` writes `javalog.properties` (`SimpleLogHandler`, 1024000-byte file limit, 20971520-byte total, formatter `SingleLineFormatter`), then adds `-XX:-MaxFDLimit`, `-Djava.lang.Thread.allowVirtualThreads=true`, and an `OnOutOfMemoryError=touch <oom file>`. A non-empty `extra_classpath` switches the launch from `-jar` to module opens, `-cp`, and main class `com.google.devtools.build.lib.bazel.Bazel`. Zel has no JVM launch path. `extra_classpath` is only stored and forwarded.
  - [P3] `java_logging_formatter`, `oom_more_eagerly` (false), and `oom_more_eagerly_threshold` (100) are constructor state with no registration. Zel has no startup options for those three.
  - [P3] `lock_install_base` is a constructor argument, not a parsed flag (the Bazel subclass passes true). Zel exposes it as a user startup boolean whose table default is false. Zel also accepts `product_name`, `install_md5`, `option_sources`, `workspace_directory`, and `default_system_javabase`, which this file does not register.
  - [P2] `safe_strto32` trims leading and trailing whitespace. Zel’s copy trims only the start, so a trailing space on those integers is valid for Bazel and invalid for Zel.
  - [P2] `ProcessArg` returns `BAD_ARGV` on the first bad value. Zel records the first `Invalid argument to` error in `deferred_error`, keeps splitting, and prints it only after rc files are loaded.

### `src/main/cpp/startup_options.h`
- Bazel lines: 441
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/startup.rs`
- Verdict: partial
- Differences:
  - [P3] Zel’s `StartupOptions` types only `output_base`, `output_user_root`, `install_base`, `batch`, `block_for_lock`, `quiet`, `max_idle_secs`, `ignore_all_rc_files`, `host_jvm_args`, `invocation_policy`, and `product_name`. The other Bazel members are not fields of that struct.
  - [P3] `server_jvm_out`, `failure_detail_out`, `lock_install_base`, `host_jvm_debug`, `autodetect_server_javabase`, `batch_cpu_scheduling`, `io_nice_level`, `command_port`, `connect_timeout_secs`, `local_startup_timeout_secs`, `client_debug`, `digest_function`, `macos_qos_class`, `unlimit_coredumps`, and `windows_enable_symlinks` exist only as entries in `parsed_options`, filled from `ALL_STARTUP_OPTIONS`.
  - [P3] No `oom_more_eagerly` (default false), `oom_more_eagerly_threshold` (default 100), or `java_logging_formatter` (default `com.google.devtools.build.lib.util.SingleLineFormatter`). They are absent from the startup flag table.
  - [P3] No `option_sources` map of flag to rc-file origin. Zel’s `option_sources` startup flag is a hidden string option. Per-flag source is `OptionValueDescription.source`.
  - [P2] No `have_invocation_policy_` once-only guard. No `JavabaseType` (`UNKNOWN`, `EXPLICIT`, `EMBEDDED`, `SYSTEM`). `server_javabase` is only a parsed startup flag.
  - [P2] `macos_qos_class` is not a `qos_class_t` (Apple-only, default `QOS_CLASS_UNSPECIFIED`). The flag table stores a string, default `"default"`, on every platform.
  - [P3] `cgroup_parent` and `run_in_user_cgroup` are Linux-only members in Bazel. Zel keeps neither as a field. The flag table uses the `experimental_` names.
  - [P2] `max_idle_secs` is `int` in Bazel (15 under test, otherwise `3 * 3600`). Zel stores `u64` and always defaults to `3 * 3600`.
  - [P3] `product_name` is a const constructor argument (`"Bazel"`). Zel’s field defaults to `"bazel"` and can be overwritten by a startup flag.
  - [P3] `host_jvm_args` starts empty in Zel. Bazel’s constructor prepends `-Dbazel.windows_unix_root=...` on Windows and Cygwin.
  - [P3] `lock_install_base` is constructor state and not a registered C++ flag. Zel does not store it on the struct. The client reads the flag table with default `true`.

### `src/main/cpp/util/errors.h`
- Bazel lines: 33
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs` (`errno_string`)
- Verdict: partial
- Differences:
  - [P3] Bazel declares a parameterless `blaze_util::GetLastErrorString()` that reads the thread-local `errno` or `GetLastError()` and embeds that code in the message.
  - [P3] Zel’s `errno_string` takes `&std::io::Error`, uses `raw_os_error().unwrap_or(0)`, and strips Rust’s ` (os error N)` suffix from `Error::to_string()` instead of calling `strerror(errno)`.
  - [P3] Both POSIX-style messages use `(error: <code>): <text>`. Zel has no Windows `FormatMessageA` path and no `"success"` result when the code is 0.
  - [P3] An identical C header also sits at `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/main/cpp/util/errors.h`.

### `src/main/cpp/util/errors_posix.cc`
- Bazel lines: 35
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/main/cpp/util/errors_posix.cc`
- Verdict: partial
- Differences:
  - [P3] The vendored C++ file is byte-identical: `GetLastErrorString` snapshots `errno`, then writes `(error: <errno>): ` plus `strerror(saved_errno)`.
  - [P3] Locale is not fixed in either copy. There is no `strerror_l`, so the message follows the process `LC_MESSAGES`. Encoding is the locale codeset, copied unchanged into `std::string`. Nothing converts to UTF-8. `strerror` is not thread-safe.
  - [P2] Zel’s client formatter `errno_string` keeps `(error: N): text`, but the text comes from `std::io::Error::to_string()` (`strerror_r`, then `String::from_utf8_lossy`, with the ` (os error N)` suffix stripped). A non-UTF-8 locale can turn bytes into U+FFFD, and a non-OS error is printed as code 0.
  - [P2] `zel-options` `errno_text` rebuilds `(error: N): msg` by parsing that Rust suffix. `zel-bzlmod` `errno_text` returns only the message and drops the `(error: N):` prefix.

### `src/main/cpp/util/exit_code.h`
- Bazel lines: 54
- Zel counterpart: `/workspace/zel-cursor/crates/zel-util/src/lib.rs` (`ExitCode`)
- Verdict: partial
- Differences:
  - [P3] `SUCCESS = 0`, `INTERRUPTED = 8`, `LOCK_HELD_NOBLOCK_FOR_LOCK = 9`, `OOM_ERROR = 33`, and `LOCAL_ENVIRONMENTAL_ERROR = 36` match Zel’s constants and infrastructure flags.
  - [P3] `BAD_ARGV = 2` is the same number as `ExitCode::COMMAND_LINE_ERROR`. `INTERNAL_ERROR = 37` is the same number as `ExitCode::BLAZE_INTERNAL_ERROR`. The enumerator names differ. `for_code(2)` returns `COMMAND_LINE_ERROR` and `for_code(37)` returns `BLAZE_INTERNAL_ERROR`. There is no `BAD_ARGV` or `INTERNAL_ERROR` constant.
  - [P3] Every numeric value in the C++ enum is present in Zel.
  - [P3] Zel also registers codes this header does not define: `BUILD_FAILURE` 1, `TESTS_FAILED` 3, `NO_TESTS_FOUND` 4, `RUN_FAILURE` 6, `ANALYSIS_FAILURE` 7, `REMOTE_ENVIRONMENTAL_ERROR` 32, `REMOTE_ERROR` 34, `PUBLISH_ERROR` 38, `REMOTE_CACHE_EVICTED` 39, `PERSISTENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR` 45, and `EXTERNAL_DEPS_ERROR` 48.
  - [P3] Unregistered aliases share a number and are omitted from `values()`: `PARSING_FAILURE` 1, `PARTIAL_ANALYSIS_FAILURE` 3, and `TRANSIENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR` as an alias of `PUBLISH_ERROR` (38).
  - [P3] An identical copy of this header is at `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/main/cpp/util/exit_code.h`.

### `src/main/cpp/util/file_posix.cc`
- Bazel lines: 572
- Zel counterpart: `/workspace/zel-cursor/crates/zel-vfs/src/lib.rs`, `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs`
- Verdict: partial
- Differences:
  - [P3] The resource copy is byte-identical and is not built. The running ports are the VFS, the daemon client, and the FFI bridge.
  - [P2] `make_directories` keeps a childmost-only `chmod` of `mode & ~umask`, an `lstat` effective-uid ownership check, and the refusal of `""` and `/`. `Dirname("a/b/")` is `a/b`. Rust `Path::parent` drops the trailing slash, so the last component is childmost immediately. Errnos are hardcoded 20, 13, 2, and 17.
  - [P2] `posix::mkdir` and `create_directory_and_parents` use `0777` masked by umask, do not force an existing directory to an exact mode, and do not require the directory to be owned by the effective uid. The in-memory filesystem stores no mode. `CreateSiblingTempDir` (`mkdtemp`, then `chmod(0777 & ~umask)`, fatal on failure) is absent. `start_server` creates the install base with `create_dir_all` only.
  - [P2] `ReadDirectorySymlink` fills a `PATH_MAX` buffer and returns success when `readlink` truncates. `fs::read_link` returns the full target. `MakeCanonical` is `realpath` (every component must exist). The in-memory filesystem follows at most 40 links.
  - [P2] `ForEachDirectoryEntry` treats only `DT_DIR` or `lstat` plus `S_ISDIR` as a directory, so a symlink is not one, and an `lstat` failure aborts the process. `opendir` failure returns with no error. Zel `readdir` reports a symlink as its own type, sorts names, and returns an error instead of aborting. `opendir` follows a symlink to a directory in both.
  - [P2] `RemoveRecursively` uses `lstat`, `rmdir`s a real directory, and `unlink`s every other node, including a symlink to a directory, with no permission fixup. Zel `delete_trees_below` and `make_writable_tree` `chmod` directories to `0755` before deleting them.
  - [P2] `IsDirectory`, `PathExists`, `CanReadFile`, `CanExecuteFile`, and `CanAccessDirectory` use `stat` and `access`: they follow symlinks, `access` uses the real uid and honors ACLs, and `CanExecuteFile` is `!IsDirectory && access(X_OK)` (root can execute mode `000`). Zel `is_executable_file` uses `mode() & 0o111` (any execute bit, no ACLs, root fails when no bit is set) and treats a missing host file as executable after a successful VFS stat.
  - [P1] `WriteFile` `unlink`s first, so a symlink is replaced rather than followed, then `open(O_CREAT|O_WRONLY|O_TRUNC, perm)` in chunks of at most `INT32_MAX`. The stored mode is `perm & ~umask`, and a failing `close` fails the write. `fs::write` follows an existing symlink and uses the default mode. Script output is then `chmod`ed to `0755`, which sets those bits exactly and ignores umask.
  - [P3] `utime` sets access and modification time together. `IsUntampered` is true for every directory and for a file whose `st_mtime` is later than process start plus 9×365 days. `SetMtimeToDistantFuture` uses 10×365 days. `SetMtimeToNowIfPossible` ignores only `EROFS` and `EPERM`. Zel `utimensat` changes modification time only and has no distant-future marker.
  - [P3] Also unported: `PosixPipe` (`pipe`, `FD_CLOEXEC`, `Send` requires one full `write`), `SyncFile` (`fsync`, fatal), `RenameDirectory` (`ENOTEMPTY` and `EEXIST` versus any other errno), `GetCwd` (fatal past `PATH_MAX`), and `ChangeDirectory`. `InitializeStdOutErrForUtf8` is empty on POSIX.

### `src/main/cpp/util/md5.cc`
- Bazel lines: 353
- Zel counterpart: `/workspace/zel-cursor/crates/zel-util/src/lib.rs` (`md5`)
- Verdict: partial
- Differences:
  - [P3] Padding matches RFC 1321: `0x80`, zeros until the length is 56 mod 64, then the 64-bit bit-length. Block words and the length suffix are little-endian in both. The low 64 bits of the bit length match.
  - [P3] Hex of a finished digest matches: lowercase encoding of A, B, C, and D as little-endian bytes. Empty, `"abc"`, and the quick-brown-fox vectors are the RFC values.
  - [P3] Raw `Finish(unsigned char digest[16])` copies `state[0..3]` as host-endian `uint32_t`s with no `htole32`. Zel’s 16-byte digest is always little-endian. They match on little-endian hosts. On big-endian, `Finish` disagrees with both Zel and Bazel’s own `String()`. `md5.h` defines `htole32(x)` as `(x)` on Apple and Windows under a little-endian CPU assumption.
  - [P3] `String()` is `const` and does not pad or run `Finish`. It hex-encodes only blocks already passed through `Transform`. Zel `hex_digest` always pads, so a buffered partial block differs until `Finish`.
  - [P3] Zel is one-shot. Bazel streams via `Update` and `Reset`. `count` is a byte counter (the header comment says bits). `Transform` then adds the padding length into `count`, so the object is not a clean finalized digest if reused.
  - [P3] Round functions, shifts, K, the index schedule, and the IVs match. Bazel writes `F` as `z ^ (x & (y ^ z))` and `G` as `F(z, x, y)`. Zel uses the RFC forms. The constants are the same.
  - [P3] A byte-identical copy of this file is vendored under `zel-bazel-tools`. `zel-exec` digest functions are SHA-1/256/384/512 and BLAKE3, not MD5.

### `src/main/cpp/util/numbers.cc`
- Bazel lines: 215
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/startup.rs` (`safe_strto32`)
- Verdict: partial
- Differences:
  - [P3] The vendored `.cc` matches Bazel. Zel does not run that file. `safe_strto32` is reimplemented only for startup range checks. `strto32` has no port.
  - [P3] `safe_strto32` is base 10 only. The digit table encodes letters as `0-35`, but both digit loops reject `digit >= 10`, so `"0x10"` fails and `"010"` is decimal 10. Zel’s startup parser agrees on that grammar.
  - [P3] One optional `+` or `-` is accepted after leading space. Both accept `i32::MIN` and `i32::MAX`. Bazel reaches `INT_MIN` by subtracting digits instead of negating a positive value. A second sign, a space between the sign and the digits, whitespace-only, and a bare `+` or `-` fail both.
  - [P3] On overflow Bazel writes `INT_MAX` or `INT_MIN` and returns false. An invalid digit writes the partial value. Zel returns `None` and does not saturate. Startup call sites only use the value when the parse succeeds, so the five range checks match on accept and reject.
  - [P3] Trailing ASCII space fails both. An embedded NUL differs: Bazel’s `c_str()` stops at the first NUL (`"123\0xyz"` succeeds as 123). Zel rejects the NUL byte.
  - [P2] `strto32` is `strtol` plus a clamp. Base `0` uses `strtol`’s `0x` and leading-zero rules. Trailing junk is left in `endptr` and is not an error. `GetTerminalColumns` accepts any result whose `endptr` is at the NUL, including a clamped `INT_MAX` and a negative value, and it ignores `errno`.
  - [P2] Zel’s `$COLUMNS` path is `parse::<usize>()` with a `> 0` filter, base 10 only. It rejects leading whitespace (`" 80"`; `strtol` accepts it), rejects a leading `-`, rejects a value that does not fit in `usize`, and accepts values above `i32::MAX` that fit in `usize`. Bazel clamps those to `2147483647` and still uses them.
  - [P3] Bazel `GetServerPid` uses `safe_strto32`, which rejects a trailing newline. Zel `read_pid` uses `trim().parse()`, which strips both ends, including Unicode whitespace.

### `src/main/cpp/workspace_layout.cc`
- Bazel lines: 87
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/lib.rs`
- Verdict: partial
- Differences:
  - [P2] Both take the nearest ancestor that contains a boundary file. Bazel’s set, in order, is `MODULE.bazel`, `REPO.bazel`, `WORKSPACE.bazel`, and `WORKSPACE`. Zel also treats `MODULE.zel` and `WORKSPACE.zel` as boundaries, so a tree with only those files is a workspace in Zel and not in Bazel.
  - [P2] Bazel counts a marker only when it exists and is not a directory (`stat`, symlinks followed). A directory, or a symlink to a directory, with one of those names is ignored. Zel’s `exists` does not reject directories, so a directory named `WORKSPACE` or `MODULE.bazel` makes that directory the workspace root.
  - [P2] Bazel’s walk stops before testing the filesystem root. A marker in `/` is found only when the cwd is `/`. Zel’s parent walk checks `/` (and a Windows drive root), so a marker at the filesystem root makes every path under it a workspace.
  - [P1] Bazel calls `GetWorkspace` on `GetCanonicalCwd()` (`realpath` of the cwd). Zel walks the `PathFragment` it is given. `main` passes `current_dir()` with no extra `realpath`, and `PathFragment::create` only collapses `.` and `..` lexically, so a cwd that still contains a symlink can land on a different root.
  - [P2] `GetWorkspace` returns `""` outside a workspace. `find_workspace_root_opt` returns `None`. `find_workspace_root`, used for Bazelisk planning, falls back to the cwd, so that search still runs when Bazel has an empty workspace.
  - [P3] `GetPrettyWorkspaceName` is `Basename(workspace)`, used as the server process title `bazel(<basename>)`. Zel never renames the server process that way.
  - [P3] `GetWorkspaceRcPath` always returns `<workspace>/tools/bazel.rc` and ignores `startup_args`. Zel has no `tools/bazel.rc` path. Workspace rc loading reads `<workspace>/.bazelrc`.
  - [P2] `ResolveWorkspaceRelativeRcFilePath` requires the prefix `%workspace%/` (the trailing slash is part of the prefix) and returns nullopt when that path does not exist. Zel matches any string that starts with `%workspace%`, including `%workspace%` with no slash and `%workspace%foo`. Bazel still joins onto an empty workspace string, which yields the relative remainder.
  - [P3] `ensure_repo_boundary` looks for the same four Bazel names on an external repo and, when the directory is not itself a symlink, creates an empty `REPO.bazel`. The C++ client only detects a workspace. It does not create a boundary file.

### `src/main/cpp/workspace_layout.h`
- Bazel lines: 66
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/lib.rs`
- Verdict: partial
- Differences:
  - [P3] Zel has no `blaze::WorkspaceLayout` type and no constants `kWorkspacePrefix` (`"%workspace%/"`) and `kWorkspacePrefixLength`.
  - [P2] `GetWorkspace` returns `""` when no boundary is found. Zel’s `find_workspace_root` falls back to the cwd.
  - [P2] Bazel boundary files are `MODULE.bazel`, `REPO.bazel`, `WORKSPACE.bazel`, and `WORKSPACE`, and each must exist and not be a directory. Zel also accepts `MODULE.zel` and `WORKSPACE.zel`, and `exists` is true for a directory of that name.
  - [P2] Bazel’s walk stops before testing the filesystem root. Zel’s parent walk checks the root.
  - [P3] `GetPrettyWorkspaceName` (workspace basename, used as the server process title) has no Zel counterpart.
  - [P3] `GetWorkspaceRcPath` returns `workspace/tools/bazel.rc`. Zel’s workspace rc is `%workspace%/.bazelrc`. The rc module comment also names `.zelrc`, which that function does not load.
  - [P2] `ResolveWorkspaceRelativeRcFilePath` requires the `"%workspace%/"` prefix and returns `nullopt` when the file is missing. Zel matches any path starting with `"%workspace%"` (slash optional). A missing `import` is an error, and a missing `try-import` is skipped.

### `src/main/native/blake3_jni.cc`
- Bazel lines: 73
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/digest.rs`
- Verdict: partial
- Differences:
  - [P3] JNI `blake3_hasher_finalize` writes `out_len` bytes of BLAKE3 extendable output. `out_len` is a signed `jint` passed through as `size_t`, so a negative length becomes a huge unsigned length. Zel `blake3` always returns exactly 32 bytes; `root_bytes` emits only the first output block (root counter 0).
  - [P3] JNI `blake3_hasher_update` hashes `input[offset, offset+input_len)` into a caller-owned hasher and can be called again on that state. `offset` and `input_len` are signed `jint`s: a negative offset addresses before the array, and a negative `input_len` wraps to a large `size_t`. Zel hashes one full `&[u8]` in a single call and keeps no resumable hasher.
  - [P3] If pinning the hasher array fails, init, update, and finalize return without writing. Update also uses the input pointer with no null check. Zel has no null hasher and always hashes the slice it is given.
  - [P3] `hasher_size` returns `sizeof(blake3_hasher)`, which depends on the native build, so Java can copy an initialized hasher blob. Zel does not expose a hasher size or a byte-copyable hasher state.

### `src/main/native/common.cc`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel exports an empty JNI symbol `Java_com_google_devtools_build_lib_jni_JniLoader_00024ForceLinkingHelper_link` from `//src/main/native:common` (`alwayslink = 1` into `libunix_jni.so`). `JniLoader.forceLinking()` calls it only when the JNI bundle loaded, so a missing or wrong library raises `UnsatisfiedLinkError` and a successful call has no effect. Zel has no JNI loader, force-link symbol, or `UnsatisfiedLinkError` probe. `crates/zel-ffi-bridge/src/native.rs` reimplements other `src/main/native` files and does not include this one. The vendored `crates/zel-bazel-tools/resources/src/main/native/BUILD` still lists `common.cc`, but that source is not present.

### `src/main/native/darwin/file_jni.cc`
- Bazel lines: 109
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P3] `portable_fstatat` has no counterpart. Any non-zero `flags` value, including `AT_SYMLINK_NOFOLLOW`, returns -1 with `errno` `ENOSYS`. An empty or absolute name ignores `dirfd` and calls `stat`. A relative name is `F_GETPATH(dirfd)`, a slash, and `name` joined with `strncat` into a `PATH_MAX*2` buffer (the relative part is truncated), then `realpath` and `stat` of that canonical path, so the call fails when `realpath` fails and cannot stat through an unlinked directory fd. `posix::stat` and `posix::lstat` stat a `Path` via `metadata` and `symlink_metadata`; `lstat` succeeds, and there is no flag rejection, truncation, or `realpath` step.
  - [P3] `StatEpochMilliseconds` returns `uint64_t` milliseconds as `tv_sec * 1000 + tv_nsec / 1000000` (nanoseconds truncated toward zero). A negative result wraps when converted to `uint64_t`. `StatNs` stores atime, mtime, and ctime as `i128` nanoseconds (`sec * 1_000_000_000 + nsec`), so sub-millisecond precision remains and pre-epoch times stay signed.
  - [P3] `ErrorMessage` is `strerror_r` into a 1024-byte buffer, or the string `strerror_r(<n>): errno <errno>` when `strerror_r` returns negative. I/O failures in Zel are `std::io::Error` values whose display text adds ` (os error N)`.
  - [P3] `portable_getxattr` and `portable_lgetxattr` call Darwin `getxattr` once (position 0, options 0 or `XATTR_NOFOLLOW`) and set `*attr_not_found` to `errno == ENOATTR` even if the call succeeded, because `errno` is not cleared. The caller-supplied size is not grown, so `ERANGE` is a failure. `getxattr_value` returns `Ok(None)` only when the call fails with errno 93, retries with a 4× buffer on `ERANGE` (34) until the value fits, and takes the attribute name as a UTF-8 `&str` (interior NUL rejected) rather than an arbitrary C string. A missing attribute is `Option::None` rather than a null result plus that side flag.

### `src/main/native/darwin/fsevents.cc`
- Bazel lines: 201
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P2] `poll` returns JNI null when `kFSEventStreamEventFlagMustScanSubDirs` is set or a directory is renamed (`everything_changed`), so the caller must treat the tree as unknown. `watch::FileWatcher::poll` always returns a `Vec` of snapshot diffs and never has that unknown result. A change that does not alter inode, size, or mtime is omitted even if the kernel reported the vnode.
  - [P2] Bazel appends each FSEvents path in callback order and can repeat a path (`std::list`). Rust emits each path once, classified as created, modified, or deleted, then sorts the vector. Snapshot and descriptor maps are `BTreeMap`s, so the pre-sort walk is path order.
  - [P2] Watch and exclusion paths are opaque UTF-8 bytes (`CFStringCreateWithBytes` / `jbyteArray`), and polled paths are returned as those raw C-string bytes. Ignore matching uses `file_name().to_string_lossy()`, so a non-UTF-8 directory name becomes U+FFFD, misses the ignore set, and is still descended into.
  - [P1] Exclusions are path prefixes passed to `FSEventStreamSetExclusionPaths`, with a latency in seconds and flags `kFSEventStreamCreateFlagNoDefer | kFSEventStreamCreateFlagFileEvents`, over every path in the array. Rust watches one root, ignores directory basenames only, silently stops registering kqueue vnodes at 4096, and `poll` blocks for a caller timeout then drains an extra 20ms. On Linux the same watcher uses inotify and saturates the poll timeout at `i32::MAX` milliseconds. `FileWatcher::new` returns `io::Error` if `kqueue` or `inotify_init1` fails; later `register_dir` errors are discarded. Bazel `create` is void and does not report `FSEventStreamCreate` failure.
  - [P2] `run` blocks in `CFRunLoopRun` only after counting down the listening latch, so the first poll cannot race stream startup. `doClose` stops that run loop and releases the stream. Rust arms the queue inside `new` and closes fds in `Drop`, with no latch and no run-loop thread.

### `src/main/native/darwin/sleep_prevention_jni.cc`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Bazel keeps a mutex-guarded push/pop stack and, on the first push, creates an `kIOPMAssertionTypeNoIdleSleep` assertion named `build.bazel` (`IOPMAssertionCreateWithName`, level on), releasing it only when the stack returns to zero. Both calls return `0` and abort if the stack is unbalanced, the assertion id is not null/non-null as required, or IOKit does not return success. Zel has no sleep-prevention API; `zel-ffi-bridge` only records this as an unimplemented gap.

### `src/main/native/darwin/system_cpu_speed_monitor_jni.cc`
- Bazel lines: 84
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel `portable_cpu_speed` returns the IOKit `IOPMCopyCPUPowerStatus` value for `kIOPMCPUPowerLimitProcessorSpeedKey` (the JNI caller treats it as a 1–100 speed limit). `system::cpu_speed_mhz` returns `hw.cpufrequency` / 1_000_000 on macOS, or the first `cpu MHz` value from `/proc/cpuinfo` elsewhere.
  - [P3] Failure is `-1` (`int`) for `kIOReturnNotFound`, any other `IOReturn`, or a missing speed key. Rust returns `None` (`Option<u64>`). A present value is a signed 32-bit `CFNumber` `int` versus a `u64` after integer division.
  - [P3] Bazel stays silent on `kIOReturnNotFound`, logs `BAZEL_LOG(ERROR)` with `mach_error_string` on other IOKit failures and on a missing key, and logs `BAZEL_LOG(USER)` `"cpu speed anomaly: "` on every successful read. Rust does not log.
  - [P2] `portable_start_cpu_speed_monitoring` runs once (`dispatch_once`), registers `notify_register_dispatch` on `kIOPMCPUPowerNotificationKey`, and calls `cpu_speed_callback(portable_cpu_speed())` on each notification. A register status other than `NOTIFY_STATUS_OK` is fatal (`BAZEL_CHECK_EQ`). Rust has no notifier, queue, or callback.
  - [P3] The test notification `com.google.bazel.test.cpuspeed` reads `notify_get_state` into a `uint64_t`, fatals on failure, and passes that state to `cpu_speed_callback(int)` (truncation to signed 32-bit). Rust has no test channel.
  - [P3] `CFNumberGetValue` with `kCFNumberIntType` ignores conversion failure, so a non-int number can return an uninitialized `int`. Rust returns a frequency only after a successful parse.

### `src/main/native/darwin/system_disk_space_monitor_jni.cc`
- Bazel lines: 61
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel `portable_start_disk_space_monitoring` registers four Darwin notify handlers once (`dispatch_once`): `kNotifyVFSLowDiskSpace`, `com.apple.system.verylowdiskspace`, `com.google.bazel.test.diskspace.low`, and `com.google.bazel.test.diskspace.verylow`. The low keys call `disk_space_callback` with level 0; the very-low keys call it with level 1. Zel `system::disk_space` never registers those names or reports a level. Each call runs `statvfs` on the given path and returns `(bavail * block_size, blocks * block_size)`, using `frsize` or `bsize` when `frsize` is 0.
  - [P3] A failed `notify_register_dispatch` aborts (`BAZEL_CHECK_EQ` against `NOTIFY_STATUS_OK`). Zel returns `io::Error` when the path contains NUL or `statvfs` fails, and does not abort.
  - [P3] A later Bazel call is a no-op. Zel queries again on every call.
  - [P3] Bazel logs `disk space low anomaly` or `disk space very low anomaly` before the callback. Zel does not.

### `src/main/native/darwin/system_load_advisory_monitor_jni.cc`
- Bazel lines: 80
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel registers Darwin `notify` handlers once for `kIOSystemLoadAdvisoryNotifyName` and `com.google.bazel.test.SystemLoadAdvisory`, and aborts if either `notify_register_dispatch` is not `NOTIFY_STATUS_OK`. The handler pushes the current advisory through `system_load_advisory_callback`. `system::load_average` never registers a notification and has no callback.
  - [P2] Bazel reads `notify_get_state` on the Apple token and returns `int` `0` / `25` / `75` for Great / OK / Bad. Rust calls `getloadavg` and returns `Option<[f64; 3]>` (1-, 5-, and 15-minute averages), with no 0/25/75 mapping.
  - [P3] Bazel aborts on `notify_get_state` failure and on any level other than those three (the `-1` sentinel is never returned). Rust returns `None` when `getloadavg` does not write exactly three values.
  - [P3] The test registration’s token is discarded; the handler still reads `gSystemLoadAdvisoryNotifyToken` (initially `0`, so a read before registration aborts). Rust has no test notification and no separate “not yet registered” failure.
  - [P3] This file is Darwin-only (Linux/BSD stubs of the same API return `0` and do not monitor). `system::load_average` is the same `getloadavg` path on every Unix target. `/workspace/zel-cursor/crates/zel-server/src/sys.rs` `load_average` is a further variant: one-minute `f64`, or `0.0` when `getloadavg` returns fewer than one sample.

### `src/main/native/darwin/system_memory_pressure_jni.cc`
- Bazel lines: 100
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel keeps a process-global level, default Normal, updated only by a `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE` event. `portable_memory_pressure` returns that cache, so it stays Normal until the first event. Zel’s `system::memory_pressure` polls `kern.memorystatus_vm_pressure_level` on every call and reports the current sysctl value with no prior event.
  - [P2] Bazel accepts only an exact flag: `DISPATCH_MEMORYPRESSURE_NORMAL` (0x1), `WARN` (0x2), or `CRITICAL` (0x4). Zel maps sysctl `2` to Warning and `4` to Critical; sysctl failure, `0`, `1` (XNU normal), `8` (XNU critical), and any other value all become Normal. A combined dispatch mask is fatal in Bazel and is not represented by the sysctl read.
  - [P3] An unknown dispatch level, `notify_get_state` failure, `notify_register_dispatch` status other than `NOTIFY_STATUS_OK`, or a null dispatch source aborts the process (`BAZEL_LOG(FATAL)` / `BAZEL_CHECK`). Zel returns Normal and does not exit.
  - [P3] `portable_start_memory_pressure_monitoring` runs once, resumes the dispatch source, and also registers notify `com.google.bazel.test.memorypressurelevel` (state 0/1/2 → normal/warn/critical; any other state aborts), then calls `memory_pressure_callback`. Zel has no start-monitoring entry, no callback, and does not observe that test notification.
  - [P3] Off macOS, the same Zel function reads Linux PSI `/proc/pressure/memory` `some avg10` (≥ 40 Critical, ≥ 10 Warning, otherwise Normal; a missing file is Normal). This Darwin file does not implement that path.

### `src/main/native/darwin/system_network_stats.cc`
- Bazel lines: 94
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P2] Darwin `sysctl` `NET_RT_IFLIST2` (ENOMEM retry, keep only `RTM_IFINFO2`) is not implemented. `system::net_io_counters` reads `/proc/net/dev` and returns an empty `Vec` when that file is missing, so macOS reports no interfaces. Bazel throws via `PostException` (usually `IOException`) on `sysctl` failure instead of returning success with an empty result.
  - [P2] Bazel records four counters per interface, in order `ifi_obytes`, `ifi_ibytes`, `ifi_opackets`, `ifi_ipackets` (`bytesSent`, `bytesRecv`, `packetsSent`, `packetsRecv`). Zel returns only `(name, bytes_received, bytes_sent)`: packet counts are omitted, and the byte order is receive then send.
  - [P2] Bazel writes a `HashMap` keyed by name (`sdl_nlen` bytes, `NewStringUTF`, so an interior NUL truncates the name; a later duplicate name overwrites). Zel returns a `Vec` of UTF-8 `String`s in source order and can keep duplicate names; invalid UTF-8 fails the whole `/proc/net/dev` read and yields an empty list.
  - [P3] Those counters are `u_int64_t` values passed as Java `long`. Zel uses `u64`, so a counter above `i64::MAX` stays non-negative in Rust and is negative as a Java `long`.

### `src/main/native/darwin/system_suspension_monitor_jni.cc`
- Bazel lines: 119
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No Zel code registers for Darwin power notifications or reports suspension. Bazel runs `portable_start_suspend_monitoring` once: `IORegisterForSystemPower` on the JNI dispatch queue, `IOAllowPowerChange` for `kIOMessageCanSystemSleep` and `kIOMessageSystemWillSleep` (the latter also calls `suspend_callback(SuspensionReasonSleep)`), `suspend_callback(SuspensionReasonWake)` on `kIOMessageSystemHasPoweredOn`, and ignores `kIOMessageSystemWillPowerOn` and `kIOMessageSystemWillNotSleep`. It sets `SIGCONT` to `SIG_IGN`, then dispatch sources call `suspend_callback` for `SIGCONT` (`SuspensionReasonSIGCONT`) and `SIGTSTP` (`SuspensionReasonSIGTSTP`). Failed port, `signal`, or `dispatch_source_create` checks abort. `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs` records suspension monitoring as unimplemented; `/workspace/zel-cursor/crates/zel-server/src/sys.rs` only installs or sends `SIGINT`, `SIGKILL`, and `SIGTERM` and does not emit those suspension reasons.

### `src/main/native/darwin/system_thermal_monitor_jni.cc`
- Bazel lines: 98
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no thermal monitor. Bazel registers `kOSThermalNotificationPressureLevelName` and `com.google.bazel.test.thermalpressurelevel` once, maps nominal/moderate/heavy/trapping/sleeping to loads 0/33/50/90/100, aborts on `notify_get_state` failure or an unknown level, invokes `thermal_callback` on each notification, and reads the current load from the registered token. `zel-ffi-bridge` only records thermal monitoring as an unimplemented gap.

### `src/main/native/darwin/util.cc`
- Bazel lines: 33
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel lazily creates one process-wide serial GCD queue named `build.bazel.jni` (`dispatch_once`) and aborts if `dispatch_queue_create` returns null. The Darwin memory-pressure, load, disk, CPU-speed, suspension, and thermal monitors enqueue callbacks on that queue. Zel has no shared serial queue or once-token; `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs` polls memory pressure, load, disk space, and CPU speed on the caller thread and does not implement suspension or thermal monitors.

### `src/main/native/darwin/util.h`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel exposes one process-wide serial libdispatch queue (`JniDispatchQueue`: `dispatch_once`, name `build.bazel.jni`, `DISPATCH_QUEUE_SERIAL`, abort if creation returns null) that the Darwin anomaly monitors share. Zel has no shared serial queue; `crates/zel-ffi-bridge/src/native.rs` only offers synchronous host probes and does not create or serialize that queue.

### `src/main/native/latin1_jni_path.cc`
- Bazel lines: 148
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P2] `NewStringLatin1` maps each C byte to one Java UTF-16 unit in U+0000..=U+00FF. `posix::readdir` builds Rust `String`s with `to_string_lossy`: valid UTF-8 becomes Unicode scalars and invalid UTF-8 becomes U+FFFD, so a lone 0xE9 is U+00E9 in Bazel and U+FFFD in Zel, while the UTF-8 bytes C3 A9 are two Latin-1 characters in Bazel and one scalar U+00E9 in Zel.
  - [P2] `GetStringLatin1Chars` copies a Latin-1 Java string’s bytes unchanged, and for a UTF-16 string replaces each UTF-16 code unit above U+00FF with `?` (a supplementary character becomes `??`) after `NativePosixFiles.logBadPath`. `cpath` passes `OsStr` bytes through `CString` with no `?` substitution and no bad-path log. A Rust string path is UTF-8, so U+00E9 is C3 A9 on disk rather than the single byte 0xE9, and scalars above U+00FF are kept.
  - [P3] A null Java string, or a null `String.value`, throws `NullPointerException` and returns null; `GetStringCritical` failure returns null with the pending out-of-memory exception. Zel POSIX helpers take `&Path` and return `io::Result`. An interior NUL is `ErrorKind::InvalidInput` from `CString::new`. Bazel copies U+0000 into the buffer, and the `const char*` / `std::string_view` view stops at the first 0.
  - [P3] `NewStringLatin1` stores `strlen` in a signed `int` before sizing the `jchar` buffer. Zel path lengths are `usize` and do not wrap at 2^31.

### `src/main/native/latin1_jni_path.h`
- Bazel lines: 52
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P2] `NewStringLatin1` maps each C byte to one Java UTF-16 unit in U+0000–U+00FF. `posix::readdir` uses `to_string_lossy()`, so valid UTF-8 becomes Unicode scalars and invalid UTF-8 becomes U+FFFD. `readlink` returns the raw `PathBuf` and leaves those bytes unexpanded.
  - [P2] `JStringLatin1Holder` copies a Latin-1 Java string as bytes and replaces every UTF-16 code unit above U+00FF, including each half of a surrogate pair, with `?`. `cpath` copies OS bytes unchanged. A Rust string path is UTF-8, so a scalar above U+00FF is encoded as multiple bytes.
  - [P3] A null `jstring` or a null `String.value` throws `NullPointerException` and the holder’s `char*` is null. `GetStringCritical` failure leaves `OutOfMemoryError` pending. Zel takes `&Path`; an interior NUL makes `cpath` return `io::ErrorKind::InvalidInput`.
  - [P3] A non-Latin-1 Java string calls `NativePosixFiles.logBadPath` before the `?` fallback. Zel has no such call.
  - [P3] `NewStringLatin1` stores `strlen` in a signed `int`, so a C string longer than `INT_MAX` wraps that length. Zel `CString` lengths are `usize`.
  - [P3] The Latin-1 copy keeps embedded NULs up to `GetStringLength`, but `operator string_view()` is built from `const char*` and stops at the first NUL. `CString::new` rejects an interior NUL.

### `src/main/native/process.cc`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel function returns the current process real uid (`getuid`) or real gid (`getgid`). The only nearby call is `geteuid` in `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs`, which checks a socket owner and is a different syscall.

### `src/main/native/unix_jni.cc`
- Bazel lines: 1376
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P2] Paths and names cross JNI as Latin-1. Rust syscalls use `OsStr` bytes, and `readdir` names go through `to_string_lossy()` (non-UTF-8 becomes U+FFFD). `readlink` is capped at `PATH_MAX` and returned as Latin-1; Rust returns the full target.
  - [P3] `stat`/`lstat` take an error mode: `'a'` always throws, `'f'` throws except `ENOENT`/`ENOTDIR` (null), any other mode returns null, and `EFAULT`/`EBADF`/`ENOMEM`/`ENOTSUP` become runtime exceptions. Rust always returns `Err`. The status is mode as `jint`, mtime and ctime in epoch milliseconds, and size and inode as `jlong`. Rust returns nanosecond `i128` mtime/atime/ctime and `u64` size and inode, so values above `2^63-1` are not truncated to a negative `jlong`.
  - [P3] Errno selects the Java type (`FileNotFoundException`, `FileAccessException`, `FileSymlinkLoopException`, `InvalidArgumentIOException`, `SocketTimeoutException`, `UnsupportedOperationException`, otherwise `IOException`). Rust surfaces one `io::Error`.
  - [P3] `utimensat` sets both atime and mtime from a millisecond `jlong`, or `UTIME_NOW` for both when `now` is true, without opening the file. Rust sets only mtime, at nanosecond resolution, by opening the file. Pre-epoch millis become a negative `nsec` in C++ (toward-zero division) and typically `EINVAL`; Rust subtracts a duration from the epoch.
  - [P2] `mkdir` returns false on any `EEXIST`. Rust returns `Ok(false)` only when the path `is_dir()` (symlinks are followed); an existing file or symlink-to-file is `Err`.
  - [P3] `readdir` keeps kernel order and `d_type` (`FILE`, `DIRECTORY`, `SYMLINK`, `CHAR`, `BLOCK`, `FIFO`, `SOCKET`, `UNKNOWN`), leaves `DT_UNKNOWN` unknown, retries `EIO`, and reports `closedir` errors. Rust sorts by name, collapses char/block/fifo/socket to `Unknown`, may classify `DT_UNKNOWN` via `file_type()`, and drops close errors.
  - [P1] `deleteTreesBelow` opens with `O_NOFOLLOW`, so a symlink root fails with `ELOOP` and is not emptied; chmod retries use mode `0700`, and the root mode is unchanged when the first open succeeds. Rust always `chmod`s the directory to `0755` (that follows symlinks), then deletes the followed target’s children and leaves that mode on the surviving root.
  - [P3] `getxattr`/`lgetxattr` use a fixed 4096-byte buffer (larger values fail) and return null when the attribute is missing. Rust grows the buffer and returns `Ok(None)` for `ENODATA`/`ENOATTR`. Attribute names are Latin-1 versus a UTF-8 `CString`.
  - [P2] `rename` and `remove` are absent from the posix module. `remove` returns false for both `ENOENT` and `ENOTDIR`; `LocalFileSystem::delete` returns `Ok(false)` only for not-found and returns `Err` for `ENOTDIR`.
  - [P3] Sleep push/pop (0 or -1) and the suspension, thermal, load-advisory, memory-pressure, disk-space, and CPU-speed register-once callbacks and `jint` queries (CPU speed 1–100, or -1) have no counterpart. `system::memory_pressure`, `disk_space`, and `cpu_speed_mhz` are separate polls (PSI enum, byte counts, MHz or `None`).

### `src/main/native/unix_jni.h`
- Bazel lines: 182
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P3] `StatEpochMilliseconds` returns epoch milliseconds in `uint64_t` (`tv_sec * 1000L` plus `tv_nsec / 1000000`, signed multiply then unsigned). Zel `posix::stat` / `lstat` keep `i128` nanoseconds, so sub-millisecond timestamps stay distinct and pre-epoch times stay negative.
  - [P3] `portable_fstatat` is `fstatat64` with flags on Linux. On Darwin any nonzero `flags` sets `errno` to `ENOSYS` and returns `-1`; a relative path is `F_GETPATH` plus `realpath` (follows links and fails if the target is missing). Zel has no directory-fd `fstatat`, only follow versus nofollow `stat` / `lstat`.
  - [P3] A missing xattr is `-1` with `attr_not_found` (`ENODATA`, or `ENOATTR` on Darwin and where `ENODATA` is absent). OpenBSD with no extattr always reports not found. The JNI caller uses a fixed 4096-byte buffer and treats `ERANGE` as an I/O error. Zel returns `Ok(None)` only for errno 61, or 93 on macOS, and grows the buffer on `ERANGE`. The attribute name is `&str`, so a non-UTF-8 name is `InvalidInput`.
  - [P3] `PostException` picks a Java exception from `errno` (`ENOENT` → `FileNotFoundException`, `EACCES` → `FileAccessException`, `ENOSYS`/`ENOTSUP` → `UnsupportedOperationException`, `ELOOP` → `FileSymlinkLoopException`, and so on) and appends `" (" + ErrorMessage(errno) + ")"`. Glibc `ErrorMessage` is an empty string when `strerror_r` fails; Darwin/BSD format `strerror_r(%d): errno %d`. Zel returns `io::Error` with no class mapping and no that suffix.
  - [P3] Darwin `portable_push_disable_sleep` / `pop_disable_sleep` keep a stack and return 0, releasing the no-idle-sleep assertion only at zero. Linux and BSD return `-1`. Zel has no sleep prevention.
  - [P3] Darwin suspend monitoring reports sleep `2` and wake `3` (SIGTSTP `0` and SIGCONT `1` are declared but not sent here), and thermal load is 0, 33, 50, 90, or 100, aborting on an unknown level. Linux/BSD suspend start is a no-op and `portable_thermal_load` returns 0. Zel has neither monitor nor callback. Starts are one-shot (`dispatch_once`).
  - [P3] `portable_system_load_advisory` is 0, 25, or 75 (great/ok/bad) on Darwin and constant 0 on Linux/BSD. Zel `load_average` is the 1/5/15-minute `getloadavg` triple, or `None` unless that call returns 3.
  - [P3] `portable_memory_pressure` is the last Darwin dispatch event, default `Normal` (0) before any event, and always `Normal` on Linux/BSD. Zel samples current state: sysctl value 2 is Warning and 4 is Critical, every other value is Normal; Linux PSI `some avg10` uses ≥40 Critical and ≥10 Warning, otherwise Normal. There is no callback. The 0/1/2 ordinals match only when the level is one of those three.
  - [P3] Disk space is callback-only: low `0` and very low `1`. Zel `disk_space` returns unprivileged-available and total bytes and never emits those levels.
  - [P3] `portable_cpu_speed` is a 1–100 limit, or `-1` on error; Linux/BSD always return `-1`. Zel `cpu_speed_mhz` is megahertz (`hw.cpufrequency / 1_000_000`, or `cpu MHz` truncated toward zero) or `None`.

### `src/main/native/unix_jni_bsd.cc`
- Bazel lines: 164
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P2] On FreeBSD, `portable_getxattr` and `portable_lgetxattr` call `extattr_get_file` and `extattr_get_link` in `EXTATTR_NAMESPACE_SYSTEM`, and set the missing-attribute flag when `errno == ENOATTR`. On OpenBSD both functions skip the syscall and return -1 with that flag set. Zel `posix::getxattr_value` calls Linux `getxattr`/`lgetxattr` or macOS `getxattr`, treats only raw errno 61 or 93 as missing (`Ok(None)`), and takes the attribute name as a UTF-8 `&str`.
  - [P3] `portable_fstatat` is `fstatat(dirfd, name, statbuf, flags)`. Zel `posix::stat` and `posix::lstat` use `std::fs::metadata` and `symlink_metadata` on a full path, so directory-fd relative lookups and other `fstatat` flags have no equivalent.
  - [P3] `StatEpochMilliseconds` returns `uint64` milliseconds (`tv_sec * 1000 + tv_nsec / 1000000`) for atime, ctime, and mtime, dropping sub-millisecond digits and converting a negative `tv_sec` to a large unsigned value. Zel `StatNs` stores signed nanoseconds in `i128`.
  - [P3] `ErrorMessage` copies `strerror_r` into a 1024-byte buffer and, when that call fails, returns `strerror_r(<n>): errno <errno>`. Zel reports `std::io::Error`, whose display text appends ` (os error N)` and uses a different string for an unknown code.
  - [P3] `portable_system_load_advisory` returns 0, and its monitoring starter does nothing. Zel `system::load_average` returns the three `getloadavg` doubles, or `None` unless `getloadavg` returns exactly 3.
  - [P3] `portable_memory_pressure` always returns `MemoryPressureLevelNormal`. On non-macOS, Zel `system::memory_pressure` maps `/proc/pressure/memory` `some` `avg10` to Warning at 10 and Critical at 40, and returns Normal when that file is missing.
  - [P3] `portable_start_disk_space_monitoring` does nothing. Zel `system::disk_space` returns unprivileged free bytes and total bytes from `statvfs` (`bavail`/`blocks` times the fragment size).
  - [P3] `portable_cpu_speed` returns -1. Zel `system::cpu_speed_mhz` returns `Some(megahertz)` from `hw.cpufrequency` on macOS or from `/proc/cpuinfo` otherwise, and `None` when that source is missing.
  - [P3] `getNetIoCountersNative` leaves the Java map unchanged. Zel `system::net_io_counters` returns `(name, bytes_received, bytes_sent)` parsed from `/proc/net/dev` when that file is readable, and an empty list otherwise.

### `src/main/native/unix_jni_linux.cc`
- Bazel lines: 139
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P3] `StatEpochMilliseconds` returns epoch milliseconds as `uint64_t` (`tv_sec * 1000 + tv_nsec / 1000000`): sub-millisecond digits are truncated, and a negative `time_t` wraps. `posix::stat` / `lstat` keep signed nanoseconds in `i128`. `portable_fstatat` is `fstatat64(dirfd, name, flags)` (directory-relative, caller flags). The Rust functions take a `Path` and only split follow-symlink `stat` from `lstat`.
  - [P3] `portable_getxattr` / `portable_lgetxattr` return the raw syscall size and set `attr_not_found` when `errno == ENODATA`. The JNI caller uses a fixed 4096-byte buffer, so `ERANGE` becomes an error. `posix::getxattr_value` maps errno 61 to `Ok(None)` and, on `ERANGE` (34), quadruples the buffer and returns the whole value. The attribute name is a Rust `&str` (UTF-8, interior NUL rejected) rather than an arbitrary C-string byte sequence.
  - [P3] `ErrorMessage` uses GNU `strerror_r` (the returned pointer may not be the stack buffer). On the POSIX branch, `strerror_r == -1` yields `""`. Rust surfaces `io::Error` text with an ` (os error N)` suffix and has no empty-string fallback.
  - [P3] `portable_memory_pressure` always returns `MemoryPressureLevelNormal` (0); monitoring is a no-op. `system::memory_pressure` on Linux reads `/proc/pressure/memory` `some avg10` and returns Warning at `>= 10` and Critical at `>= 40`.
  - [P3] `portable_cpu_speed` returns `-1` (the Java module treats that as no reading). `system::cpu_speed_mhz` returns the first `/proc/cpuinfo` `cpu MHz` value truncated to `u64`, or `None` when the line is absent.
  - [P3] `getNetIoCountersNative` does not write the counters map. `system::net_io_counters` parses `/proc/net/dev` (skipping the two header lines) into `(name, rx bytes, tx bytes)` in file order, omits packet counts, drops a line that fails to parse, and returns an empty `Vec` when the file cannot be read as UTF-8.

### `src/main/native/windows/build_windows_jni.sh`
- Bazel lines: 130
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel code locates `VCVARSALL.BAT` (`BAZEL_VC`, then `BAZEL_VS` or the newest `Microsoft Visual Studio N` under `C:/Program Files (x86)`, then Visual Studio 2019 BuildTools/Enterprise/Professional/Community), locates JDK headers (`JAVA_HOME` or the last sorted `jdk*` under `C:/Program Files/java`), keeps only `*.cc` arguments, copies `jni.h` and `win32/jni_md.h`, and `exec`s a generated batch that runs `CL /O2 /EHsc /LD` into the given DLL with `/DEFAULTLIB:advapi32.lib`. Failure still exits 1 after `ERROR:` on stderr when `which` cannot be found or those installs are missing. `zel-ffi-bridge` documents Windows native support as unimplemented and does not build this DLL.

### `src/main/native/windows/file-jni.cc`
- Bazel lines: 184
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Rust code implements these Windows JNI entry points or their integer result codes: symlink-or-junction (0 success, 1 error, 2 does not exist), change time (0–3, including access denied, FILETIME, follow-reparse-points), create junction (0–7, including target-name-too-long, already-exists-with-different-target, not-a-junction, disappeared, not supported), create symlink (0–2, directory target rejected), read symlink-or-junction (0–4, including not-a-link), delete path (0–4, including directory-not-empty and access denied), and get long path (boolean success, 8.3 expansion, `\\?\` kept only when the input had it).
  - [P3] Errors are written as Java UTF-16 (`jchar` from `wchar`) into `error_msg_holder[0]` only when that array is non-null, has length greater than 0, and the native error string is non-empty; success writes the boolean, long, or string holder and leaves the error slot untouched. Nothing in Zel uses that out-parameter contract.
  - [P1] `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs` is `cfg(unix)` and records Windows as unimplemented. `/workspace/zel-cursor/crates/zel-vfs/src/lib.rs` is a separate VFS: `create_symbolic_link` returns unsupported off Unix, `delete` maps a missing path to `Ok(false)` rather than code 2, `read_symbolic_link` uses lossy UTF-8, and `stat` reports modification time in milliseconds rather than Windows change time.

### `src/main/native/windows/file.cc`
- Bazel lines: 943
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No Rust port of this Windows file layer: developer-mode symlink privilege, `\\?\` / `\??\` / `\\.\` UNC handling, `NUL` as absolute, absolute-normalized path checks, reparse-point symlink/junction detection, change time from Windows `FILETIME` to Unix millis (follow vs open-reparse), long-path expansion, junction create (length limit, existing-target compare, `ERROR_DIR_NOT_EMPTY` / `ERROR_INVALID_FUNCTION` codes), file symlink vs directory-junction refusal, read of NTFS/WSL/mount-point/ProjFS reparse tags, delete that clears read-only and retries an empty-looking directory up to 20 times with 5 ms sleeps, backslash `Normalize`, and `GetCurrentDirectoryW` capped at 32767. `crates/zel-ffi-bridge/src/native.rs` is Unix-only and records Windows as unimplemented.

### `src/main/native/windows/file.h`
- Bazel lines: 249
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No Rust implementation of the Windows helpers: UNC prefixes (`\\?\`, `\\.\`, `\??\`), the case-insensitive `NUL` prefix check, drive-specifier detection, the developer-mode symlink privilege flag, junction/symlink/delete/change-time/read result codes, `GetLongPath`, `GetCwd`, or backslash `Normalize` (preserve case, keep a lone `.`, reject a leading separator, trailing `\` only for a bare drive).

### `src/main/native/windows/jni-util.cc`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Rust function copies a Java UTF-16 `jstring` into a Windows wide string, maps a null `jstring` to an empty string, or rewrites `/` to `\` (`GetJavaWstring` / `GetJavaWpath`). `crates/zel-ffi-bridge/src/native.rs` is Unix-only and records Windows as unimplemented.

### `src/main/native/windows/jni-util.h`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Rust implementation of these Windows JNI helpers. Bazel copies a non-null `jstring` as raw UTF-16/`WCHAR` units (`null` becomes an empty wide string) and `GetJavaWpath` then rewrites `/` to `\`. `zel-ffi-bridge` documents the Windows native layer as unimplemented, and nothing else converts JNI strings to wide Windows paths.

### `src/main/native/windows/process.cc`
- Bazel lines: 454
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] `WaitableProcess` is absent in the Rust tree: job creation with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, completion-port wait for `JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO`, `CreateProcessW` (`CREATE_SUSPENDED`, `CREATE_UNICODE_ENVIRONMENT`, `CREATE_NO_WINDOW` or `CREATE_NEW_PROCESS_GROUP`), the 32767-character command-line limit, the Windows 7 inheritable-console failure, the pre-Windows 8 nested-job fallback to `TerminateProcess`, terminate status 130 (`128 + SIGINT`), and `WindowsEscapeArg` (empty argument becomes `""`; quote and trailing-backslash runs are escaped for `CreateProcessW`). `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs` records Windows as unimplemented and only exposes Unix process helpers (`kill` with SIGTERM/SIGKILL and signal-0 liveness).

### `src/main/native/windows/process.h`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No Zel type implements `WaitableProcess` (`Create` with wide argv, env block, cwd, optional std handles, start time, `create_window`, and `handle_signals`; `WaitFor` codes 0/1/2; `GetExitCode`; `Terminate`; `GetPid`) or `WindowsEscapeArg`. `zel-ffi-bridge` `process` is POSIX pid/kill helpers, and `zel-sandbox` `process_wrapper.rs` ports `src/main/tools/process-wrapper*.cc`.

### `src/main/native/windows/processes-jni.cc`
- Bazel lines: 570
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`
- Verdict: partial
- Differences:
  - [P2] Zel `process` is Unix-only (`cfg(unix)`): current pid, parent pid, `kill` 0/15/9, nice, rusage, and CPU count. It does not create a child, wire stdin/stdout/stderr, wait, or read an exit code. The same file lists Windows as unimplemented.
  - [P3] `createProcess` always returns a `NativeProcess` pointer. A failed `Create` is a sticky wide-string error read later by `processGetLastError` and then cleared. Zel returns `io::Result` immediately and has no clearable last-error string.
  - [P3] A null Java string becomes an empty `wstring`. A null env `jbyteArray` means inherit. An env block must be at least 4 bytes and end in 4 zero bytes or creation fails. Empty stdout/stderr redirects mean pipes. Zel has no null-vs-empty path or env-block checks.
  - [P3] Paths are Java UTF-16 copied as `WCHAR`, `/` is rewritten to `\`, and redirects gain a `\\?\` prefix unless empty, `NUL`, or already UNC. Two non-empty redirects share one handle when `_wcsnicmp` matches. `redirectErrorStream` duplicates the stdout handle onto stderr and ignores the stderr path. Zel paths are raw Unix bytes with no slash rewrite, UNC prefix, or case-insensitive redirect merge.
  - [P3] Stdout/stderr files are `CreateFileW` `GENERIC_WRITE`, `FILE_SHARE_READ`, `OPEN_ALWAYS`, then seek to `FILE_END`. Pipes are 65536 bytes with inheritable child ends. Read/write bounds errors and I/O errors return `-1`. A broken pipe or a closed stream returns 0. `Close` calls `CancelIoEx` then `CloseHandle`. Zel has none of this stream or append-redirect behavior.
  - [P3] `waitFor` takes an `int64` timeout. Negative means infinite. It returns `0` success, `1` timeout, `2` error, and on success or timeout `WaitableProcess::WaitFor` terminates the process and its job with exit code 130 before returning. `getExitCode` returns `259` (`STILL_ACTIVE`) while running, the process code after exit, or `-1` if the query fails, all as `jint`. `getProcessPid` casts `DWORD` to `jint`, so pids above `2^31-1` wrap negative. `streamBytesAvailable` returns a `DWORD` as `jint`. Zel uses `u32` pids and `io::Result`, with no `259` sentinel, no `0`/`1`/`2` wait codes, and terminate is `SIGTERM` to one pid (`kill_hard` is `SIGKILL`) without a job object or exit code 130.
  - [P3] Error text is a Windows `wchar` message returned as a Java UTF-16 string. Zel surfaces `io::Error` as UTF-8.

### `src/main/native/windows/sleep_prevention_jni.cc`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Bazel keeps a process-wide `std::atomic_int` sleep stack starting at 0. The first `pushDisableSleep` calls `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_AWAYMODE_REQUIRED)`; later pushes only increment. `popDisableSleep` decrements and, when the count returns to 0, calls `SetThreadExecutionState(ES_CONTINUOUS)`. Both calls always return `0`. A pop that drives the count below 0 hits `assert` (abort when asserts are enabled). Zel never calls `SetThreadExecutionState` and has no push/pop sleep-prevention API.

### `src/main/native/windows/system_cpu_speed_monitor_jni.cc`
- Bazel lines: 46
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel `registerJNI` is a no-op and `cpuSpeed` always returns `jint` `-1` (the documented error). Zel has no Windows CPU-speed monitor: `cpu_speed_mhz` in `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs` is the Darwin/Linux frequency probe (`Option<u64>` MHz, `#![cfg(unix)]`), not this JNI.

### `src/main/native/windows/system_disk_space_monitor_jni.cc`
- Bazel lines: 32
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel `registerJNI` is an empty no-op: it does not register a monitor, query free space, or invoke `diskSpaceCallback`. The only related Zel function, `system::disk_space` in `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs`, is a Unix `statvfs` snapshot of `(bavail * frsize, blocks * frsize)` documented as the Darwin monitor, and that module is `#![cfg(unix)]` with Windows listed as unimplemented.

### `src/main/native/windows/system_load_advisory_monitor_jni.cc`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel `registerJNI` does nothing and `systemLoadAdvisory` always returns jint `0`. Zel has no Windows load-advisory entry point. `system::load_average` in `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs` is the Darwin `getloadavg` mapping (`Option<[f64; 3]>`, unix-only, `None` when fewer than three samples are returned), and `load_average` in `/workspace/zel-cursor/crates/zel-server/src/sys.rs` returns an `f64` one-minute average (`0.0` off Unix), not this advisory level.

### `src/main/native/windows/system_memory_pressure_jni.cc`
- Bazel lines: 46
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel’s Windows `systemMemoryPressure` always returns `0` (`Level.NORMAL`) and `registerJNI` does nothing. Zel has no Windows memory-pressure function: `system::memory_pressure` and `zel_memory_pressure_level` are compiled only for Unix and implement the Darwin/Linux monitor, not this stub.

### `src/main/native/windows/system_network_stats.cc`
- Bazel lines: 25
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `Java_com_google_devtools_build_lib_profiler_SystemNetworkStats_getNetIoCountersNative` is an empty JNI stub that leaves the counters map unchanged. Zel has no Windows implementation of that entry point. The only related function is Linux `/proc/net/dev` parsing in `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs` (`system::net_io_counters`, `#![cfg(unix)]`), which returns per-interface `(name, bytes_received, bytes_sent)` and is the counterpart of `unix_jni_linux.cc`, not this file.

### `src/main/native/windows/system_suspension_monitor_jni.cc`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel `SystemSuspensionModule.registerJNI` is an empty successful void return. Zel has no function that registers a Windows suspension monitor or accepts that call.

### `src/main/native/windows/system_thermal_monitor_jni.cc`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel `registerJNI` is a no-op and `thermalLoad` always returns `0`. Zel has no thermal-load query and no registration hook; `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs` only records thermal monitors as an unimplemented gap and is Unix-only.

### `src/main/native/windows/util.cc`
- Bazel lines: 331
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No Rust port of the Windows `CreateProcessW` helpers: `FormatMessageW` error text, `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` stdio inheritance, 8.3 `GetShortPathNameW` shortening under `MAX_PATH - 4`, unconditional quoting, the `\\?\` extended-path fallback, or the `.bat`/`.cmd` refusal of that fallback. Zel spawning is the Unix process-wrapper path and does not apply these Windows path or handle rules.

### `src/main/native/windows/util.h`
- Bazel lines: 176
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no Windows `HANDLE` closer, proc-thread attribute list for inherited stdin/stdout/stderr, or `FILE_TYPE_CHAR` console detection.
  - [P2] Zel has no `FormatMessageW` error text (`""` for code 0, otherwise the system message, or `Error code N; cannot format message due to error code M`) and no `ERROR: file(line): func(arg): message` wrapper.
  - [P2] Zel has no 8.3 short-path conversion (`MAX_PATH - 4`, reject quoted, drive-less absolute, non-normalized, or over-long bare names; `\\?\` input with the prefix stripped from the result) and no CreateProcess executable path (unconditional quoting, cwd join for relative paths with a separator, `\\?\` extended path only for a non-`.bat`/`.cmd` absolute normalized path when shortening fails).

### `src/main/protobuf/cache_salt.proto`
- Bazel lines: 43
- Zel counterpart: `/workspace/zel-cursor/crates/zel-proto/src/reapi.rs`
- Verdict: partial
- Differences:
  - [P2] `CacheSalt` keeps `may_be_executed_remotely` (1) and `workspace` (2) with proto3 defaults, but drops nested `ScrubSalt` and field 3 `scrub_salt`. Encoded action salt never includes the scrub-busting submessage, and a Bazel salt that has field 3 loses it on decode, so a scrubbed spawn can share a cache key with a non-scrubbed one.

### `src/main/protobuf/failure_details.proto`
- Bazel lines: 1419
- Zel counterpart: `/workspace/zel-cursor/crates/zel-proto/src/lib.rs`
- Verdict: partial
- Differences:
  - [P2] Typed `FailureDetail` encode/decode keeps only `message` and the category submessage's field-1 `code`. `Spawn.catastrophic`, `Spawn.spawn_exit_code`, `Crash.causes`, `Crash.oom_cause_category`, and `IncludeScanning.package_loading_code` are dropped, so the spawned process status, OOM category, and throwable chain do not round-trip.
  - [P2] Decoding oneof fields 101 (`Interrupted`) and 122 (`Command`) maps any code outside the hardcoded match to the 0 unknown value. `Command` codes 5 and 6 therefore use exit code 37 instead of metadata exit code 36. Deprecated `Interrupted` codes keep exit code 8 but lose their subcategory number. Other categories keep the numeric code and read `FailureDetailMetadata.exit_code` from the schema.

### `src/main/protobuf/invocation_policy.proto`
- Bazel lines: 213
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/invocation_policy.rs`
- Verdict: partial
- Differences:
  - [P2] `strategy_policy` (field 2) is not kept. Text input is schema-checked and then dropped; on the wire the length-delimited blob is skipped without parsing the submessage, so a corrupt `StrategyPolicy` still decodes.
  - [P2] `config_policies` is not part of this message. Text input that sets it is accepted, and binary field 3 is decoded into `ConfigPolicy`. Bazel text parsing rejects the unknown field, and `parseFrom` leaves field 3 unknown.
  - [P2] Text-format `\x` and octal escapes are accumulated as bytes and UTF-8-decoded, so `\xC3\xA9` becomes U+00E9 and a lone `\x80` fails the parse. Protobuf text format inserts each escaped byte as a Unicode code point. `\u` and `\U` are rejected, and an octal value above 255 is truncated with `as u8` instead of failing.
  - [P2] Base64 uses `char::is_whitespace` and accepts unpadded input. Bazel strips `Character.isWhitespace` (it strips U+001C–U+001F and keeps NBSP, U+2007, and U+202F) and `BaseEncoding.base64()` requires padding. After a successful base64 decode, a binary parse failure still falls through to the text parser; Java only does that when base64 decoding throws `IllegalArgumentException`.
  - [P2] An out-of-range `SetValue.behavior` varint is collapsed to `UNDEFINED` with `behavior_set` true. Proto2 keeps the unknown enum in the unknown-field set and the getter reports the default with the field unset. An omitted `flag_name` is stored as `""`, so unset and empty are the same.

### `src/main/protobuf/option_filters.proto`
- Bazel lines: 63
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/tags.rs`
- Verdict: partial
- Differences:
  - [P3] `OptionEffectTag` values 0–14 match. `OptionMetadataTag` matches through `NON_CONFIGURABLE = 8` and still leaves 5–7 unused, but Zel adds `FullyRedactedInLogs = 9`, which this proto does not define.

### `src/main/protobuf/test_status.proto`
- Bazel lines: 116
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs`
- Verdict: partial
- Differences:
  - [P1] Absent proto2 defaults stay `None`: `TestCase.run` (`[default = true]`), `TestResultData.status` (`[default = NO_STATUS]`), and `failed_status` (proto2 getter is `FULL`). `read_cache_status` in `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` then drops a `test.cache_status` whose status field was omitted (`d.status?`).
  - [P2] Unknown `TestCase.type` values other than 0–2 decode as `Unknown` (3), and unknown `TestCase.status` values other than 0–2 decode as `Skipped` (3). Proto2 leaves an unrecognized enum unset (getter returns `TEST_CASE` / `PASSED`) and keeps the original number in unknown fields. Re-encode writes the collapsed value and drops unknown fields.
  - [P2] `BlazeTestStatus.REMOTE_FAILURE` (6) survives on the wire as a raw `u64`, but `TestStatus` has no such variant. `TestStatus::from_i32` maps 6 and every other unknown value to `NoStatus` (0).
  - [P2] `test_times` and `test_process_times` are unpacked `repeated int64` (no `[packed = true]`). Decode also accepts a packed length-delimited payload and appends those values; protobuf-java for this schema skips that wire type.

### `src/main/tools/daemonize.cc`
- Bazel lines: 445
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/daemon_client.rs`
- Verdict: partial
- Differences:
  - [P2] The log is opened `O_WRONLY|O_CREAT|O_TRUNC` (mode `0666`) unless `-a`, which uses `O_APPEND`. Bazel passes `-a` only when `--server_jvm_out` is set, so the default `server/jvm.out` is truncated on each start. `start_server` always uses `OpenOptions` append, so the default log keeps growing.
  - [P1] The child `setsid()`s and sets `SIGHUP` to `SIG_IGN` before `exec`. Zel only calls `process_group(0)` (`setpgid(0, 0)`), so the server stays in the client session, and `ignore_signal` is never used. A later `SIGHUP` still kills it.
  - [P3] The parent writes the child pid as `%d` with no newline, then unblocks the child on a pipe. Exit `0` means that pid file exists before `exec`, and a later log-open or `exec` failure is the child’s exit `1` after the helper has already succeeded. Zel does not write the pid in the spawner. The server writes `server.pid.txt` only once it is listening, and `start_server` waits for a successful ping. Open, spawn, and startup-timeout failures return `36`. The child exiting first returns `37`. There is no separate helper exit `1` for missing `-l` or `-p`, unknown flags, or fewer than two positionals.
  - [P2] On Linux, `-c` writes pid `0` into `<mount><cgroup>/cgroup.procs` for every `cgroup` and `cgroup2` mount. Open, write, and close failures are non-fatal. Zel accepts `experimental_cgroup_parent` and does not move the server.
  - [P2] On Linux, `-s` runs `/usr/bin/systemd-run --user --scope -- /bin/true` when that binary is executable. If that exits `0`, it writes `#!/bin/bash` plus `exec -a <argv0> <exe> <args>` and replaces the process with `systemd-run --user --scope -- /bin/bash <wrapper>`. Arguments are left raw when every byte is in `A-Za-z0-9+-_.=/:,@`, otherwise wrapped in single quotes, or in double quotes with `\`, `$`, `"`, and `` ` `` backslash-escaped. `exe` is not escaped. A failed probe falls through to `execv`. Zel accepts `experimental_run_in_user_cgroup` and never probes `systemd-run` or writes that wrapper. `-c` and `-s` are ignored off Linux in Bazel. They are ignored on every platform in Zel.

### `src/main/tools/dummy-sandbox.c`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel `main` ignores `argc`/`argv` and always returns exit status 1 (a linkable `linux-sandbox` stand-in on platforms where sandboxing is not built). Zel has no binary with that behavior; `linux.rs` only reports the `linux-sandbox` strategy as unsupported and does not register it.

### `src/main/tools/linux-sandbox-options.cc`
- Bazel lines: 337
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No Rust parser of `linux-sandbox` argv. `getopt` handling of `-W/-T/-t/-i/-l/-L/-w/-e/-M/-m/-S/-h/-p/-C/-H/-n/-N/-R/-U/-P/-D`, `@file` expansion (recursive, stop at `--`, skip empty lines), absolute-path checks, `-M`/`-m` pairing, single-destination and `-n`/`-N`/`-R`/`-U` exclusivity, trailing-slash `-h` stripping, working-dir-under-sandbox-root check, `sscanf` integer timeouts, default `getcwd`, usage `exit(EXIT_FAILURE)`, and `DIE` on `@file` or `getcwd` failure are absent. `/workspace/zel-cursor/crates/zel-sandbox/src/linux.rs` only emits those flags (`LinuxSandboxCommandLineBuilder`); it does not parse or apply them.

### `src/main/tools/linux-sandbox-options.h`
- Bazel lines: 80
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/linux.rs`
- Verdict: partial
- Differences:
  - [P2] Zel only builds the helper argv (`LinuxSandboxCommandLineBuilder`). It does not implement `ParseOptions` into a global `Options`: no `@FILE` expansion, no `getcwd` when the working directory is unset, no `EXIT_FAILURE` usage path, and an empty command is accepted.
  - [P2] Bind mounts are two ordered vectors (duplicate targets kept). Zel stores `BTreeMap` target to source, so a repeated target overwrites and mounts are emitted in target sort order.
  - [P2] Timeouts and kill delays are signed 32-bit seconds, default 0. Zel uses `Option<Duration>` and emits `as_secs()` (`u64`), dropping any fractional second and allowing values that do not fit in a signed 32-bit `int`.
  - [P2] Paths, the sandbox root, and the command are byte strings (`std::string` / `char *`). Zel requires UTF-8 `String`, so non-UTF-8 paths and arguments cannot be represented.
  - [P2] Unset paths are empty strings, and hermetic mode is a separate bool plus `sandbox_root`. Zel uses `Option<String>`, so `Some("")` is distinct from unset, and hermetic-without-root or a root with hermetic false cannot be represented.
  - [P2] The C struct can hold both fake-root and fake-username. `build_for_command` returns an error when both are set.

### `src/main/tools/linux-sandbox-pid1.cc`
- Bazel lines: 815
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] No Rust port of PID 1 sandbox setup (`Pid1Main`: parent-death `SIGKILL`, user/mount/UTS maps, hermetic `pivot_root` vs non-hermetic bind/tmpfs remount, `/dev` and proc/sys mounts, loopback and ping-group range, signal forwarding, `128 + signal` wait status). Closest related file `/workspace/zel-cursor/crates/zel-sandbox/src/linux.rs` only builds the `linux-sandbox` command line and states the helper is not in the Rust build.

### `src/main/tools/linux-sandbox-pid1.h`
- Bazel lines: 25
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `Pid1Args` (`pipe_to_parent`, `pipe_from_parent`) and `Pid1Main` have no Rust entry point. `/workspace/zel-cursor/crates/zel-sandbox/src/linux.rs` only builds the external `linux-sandbox` command line and leaves the PID-1 helper unimplemented.

### `src/main/tools/linux-sandbox.cc`
- Bazel lines: 368
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] No Zel code runs the helper. Nothing clones a pid-1 child with `CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWIPC | CLONE_NEWPID | SIGCHLD`, adds `CLONE_NEWNET` when `create_netns != NO_NETNS` or `CLONE_NEWUTS` when `fake_hostname` is set, writes the child into each `cgroups_dirs` entry’s `cgroup.procs`, or does the parent/child pipe handshake after `prctl(PR_SET_PDEATHSIG, SIGKILL)`.
  - [P1] Timeout and signal handling from this file is absent: `alarm(timeout_secs)` when `timeout_secs > 0`, then `SIGTERM` and a second `alarm(kill_delay_secs)` before `SIGKILL` when `kill_delay_secs > 0` (immediate `SIGKILL` otherwise), the same handler for `SIGINT` only when `sigint_sends_sigterm` is set, and `SIGTTIN`/`SIGTTOU` ignored.
  - [P1] Lifetime and status behavior is absent: `PR_SET_PDEATHSIG` is `SIGKILL` unless `persistent_process` clears it; fds above stderr are closed except the debug file; `wait4` stops if `getppid()` is no longer the original parent; a signaled child exits `128 + signal`; before stats are written, the max of `memory.peak` and `memory.max_usage_in_bytes` across cgroup dirs replaces `ru_maxrss` with `peak / 1024` only when that peak is `> 0`.
  - [P1] `/workspace/zel-cursor/crates/zel-sandbox/src/linux.rs` only builds argv for an external `linux-sandbox` binary. `get_unsupported_reason(None)` always reports the binary missing, so the `linux-sandbox` strategy is never registered.

### `src/main/tools/linux-sandbox.h`
- Bazel lines: 22
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The process-wide outer credentials `global_outer_uid` and `global_outer_gid` (filled from `getuid()`/`getgid()` and written into the user-namespace `/proc/self/uid_map` and `/proc/self/gid_map`) are not stored or mapped anywhere under `/workspace/zel-cursor`. `crates/zel-sandbox/src/linux.rs` only builds the `linux-sandbox` command line and reports when that helper is unsupported.

### `src/main/tools/logging.cc`
- Bazel lines: 17
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel defines one process-wide `FILE* global_debug`, initialized to null so debug logging stays off until another tool assigns a stream (`fopen` of `-D` in `linux-sandbox`, or stderr in `daemonize`). Zel never opens or writes that log; `LinuxSandboxCommandLineBuilder` only forwards `-D` to an external `linux-sandbox` binary.

### `src/main/tools/logging.h`
- Bazel lines: 62
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs`
- Verdict: partial
- Differences:
  - [P2] `DIE` writes `file:line: "<printf>", <strerror(errno)>\n` (`", "` plus glibc `perror(nullptr)`, which is the errno string only) and `exit(1)`. `run_process` returns status 1 with stderr `src/main/tools/process-wrapper-legacy.cc:80: "execvp({}, ...)": {}\n` (colon, and the process keeps running). `errno_text` drops Rust's ` (os error N)` so the words can match `strerror`.
  - [P2] That line is only the `execvp` spawn failure. Other `DIE` sites (fork, setsid, fopen, mount, wait, bad stats) are not produced. `/workspace/zel-cursor/crates/zel-sandbox/src/bin/zel-process-wrapper.rs` prints `process-wrapper: execvp({}, ...): {e}` including `(os error N)` and then exits 1. `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs` rewrites only a macOS `sandbox-exec` status 71 into the colon form and remaps that spawn result to code 1.
  - [P2] `PRINT_DEBUG` is a no-op while `global_debug` is null, and otherwise writes `CLOCK_REALTIME` `sec` plus zero-padded 9-digit `nsec`, the call-site file and line, the message, a newline, and `fflush`s. Zel never opens that file or writes those lines. `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs` stores `-d` in `debug` and `run` never reads it. `/workspace/zel-cursor/crates/zel-sandbox/src/linux.rs` only appends `-D <path>` to an external `linux-sandbox` argv.
  - [P2] `DIE` is a printf format and always appends the current errno. Zel formats a Rust `String` plus `io::Error` display, so a non-errno failure has no `strerror` suffix, and a non-UTF-8 path cannot be passed through as the raw `%s` bytes.

### `src/main/tools/process-tools-darwin.cc`
- Bazel lines: 134
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel `WaitForProcessToTerminate` blocks in `kevent` (`EVFILT_PROC`/`NOTE_EXIT`) and does not reap, so the leader stays a zombie and holds the process-group id. Zel `run` polls `wait4` with `WNOHANG` and reaps the leader before `kill(-pid, SIGKILL)`, so the group id can be reused and that signal can hit another group.
  - [P1] Bazel `WaitForProcessGroupToTerminate` loops on `sysctl(KERN_PROC_PGRP)` until exactly one process remains and its pid is the leader, then returns while leaving that leader in the table (still alive or a zombie). Extra members get `SIGKILL` and a 1ms `nanosleep`, and the call does not return until they are gone. Zel sends one `SIGKILL` after the leader has already exited and does not wait for the rest of the group.
  - [P2] `pid < 0`, a `kevent` result other than one `NOTE_EXIT` for that pid, `nprocs < 1`, a leftover process whose pid is not the leader, and a `sysctl` error other than `ENOMEM` call `DIE` and `exit(1)`. `kqueue`, `kevent`, the size `sysctl`, and `nanosleep` (including `EINTR`) return `-1`, which the legacy wrapper also turns into `exit(1)`. Zel returns `io::Error` only when `wait4` fails with something other than `EINTR`, and otherwise returns `Ok` with the reaped status.
  - [P3] `WaitForProcessToTerminate` retries `kevent` on `EINTR` and its success value is `close(kq)` (`0`, or `-1` if `close` fails). Zel has no `kqueue` and sleeps 500µs between polls.

### `src/main/tools/process-tools-linux.cc`
- Bazel lines: 55
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel `TerminateAndWaitForAll` is used only after `PR_SET_CHILD_SUBREAPER`. Zel never becomes a child subreaper and, once the direct child is reaped, only `kill(-pid, SIGKILL)`s that process group. Descendants that left the group (`setsid`) are left running.
  - [P2] Bazel then `waitpid(-1, WNOHANG)` until it stops returning a pid, discarding every child’s status. `res == -1` (including `EINTR`, not only `ECHILD`) returns 0 immediately. Zel’s `wait4` waits only for the direct child and does not reap grandchildren.
  - [P2] If any child is still alive, Bazel reads `/proc/thread-self/children` and, while `waitpid(-1, WNOHANG)` is not `ECHILD` and `fscanf` yields a pid, `SIGKILL`s that pid, `usleep(100)`, and rewinds. A failed `fopen` returns 0 and leaves those children to be reparented to init. Zel has no `/proc` scan, no per-pid kill, and no retry. The C function always returns 0, so the wrapper’s `TerminateAndWaitForAll` failure path (`DIE`) is unreachable.

### `src/main/tools/process-tools.cc`
- Bazel lines: 350
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs`
- Verdict: partial
- Differences:
  - [P2] A graceful delay of 0 (the default) or any delay that truncates to 0 µs calls `setitimer(ITIMER_REAL)` with a zero value, which disarms the timer, so `KillEverything` sends only `SIGTERM`. Zel treats `kill_delay <= 0` as already elapsed and sends `SIGKILL` in that same poll. A negative delay makes Bazel `DIE` (exit 1) on `EINVAL`; Zel clamps it to 0 and `SIGKILL`s. Timeouts and the kill delay are a 500 µs poll, so they fire late relative to the real-time interval timer.
  - [P1] `graceful_sigterm` is stored and never read. No `SIGALRM`, `SIGTERM`, or `SIGINT` handler is installed, so those signals are not turned into `KillEverything`. Default disposition kills the wrapper and leaves the child process group running. Bazel handles `SIGINT` as an immediate group `SIGKILL`, and `SIGTERM` as an immediate group `SIGKILL` unless `graceful_sigterm` is set, in which case the group gets `SIGTERM` and the kill-delay alarm.
  - [P1] The child does not `ClearSignalMask`. Handlers are not reset to `SIG_DFL` and the signal mask is not cleared, and the Rust runtime’s ignored `SIGPIPE` is inherited across exec. Bazel unblocks every signal and restores default dispositions (except `SIGKILL` and `SIGSTOP`) before exec, and `sigprocmask` failure exits 1.
  - [P2] `PR_SET_CHILD_SUBREAPER` is not set, and the wait is `wait4(pid, WNOHANG)` for that pid only. Bazel’s subreaper path uses `wait`/`wait3` and keeps looping for every result other than that pid, including a hard `wait` error, so it reaps other zombies and can hang instead of exiting 1. Zel does not wait out grandchildren after the direct child exits.
  - [P1] Redirect runs for `""` and `"-"`, which Bazel skips. The file is opened append-only and then `set_len(0)`, so `ftruncate` fails on non-regular targets such as `/dev/null`, where Bazel’s `O_WRONLY|O_CREAT|O_TRUNC|O_APPEND` open succeeds. Bazel also exits 1 if `open` returns fd 0, 1, or 2. The redirect is attached only to the child; Bazel `dup2`s it onto the wrapper, so later wrapper diagnostics share that fd.
  - [P2] `SwitchToEuid` and `SwitchToEgid` are absent, so a setuid wrapper never does `setreuid(euid, euid)` or `setregid(egid, egid)` and never exits 1 on failure. `WriteFile`, `WaitPipe`, and `SignalPipe` are absent, so the formatted proc/cgroup writes and the one-byte pipe barrier are not there. Stats `write` is a single full-buffer write; Bazel’s loop still applies a negative count on `EINTR` or `EAGAIN`, which corrupts the stats file or spins.

### `src/main/tools/process-tools.h`
- Bazel lines: 110
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs`
- Verdict: partial
- Differences:
  - [P2] `SwitchToEuid` and `SwitchToEgid` call `setreuid` and `setregid` so the real id matches the effective id, and abort on failure. The Rust wrapper never changes uid or gid.
  - [P1] `Redirect` does nothing for an empty path or `"-"`, opens with `O_WRONLY|O_CREAT|O_TRUNC|O_APPEND` and mode `0666`, `dup2`s onto the wrapper fd, and aborts if that fd is below 3. Rust opens any `Some` path, including `""` and `"-"`, with create plus append and `set_len(0)`, and only the child receives that fd. `set_len(0)` fails on non-regular files such as `/dev/null`, where `open` with `O_TRUNC` succeeds. Open or `dup2` failure aborts the process there and returns `io::Error` here.
  - [P2] `KillEverything` with the graceful flag sends `SIGTERM` to the group and arms a real-time interval timer. A delay of 0, or any positive delay that truncates to 0 microseconds, disarms that timer, so `SIGKILL` is never sent. Rust clamps the delay to at least 0 and sends `SIGKILL` on the same poll once the elapsed time reaches that delay, so a 0 delay kills immediately. The timeout is checked about every 500 microseconds instead of being delivered by `SIGALRM`.
  - [P1] `InstallSignalHandler`, `IgnoreSignal`, `InstallDefaultSignalHandler`, and `ClearSignalMask` are absent. The child does not reset the signal mask or set handlers back to default before exec. The wrapper does not catch `SIGINT` or `SIGTERM`, so `graceful_sigterm` has no effect. A default `SIGINT` or `SIGTERM` kills only the wrapper, and the child, already in its own session, keeps running. In the C++ wrapper those signals kill the process group, and `SIGTERM` is graceful only when that flag is set.
  - [P2] `SetTimeout` uses `setitimer` with microsecond truncation. `Duration::from_secs_f64` panics on an infinite timeout, which `sscanf` accepts and which the C++ timer call does not reject.
  - [P2] `WaitChild` and `WaitChildWithRusage` block in `waitpid`, `wait`, `wait3`, or `wait4`, and on Linux with the child subreaper they reap unrelated zombies until the target pid is collected. Any `wait` error other than a matching pid spins forever because the loop condition stays true. Rust polls `wait4` with `WNOHANG` for that pid only, and returns the OS error instead of aborting.
  - [P2] `WaitForProcessToTerminate`, `WaitForProcessGroupToTerminate`, and `TerminateAndWaitForAll` are absent. There is no `kqueue` `NOTE_EXIT` wait, no Darwin process-group poll that re-sends `SIGKILL` until only the leader remains, and no Linux subreaper reap of `/proc/thread-self/children`. After the child is collected, Rust sends one `SIGKILL` to the group and does not wait for it. On macOS the C++ wrapper kills the group and waits for it before reaping the leader, so the process-group id cannot be reused in between.
  - [P2] `WriteStatsToFile` aborts if the open, the write, or an empty serialization fails. Rust uses `std::fs::write` and surfaces an `io::Error`, which the wrapper binary reports as an `execvp` failure and exits 1.
  - [P3] `WriteFile`, `WaitPipe`, and `SignalPipe` have no implementation. The C++ versions abort on `fopen`, `printf`, `close`, `read`, or `write` failure. The pipe helpers exchange a single zero byte and close the unused end.

### `src/main/tools/process-wrapper-legacy.cc`
- Bazel lines: 181
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs`
- Verdict: partial
- Differences:
  - [P1] On Linux the wrapper calls `prctl(PR_SET_CHILD_SUBREAPER, 1)` before `fork` and aborts if that fails with anything other than `EINVAL`. After the main child is reaped it `SIGKILL`s the process group and any remaining pids from `/proc/thread-self/children`, then waits them out. `run` never becomes a subreaper: it `wait4`s only the direct child and sends one `kill(-pid, SIGKILL)`.
  - [P1] `SIGALRM` always takes the graceful path (`SIGTERM` to the group, then `SIGKILL` after `kill_delay`). `SIGTERM` is graceful only when `graceful_sigterm` is set; otherwise it is an immediate `SIGKILL`. `SIGINT` is always an immediate `SIGKILL`. `last_signal` then replaces the child’s status: the default disposition is restored and that signal is re-raised. `run` never reads `graceful_sigterm`, installs no `SIGINT`/`SIGTERM` handlers, and always uses the graceful path for its own timeout. `request_global_cancel` and `CancelFlag` `SIGKILL` the group and set `cancelled`. Only `zel-process-wrapper` re-raises a signal, and only for the child’s terminating signal or `SIGALRM` after a timeout.
  - [P2] A graceful `kill_delay` of 0 (the unset default) is passed to `setitimer`, which disarms a zero timer, so the wrapper sends `SIGTERM` and never `SIGKILL`. `run` treats an elapsed delay of 0 as due and sends `SIGKILL` on that same poll. A negative delay makes `setitimer` fail and `DIE`s; `run` clamps it with `.max(0.0)` and `SIGKILL`s immediately.
  - [P1] The child clears the signal mask and resets every handler except `SIGKILL`/`SIGSTOP` to `SIG_DFL`, then `setsid`, `umask(022)`, and `execvp`. The Rust child only calls `setsid` and `umask(022)`, so ignored and blocked signals are still in effect after `exec`.
  - [P2] On macOS the wrapper waits with `kqueue`, `SIGKILL`s the group, and waits until only the leader remains before collecting status. `run` reaps with `wait4` first, then signals the group. On OpenBSD the wrapper does not `SIGKILL` the group after the wait; `run` does.
  - [P2] The timeout is an `ITIMER_REAL` `SIGALRM`, so `last_signal` is set even if the child exits while `waitpid` is blocked, and the wrapper still dies with `SIGALRM`. `run` checks the deadline only after a non-successful `wait4`, sleeping 500µs between polls, and if the child has already exited it reports that child’s status.
  - [P1] Resource stats are collected and written only when `stats_path` is non-empty, and a failed open or write `DIE`s before the post-wait group kill. With the subreaper on, `wait3`’s `rusage` is from whichever child the loop reaps last. `run` always `wait4`s the direct child, writes the proto after the group `SIGKILL`, and returns the I/O error. On macOS its `Timeval` has no padding after the 32-bit `tv_usec`, so the later `rusage` fields are shifted.
  - [P2] `fork`, `setsid`, and `execvp` failures call `DIE` (the exec case prints `process-wrapper-legacy.cc:80` and `execvp(...)`, then exits 1, and the parent forwards that status). `Command::spawn` fails in the parent; the binary prints `process-wrapper: execvp(...): ...` and exits 1.
  - [P1] This file leaves the child’s standard descriptors attached, so output is written live. `run` pipes them, or redirects to files opened `create`/`append` and then truncated, and the binary writes the captured stdout and then stderr only after the child has exited.

### `src/main/tools/process-wrapper-legacy.h`
- Bazel lines: 51
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs`
- Verdict: partial
- Differences:
  - [P1] `SetupSignalHandlers` installs `SIGALRM` as graceful, `SIGTERM` as graceful only when `graceful_sigterm` is set and otherwise abrupt, and `SIGINT` as abrupt. `OnAbruptSignal` sends `SIGKILL` immediately. `OnGracefulSignal` sends `SIGTERM` and arms `kill_delay`. `run` never installs those handlers and never reads `graceful_sigterm` while the child runs. A timeout is always `SIGTERM` followed by `SIGKILL`. The cancel flag and global cancel always send `SIGKILL`. A `SIGINT` or `SIGTERM` delivered to the wrapper does not kill the child's process group.
  - [P2] A graceful delay of `0` calls `setitimer` with a zero value, which disarms `ITIMER_REAL`, so no `SIGKILL` follows the `SIGTERM`. `run` treats a delay of `0` as already elapsed and sends `SIGKILL` in that same poll.
  - [P2] `WaitForChild` does not return. If `last_signal` is set it restores the default disposition and `raise`s that signal, ignoring the child's status. Otherwise it `exit`s with `WEXITSTATUS` or `raise`s `WTERMSIG`. `fork`, `execvp`, `prctl` (other than `EINVAL`), and wait failures use `DIE`, which writes `process-wrapper-legacy.cc:<line>: "…": <errno>` to stderr and `exit(1)`. `run` returns `WrapperOutcome` or `io::Error`. A timeout forces `Signaled(SIGALRM)` even when the child has already exited. Cancellation leaves the child's wait status in place and does not raise. An `exec` failure is reported on the parent, not written by the child onto the redirected stderr.
  - [P2] On Linux the wrapper calls `prctl(PR_SET_CHILD_SUBREAPER)`. After the target pid is reaped, `TerminateAndWaitForAll` `SIGKILL`s the group and also kills pids listed in `/proc/thread-self/children` that left the group, waiting until they exit. `run` only `wait4`s the direct child, then `kill(-pid, SIGKILL)` once, with no subreaper and no wait, so grandchildren that changed process group survive.
  - [P2] On macOS the group is `SIGKILL`ed and `WaitForProcessGroupToTerminate` runs before the child's status is collected. On OpenBSD there is no post-wait group kill. `run` always `SIGKILL`s the group after `wait4` and does not wait for it.
  - [P1] The child runs `ClearSignalMask` (empty mask, `SIG_DFL` for every catchable signal), then `setsid`, `umask(022)`, and `execvp`. The Rust child only calls `setsid` and `umask(022)`, so it keeps the parent's signal mask and dispositions.

### `src/main/tools/process-wrapper-options.cc`
- Bazel lines: 142
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs`
- Verdict: partial
- Differences:
  - [P2] `Usage()` prints one newline between the error and `Usage:`. `usage_text` prints a blank line there. Failure still exits 1 (`zel-process-wrapper`).
  - [P2] Unrecognized-option text uses `optopt` and `optind`: argv `-z` is `Unrecognized argument: -z (2)`. Unknown or ambiguous long options (`--foo`, `---`, `--s`) leave `optopt == 0`, so `%c` is a NUL. Rust prints `-{char}` or a literal `-?` and the 1-based index in `argv[1..]` (`-z (1)`, `-? (1)`).
  - [P2] `getopt_long` accepts a unique long-option prefix (`--t`, `--d`, `--kill_d`). Rust requires the full name.
  - [P2] A `no_argument` long option with `=value` (`--debug=1`, `--graceful_sigterm=1`) is an error (`optopt` is `d` or `g`). Rust sets the flag and ignores the value.
  - [P2] Clustered shorts are scanned left to right, so a no-arg flag may precede an argument-taking flag in the same word: `-dt5` is debug plus timeout `5`, `-god` is graceful plus stdout `d`, `-gdo FILE` is graceful, debug, and stdout `FILE`. If the first character takes no argument, Rust treats the whole cluster as flags and rejects `t`, `k`, `o`, `e`, or `s` inside it.
  - [P2] `sscanf` `%lf` accepts C99 forms Rust rejects or mis-parses: `inf` / `infinity` / `nan`, and hex floats (`0x10` is 16, `0x1.8p1` is 3). Rust stops at the first non-decimal byte, so `0x10` and `+0X10` become 0 and `0x` is accepted as 0, while `sscanf` rejects `0x`. Rust also skips Unicode `trim_start` whitespace; `%lf` skips only C-locale `isspace`.
  - [P2] A second `-o`, `-e`, or `-s` is rejected only when the stored `std::string` is non-empty, so an empty argument does not count and a later flag overwrites it. Rust treats `Some("")` as already set and returns the “more than one destination” error.
  - [P2] Paths and the command are raw `char*` bytes. `parse_args` takes `String`, and `/workspace/zel-cursor/crates/zel-sandbox/src/bin/zel-process-wrapper.rs` builds argv with `std::env::args()`, which replaces invalid UTF-8 with U+FFFD.

### `src/main/tools/process-wrapper-options.h`
- Bazel lines: 46
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs`
- Verdict: partial
- Differences:
  - [P2] `stdout_path`, `stderr_path`, and `stats_path` are empty `std::string` sentinels. Bazel’s duplicate check is `.empty()`, so `-o ''` stays unset (a later `-o` overwrites it) and `Redirect` skips `""` and `"-"`. Zel uses `Option<String>`: `Some("")` or `Some("-")` counts as set, a second flag errors, and `run` opens that path.
  - [P2] `ParseOptions` keeps raw `char*` argv. The wrapper builds `args` with `std::env::args()`, which requires UTF-8 and panics on non-UTF-8 paths or command arguments.
  - [P2] Timeouts use `sscanf` `%lf` (`strtod`: `inf`/`nan`, hex floats, C-locale whitespace). Zel’s `scan_double` accepts only a decimal `f64` after Unicode `trim_start`, so those inputs fail or parse differently (`0x10` is 16 vs 0).
  - [P2] A no-arg flag glued to a valued flag (`-gt1`, `-dt5`) is graceful/debug plus a timeout in `getopt`. Zel treats the cluster as boolean flags and rejects `t`. `--debug=…` and `--graceful_sigterm=…` are rejected by `getopt_long` and ignored as success in Zel.
  - [P2] Unknown-argument text differs: Bazel prints `optopt` and `optind` (`-z (2)` for `prog -z`; unknown long options use `optopt` 0). Zel prints `-?` for unknown long options and `i + 1` into the args after `argv[0]` (`-z (1)`).
  - [P3] `Options` is one global that `ParseOptions` does not clear (`getopt`’s `optind` sticks). Zel returns a fresh struct and adds `cancel`, which `SIGKILL`s the child process group.

### `src/main/tools/process-wrapper-windows.cc`
- Bazel lines: 27
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/bin/zel-process-wrapper.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel always writes to stdout `ERROR: process-wrapper is not yet implemented on Windows.`, then `Called with args:`, then `argv[i]=(...)` for every argument including `argv[0]`, and returns exit code 1. It never parses options or runs a child.
  - [P3] Zel parses the Unix wrapper flags and runs the child. It forwards the child's stdout and stderr and exits with the child's status, or re-raises a signal and then exits `128 + sig`. Parse errors and `execvp` failures go to stderr and exit 1. It never prints the Windows unimplemented message or the argv dump.

### `src/main/tools/process-wrapper.cc`
- Bazel lines: 58
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/bin/zel-process-wrapper.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel calls `SwitchToEuid` and `SwitchToEgid` (`setreuid`/`setregid` so the real ids match the effective ids) before redirect and spawn. Zel never does this, so a setuid or setgid wrapper keeps split credentials and the child and created files follow the real user.
  - [P1] Bazel `Redirect`s the wrapper’s own stdout and stderr with `dup2` before `RunCommand`. An empty path or `"-"` leaves the descriptor unchanged. The open is `O_WRONLY|O_CREAT|O_TRUNC|O_APPEND` mode `0666`, and a returned fd below 3 is fatal. The child inherits those descriptors, so output is live and later wrapper diagnostics go to the same files. Zel redirects only the child, treats `"-"` as a filename, and when there is no redirect it buffers both streams and writes all of stdout and then all of stderr after the child exits. Redirect and spawn failures are printed on the original stderr as `process-wrapper: execvp(...): ...` and exit 1. Bazel `DIE`s with `file:line: "...": perror` and exit 1, and an `execvp` failure happens in the child, so a redirected stderr receives that message and the parent reaps the child’s status.
  - [P1] The header contract is that a signal delivered to the wrapper (for example `SIGTERM`) kills this process with `raise` of that signal even when the child exits 0. Bazel installs `SIGALRM` as graceful, `SIGTERM` as graceful only with `-g` and otherwise as an immediate `SIGKILL`, and `SIGINT` as abrupt, then `raise`s `last_signal` or the child’s terminating signal under the default disposition and does not return. `return 0` is not the success path. Zel installs none of those handlers, so `-g`/`--graceful_sigterm` does not change how the wrapper handles `SIGTERM`, and `SIGINT`/`SIGTERM` are not forwarded to the process group. After `kill` of itself, if that signal does not terminate the process, Zel exits `128+sig`. If Bazel’s `raise` returns, `main` returns 0.
  - [P1] Bazel enables `PR_SET_CHILD_SUBREAPER` on Linux and, in the child, clears the signal mask and resets handlers to default before `execvp`. Zel’s `pre_exec` only calls `setsid` and `umask(022)`, then after `wait` sends `SIGKILL` to that process group, so grandchildren that leave the group are not reaped and the child keeps the parent’s signal mask and handlers.

### `src/md5_darwin_freebsd.sh`
- Bazel lines: 17
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/lib.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel runs `/sbin/md5` on each argument, pipes that labeled text into a second `/sbin/md5` on stdin, and keeps the first 32 bytes (`head -c 32`). Darwin/FreeBSD `md5` prints `MD5 (name) = <hex>`, so those 32 bytes include the `MD5 (` label and only a prefix of the hex. `binary_install_md5` returns 32 lowercase hex digits from an in-process MD5 (`hex_digest`), with no label.
  - [P3] The shell hashes the `md5` listing of every path in `"$@"` (argument order and path text are part of the input). Zel hashes the running executable’s bytes, or the build-label bytes if that file cannot be read. `ZEL_INSTALL_MD5_OVERRIDE` is returned as-is, with no length or hex check. A temp cache keyed by MD5 of `path|size|mtime` is reused only when the cached file is exactly 32 bytes.
  - [P3] The script does not set `pipefail`, so the exit status is `head`’s. A failing or missing `/sbin/md5` can still exit 0 and write a short or empty key. Zel does not fail that step; a read error still yields a digest.

### `src/md5_default.sh`
- Bazel lines: 17
- Zel counterpart: `/workspace/zel-cursor/crates/zel-client/src/lib.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel runs `md5sum` on each argument, then MD5s that textual listing (32 hex digits, two spaces, path, and a newline per file, in argv order) and prints the first 32 hex characters with no trailing newline. Zel’s `binary_install_md5` MD5s the raw bytes of the current executable; `daemon_client.rs` writes that string to `install_base_key`.
  - [P3] With no arguments, Bazel’s first `md5sum` reads stdin. Zel never reads stdin: `ZEL_INSTALL_MD5_OVERRIDE` is returned as-is when set (no length or hex check), otherwise the executable bytes, otherwise the MD5 of `build_label()`.
  - [P2] The script sets neither `set -e` nor `pipefail`, so a missing input still exits with `head`’s status (0 when 32 bytes are written) and a digest of whatever `md5sum` wrote to stdout. Zel ignores side-cache and `install_base_key` write errors, and an unreadable executable falls back to the build-label digest.
  - [P3] Bazel recomputes on every run, and the inner hash includes the `md5sum` line format and paths. Zel memoizes in a process `OnceLock` and reuses `<temp>/zel-install-md5-$USER/<md5(path|size|mtime)>` whenever that file is exactly 32 characters.

### `src/md5_openbsd.sh`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel source runs `/bin/md5` on `"$@"`, hashes that text with a second `/bin/md5`, and copies 32 bytes with `dd bs=32 count=1`. The pipeline status is `dd`'s, and `dd` also writes a record count to stderr.

### `src/merge_zip_files.sh`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel code merges input zips into one archive under an optional directory prefix (`-` means no prefix), with later zips overwriting earlier paths (`unzip -o`) and the result rewritten at deflate level 9 (`zip -9 -r`). Zip support in `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` only extracts archives.

### `src/minimize_jdk.sh`
- Bazel lines: 184
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never builds a minimized embedded JDK. The script selects modules from argument 3 (comma-joined, CR-stripped, plus `jdk.crypto.ec`) or `ALL-MODULE-PATH` after `--allmodules`, optionally unpacks a separate jmods archive, adds `jdk.crypto.mscapi` on Windows, runs `jlink` with `--enable-native-access=ALL-UNNAMED -XX:+UseCompactObjectHeaders`, deletes the listed distrusted cacert SHA-256 aliases, patches `java.exe` to an UTF-8 active code page, copies the debug libraries and license files, normalizes timestamps to `198001010000` before `zip -X` on non-Windows, and exits 1 when `jmods/java.base.jmod` is missing and no archive was passed.

### `src/package-bazel.sh`
- Bazel lines: 106
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Nothing under `/workspace/zel-cursor` builds the client install zip. Bazel copies the extra inputs, and unless the output name ends in `jdk_allmodules.zip` it unpacks the deploy jar, stamps `198001010000.00`, stores a name-sorted recompress, and writes `build.label` (or `no_version`) to `build-label.txt`. It then unzips embedded tools when that path is non-empty, extracts the platforms tar, and zips `A-server.jar`, the sorted files, and `install_base_key` last at level 1 for that dev output and level 9 otherwise. `/workspace/zel-cursor/scripts/package_zel.sh` only copies one native binary to three bazelisk names and may write SHA-256 sidecars.

### `src/package_zip.bzl`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Rust rule schedules a `PackageZip` action whose arguments are an optional `--fast` (when `dev_build` is true; default false), then the output path, `server_jar`, `install_base_key`, and `srcs`, with those files as action inputs and the Java `package_zipper` as the exec tool.

### `src/read_manifest.cc`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel code loads a Windows executable as a data file, extracts resource type `RT_MANIFEST` id 1, and writes those raw bytes to stdout, exiting 1 when the argument count is not 2 or when the load, find, load-resource, or lock step fails.

### `src/release_archive.bzl`
- Bazel lines: 131
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `release_archive` macro or `_rename` rule: no `src_map` copy-into-`name/target` targets, no sorted `zip -qjX` of sources, no merge of dependency zips under `package_dir` (default `-`) with a trailing `_zip` stripped from the output name and `output_to_bindir` forced on, and no `with_cfg` wrappers that pin `java_language_version` to 8 (plus `-Xlint:-options`) or 17.

### `src/rule_size_test.bzl`
- Bazel lines: 137
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel source implements this analysis-phase test: counting `DefaultInfo.files`, rejecting `expect < 0`, `margin` outside `[0, 100]`, or a non-zero `margin` when `expect` is 0, the integer percentage check `((diff * 100) // expect) > margin`, or writing the dummy `@REM dummy` `.bat` / `#!/bin/sh` executable selected by `is_windows`.

### `src/upload_all_java_tools.sh`
- Bazel lines: 120
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The script is a Buildkite release driver. It detects Windows, Linux, or other from `uname`, then on Windows sets `MSYS_NO_PATHCONV` and `MSYS2_ARG_CONV_EXCL` and forces the ACP code page to UTF-8 (`65001`), and on Linux appends a `C.UTF-8` locale alias.
  - [P3] It stamps one `git rev-parse HEAD`, Unix timestamp, and `bazel info release` version, checks `sun.jnu.encoding` with `//src:CheckSunJnuEncoding` (`-c opt`), builds `//src:java_tools_zip` and `//src:java_tools_prebuilt_zip`, and copies both zips out of `bazel-bin` (Windows paths via `cygpath -m`).
  - [P3] On non-Windows it parses `JAVA_VERSIONS` from `src/test/shell/bazel/BUILD` and runs `bazel_java_test_local_java_tools_jdk<version>` against those local zips, adding `--sandbox_tmpfs_path=/tmp` only on Linux. Windows skips that suite.
  - [P3] It always uploads the prebuilt zip through `//src:upload_java_tools_prebuilt`. Only Linux also uploads `//src:upload_java_tools` and `//src:upload_java_tools_dist`, passing the same commit, timestamp, and Bazel version. Zel has no equivalent build, test, or GCS upload flow.

### `src/upload_java_tools.sh`
- Bazel lines: 122
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no helper that resolves a java_tools zip through Bazel runfiles (`rlocation io_bazel/<zip>`), copies it to a writable temp zip, and appends a `README.md`.
  - [P3] That README records the commit hash, Bazel version, optional platform, and the rebuild command `bazel build //src:<zip basename>`.
  - [P3] Zel does not upload the zip with `gcloud` (`gcloud.cmd` on Windows) to `gs://bazel-mirror/bazel_java_tools/<dir>/<commit_hash>/java/java_tools[_<platform>]-<timestamp>.zip`.
  - [P3] The required flags (`--java_tools_zip`, `--gcs_java_tools_dir`, `--platform`, `--commit_hash`, `--timestamp`, `--bazel_version`) and Windows path handling (`MSYS_NO_PATHCONV`, `MSYS2_ARG_CONV_EXCL`, `cygpath -m`) are absent.

### `src/write_manifest.cc`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Windows tool that takes one executable path, reads a replacement application manifest from stdin, and writes it into that PE file.
  - [P3] Bazel requires exactly one argument. Any other count prints `Usage: <argv0> <filename>` to stderr and exits 1.
  - [P3] Stdin is copied as raw bytes in 4096-byte chunks. A read error is treated as end of input, and a short or empty buffer is still applied.
  - [P3] The update keeps every other resource (`BeginUpdateResourceW` with delete-existing false) and replaces only type `RT_MANIFEST` (24), id 1, language neutral (`MAKELANGID(LANG_NEUTRAL, SUBLANG_NEUTRAL)`), with the stdin length and no added trailing NUL.
  - [P3] Open, update, and commit failures print `Error opening file <path> for update: <GetLastError>`, `Error updating resource: <GetLastError>`, or `Error finalizing update: <GetLastError>` to stderr and exit 1. A failed `UpdateResource` returns without `EndUpdateResource`, so the file is not committed. Success exits 0.
  - [P3] The launcher `win_resources.rc` only embeds a fixed `win_manifest.xml` at link time. It does not update an existing executable from stdin.

### `src/zip_builtins.sh`
- Bazel lines: 61
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/src/lib.rs`
- Verdict: partial
- Differences:
  - [P3] Zel does not build a zip. `materialize_builtins` writes embedded file bytes into a directory. There is no `builtins_bzl/` entry prefix, no `zip -qDX0r@` (quiet, no directory entries, no extra fields, store-only, names from stdin), and no sorted path list passed to a zip binary.
  - [P3] Timestamps are not normalized to `198001010000.00`. The script `touch`es every file before zipping so the archive bytes are deterministic. `std::fs::write` leaves the current mtime.
  - [P3] Inputs are not the caller-supplied list under `builtins_root`. The script `cp -L`s each argument (symlink targets), then `mv staging/$builtins_root builtins_bzl` and archives only that tree, dropping anything outside the prefix. Zel embeds the tree copied by `/workspace/zel-cursor/scripts/sync_bazel_tools.sh`, which also copies `BUILD`. The Bazel genrule passes only `**/*.bzl`, so the bundled zip has no `BUILD` file.
  - [P3] Invocation and failure behavior differ. An empty first argument selects `zip` on `PATH`; otherwise the program is `$PWD/$1`. The output path is `$PWD/$2`. `set -euo pipefail` aborts on a missing zip, a failed `cp -L` (dangling symlink), or `mv` when `builtins_root` is not a prefix of the staged files. Zel has no such CLI.

### `src/zip_files.sh`
- Bazel lines: 50
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no tool that packs loose input files into a new zip rooted under a caller-supplied directory prefix.
  - [P3] Bazel takes `directory_prefix`, `output`, and input paths, stores each input under only its basename (`zip -j`), extracts those entries into `directory_prefix`, then rewrites the archive with `zip -r` so names are `directory_prefix/<basename>` (prefix directories included) and replaces `output` with `mv -f`.
  - [P3] `set -euo pipefail` aborts on a missing prefix or output, or on a failing `zip`, `unzip`, or `mv`. The temporary directory is removed on exit.
