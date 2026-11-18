# Bazel vs zel code differences: examples

Category rule: `examples/` sample projects and demonstration sources.

- Failure entries: 28
- Missing: 28
- Partial: 0
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `examples/cpp/hello-fail.cc`
- Bazel lines: 22
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this sample (no `examples/cpp` tree). It is a `cc_test` (`//examples/cpp:hello-fail_test` in `examples/cpp/BUILD`) that includes `examples/cpp/hello-lib.h` (line 1) and returns 1 (line 21) so the test fails. Zel’s test runner treats a non-zero exit as failure (`crates/zel-exec/src/test_runner.rs`). No separate include-path divergence was found in `crates/zel-rules` or `crates/zel-bazel-tools/builtins/common/cc` for this sample, because the sample is not part of the zel tree.

### `examples/cpp/hello-lib.cc`
- Bazel lines: 18
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Sample C++ library source (lines 1–18). Zel does not ship `examples/cpp/`. The only zel-owned examples directory found is `crates/zel-starlark/examples/` (`syntax_errors.rs`), which is unrelated.

### `examples/cpp/hello-lib.h`
- Bazel lines: 21
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Include guard `EXAMPLES_CPP_HELLO_LIB_H_`, includes `<string>` and `<memory>`, namespace `hello`.
  - Class `HelloLib`: explicit constructor `HelloLib(const std::string &greeting)`, method `void greet(const std::string &thing)`, private `std::unique_ptr<const std::string> greeting_`.
  - No `hello-lib` header, `HelloLib` symbol, or `examples/cpp` tree in zel.

### `examples/cpp/hello-world.cc`
- Bazel lines: 26
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Includes `examples/cpp/hello-lib.h` and `<string>`, builds `hello::HelloLib("Hello")`, and calls `greet` with `"world"` or `argv[1]` when present. The comment says that prints `"Hello world"` (or `"Hello "` plus the argument). `main` always returns 0, so the same file can pass as a `cc_test`.
  - Zel does not ship this example: no `examples/cpp` tree, no `hello-world.cc`, and no `HelloLib`.

### `examples/java-native/src/main/java/com/example/myproject/Greeter.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this sample. A filename string for this path shows up only inside a patch-corpus fixture (`crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/...`), not as Java source.
  - No UTF-16-specific indexing. The sample concatenates `String`s, calls `trim()`, and splits a `Scanner` on `"\n"`. It does not use `charAt`, `length`, or `substring`.

### `examples/java-native/src/main/java/com/example/myproject/HelloErrorProne.java`
- Bazel lines: 10
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Sanity-check class `HelloErrorProne` with `main`. The comment says it is an Error Prone integration check. There are no annotations.
  - The triggered pattern is `byte b = 0; result = b == 255`. A Java `byte` is signed (-128..127). `==` promotes it to `int`, so the comparison with the `int` constant 255 is always false. That is Error Prone’s `ComparisonOutOfRange` check. Bazel’s `java_library` `hello-error-prone` compiles only this file (it is excluded from `hello-lib`).
  - No string handling and no `char` indexes, so no `java.lang.String` UTF-16 behavior.
  - Zel has no `HelloErrorProne` sample. Its crates do not implement Error Prone or `ComparisonOutOfRange` and do not pass default `-Xep` flags. `java_native.rs` only forwards caller-supplied `plugins` and `javacopts`. Vendored BCR files mention the `error_prone_annotations` jar, not this compiler check.

### `examples/java-native/src/test/java/com/example/myproject/Fail.java`
- Bazel lines: 14
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - JUnit 4 test (`org.junit.Test`, `org.junit.Assert`). `testFail` calls `Assert.fail("This is an expected test failure.")` (lines 10–13). Zel does not ship this class, the package `com.example.myproject`, or that message.

### `examples/java-native/src/test/java/com/example/myproject/TestCustomGreeting.java`
- Bazel lines: 32
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this source. The path appears only as a filename inside a patch-corpus fixture, not as Java.
  - JUnit 4: `org.junit.Test` and `org.junit.Assert.assertEquals`. The `java_test` targets `custom`, `custom_with_test_class`, and `resource-fail` depend on `//third_party:junit4` (`junit:junit:4.13.2`).
  - After UTF-8 decode and `trim`: `testNoArgument` expects `Bye world` from `Greeter.main()`. `testWithArgument` expects `Bye toto` from `Greeter.main("toto")`. Both redirect `Greeter.out` to a `ByteArrayOutputStream`.

### `examples/java-native/src/test/java/com/example/myproject/TestHello.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this test. The only hit is a path string inside a patch-corpus fixture, not Java source.
  - JUnit 4: `org.junit.Test` and `org.junit.Assert.assertEquals`. `testNoArgument` expects `"Hello world"`. `testWithArgument` calls `Greeter.main("toto")` and expects `"Hello toto"`. Output is decoded with `StandardCharsets.UTF_8` and `trim()`.
  - No Java `String` UTF-16 indexing (`charAt`, `codePointAt`, or index-based `substring`/`length`).

### `examples/java-native/src/test/java/com/example/myproject/TestSleep.java`
- Bazel lines: 12
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `TestSleep.java` and no `com.example.myproject` test sources.
  - The single `@Test` method `testSleep` calls `Thread.sleep(15 * 1000)` (15 seconds) and then returns. There is no JUnit `@Test(timeout=...)` in this file. A Bazel `java_test` of the default size (`medium`, 300s) would still pass. Zel does not carry this example or that sleep.

### `examples/java-starlark/src/main/java/com/example/myproject/Greeter.java`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `examples/java-starlark` sample. The path appears only as a filename in a patch-corpus fixture.
  - `hello` loads `greeting.txt` from the runfiles tree (`JAVA_RUNFILES`, else `TEST_SRCDIR`, plus `/_main/examples/java-starlark/src/main/resources/greeting.txt`) via `FileInputStream`. Only `FileNotFoundException` falls back to `"Hello"`.
  - Java string concatenation: if both env vars are unset, `getRunfiles()` is `null` and `null + "/_main/..."` becomes the path `null/_main/...`, which then misses and falls back to `"Hello"`. `Scanner` with delimiter `"\n"` returns the first line only and does not `trim`. Output is `greeting + " " + obj`, so the default is `"Hello world"` (`args[0]` replaces `"world"`).

### `examples/java-starlark/src/test/java/com/example/myproject/Fail.java`
- Bazel lines: 7
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this file (no `examples/java-starlark` tree and no `com.example.myproject.Fail`).
  - This is not a JUnit test. Package-private `Fail` has `main` that only calls `System.exit(1)` (lines 3–6). No assertion message and no sleep. A `java_test` / `java_binary` of this class fails the action because the process status is 1.

### `examples/java-starlark/src/test/java/com/example/myproject/TestHello.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel file under `examples/java-starlark` and no `com.example.myproject` sources.
  - Expects `"Hello world\n"` from `Greeter.main()` with no args, and `"Hello toto\n"` from `Greeter.main("toto")`. Captured bytes are decoded with `StandardCharsets.UTF_8`. The expected value includes the trailing newline. The java-native `TestHello` trims and expects the string without that newline. This file does no UTF-16 indexing.
  - This test does not set `JAVA_RUNFILES` or `TEST_SRCDIR` and does not read `greeting.txt`. It only replaces `Greeter.out` with a `PrintStream` over a `ByteArrayOutputStream`. Runfiles lookup and the `"Hello"` fallback live in `Greeter.java`, which also has no zel counterpart.

### `examples/py/bin.py`
- Bazel lines: 3
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No `examples/py/bin.py` (or `examples/py/`) in the zel tree. Other `bin.py` hits are vendored `rules_python` tests and examples, not this sample.
  - The program is `print("Fib(5)=%d" % lib.Fib(5))`, a function call with no `from __future__ import print_function`. That is Python 3. Bazel runs it under the toolchain interpreter (default PY3). The launcher stub in `python_bootstrap_template.txt` still carries Py2-compat futures (`print_function`, and a comment that the stub itself targets 2.7). A Rust-hosted `py_binary` does not emit that stub (`py_*` without a toolchain is an analysis error in `zel-rules`). Executing this source as-is is a `SyntaxError` on Python 2.
  - The only string is a plain `"..."` literal. No `u` prefix and no `unicode()`. Python 3 treats it as Unicode text. Python 2 would treat it as a byte string. The file does not force either.
  - The source has no shebang. Bazel `py_binary` writes a separate stub whose first line is `#!/usr/bin/env python3` (`DEFAULT_STUB_SHEBANG` in `rules_python` `py_runtime_info.bzl`) and then runs this file. Copying `bin.py` itself as the executable leaves a file the kernel cannot exec.

### `examples/py/lib.py`
- Bazel lines: 5
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `examples/py` tree and does not ship this file. Vendored `rules_python` `lib.py` files are unrelated modules.
  - This file is only `Fib`. No string literals and no `print`, so there is no Python 2/3 string or print difference. The function is valid on both.
  - The Bazel example loads Starlark `@rules_python//python:py_library.bzl` (srcs only, no `imports`). Zel vendors rules_python 1.7.0 and runs that rule in Starlark. `py_library` puts `lib.py` in runfiles and `PyInfo`. It does not compile it. A dependent `py_binary` bootstrap (`site_init_template.py`) appends the `imports` list (empty here) and the workspace runfiles root to `sys.path`. It does not set `PYTHONPATH`. `from examples.py import lib` then resolves `examples/py/lib.py` under that root.
  - A native `py_library` or `py_binary` with no Starlark implementation fails analysis in `crates/zel-rules/src/lib.rs` (`rule class 'py_*' has no implementation`). Zel’s `py_builtins.merge_runfiles_with_generated_inits_empty_files_supplier` can add empty `examples/__init__.py` and `examples/py/__init__.py` so the package import does not depend on checked-in inits.

### `examples/py_native/fail.py`
- Bazel lines: 13
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `examples/` tree and no copy of this file. `TestGetNumber` / `test_fail` do not appear outside third-party trees.
  - No shebang. Run as `__main__`, `unittest.main()` runs `test_fail`, which calls `assertEqual(GetNumber(), 0)`. `examples.py_native.lib.GetNumber` returns 42, so the test raises `AssertionError: 42 != 0`. The runner records a failure, writes the usual unittest report to stderr (`F`, `FAIL: test_fail …`, traceback, `FAILED (failures=1)`), and `sys.exit(1)`.
  - Source is valid on Python 2 and 3 (no `print` statement; `unittest.TestCase` / `unittest.main` are the same). A `py_test` uses the toolchain interpreter, not a shebang, and only runs the body because it executes the file as `__main__`.
  - The failure comes from `unittest` itself, so it does not depend on zel `py_test` failure handling.

### `examples/py_native/lib.py`
- Bazel lines: 5
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this example. The path is only listed in a patch-corpus fixture (`crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/.../bazel_code_files.txt`), not as source.
  - `GetNumber()` returns the integer `42`. The body is a single `return` with no Python 2/3 branch, print, or version check.
  - `examples/py_native/fail.py` asserts `GetNumber() == 0`, so that assertion fails against this return value.

### `examples/py_native/bin.py`
- Bazel lines: 7
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship `examples/py_native/bin.py` or that package. Native `py_binary` is not implemented: `analyze_rule` in `crates/zel-rules/src/lib.rs` errors for rule class `py_binary`. The sample `BUILD` loads `@rules_python//python:py_binary.bzl`.
  - `from examples.py_native.lib import GetNumber` needs the main-repo runfiles root on `PYTHONPATH`. `from fib import Fib` needs `//examples/py_native/fibonacci` with `imports = ["."]`. Native `py_binary` does not collect `PyInfo.imports` or write the bootstrap `%imports%` and workspace path entries.
  - `bin.py` has no shebang. The rules_python stub starts with `#!/usr/bin/env python3` (`DEFAULT_STUB_SHEBANG`). Native `py_binary` emits no stub.
  - The two `print(...)` calls run only after that stub execs Python. Native `py_binary` does not invoke an interpreter.
  - `bin.py` does not read `sys.argv`. The Bazel bootstrap uses `sys.argv[0]` to find `.runfiles` and forwards `sys.argv[1:]` via `os.execv`. Native `py_binary` has no bootstrap.
  - No `# coding:` cookie. The file is ASCII. Python 3’s default source encoding is UTF-8. Native `py_binary` does not rewrite or declare an encoding.

### `examples/py_native/fibonacci/fib.py`
- Bazel lines: 8
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this file. The only hit is a path string in a patch-corpus fixture, not source.
  - `Fib(n)` returns `1` when `n` is `0` or `1`, and otherwise `Fib(n-1) + Fib(n-2)`. That is two recursive calls per step, with no tail call, memoization, or guard for `n < 0` (that case does not terminate). `Fib(5)` is `8`, classic Fibonacci shifted by one (`F(n+1)`).
  - Arithmetic is subtraction and addition of the results. There is no `/` or `//`, so Python 2 classic division and Python 3 true division do not apply. Small results match on both. The recursion limit is hit before integer width matters. Past the stack limit, Python 2 raises `RuntimeError` and Python 3 raises `RecursionError`.
  - The module docstring is an ordinary `"""` literal and is never printed or formatted. There is no `%` formatting and no `str`/`unicode`/`bytes` use. In Python 2 that literal is a byte `str`. In Python 3 it is a Unicode `str`. Nothing in the file reads it.
  - `examples/py_native/fibonacci/BUILD` loads `py_library` from `@rules_python//python:py_library.bzl` with `srcs = ["fib.py"]` and `imports = ["."]`. Dependents use `from fib import Fib`. A native `py_library` with the same `srcs` and `imports = ["."]` exposes this same source as the module `fib`. The file has no `python_version` or `srcs_version`, and its syntax is the same under either rule. Zel still has no native `py_library` implementation.

### `examples/py_native/test.py`
- Bazel lines: 17
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this example. There is no `examples/py_native/` tree. The `py_native` hit in `crates/zel-analysis/src/py_native.rs` is the analysis rule, not this sample.
  - `unittest.TestCase` named `TestGetNumber`: `test_ok` expects `GetNumber()` == 42. `test_fib` expects `Fib(5)` == 8.
  - Imports `unittest`, `from examples.py_native.lib import GetNumber`, and `from fib import Fib` (absolute top-level `fib`, not a relative import).
  - Runs only when `__name__ == '__main__'`, via `unittest.main()`, which calls `sys.exit` (0 on success, 1 on failure or error). Importing the module does not run the tests.
  - No shebang. The source is valid in both Python 2 and 3 (no `print` statement and no Python-2-only syntax). The module docstring calls it a binary.

### `examples/shell/bin.sh`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Shebang is `#!/usr/bin/env bash`. The script takes no arguments and does not read `$@`. `set -euo pipefail` is on. The only explicit exit is `1` on stderr when `bazel_tools/tools/bash/runfiles/runfiles.bash` cannot be found. It then `source`s `$(rlocation io_bazel/examples/shell/lib.sh)` and calls `showfile` with no arguments. Any other failure exits with that command’s status.
  - Zel does not ship `examples/shell/`. `crates/zel-rules` has no native `sh_binary` (`native_rule_classes()` omits it; `analyze_rule` errors unless the rule is Starlark). The `native.rs` comment that lists `sh_binary` is stale. Bazel 9 and zel both use vendored `@rules_shell` 0.6.1: exactly one `src`, and on non-Windows the output is a symlink to that script so the shebang runs it. Runfiles come from `data` and `deps`.
  - `zel-bazel-tools` only aliases `@bazel_tools//tools/bash/runfiles` to `@rules_shell//shell/runfiles` and contains the Windows `bash_launcher.cc` sources. It does not embed `runfiles.bash`. Bazel on Windows runs that launcher (`bash -c` plus the original argv). Zel `run` execs the output file directly and does not build the `.exe` launcher.
  - For a non-test, zel clears `RUNFILES_DIR` and `RUNFILES_MANIFEST_FILE` and materializes `<exe>.runfiles` (including `MANIFEST` and `_repo_mapping`), so this script’s `$0.runfiles` lookup is the same path Bazel uses. The main-repo runfiles directory is `_main`. `rlocation io_bazel/...` works only if the repo mapping maps the `MODULE.bazel` `repo_name` `io_bazel` to that directory.

### `examples/shell/test.sh`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship `examples/shell/` (`test.sh`, `lib.sh`, `BUILD`, or `data/test_file.txt`). The path only appears as a name in a patch-corpus fixture.
  - `test_output` sets `OUTPUT=$(showfile)` after `source` of `rlocation io_bazel/examples/shell/lib.sh`, and `EXPECTED_OUTPUT` from `cat` of `rlocation io_bazel/examples/shell/data/test_file.txt`. A POSIX `[ "${OUTPUT}" != "${EXPECTED_OUTPUT}" ]` is the only check. Both data files are the same text (`Hello Bazel!` plus a newline), so a hit is equality of those two reads.
  - Runfiles init failure prints `ERROR: cannot find @bazel_tools//tools/bash/runfiles:runfiles.bash` and `exit 1`. A mismatch takes the explicit `exit 1`. A match falls out of the function and the script ends 0. `set -euo pipefail` does not abort on the false `[` inside `if`. An unset expansion or a failed `source` still ends non-zero.
  - Sourcing: bash `source` of `runfiles.bash` from `RUNFILES_DIR`, the manifest (`grep -m1` plus `cut -d ' ' -f 2-`), or `$0.runfiles`, then `source` of `lib.sh`. `showfile` is defined only in that library. `sh_test` in `third_party/bcr/rules_shell/0.6.1/shell/sh_test.bzl` is `getattr(native, "sh_test", _sh_test)`. Zel has no native `sh_test` (`native_rule_classes` omits `sh_*`, same as Bazel 9), so the Starlark rule in `shell/private/sh_executable.bzl` runs: one `srcs` file, `deps`/`data` merged into runfiles, default `use_bash_launcher` false so the output is a symlink to the script.
  - The script is bash (`#!/usr/bin/env bash`, `[[ ]]`, `function`, `source`, `set -o pipefail`). The default launcher does not wrap it in the toolchain shell, so the shebang is what runs. Zel’s test runner execs that binary directly. `/bin/bash` is inserted only for a `--run_under` token with no `/`. The optional launcher shebang is the toolchain path from `shell/private/repositories/sh_config.bzl` (`BAZEL_SH`, else `bash`, else `sh`). POSIX `sh` would reject `[[`, `function`, and `pipefail`. `test-setup.sh` on both sides is `#!/usr/bin/env bash` and exports `RUNFILES_DIR`. This script does not use test-setup’s `rlocation`. It loads `runfiles.bash` itself.

### `examples/shell/lib.sh`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this file or `examples/shell`. The path is only listed in a patch-corpus fixture. Other `lib.sh` files under rules_shell tests and `crates/zel-server/tests/fixtures/build_matrix/ws/lib.sh` are unrelated.
  - The script is bash-only (`#!/usr/bin/env bash`, `set -euo pipefail`, `[[ ]]`). It defines `showfile`, which runs `cat "$(rlocation io_bazel/examples/shell/data/file.txt)"`. Its init sources `bazel_tools/tools/bash/runfiles/runfiles.bash` from `$0.runfiles`, `$0.runfiles/MANIFEST`, `$0.runfiles_manifest`, `RUNFILES_DIR`, or `RUNFILES_MANIFEST_FILE`.
  - Neither Bazel 9 nor zel has a native `sh_library`. Both use rules_shell: `getattr(native, "sh_library", _sh_library)` in `third_party/bcr/rules_shell/0.6.1/shell/sh_library.bzl`. The impl only unions `srcs`, `deps`, and `data` into `DefaultInfo.files` and runfiles (`ctx.runfiles(transitive_files=..., collect_default=True)`) plus coverage info. It does not rewrite the script or its shebang. The example BUILD puts `lib.sh` and `data/file.txt` on `data`, which is enough for both to be runfiles of `:lib`.
  - A dependent `sh_binary` (`bin.sh`) sources the library. The shebang is ignored and the binary's interpreter must be bash. `bin.sh` is also bash. `use_bash_launcher` defaults to false, so the binary is a symlink to its source. A `/bin/sh` shebang fails on `pipefail` and `[[`.
  - Under Bzlmod the files sit at `_main/examples/shell/...`. `rlocation` rewrites the apparent repo `io_bazel` (`MODULE.bazel` `repo_name`) to `_main` using the caller's repo. `showfile` calls `rlocation`, and `runfiles_current_repository` uses `BASH_SOURCE` of `bin.sh`. Init uses `$0`, which while sourced is the binary, so it finds `<bin>.runfiles`. Zel's execroot and runfiles workspace name are `_main` with Bzlmod, and the root module mapping records `repo_name` to the main repo, so that lookup is the same. Zel does not rewrite the script.
  - Both trees alias `@bazel_tools//tools/bash/runfiles` to `@rules_shell//shell/runfiles`. Zel has no native `sh_binary`, so `ROOT_SYMLINKS_SUPPORTED` is true and that target is a root symlink at `bazel_tools/tools/bash/runfiles/runfiles.bash`, the path this init searches.

### `examples/windows/dll/hello-library.cpp`
- Bazel lines: 15
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this file or `examples/windows/dll/` (`hello-library.h`, `BUILD`, `windows_dll_library.bzl`). The only hit is a path listing in a patch-corpus fixture.
  - This source exports `get_time` and `say_hello` with `DLLEXPORT`. The Bazel header maps that to `__declspec(dllexport)` when `COMPILING_DLL` is set and `__declspec(dllimport)` otherwise. `BUILD` passes `copts = ["/DCOMPILING_DLL"]` into `windows_dll_library`, which is `cc_binary(linkshared = 1)` plus an `interface_library` filegroup and `cc_import`. It does not use `windows_export_all_symbols`.
  - Vendored rules_cc does implement that link path: `third_party/bcr/rules_cc/0.2.17/cc/private/toolchain/windows_cc_toolchain_config.bzl` has `/IMPLIB:%{interface_library_output_path}`, `/DEF:%{def_file_path}`, and the `windows_export_all_symbols` / `no_windows_export_all_symbols` features (declared, not enabled by default). `cc_binary` / `cc_library` / `cc_shared_library` generate a DEF via `def_parser` only when that feature is on and `win_def_file` is unset. Otherwise they pass an empty DEF so the import library still comes from `__declspec(dllexport)`. The same logic is copied under `crates/zel-bazel-tools/builtins/common/cc/`, and `def_parser` is at `crates/zel-bazel-tools/resources/third_party/def_parser/def_parser.cc`. `__declspec` itself is left to the MSVC compiler.

### `examples/windows/dll/hello-library.h`
- Bazel lines: 13
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No `hello-library.h` and no `examples/windows/dll` tree in zel.
  - Include guard is `HELLO_LIBRARY_H`.
  - `DLLEXPORT` is `__declspec(dllexport)` when `COMPILING_DLL` is set, otherwise `__declspec(dllimport)`. The Bazel DLL target sets that only on the DLL compile (`copts = ["/DCOMPILING_DLL"]`). Importers must leave it unset.
  - Both exports are `extern "C"` with narrow `char *` (`get_time`, `say_hello`). No `__stdcall`/`WINAPI` and no `wchar_t`. A matching DLL build must export unmangled C names and `char *`, not wide strings.
  - Zel builtin cc rules do implement the surrounding mechanisms: `interface_library` is handled in `cc_library.bzl`, `cc_binary.bzl`, `cc_import.bzl`, and `cc_shared_library.bzl`, and `cc_helper.bzl` gates DEF generation on the `windows_export_all_symbols` feature. Zel’s own `resources/tools/cpp` toolchain has no Windows feature that defines that name (only vendored `rules_cc` does). This header still needs the explicit `declspec` macro either way.

### `examples/windows/dll/hello_world-load-dll-at-runtime.cpp`
- Bazel lines: 30
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this example. The only hit is a path string in a patch-corpus fixture. There is no `examples/windows/` tree.
  - `LoadLibrary(TEXT("hellolib.dll"))` then `GetProcAddress` for narrow `"get_time"` and `"say_hello"`, cast to `__cdecl` `char *()` and `void(char *)`. Both pointers must be non-null. `FreeLibrary` runs whenever the module loaded. No `GetLastError`. Failure prints `Failed to load dll and call functions\\n` and still `return 0`. `get_time()` is not null-checked.
  - `TEXT` selects `wchar_t` / `LoadLibraryW` only if `UNICODE` is already defined. Otherwise it is `char` / `LoadLibraryA`. Proc names stay `LPCSTR`. The DLL exports are `extern "C"` `char *`, not wide strings.
  - The basename has no directory and no runfiles lookup. `cc_binary` `data = [":hellolib.dll"]` only places `hellolib.dll` at the package runfiles path `examples/windows/dll/hellolib.dll`. This call succeeds only if that basename is also on the Win32 loader search path (exe directory, CWD, or `PATH`). A nested runfiles-only layout does not.

### `examples/windows/dll/hello_world-link-to-dll-via-lib.cpp`
- Bazel lines: 6
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this example. There is no `examples/windows/dll/` source tree. The filename appears only as a list entry in a patch-corpus fixture.
  - The `.cpp` is the implicit-link client. It includes `examples/windows/dll/hello-library.h`, uses `int main()` (`wmain` is not used), and uses narrow `char *` (`wchar_t` / `TCHAR` are not used). It calls `get_time()` and passes that pointer to `say_hello`. It does not call `LoadLibrary` or `GetProcAddress`.
  - `hello-library.h` declares both functions `extern "C"` and `__declspec(dllimport)` unless `COMPILING_DLL`, in which case they are `dllexport`. The DLL is built with `/DCOMPILING_DLL`. This binary is not, so the calls are imports.
  - The link is in `examples/windows/dll/BUILD` and `windows_dll_library.bzl`, not in this file. `cc_binary(name = "hello_world-link-to-dll-via-lib", deps = [":hellolib"])`. The macro builds `hellolib.dll` with `cc_binary(linkshared = 1)`, takes output group `interface_library` (MSVC import library, `.if.lib`, `/IMPLIB:%{interface_library_output_path}`), and wraps it with `cc_import(interface_library = ..., shared_library = ...)` plus the headers as `cc_library` `hellolib`. No user `.def` is set. Exports are `__declspec(dllexport)`.
  - That macro depends on `cc_binary(linkshared = 1)` publishing a real import library in `interface_library`, which on Windows also requires a def-file linker input (`win_def_file`, a `def_parser` `.gen.def` when `windows_export_all_symbols` is on, otherwise an empty `<name>.gen.empty.def` so the import library is still emitted). Zel’s Starlark rule in `crates/zel-bazel-tools/builtins/common/cc/cc_binary.bzl` implements `linkshared`, that def-file choice, and `output_groups["interface_library"]`, and `cc_import` accepts `interface_library`. Nothing in the zel tree builds this sample. Unix `linkshared` notes do not cover `.if.lib`, `/IMPLIB`, or the def file. A note in `zel_differences.md` that the Rust `cc_binary` analyzer ignores `linkshared` and stubs `cc_import` describes the legacy analyzer, not this Starlark rule.

### `examples/windows/dll/windows_dll_library.bzl`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not ship this macro or `examples/windows/dll/`. The only hit is a path string in a patch-corpus fixture.
  - The macro has no `select` and no OS test. It always creates four targets: `cc_binary(name = name + ".dll", srcs = srcs + hdrs, deps = deps, linkshared = 1, **kwargs)`, `native.filegroup(name = name + "_import_lib", srcs = [":" + name + ".dll"], output_group = "interface_library")`, `cc_import(name = name + "_dll_import", interface_library = ":" + name + "_import_lib", shared_library = ":" + name + ".dll")`, and `cc_library(name = name, hdrs = hdrs, visibility = visibility, deps = deps + [":" + name + "_dll_import"])`.
  - `visibility` is applied only on that `cc_library`. `**kwargs` (`copts`, `linkopts`, `defines`, `includes`, `data`, `win_def_file`, and the rest) go only to the `cc_binary`. The filegroup and `cc_import` receive none of them. `deps` is passed to both the DLL and the wrapper library. `linkshared = 1` is a bool true on the zel attribute converter (`0`/`1` are accepted).
  - The Windows branch is inside builtin `cc_binary`, and `crates/zel-bazel-tools/builtins/common/cc/cc_binary.bzl` matches `src/main/starlark/builtins_bzl/common/cc/cc_binary.bzl`. With `linkshared` and feature `targets_windows`, it passes a DEF file as `def_file_path`: `win_def_file` when set, otherwise a generated DEF only when `windows_export_all_symbols` is enabled and `no_windows_export_all_symbols` is off, otherwise `{dll}.gen.empty.def`. A target name ending in `.dll` is a legacy shared-library name, so the linked file is exactly `{name}.dll`. Output group `interface_library` is `library_to_link.interface_library` (resolved symlink first), otherwise the dynamic library. Builtin `cc_import.bzl` matches Bazel: `interface_library` is one file of `.ifso`, `.tbd`, `.lib`, `.so`, or `.dylib`. `shared_library` must be `.so`, `.dylib`, `.dll`, or `.pyd`.
  - Zel `filegroup` (`crates/zel-rules/src/native.rs`) fails analysis when `output_group` is not a key on the dependency. Bazel `Filegroup.getArtifactsForOutputGroup` uses `OutputGroupInfo.getOutputGroup`, which returns an empty set for a missing key. Bazel also rejects group names ending in `_INTERNAL_` before that lookup. This macro requests `interface_library`.
  - Zel’s exported builtin `cc_common` (`crates/zel-bazel-tools/builtins/common/cc/cc_common.bzl`, and `CC_COMMON_NATIVES` in `crates/zel-packages/src/builtins.rs`) has no `link` or `create_library_to_link`. Those calls are what fill `library_to_link.interface_library` for the filegroup. Bazel `cc_common.link` turns on interface-library generation for a dynamic-library output when `targets_windows` is enabled (`emit_interface_shared_libraries` in vendored `rules_cc` `cc/private/link/link.bzl`, still gated by `supports_interface_shared_libraries` and `interface_shared_objects`). That `.lib` is the file `cc_import` accepts. A group that instead contains the `.dll` fails the `interface_library` extension check.
