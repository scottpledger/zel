# Bazel vs zel code differences: java tools

Category rule: `src/java_tools/` JavaBuilder, SingleJar, JUnit runner, import-deps checker, and related Java tools.

- Failure entries: 230
- Missing: 161
- Partial: 69
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/BazelJavaBuilder.java`
- Bazel lines: 140
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (spawn setup); reduced-classpath retry in `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs` and `/workspace/zel-cursor/crates/zel-exec/src/lib.rs`; workers in `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`. Zel is a Rust binary and does not contain `BazelJavaBuilder`. It shells out to a JDK `java -jar <javabuilder.jar>` (or a non-jar toolchain executable).
- Verdict: partial
- Differences:
  - Args: this `main` either takes a single `--persistent_worker` or the full argument list, expands `@` files inside `OptionsParser`, and always installs `ErrorPronePlugin` plus `JavacOptions.createWithWarningsAsErrorsDefault`. Zel only builds the outer command line (`--output`, `--bootclasspath`, `--classpath`, `--javacopts` … `--`, `--strict_java_deps`, `--target_label`, and so on) as multiline `@params` files. It does not parse JavaBuilder flags or register Error Prone.
  - Classpath: `parseAndBuild` picks `ReducedClasspathJavaLibraryBuilder` when `dependencyModule.reduceClasspath()` is set, otherwise `SimpleJavaLibraryBuilder`. Zel never selects those builders. For `--experimental_java_classpath=bazel` it rewrites the classpath param file to direct jars plus `.jdeps` paths with `--reduce_classpath_mode BAZEL_REDUCED` (or `NONE`), then on `requires_reduced_classpath_fallback` reruns with the full classpath and `BAZEL_FALLBACK`. `JAVABUILDER` mode only passes `JAVABUILDER_REDUCED` and `--deps_artifacts` through to the external tool.
  - Compilation: `builder.run` and the `REQUIRES_FALLBACK` status (return 0, no diagnostics) live in the Java tool. Zel treats a successful spawn plus that `.jdeps` bit as the fallback signal and deletes outputs before the second attempt. There is no in-process compile.
  - Error handling: invalid command lines print `BazelJavaBuilder threw exception: ` plus the message and return 1. Any other exception calls `printStackTrace()` and returns 1. Diagnostics and `result.output()` go to the error writer, then `System.exit`. The worker path uses `WorkRequestHandler` and `ProtoWorkerMessageProcessor`, prints an `IOException` message, and `System.exit`s in `finally` after a 10-second CPU-before-GC setting. Zel wraps any non-zero JDK exit in its own `ERROR: … (Exit N)` action failure and does not reproduce that prefix, stack-trace path, or worker-lifecycle behavior. Its worker support only appends `--persistent_worker` and speaks the generic proto/JSON worker protocol.
  - Encoding: the one-shot path wraps `System.err` in `OutputStreamWriter(..., Charset.defaultCharset())`. Java strings are UTF-16 and are encoded with that charset when diagnostics are written. Under Bazel that default is ISO-8859-1 (`-Dfile.encoding=ISO-8859-1`). `printStackTrace()` bypasses this writer and uses `System.err` directly. Zel stores args as Rust UTF-8, writes param files with `as_bytes()`, sets the spawn `LC_CTYPE` to `C.UTF-8` (`en_US.UTF-8` on macOS), and does not pass `-Dfile.encoding=ISO-8859-1`, so the same jar’s `defaultCharset()` is typically UTF-8. `.jdeps` paths are decoded with `String::from_utf8_lossy` (U+FFFD), not the JVM charset.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/BazelScannerSuppliers.java`
- Bazel lines: 395
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel pins a frozen Error Prone error set (`bazelChecks()` filters `BuiltInCheckerSuppliers.allChecks()` to the 2023-08-17 `ENABLED_ERRORS` list, about 170 checkers) and wires it only from `BazelJavaBuilder.parse` as `ErrorPronePlugin`. Zel never builds a `ScannerSupplier` or selects checkers. `java_native.rs` only launches the toolchain `_javabuilder` jar; the vendored `error_prone_annotations-2.0.15` jar is annotations, not this checker runtime.
  - Charset: this class does no I/O. Zel’s compile action sets `LC_CTYPE` to `C.UTF-8` (`en_US.UTF-8` on macOS) in `utf8_env`, and `zel-exec/src/java_compile.rs` decodes `.jdeps` `dependency.path` with `String::from_utf8_lossy`.
  - Path: this class does not touch source or classpath paths. Zel’s reduced classpath keeps jars by raw string equality of those `.jdeps` paths against `transitive_inputs`, with no separator normalization.
  - Missing JDK: this class does not look up a JDK or bootclasspath. Zel fails the compile action only when `java_toolchain` has no `_javabuilder`; an empty bootclasspath is still passed through `--bootclasspath`, and there is no in-tree fallback when the Error Prone or javac classes inside that jar are absent.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/InvalidCommandLineException.java`
- Bazel lines: 27
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Type only: a checked `Exception` with `(String message)` and `(String message, Throwable cause)`. No message text or exit logic in the file.
  - `BazelJavaBuilder.parseAndBuild` catches it, writes `BazelJavaBuilder threw exception: ` plus `getMessage()` to the error writer, and returns 1. The non-worker `main` then `System.exit`s that code. Other exceptions print a stack trace and also return 1. The persistent-worker path returns the same 1 to the work-request callback.
  - `VanillaJavaBuilder.run` returns a failed result whose output is only `getMessage()` (no prefix, no exit in that method). `BlazeJavacMain.compile` returns `BlazeJavacResult.error(getMessage())`.
  - `OptionsParser` throws `unknown option : '<arg>'`, `processor argument may not contain commas: <arg>`, and `<arg>: missing argument` (cause `NoSuchElementException`). Unterminated `--javacopts` throws `IllegalArgumentException`, not this type. `JacocoInstrumentationProcessor` throws when fewer than one argument is given. `ErrorPronePlugin` wraps the Error Prone option exception message.
  - Zel has no JavaBuilder sources. It launches the external JavaBuilder and treats a non-zero spawn as a generic action failure. `zel-options` is the client options parser, not this exception.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/JarOwner.java`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel type stores a jar together with a rule label and an aspect. `JarOwner` is an AutoValue of `Path jar`, `Optional<String> label`, and `Optional<String> aspect` (`create` / `withLabel` only).
  - Closest strict-deps and `.jdeps` code only forwards flags and matches classpath path strings: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`--strict_java_deps`, `--target_label`, `--injecting_rule_kind`, `--direct_dependencies`) and `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs` (`reduced_classpath` via `HashSet<String>`). Manifest `Target-Label` / injecting-rule attribution of a class to a jar is not implemented.
  - This file does not compare names itself. Generated equality is `Path.equals` (abstract default-filesystem path; case-sensitive on Linux; not `String.equals` and not normalized) plus `String.equals` inside `Optional` for label and aspect (UTF-16 code units). Zel membership is Rust `String` `==` / `Hash` (UTF-8 bytes). The `\` to `/` rewrite lives in the dependency-proto writer, not here, and Zel does not do it.
  - Nulls: `jar` and the `Optional` wrappers are non-null. `create(Path)` stores empty label and aspect. `create(Path, String, Optional)` uses `Optional.of(label)`, so a null label throws; the aspect `Optional` may be empty. `withLabel` can clear the label and leave the aspect. Zel uses `Option::None` and omits the flag; compile actions default a missing `target_label` to the rule label. `.jdeps` parsing ignores `rule_label` and has no aspect.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/JavaLibraryBuildRequest.java`
- Bazel lines: 447
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`java_create_compile_action`, `java_register_compile`, `utf8_env`); classpath rewrite in `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs`
- Verdict: partial
- Differences:
  - Zel emits the JavaBuilder command line this request is built from. It does not reimplement the in-process request: `workDir.resolve` of every path, dependency and annotation-processing modules, compiler plugins, or `toBlazeJavacArguments` / `addJavacArguments`.
  - Sources: Bazel keeps a mutable source-file list plus source jars and a source path, and later expands jars into that list before javac. Zel passes `--sources`, `--source_jars`, and `--sourcepath` and does not expand jars.
  - Classpath: Bazel stores the full classpath, bootclasspath, `--system`, reduce mode, and full/reduced lengths, then compiles against a caller-supplied classpath and sets fail-fast only for `BAZEL_REDUCED` with no processors. Zel puts the full classpath (bootclasspath auxiliary entries first) on its own param line and rewrites it to a reduced classpath, then `BAZEL_FALLBACK`. It forces mode `OFF` when strict deps are off, `.jdeps` is missing, or a processor is reduced-classpath-incompatible. It does not record classpath lengths or fail-fast.
  - Processors: Bazel joins deduped processor names into one `-processor` argument when both processors and source files are present, otherwise forces `-proc:none`, and rejects `-processor` / `-processorpath` inside javacopts. It also registers the dependency and processing plugins and derives a source-gen directory. Zel only forwards `--processors` and `--processorpath`.
  - Outputs: Bazel resolves the output jar, generated-sources jar, and native-header jar, and derives `_javac/<label-name>/<jar>_classes|_tmp|_sources` plus `native_headers` under the temp dir. Zel passes `--output`, `--generated_sources_output`, `--native_header_output`, `--output_deps_proto`, `--output_manifest_proto`, and always `--compress_jar`. It does not derive those directories. Jacoco and `requestId` exist only on the Bazel request.
  - Encoding: this file never sets `-encoding`. Javac opts, labels, and processor names are Java `String` (UTF-16); `label.substring` / `lastIndexOf(':')` are UTF-16 code-unit indexes; paths are `java.nio.file.Path` from `workDir.resolve(String)`. Zel flag and path strings are Rust UTF-8. The spawn only sets `LC_CTYPE` to `C.UTF-8` (`en_US.UTF-8` on macOS) so the JVM reads that argv as UTF-8 into UTF-16. Unpaired UTF-16 surrogates can sit in this request’s strings and cannot round-trip through Zel’s UTF-8 argv.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/OptionsParser.java`
- Bazel lines: 445
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (emits the flags into multiline `@` param files), `/workspace/zel-cursor/crates/zel-analysis/src/action.rs` (`render_param_file`), `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs` (`expand_argument`), `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`expand_param_file_args`). No JavaBuilder options parser.
- Verdict: partial
- Differences:
  - `@`-file expansion in `OptionsParser` is recursive, treats `@@` as an escaped `@` (`substring(1)`), skips only `length() == 0` lines, does not trim or unquote, and does not special-case `@repo//`. Zel's worker `expand_argument` leaves `@@` intact, refuses undeclared inputs, and does not expand `@repo//` (that matches `WorkerSpawnRunner`, not this parser). Empty lines become empty args there. `expand_param_file_args` is one level, trims and drops blank lines, and turns `@@x` into a lookup of `@x` (kept only if that read fails).
  - No quote processing in this parser: one non-empty line is one argument. Java compile param files are `ParamFileFormat::Multiline` (raw line, no `shell_escape`), which matches that reader. Shell param files would not. Quoting exists only earlier, in `java_tokenize_opts` / `shell_tokenize`, which splits `javacopts` before they are written.
  - Unknown flags throw `InvalidCommandLineException("unknown option : '" + arg + "'")`. Zel does not parse JavaBuilder argv and does not reject unknown flags; `extra_args` are appended as-is.
  - `--javacopts` is read until an exact `--`, or it throws `IllegalArgumentException`. A following `-sourcepath <path>` is removed from `javacOpts` and split on `File.pathSeparatorChar` (trim, omit empty) into `sourcePath`. Zel always writes a terminating `--` and a separate `--sourcepath` from the attribute; it does not pull `-sourcepath` out of javacopts.
  - `--processors` stops at any arg starting with `-` and rejects any value that contains `,`. Zel emits one argument per processor class and does not check for commas. `--processorpath` is a separate list in both.
  - `@` files are `Files.readAllLines(..., UTF_8)`. Java's decoder reports malformed UTF-8 (`MalformedInputException`) into a UTF-16 `String`; `BufferedReader` breaks on `\n`, `\r`, and `\r\n`; emptiness is UTF-16 code-unit length. Zel worker `read_to_string` also fails the file on invalid UTF-8, but `str::lines()` does not break on a lone `\r`, and a `String` cannot hold unpaired surrogates. `expand_param_file_args` uses `String::from_utf8_lossy` (U+FFFD) instead of failing. Param bodies are written as UTF-8 with no BOM, so valid Unicode round-trips; a Java `String` that is not valid Unicode cannot be represented in Zel.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/ReducedClasspathJavaLibraryBuilder.java`
- Bazel lines: 121
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs` and `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (spawn retry in `/workspace/zel-cursor/crates/zel-exec/src/lib.rs`)
- Verdict: partial
- Differences:
  - `JAVABUILDER_REDUCED` reduction and the in-process second compile stay in this class. It calls `computeStrictClasspath` only when there are sources, then `fallback` resets the output dirs with `prepareSourceCompilation` and invokes javac again on `getClassPath()`. `java_native.rs` only emits `--reduce_classpath_mode JAVABUILDER_REDUCED` and `--deps_artifacts`. The executor retry loop does not run for `JAVABUILDER`.
  - `BAZEL_REDUCED` does not shrink the classpath here. `shouldFallBack` returns `BlazeJavacResult.fallback()` (`REQUIRES_FALLBACK`, exit 0, `requires_reduced_classpath_fallback` on the `.jdeps`). Zel’s spawn retry (`lib.rs`) reads that flag, deletes outputs, clears stdout and stderr, and re-runs with the full classpath. `BAZEL_FALLBACK` does not request another fallback.
  - `shouldFallBack` is only in this class: success never falls back; `CRASH` always does; otherwise an empty diagnostic list or a list that is entirely JSpecify (`compiler.err.proc.messager` and message prefix `[nullness] `) does not fall back. Zel never inspects diagnostics or crash status.
  - Classpath membership for the javabuilder shrink is `java.nio.file.Path` (`Paths.get` on the proto path, then `HashSet<Path>::contains`), limited to dependency kinds `EXPLICIT`, `IMPLICIT`, and `INCOMPLETE`, and a `.jdeps` without `rule_label` fails the read. Zel `reduced_classpath` keeps every dependency path and tests `HashSet<String>` equality against transitive exec-path strings, so duplicate slashes and a trailing slash stay distinct.
  - This class does not decode bytes. Zel `parse_jdeps` turns dependency paths into strings with `String::from_utf8_lossy`. `java_native.rs` also sets the action environment `LC_CTYPE` to `C.UTF-8` (`en_US.UTF-8` when the platform is macOS).
  - Classpath length, the fallback bit, `minClasspathLength`, and processor simple names are written only on this class’s `BlazeJavacStatistics`.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/SimpleJavaLibraryBuilder.java`
- Bazel lines: 234
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`java_create_compile_action`)
- Verdict: partial
- Differences:
  - Zel does not run this builder. It spawns `_javabuilder` (`java -jar`). `BazelJavaBuilder` picks this class only when classpath reduction is off. Zel's default `--experimental_java_classpath=bazel` selects `ReducedClasspathJavaLibraryBuilder` instead. This class runs only for Zel mode `OFF` (strict deps off, an incompatible processor, or no `.jdeps` output), and then only inside that jar.
  - Left in the jar: in-process `BlazeJavacMain.compile`; wipe and recreate the class, generated-source, native-header, and temp dirs; skip javac and return ok when there are no sources after source-jar extraction; on success, write the output jar (`JarCreator`, normalize, compression, class dir, target label and injecting-rule-kind owner) and the native-header jar; if any processors are set, write the gensrc jar even when compile failed; in `finally`, emit `.jdeps` (`requiresFallback` only for `REQUIRES_FALLBACK`) and the manifest proto. Stats set both classpath lengths to the full classpath size and fallback to false. Zel records none of that.
  - Source-jar paths are rewritten only here: `.java` entries are extracted under `<output sibling>/_javac/<name after ':'>/<jar>_tmp`, a leading `/` is stripped, and an existing destination is nested under `_tmp/<n>/` so case-only clashes survive a case-insensitive filesystem. `protobuf.meta` entries are concatenated into the class dir, or a stale file is deleted. Scratch dirs are `workDir.resolve(output)` siblings `_classes`, `_sources`, and `_tmp` (`native_headers` under `_tmp`). `workDir` is `""` (exec-root cwd) or the worker sandbox. Zel only passes exec paths with `/` via `exec_path.path_string()`.
  - Charset: this class copies entry bytes with `Files.copy` / `transferTo` and opens jars with `JarFile(File)` (JDK zip UTF-8). It names no charset. Zel's param files are UTF-8, one raw argument per line (`ParamFileFormat::Multiline`), which matches `OptionsParser` reading `@` files as `UTF_8`. Bazel writes those same bytes with `StringUnsafe.getInternalStringBytes` and prefers an `en_US.ISO-8859-1` locale so `sun.jnu.encoding` stays Latin-1. Zel stores Unicode paths and sets `LC_CTYPE` to `C.UTF-8` (`en_US.UTF-8` when a platform name contains `osx` or `macos`). Non-ASCII or non-UTF-8 exec paths therefore reach `workDir.resolve` as different `Path` strings, and the wrapper's diagnostic writer uses `Charset.defaultCharset()`.
  - External-repo `--target_label` values get an extra `@` (`@@@…` → `@@…` after `@@` unescaping), matching `JavaCompileActionBuilder`. Main-repo labels are `//pkg:name`. The scratch directory uses only the substring after the last `:`.
  - `buildJar` runs Jacoco when `--post_processor jacoco` is set. Zel never emits `--post_processor`, so that step does not run. `--compress_jar` is unconditional in Zel; Bazel adds it only when `compressJar` is set, which `JavaCompilationHelper.createCompileAction` does set.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/VanillaJavaBuilder.java`
- Bazel lines: 390
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (spawn built by `java_create_compile_action` / `java_register_compile`; run by `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` and `/workspace/zel-cursor/crates/zel-sandbox/src/lib.rs`)
- Verdict: partial
- Differences:
  - Zel does not implement this class. The non-worker path stays inside the JDK jar: `ToolProvider.getSystemJavaCompiler()`, temp class/source/header dirs, source-jar zip entries, annotation-processor loading, `JarCreator` outputs, the stub `Deps.Dependencies` (label + success only), the empty manifest proto, and the diagnostic filter (`compiler.note.deprecated`, `compiler.note.unchecked`, `compiler.warn.sun.proprietary`).
  - Charset inside the tool is fixed: `getStandardFileManager(..., ENGLISH, UTF_8)` and `Files.readString` (UTF-8) for source-jar text. Zel never passes `-Dfile.encoding`. It only sets the spawn env `LC_CTYPE` to `C.UTF-8`, or `en_US.UTF-8` when a configured platform contains `osx`/`macos` or the Zel binary is built for macOS. That locale is what makes `System.err.print` use a UTF-8 default charset; the file-manager charset does not follow it. Bazel chooses that locale from the execution platform only, so a macOS-built Zel can set `en_US.UTF-8` for a Linux execution platform.
  - Non-worker `main` folds every result into `System.exit(ok ? 0 : 1)` after printing compiler output on stderr (empty sources are success). Zel’s standalone spawn keeps the wait status: `0` is success, any other status is `NonZeroExit` with that code, a signal is `128+signal`, a failed exec is `127`, and a timeout is forced to `142`. A VanillaJavaBuilder failure therefore shows up as `(Exit 1)` only because the jar already collapsed it. After exit 0, Zel still fails the action if a declared output is missing, and it marks regular outputs executable.
  - On the non-reduced path (`--experimental_java_classpath=off`, strict deps `OFF`, or no `.jdeps`), Zel passes the full `--classpath` and does not retry. The default mode is still `BAZEL`, which rewrites the classpath before the spawn. Vanilla parses `--strict_java_deps`, `--direct_dependencies`, `--experimental_fix_deps_tool`, and `--reduce_classpath_mode` and then ignores them, and its deps proto never sets `requires_reduced_classpath_fallback`, so a reduced miss cannot fall back.
  - The vanilla toolchain still defaults `javac_supports_workers` to true, and Zel copies that into `supports-workers`. The comparison above is the standalone exec, which does not pass `--persistent_worker`.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/genclass/GenClass.java`
- Bazel lines: 172
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`java_gen_jar_action`, lines 1117–1163; `--generated_sources_output` in `java_create_compile_action`)
- Verdict: partial
- Differences:
  - Zel does not extract classes. `java_gen_jar_action` only spawns `java -jar` on the toolchain genclass jar with `--manifest_proto`, `--class_jar`, and `--output_jar` (same flags and `JavaSourceJar` mnemonic as `JavaCompilationHelper.createGenJarAction`). Prefix filtering, the temp directory, and the output jar are entirely inside that Java tool.
  - Manifest handling stays in Java. Bazel `Manifest.parseFrom`s the compilation-unit proto and builds prefixes from `pkg` (`.` replaced with `/`, plus a trailing `/`) plus each top-level name. Zel never reads `generated_by_annotation_processor`, packages, or top-level names; it only passes the proto path and errors if `manifest_proto` is missing.
  - Inclusion is not implemented in Rust. A `.class` entry is kept when its name with the `.class` suffix removed matches a generated prefix, or matches no hand-written prefix (unknown classes count as generated). Inner classes match any `$`-bounded prefix (`Foo$Inner`, `Foo$0`). Those checks are Java `String` operations (`endsWith`, `substring`, `indexOf('$')`) on `JarFile` entry names. Zel has no zip reader and no equivalent byte-or-UTF-16 scan.
  - Timestamps and compression are `JarCreator.setCompression(true)` plus `setNormalize(true)`, which deflates entries and sets `JarHelper` times (2010-01-01 in the JVM default zone; `.class` entries two seconds later). Zel writes no zip, so it does not reproduce DOS-epoch normalization, entry sorting, or raw `Files.copy` of entry bytes.
  - Encoding is not applied on the genclass spawn. `JarFile` decodes names as UTF-8 when the zip language flag is set, otherwise the JVM default charset, then GenClass treats them as UTF-16 strings. Zel’s `utf8_env` (`LC_CTYPE=C.UTF-8`, or `en_US.UTF-8` on macOS) is only attached to the javac action. The genclass action uses the default shell environment and sets no charset.
  - Generated sources are a different output. Zel forwards `--generated_sources_output` to JavaBuilder (the processor source jar). Resource jars go through singlejar with `--normalize`. Neither path filters annotation-processor class files the way GenClass does.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/genclass/GenClassOptions.java`
- Bazel lines: 78
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`java_gen_jar_action`)
- Verdict: partial
- Differences:
  - Zel has no options type or builder. The three fields are inlined as spawn arguments: `--manifest_proto`, `--class_jar`, `--output_jar`, in that order.
  - Inputs are the manifest proto and the class jar. The output is the genclass jar. Those three flags match.
  - This file does not parse paths or labels. It stores `java.nio.file.Path`. String-to-path conversion is in `GenClassOptionsParser` (`Paths.get` on the next argument). Zel passes `Artifact.exec_path().path_string()` as a Rust `String` (an exec path, not a label).
  - `build()` rejects a null manifest, class jar, or output jar. Zel errors only when the manifest is missing. The class jar and output jar are required by the function signature.
  - Zel also prefixes `jvm_opt`, `-jar`, and the genclass deploy jar, and adds the Java runtime files as inputs. Those belong to the spawn wrapper, not to this options class.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/genclass/GenClassOptionsParser.java`
- Bazel lines: 53
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`java_gen_jar_action`)
- Verdict: partial
- Differences:
  - Zel does not parse GenClass argv. It builds a `JavaSourceJar` spawn whose post-`-jar` args are `--manifest_proto`, `--class_jar`, and `--output_jar`, each followed by an exec-path string (`jpath`). `param_file` is `None`, so those six strings are not spilled to a param file. The Java parser is what still accepts or rejects them if the real GenClass jar runs.
  - Parser flags are an exact `switch`: only those three names. No `--flag=value`, no abbreviations. The next token is always `Paths.get`, even if it looks like a flag. Repeating a flag overwrites (last wins). Zel emits each flag once, in that order.
  - Unknown token: `IllegalArgumentException` via `String.format("Unexpected argument: '%s' in %s", arg, args)`. The second `%s` is the `Iterable`'s Java `toString` (`[a, b, c]` with comma-space for `Arrays.asList`). A missing value is `String.format("Expected more arguments")` and does not name the flag. Omitted required paths are not checked here; `GenClassOptions.build` `checkNotNull` throws a bare `NullPointerException`. Zel never formats those strings. It fails earlier in analysis: `"gen_class requires a manifest_proto"`, `"java_toolchain has no genclass jar"`, or a missing `java_runtime` / `java_executable_exec_path`.
  - This parser does not expand `@` files and does not read text, so it has no charset and no line splitting. An `@…` argument is an unexpected argument. `@@` is not an escape. A path is one Java `String` (`UTF-16`, including a possible unpaired surrogate) passed to `Paths.get`; embedded `\n` or `\r` stay inside that one argument. Zel paths are Rust `String`s (`UTF-8` only).
  - Param-file expansion elsewhere is not this parser and is not applied to the executed genclass argv. `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` `expand_param_file_args` rewrites only announce/failure text when `--expand_param_files` is set. Compared with buildjar `OptionsParser.expandArgument` (UTF-8 `Files.readAllLines`, which GenClass does not call): Zel `strip_prefix('@')` treats `@@file` as path `@file`, while Java `@@` becomes the literal `@file`; Zel uses `from_utf8_lossy` (`U+FFFD`) and Java UTF-8 throws; `str::lines()` splits on `\n` and `\r\n` only (a lone `\r` stays in the line) while Java `readLine` also breaks on a lone `\r`; Zel `trim`s and drops blank lines, Java drops only `length == 0` and does not trim; Zel is not recursive and leaves an unreadable `@path` unchanged, Java re-expands each line and throws `IOException`.
  - `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs` `expand_argument` is also unused for this spawn (no worker requirements). It keeps a leading `@@` verbatim (Java drops one `@`), errors unless the file is a declared input, uses strict UTF-8 `read_to_string`, and `lines()` with no trim, so empty lines become empty arguments. `/workspace/zel-cursor/crates/zel-analysis/src/action.rs` `render_param_file` always ends each argument with `\n` (LF only), and genclass does not use it.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/instrumentation/JacocoInstrumentationProcessor.java`
- Bazel lines: 130
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Arguments: Bazel `create` requires at least one argument, keeps only `args.get(0)` as the coverage-info path, and ignores extras. The compile action passes `--post_processor jacoco <exec path of <jar>-paths-for-coverage.txt>`. Fewer than one argument throws `InvalidCommandLineException` with the unclosed message `Number of arguments for Jacoco instrumentation should be 1+ (given N: pathsForCoverageFile`. Zel’s Java compile command in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` stops at `--experimental_fix_deps_tool` and never emits `--post_processor` or that artifact. An unknown processor name is an `AssertionError` in `JavaLibraryBuildRequest`, not in this class.
  - Which classes: every recursive `*.class` under the class directory is instrumented. There is no `coverage_instrumentation_filter` and no `*Test` skip. Non-class files are not touched here. Zel coverage is LCOV collection at test time and does not instrument class files.
  - Jar rewrite: each class is copied to `<metadata-dir>/<relative-path>.uninstrumented`, then offline-instrumented back over the original with `TRUNCATE_EXISTING`. The jar then gets that metadata directory plus `jar.addEntry(coveragePath, coveragePath)`. The metadata directory is `outputJar.resolveSibling(outputJar + "-coverage-metadata")`, so a relative jar path is nested under the jar’s parent. `cleanup` deletes it with `ALLOW_INSECURE`. Zel has none of this rewrite.
  - Charset and paths: this processor does not transcode. The JaCoCo name is `file.toString()` (platform path, not the jar-relative class name). `JarCreator` then strips a leading `/`, a Windows `X:` prefix, or `./` from the coverage entry name. The embedded paths file is written elsewhere as Latin-1 internal bytes (`StringUnsafe.getInternalStringBytes`): exec paths of source artifacts, one per line. Zel only sets `LC_CTYPE` to `C.UTF-8` (`en_US.UTF-8` on macOS) on the Java action and never writes that Latin-1 paths file or jar entry.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/jarhelper/JarCreator.java`
- Bazel lines: 256
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No production jar or zip writer. `crates/zel-bzlmod/src/archive.rs` only reads and extracts zips. The only writer is the test helper `build_zip` in that file (around lines 1623–1660). Java rules spawn `singlejar`; they do not implement this class.
  - Compression: `JarCreator.execute` uses `JarOutputStream` and `JarHelper`. The default method is DEFLATED. `setCompression` switches DEFLATED and STORED. Size-0 entries, including directories, are always STORED with CRC 0. A manifest under 256 bytes is forced STORED. STORED entries get a CRC32 before the data is written. `build_zip` uses method 0 or method 8 per entry. Its method 8 body is stored-block deflate, not `java.util.zip` Deflater output, and empty entries are not forced to STORED.
  - Timestamps: with `setNormalize(true)`, non-`.class` entries use 2010-01-01 00:00:00 in the JVM default zone (`JarHelper.DEFAULT_TIMESTAMP`). `.class` entries add 2000 ms because ZIP time has 2-second resolution. Otherwise `copyEntry` uses the file mtime. `build_zip` writes DOS time and date as zero (1980-01-01 00:00:00) and has no `.class` offset.
  - Entry order: entries sit in a `TreeMap`, so the payload order is Java `String` order (UTF-16 code units), independent of insertion. `META-INF/` and `META-INF/MANIFEST.MF` are written first and then skipped if they also appear in the map. Directory walks add every directory except the root, with `/` separators and a trailing `/`. Leading `/`, `./`, and a Windows `X:\` or `X:/` prefix are stripped. `build_zip` keeps caller order, does not sort, and does not rewrite names.
  - Manifest: a file via `setManifestFile`, or a generated one. It always sets `Manifest-Version: 1.0`, `Created-By: bazel` when absent, and optionally `Main-Class`, `Target-Label`, `Injecting-Rule-Kind`, and `Multi-Release: true`, through `java.util.jar.Manifest.write`. Zel has none of that.
  - Executable bits: `JarEntry` does not store a Unix mode or external attributes, so executable bits are dropped. `build_zip` copies a caller-supplied `u32` into the central-directory external attribute. Tests pass `mode << 16`.
  - Entry-name charset: names are Java strings handed to `JarOutputStream` with no charset argument, so the JDK Zip coder applies (UTF-8 and general-purpose bit 11, the UTF-8 flag, on JDK 11+). `build_zip` writes `str::as_bytes()` (UTF-8) but leaves that flag at 0, so readers may treat the names as CP437.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/jarhelper/JarHelper.java`
- Bazel lines: 251
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `JarHelper`. `crates/zel-bzlmod/src/archive.rs` only reads zip/jar. The nearest writers are vendored and do not implement this contract.
  - Fixed epoch is 2010-01-01 00:00:00, not the 1980 DOS epoch. `DEFAULT_TIMESTAMP` is `LocalDateTime.of(2010, 1, 1, 0, 0, 0).atZone(ZoneId.systemDefault()).toInstant().toEpochMilli()`. The millis value follows the JVM default zone. `ZipEntry.setTime` converts those millis back in that same zone, so the stored DOS clock time is local midnight. `setNormalize`'s comment says "DOS epoch"; the value is 2010-01-01.
  - DOS time is not packed here. `JarEntry.setTime` stores seconds as `second >> 1` (truncate to a 2-second boundary). `MINIMUM_TIMESTAMP_INCREMENT` is 2000 ms. `.class` entries get `DEFAULT_TIMESTAMP + 2000` (local 00:00:02, the next DOS tick). Other entries stay on local 00:00:00, which is already aligned, so truncation does not move them. A post-1980 time avoids the pre-1980 extended-timestamp extra field that `setTime(0)` can emit.
  - With normalization off, `writeEntry` uses `System.currentTimeMillis()` and `copyEntry` uses the file mtime.
  - Default method is `DEFLATED`. `setCompression(false)` selects `STORED` and then requires a CRC32 before `putNextEntry`. Size 0 (including directories) is always `STORED` with CRC 0 and no body. A manifest shorter than 256 bytes, and its `META-INF/` entry, are forced `STORED`, then the previous method is restored.
  - This class does not sort. Entries are written in call order and deduped with a `HashSet`. Alphabetical order is only claimed in the class comment; `JarCreator` sorts with a `TreeMap`.
  - Names are raw Java strings passed to `JarEntry`. There is no charset flag, NFC normalization, or slash rewriting. `copyEntry` appends `/` to directories before the duplicate check. Encoding and the UTF-8 general-purpose bit are left to `JarOutputStream`.
  - Vendored `crates/zel-bazel-tools/resources/third_party/ijar/zip.cc` writes DOS `30<<25 | 1<<21 | 1<<16` (2010-01-01 00:00:00) for every entry, including `.class`, with no timezone and no +2s. General-purpose flags are 0. Deflate is used only when requested and smaller than stored. `third_party/.../StableZipEntry.java` copies the 2010-01-01 system-zone timestamp and the `.class` +2000 ms adjustment, and does not write archives. Protobuf `zip_writer.cc` uses DOS date 1980-01-01, time 0, method stored, and flag 0. `wheelmaker.py` uses `(1980, 1, 1, 0, 0, 0)`.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/BlazeJavaCompiler.java`
- Bazel lines: 134
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not subclass `com.sun.tools.javac.main.JavaCompiler`. The compile action in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`java_tool_command`, `java_create_compile_action`) is a `java … -jar` spawn of the toolchain JavaBuilder. `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs` only parses the `.jdeps` proto and retries a reduced classpath. Nothing in the Zel tree registers a javac `Context` factory.
  - Not reimplemented: `preRegister` installs a `compilerKey` factory that constructs `BlazeJavaCompiler` once and throws `AssertionError` on a second creation (annotation-processing rounds after JDK-8038455). The constructor calls `plugin.init(context, log, this, statisticsBuilder)` for every `BlazeJavaCompilerPlugin`.
  - Not reimplemented: after `attribute`, each plugin's `postAttribute` runs unless `errorCount() > 0` and `runOnAttributionErrors()` is false. After `flow`, plugins are skipped when `CompileState.FLOW` was already done; otherwise `postFlow` runs under the same error gate via `runOnFlowErrors()`. `close()` calls `plugin.finish()`, clears the plugin list, then `super.close()`. `pluginsContain` is test-only and has no Zel equivalent.
  - Dependency collection and diagnostic formatting are not in this class. They are plugin callbacks on javac's attributed `Env` and `Log` (`errorCount()` only gates those callbacks). Zel forwards `--processors`, `--processorpath`, `--strict_java_deps`, `--direct_dependencies`, and `--output_deps_proto` to the jar and reads `dependency[].path` plus `requires_reduced_classpath_fallback` afterward. It does not walk the AST.
  - This file has no charset, encoding, or `String`/`byte[]` conversion. Zel's only related behavior is the spawn environment from `utf8_env`: `LC_CTYPE=C.UTF-8`, or `en_US.UTF-8` when the platform string contains `osx`/`macos` or the host is macOS.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/BlazeJavacArguments.java`
- Bazel lines: 141
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`java_create_compile_action`) and `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs` (`classpath_param_file`)
- Verdict: partial
- Differences:
  - This file is a structured argument holder (`Path` lists and `ImmutableList<String>` options). It does not emit `-classpath`, `-bootclasspath`, `-processorpath`, `-encoding`, `-source`, or `-target`. Zel builds the JavaBuilder command line that later fills this object.
  - Classpath: Bazel stores `classPath()` (default empty) and later installs it as both `CLASS_PATH` and `MODULE_PATH` in `BlazeJavacMain`. Zel leaves the classpath off the analysis argv and writes `--classpath` plus `--reduce_classpath_mode` in the executor param file. Bootclasspath auxiliary jars are prepended to Zel's full classpath and direct jars.
  - Bootclasspath: both carry a bootclasspath path list and an optional system image (`system()` vs `--system`). An empty bootclasspath stays empty here (legal with `--release`). Zel omits `--bootclasspath` when the list is empty.
  - Processors: this class stores `processorPath()` and compiler `plugins()`, not processor class names. `-processor` / `-proc:none` are added in `JavaLibraryBuildRequest.addJavacArguments`, not here. Zel passes class names as `--processors` and jars as `--processorpath`.
  - Encoding: this file builds no argument strings and does no charset conversion. Options stay Java `String` (UTF-16); the only path literal is `Path.of("")` for `workDir`. Zel argv is Rust `String` (UTF-8) from `to_string()` / `path_string()`, and the action env sets `LC_CTYPE` to `C.UTF-8` (`en_US.UTF-8` on macOS) in `utf8_env`.
  - Source/target: not fields here. They ride inside `javacOptions()` if the caller supplied them; `-source`/`-target` vs `--release` is normalized in `JavacOptions`, not this class. Zel forwards tokenized `javac_opts` under `--javacopts` … `--` and does not apply that release normalizer.
  - Bazel-only fields with no Zel struct: `blazeJavacOptions`, `plugins`, `failFast`, `inputsAndDigest` (`ByteString` digests keyed by `String`), `requestId`, `workDir`. Outputs are `classOutput` / `nativeHeaderOutput` / `sourceOutput` paths; Zel uses `--output`, `--native_header_output`, and `--generated_sources_output`.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/BlazeJavacMain.java`
- Bazel lines: 385
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`java_create_compile_action`, mnemonic `Javac`) and `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (spawn plus reduced-classpath retry)
- Verdict: partial
- Differences:
  - Zel never hosts javac. It spawns the toolchain `_javabuilder` jar (`java -jar`). `JavacTool.getTask`, `JavacTaskImpl.doCall`, `Context`, `CacheFSInfo`, statistics, and `BlazeJavaCompiler.preRegister` stay inside that JDK tool.
  - Plugins: Bazel runs `BlazeJavaCompilerPlugin.processArgs` and `initializeContext` before the task, mapping `CancelRequestException` to cancelled and `InvalidCommandLineException` to error. Zel has no compiler-plugin API. A `java_plugin` only becomes `--processors`, `--processorpath`, and processor data inputs.
  - `ClassloaderMaskingFileManager` forces Error Prone, Guava, Checker dataflow, and buildjar statistics classes onto the JavaBuilder loader. Zel has no equivalent.
  - Processors run inside javac. Zel only forwards jars and class names. The `UnsupportedClassVersionError` rewrite (processor class-file version, `--tool_java_language_version`) and the `compiler.warn.proc.*` filters stay in this driver.
  - Diagnostics: Bazel uses `FormattedDiagnostic.Listener` (`failFast`, `-Werror:`), drops a fixed note/warning code set, sorts errors last, appends a `[BazelJavaConfiguration]` message for `compiler.err.cant.access` on an incompatible `--system` path, and turns `OK` into `ERROR` when warnings meet `-Werror`. Zel prints the process stdout and stderr and does not parse javac diagnostics. The only extra retry is `.jdeps` `requires_reduced_classpath_fallback` (`/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs`).
  - Exit status: Bazel maps `Result` to `OK`, `ERROR` (`ERROR`, `CMDERR`, `SYSERR`), `CRASH` (`ABNORMAL` or any other exception), or cancelled. Zel treats the JavaBuilder process as a generic spawn: success versus nonzero exit or execution failure, and reports that process exit code.
  - Encoding: Bazel constructs `JavacFileManager` with `UTF_8`. Zel only sets the spawn environment `LC_CTYPE` to `C.UTF-8` (`en_US.UTF-8` on macOS) in `utf8_env`. The source charset stays inside the file manager.
  - Locations Bazel sets in-process and Zel does not: classpath also as `MODULE_PATH`, inferred source path from a single `module-info.java`, `PLATFORM_CLASS_PATH` only when the bootclasspath is non-empty, `-Xlint:path`, and `expandJarClassPaths=false`. Zel passes the outer JavaBuilder flags instead (`--sources`, `--source_jars`, `--classpath`, `--bootclasspath`, `--system`, `--sourcepath`, `--javacopts`, `--native_header_output`, `--generated_sources_output`).

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/BlazeJavacResult.java`
- Bazel lines: 97
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs` and `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (spawn loop)
- Verdict: partial
- Differences:
  - Zel has no `BlazeJavacResult` or status enum (`OK`, `ERROR`, `CRASH`, `REQUIRES_FALLBACK`, `CANCELLED`). It only sees the JavaBuilder process exit code and the `.jdeps` proto.
  - Bazel maps `OK` and `REQUIRES_FALLBACK` to exit 0, and `ERROR`, `CRASH`, and `CANCELLED` all to exit 1 (`isOk()` is only `OK`). Zel treats any nonzero exit as failure and never separates crash, error, or cancel.
  - `REQUIRES_FALLBACK` returns 0 before diagnostics or `output()` are written, and `success` in `.jdeps` is false while `requires_reduced_classpath_fallback` is true. Zel retries only after exit 0 when field 5 is nonzero, then deletes outputs and clears stdout/stderr. It never reads the `success` field.
  - `output()` is a Java `String`, written after each diagnostic plus a newline. The non-worker path encodes that writer with `Charset.defaultCharset()`. Zel keeps stderr as raw bytes and later decodes logs with `String::from_utf8_lossy` (invalid bytes become U+FFFD). `.jdeps` dependency paths use the same lossy UTF-8 conversion.
  - Zel’s own failure text `ERROR: … failed: .jdeps read IOException: …` is not a JavaBuilder or `BlazeJavacResult` message. A missing or unparseable `.jdeps` after exit 0 fails the action in Zel with exit 1.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/CancelCompilerPlugin.java`
- Bazel lines: 86
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Registers on javac’s `MultiTaskListener` and, for one `requestId`, polls `WorkerCancellationRegistry` at `processArgs`, `postAttribute`, `postFlow`, and every `TaskListener` `started`/`finished`. If that id is cancelled it throws `CancelRequestException`, which `BlazeJavacMain` turns into a cancelled compile result while the compiler process stays alive.
  - Zel has no in-process javac compiler plugin. Java compile is an external spawn.
  - Zel’s cancel and timeout paths kill a process group: `process_wrapper` sends `SIGTERM` then `SIGKILL` on timeout, and global cancel / `CancelFlag` send `SIGKILL`. `Worker::destroy` `SIGKILL`s the whole worker. None of those poll compiler phases or raise a cancel exception from inside javac.
  - `Worker::send_cancel` can write a worker-protocol `WorkRequest` with `cancel: true` when the key is cancellable, but nothing calls it. `WorkerSpawnRunner::exec` always sends `cancel: false` and blocks until the response.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/FormattedDiagnostic.java`
- Bazel lines: 227
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (spawn failure text) and `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` (raw stderr). No port of this class. `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs` only retries on `.jdeps` `requires_reduced_classpath_fallback`.
- Verdict: partial
- Differences:
  - Zel does not listen to javac diagnostics. A failed `JavaCompile` spawn is `NonZeroExit` / `ExecutionFailed`: one `ERROR: {loc}: {progress} failed: (Exit {code}): …` line, then the child stdout/stderr bytes unchanged. No code filter, no kind sort, no `-Werror:` promotion, no fail-fast listener, no work-dir strip.
  - This class keeps the message as a Java `String` from `DiagnosticFormatter.format` in `JavacMessages.getCurrentLocale()`. It never encodes. `BazelJavaBuilder` later writes that string with `Charset.defaultCharset()`. Zel copies stderr with `extend_from_slice` and does not decode it. The only lossy decode on this path is `String::from_utf8_lossy` for `.jdeps` path bytes in `java_compile.rs`.
  - `-Werror:` rewrite is `formatted.replaceFirst(formatKind, localized "compiler.err.error")`. `replaceFirst` is a regex replace of the first match anywhere, and `$` / `\` in the replacement are special. It is not a prefix slice. `getKind()` then returns `ERROR`. Only `WARNING` and `MANDATORY_WARNING` with a lint category are promoted. The status line `error: warnings found and -Werror specified` is in `BlazeJavacMain`, not here.
  - Fail-fast throws `FailFastException(formatted)` only when the original `diagnostic.getKind()` is `ERROR`, after the rewrite. A warning promoted by `-Werror:` does not abort here. Zel always waits for process exit.
  - Work-dir stripping is `Pattern.compile("^" + Pattern.quote(workDir + File.separator))` then `replaceAll("")`. `^` is start-of-string only (`MULTILINE` is off), so only a message that begins with that prefix is stripped. An empty work dir disables it. Zel does not strip paths from compiler text.
  - Code handling in this file is two exact `String.equals` checks: `compiler.warn.sun.proprietary` maps to lint option `sunapi`, and JSpecify is `compiler.err.proc.messager` plus `getMessage(ENGLISH).startsWith("[nullness] ")`. That `startsWith` is a UTF-16 code-unit prefix on the unformatted English message, not on `formatted` and not on bytes. `toString()` is the formatted string; `getMessage(locale)` is the raw one. The ignored-code set and the errors-last sort (`comparing(getKind).reversed()`) are in `BlazeJavacMain`. This listener stores diagnostics in report order. Zel does neither.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/JavacOptions.java`
- Bazel lines: 424
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`java_tokenize_opts`, `--javacopts` in `java_create_header_action` and `java_create_compile_action`)
- Verdict: partial
- Differences:
  - Bazel rewrites the javac option list in `normalize`: unrecognized flags stay in place, then `XlintOptionNormalizer`, `WErrorOptionNormalizer`, and `ReleaseOptionNormalizer` append collapsed flags. Zel shell-tokenizes the reversed `javac_opts` depset and appends those tokens under `--javacopts` … `--` with no rewrite.
  - `ReleaseOptionNormalizer` treats `-source`, `-target`, and `--release` / `--release=` as last-writer-wins (`--release` clears source and target; a later `-source` or `-target` clears release). It re-emits `--release` as two tokens. `--add-exports`, `--add-opens`, `--add-modules` (two-token or `=`), and `--enable-preview` are held and emitted only when `--release` or `-target` parses as an integer above 8 after an optional `1.` prefix (`Ints.tryParse`); a source-only level, version ≤ 8, or a non-numeric level drops them. A flag with no following argument is consumed and dropped. Zel forwards every token in order.
  - `-Werror` becomes a single rebuilt `-Werror:` via `WerrorCustomOption` (`all` / `none` / `+` / `-`, later entries override, default categories cannot be turned off). `-Werror:` and anything starting with `-Xep` are Bazel-specific (`isBazelSpecificFlag` / `removeBazelSpecificFlags`); bare `-Werror` is not. `BazelJavaBuilder` runs all three normalizers with an empty enforced-warning list; `VanillaJavaBuilder` runs only the release normalizer, then strips Bazel-specific flags. Zel does not split or rebuild these flags.
  - `-nowarn`, `-Xlint`, and `-Xlint:` are collapsed with a basis (`Empty` / `Recommended` / `All` / `None`). Categories are split on `,` with limit `-1` (trailing empty fields kept). `all` or an empty field resets to all; `none` resets to none; `-name` disables unless the category is enforced. Output is one basis flag plus `-Xlint:a,b` and `-Xlint:-a,-b`. Zel leaves each `-Xlint` / `-nowarn` token as written.
  - This file never reads `-encoding` or any charset. Untouched options, including `-encoding` and its value, pass through in their original place. Parsing uses Java `String` methods (`startsWith`, `substring`, `split`) on already-decoded UTF-16 strings, with no `Charset` conversion. Zel stores the same flags as UTF-8 `String`s and splits them with `shell_tokenize` over Unicode scalar values (`chars()`, Unicode `is_whitespace()`). A failed tokenize keeps the raw string. There is no `substring` / version parse, so no Java `1.` / `Ints.tryParse` path, and a non-UTF-8 option byte sequence cannot be represented in the Rust string.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/JavacRunner.java`
- Bazel lines: 23
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (spawn built in `java_create_compile_action` / `java_register_compile`; run in `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` and `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs`)
- Verdict: partial
- Differences:
  - This file is only the hook `BlazeJavacResult invokeJavac(BlazeJavacArguments)`. It does not start a process, set a classpath, set an encoding, or exit. The production binding is `BlazeJavacMain::compile`.
  - Process: that binding calls `com.sun.tools.javac.api.JavacTool` and `JavacTaskImpl.doCall()` in-process. Zel spawns an external `java -jar` JavaBuilder (`java_tool_command` → `ActionKind::Spawn`).
  - Classpath: inside the JDK tool, `setLocationFromPaths` sets `CLASS_PATH` and `MODULE_PATH` (and class, source, native-header, system, platform, processor, and source output) from `BlazeJavacArguments`. Zel passes `--classpath` on a param file and, for `BAZEL` / `BAZEL_NO_FALLBACK`, rewrites it per attempt.
  - Encoding: the JDK file manager is constructed with `StandardCharsets.UTF_8`. Zel does not pass `-encoding`; it sets the spawn env `LC_CTYPE` to `C.UTF-8` (or `en_US.UTF-8` on macOS) in `utf8_env`.
  - Exit: the runner returns a `BlazeJavacResult` (`OK` / `ERROR` / `CRASH` / `CANCELLED`, mapped from `com.sun.tools.javac.main.Main.Result`). `System.exit(result.isOk() ? 0 : 1)` is in `BazelJavaBuilder`, outside this file. Zel fails the action on the spawn’s nonzero exit code (`SpawnStatus::NonZeroExit`).
  - Stays inside the JDK tool, and is absent from Zel: the `JavacTool` task, file-manager locations, diagnostic listener, and status mapping. Zel never calls `com.sun.tools.javac`.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/WerrorCustomOption.java`
- Bazel lines: 127
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - Zel does not parse `-Werror:` or bare `-Werror`. `java_tokenize_opts` shell-tokenizes `javac_opts` and appends those tokens after `--javacopts`, ended by `--` (header compile and full compile). It never builds the category map (`all`, `+all`, `-all`, `none`, `+name`, `-name`, bare name).
  - This file matches with Java `String` equality and `startsWith`/`substring`, not `Pattern`. `isEnabled` is `containsKey("all")` then `getOrDefault(lintCategory, all)`. `Splitter.on(',')` is a literal comma split and keeps empty tokens. Indexes are UTF-16 code units. `-`, `+`, and `,` are in the BMP, so `substring(1)` does not split a supplementary-plane character. Category text is a literal map key. Regex metacharacters are not compiled.
  - Zel’s `shell_tokenize` walks Unicode scalars (`str::chars()`), not UTF-16 code units, and only splits on whitespace, quotes, and backslashes. A value such as `-Werror:cast,-deprecation` stays one argument. `+` and `-` are not operators there.
  - Bazel folds every `-Werror:` into one `LinkedHashMap`: later entries override, `-` cannot clear `warningsAsErrorsDefault`, `-all`/`none` clears and then restores those defaults, and `toString` emits one flag, dropping the trailing comma with `deleteCharAt`. Zel forwards each token as given and does not normalize.
  - Turning `WARNING` and `MANDATORY_WARNING` into errors (and the `compiler.warn.sun.proprietary` to `sunapi` alias) is outside this file, in `FormattedDiagnostic`, and is also absent from Zel. A real JavaBuilder jar can still apply this class to the forwarded argv. Zel itself never decides which warnings are errors.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/WorkerCancellationRegistry.java`
- Bazel lines: 36
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel keeps a `ConcurrentHashMap<Integer, Integer>` of request ids that should be cancelled. The id is both the key and the value. Zel has no in-process set of cancelled request ids.
  - `registerRequest` marks a compile cancelled by inserting that id. `checkIfRequestIsCancelled` is a concurrent `containsKey` poll. `unregisterRequest` uses `remove(key, value)`, so the entry is dropped only while it is still mapped to that same id. Zel never records or clears a per-request cancelled mark.
  - The registry does not kill a process or write a protocol message. `CancelCompilerPlugin` polls it at javac stages and throws `CancelRequestException`. Zel cancellation is elsewhere: `process_wrapper.rs` sets a process-wide `AtomicBool` (`GLOBAL_CANCEL` or `CancelFlag`) and `SIGKILL`s the process group, and `worker.rs` `send_cancel` can write a `{request_id, cancel: true}` `WorkRequest` only when the worker key is cancellable.
  - `WorkerSpawnRunner` in Zel never calls `send_cancel`. It waits until the worker returns. `zel-test-worker.rs` ignores `cancel` requests and always replies with `was_cancelled: false`, so an in-flight multiplex compile is not marked cancelled and is not stopped at compiler phase boundaries.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/plugins/BlazeJavaCompilerPlugin.java`
- Bazel lines: 111
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no port of this abstract javac AST-plugin base (`Context`, `Log`, `BlazeJavacStatistics.Builder`) and no `BlazeJavaCompiler` registration. A search of `/workspace/zel-cursor` finds no `BlazeJavaCompilerPlugin`, `postAttribute`, `postFlow`, `runOnAttributionErrors`, or `runOnFlowErrors`.
  - Not reimplemented: `processArgs` (standard vs Blaze javacopts, `InvalidCommandLineException`), `initializeContext`, `init(Context, Log, JavaCompiler, statisticsBuilder)`, `postAttribute(Env<AttrContext>)`, `postFlow`, and `finish`.
  - Not reimplemented: the error gates, both defaulting to false, so a plugin does not run after attribution or flow errors unless a subclass opts in.
  - The nearest Zel path is a different mechanism. `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` forwards `java_plugin` data to external JavaBuilder/Turbine as `--processors`, `--processorpath`, and `--builtin_processors`, and passes strict deps as `--strict_java_deps` instead of an in-process plugin such as `StrictJavaDepsPlugin`. `/workspace/zel-cursor/crates/zel-rules/src/native.rs` (`java_plugins_flag_alias`) only merges `JavaPluginInfo` (`processor_classes`, `processor_jars`, `processor_data`) for `--plugin`.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/plugins/dependency/DependencyModule.java`
- Bazel lines: 493
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`, `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs`
- Verdict: partial
- Differences:
  - Strict deps: Bazel `StrictJavaDeps` is `OFF` / `WARN` / `ERROR` (builder default `OFF`; `valueOf` is case-sensitive). Zel uppercases `strict_deps_mode`, defaults to `ERROR`, and passes `--strict_java_deps` only when not `OFF`, plus a hardcoded `--experimental_fix_deps_tool add_dep`. Exempt processors (Dagger `ComponentProcessor`, Hilt `RootProcessor`), `FixMessage` / buildozer text, and `hasMissingTargets` are absent. `OFF` drops direct jars and `.jdeps` artifacts in the analysis action before they reach this module.
  - Direct jars: Bazel stores `ImmutableSet<Path>` and unions those `Path`s into the reduced classpath. Zel stores exec-path strings and also prepends bootclasspath auxiliary jars onto `--direct_dependencies` and `JavaCompileSpec.direct_jars`.
  - `.jdeps` kinds: `collectDependenciesFromArtifact` keeps only `EXPLICIT`, `IMPLICIT`, and `INCOMPLETE` (`UNUSED` = 2 is skipped) and throws if `rule_label` is missing. `parse_jdeps` keeps every `dependency.path`, ignores `kind` (so `UNUSED` jars are included), and does not require `rule_label`. It does read `requires_reduced_classpath_fallback`.
  - Classpath reduction: Bazel `computeStrictClasspath` runs only in strict-classpath mode, seeds from direct jars, adds those three kinds, and filters the original classpath in order (paths not on that classpath are dropped). Zel `JAVABUILDER` passes `JAVABUILDER_REDUCED` and `--deps_artifacts` so JavaBuilder still does that. Zel `BAZEL` reimplements it in `reduced_classpath` without the kind filter. `BAZEL_NO_FALLBACK` writes reduce mode `NONE`. Missing `.jdeps`, strict `OFF`, or incompatible processors force mode `OFF`. Zel does not emit the `Dependencies` proto (label, success, sorted packages, explicit/implicit in classpath order, fallback bit).
  - Path equality: Bazel membership is `java.nio.file.Path.equals` (Unix paths collapse duplicate and trailing separators, then compare filesystem-encoded bytes). Zel uses exact `HashSet<String>` equality of exec-path strings against the decoded `.jdeps` text, so `a//b`, `a/b/`, and `.` segments diverge.
  - Charset: Bazel decodes protobuf path bytes as UTF-8 into a Java `String`, then `Paths.get` re-encodes with `sun.jnu.encoding` for equality. Zel uses `String::from_utf8_lossy` (`U+FFFD` for invalid UTF-8) and compares that string directly, with no filesystem-charset re-encode.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/plugins/dependency/ImplicitDependencyExtractor.java`
- Bazel lines: 126
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs`
- Verdict: partial
- Differences:
  - Zel has no port of this class. The symbol-table scan stays inside the prebuilt JavaBuilder (`@remote_java_tools//:JavaBuilder`, `java_tools/JavaBuilder_deploy.jar` in `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/jdk/BUILD.java_tools`). That jar is not in the Zel tree. Zel only reads the `.jdeps` proto the tool writes.
  - Bazel `accumulate` ignores the `roots` argument and walks `Symtab.getAllClasses()`. For each symbol it uses `classfile`, otherwise `sourcefile`. Zel does not walk the AST, the symbol table, or class files.
  - `getJarPath` reflects the `userJarPath` field on the `JavaFileObject` (`NoSuchFieldException` returns null; other reflection failures throw `LinkageError`). Zel has no `userJarPath` lookup.
  - `collectJarOf` drops paths in `platformJars` (comment: `rt.jar`), stores `path.toString()`, and sets `Deps.Dependency.Kind` to `IMPLICIT` when `ClassSymbol.isCompleted()`, otherwise `INCOMPLETE`, upgrading `INCOMPLETE` to `IMPLICIT`. Zel’s `parse_jdeps` keeps only `Dependencies.dependency` (field 1) nested `path` (field 1) and `requires_reduced_classpath_fallback` (field 5). It drops `kind` (`IMPLICIT` = 1, `INCOMPLETE` = 3), `rule_label`, `success`, and `contained_package`.
  - Zel matches those path strings exactly against `transitive_inputs` (`reduced_classpath`). It does not filter platform jars or preserve classpath order itself. The reduced-classpath modes it writes are `BAZEL_REDUCED`, `BAZEL_FALLBACK`, and `NONE` (`--reduce_classpath_mode`).

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/plugins/dependency/StrictJavaDepsPlugin.java`
- Bazel lines: 727
- Zel counterpart: `none`
- Verdict: partial
- Differences:
  - The checker is not in the Zel tree. It stays inside the JavaBuilder jar (`JavaLibraryBuildRequest` always registers `dependencyModule.getPlugin()`). Zel only builds the argv in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`.
  - Direct vs indirect is decided in the jar: a classpath jar not listed in `--direct_dependencies` is indirect (one diagnostic per `JarOwner`). JSpecify annotations loaded from the platform are treated as a missing direct dep on `//third_party/java/jspecify_annotations`. `@Generated` code from `dagger.internal.codegen.ComponentProcessor` and `dagger.hilt.processor.internal.root.RootProcessor` is exempt. Zel does not reimplement any of that.
  - Diagnostic text is only in the jar, so Zel has no alternate wording: `[strict] Using type <symbol> from an indirect dependency (TOOL_INFO: "<canonical label, jar path, or '<label> wrapped in <aspect>'"). See command below **`. `package-info` uses `package <enclosing>` instead of `type`. The `See command below **` suffix is omitted when the jar has no `Target-Label`.
  - Modes, also only in the jar: `ERROR` emits `log.error` via `Errors.ProcMessager`; `WARN` emits `log.warning` via `Warnings.ProcMessager`; `OFF` prints neither the diagnostic nor the fix footer. The plugin still records explicit jars for the `.jdeps` proto when `OFF`.
  - Fix footer (mode not `OFF`, and at least one missing owner has a label) is hardcoded `buildozer`, not the tool name: `** Please add the following dependencies:` then `** You can use the following buildozer command:` and `buildozer 'add deps <labels>' <target>` (magenta ANSI). `OptionsParser` stores `--experimental_fix_deps_tool` and never applies it.
  - Flag wiring matches `JavaCompileActionBuilder` for `OFF`/`ERROR`/`WARN`: both omit `--strict_java_deps` and `--direct_dependencies` when `OFF`, and otherwise pass the mode plus direct jars. Bootclasspath auxiliary jars are prepended to both the classpath and the direct jars. Header/Turbine actions on both sides pass `--direct_dependencies` only, so this plugin's `OFF`/`WARN`/`ERROR` switch is the full Javac action.
  - Bazel `JavaStarlarkCommon.getStrictDepsMode` maps `DEFAULT` to `ERROR` before `toString()`, so the jar sees `ERROR`. Zel uppercases the Starlark string and passes it through, so fragment value `default` becomes `--strict_java_deps DEFAULT`, which `StrictJavaDeps.valueOf` rejects. `java_library` goes through `compile_action.bzl` `_filter_strict_deps` (`default` and `strict` become `error`) and still sends `ERROR`. `java_common.compile` does not filter, so `DEFAULT` and `STRICT` reach the jar. Bazel rejects `STRICT` at analysis (`Only OFF and ERROR values are accepted.`); Zel would fail later inside `valueOf`.
  - `--experimental_fix_deps_tool` is always passed, including when strict deps are `OFF`. Bazel passes `JavaConfiguration.getFixDepsTool()` (flag default `add_dep`). Zel hardcodes `add_dep` and ignores a custom `experimental_fix_deps_tool`. That changes the argv only; the plugin text stays the buildozer paragraph.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/plugins/errorprone/ErrorPronePlugin.java`
- Bazel lines: 148
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`java_tokenize_opts`, `java_create_compile_action`, `java_create_header_action`)
- Verdict: partial
- Differences:
  - The plugin, checker set, and analysis stay inside the JavaBuilder jar. Zel never builds `ErrorPronePlugin`. `BazelJavaBuilder.parse` (same jar) installs `new ErrorPronePlugin(BazelScannerSuppliers.bazelChecks())`. The no-arg constructor’s `BuiltInCheckerSuppliers.errorChecks()` is not what Bazel runs.
  - `processArgs` drops `standardJavacopts` and passes only blaze javacopts (`-Xep…` and `-Werror:`, split by `JavacOptions` in the jar) to `ErrorProneOptions.processArgs`, then always appends `-XepIgnoreUnknownCheckNames`. Zel shell-tokenizes the whole `javac_opts` depset in reverse order and appends it as `--javacopts … --` on the full compile and the turbine header action. It does not split those flags and does not add `-XepIgnoreUnknownCheckNames`.
  - Severity is the jar’s frozen `ENABLED_ERRORS` list (those checks as errors) plus `-Xep:Name:SEVERITY` overrides parsed in the jar. Zel sets no default severity. `-Xep` tokens are forwarded only if the toolchain or target put them in `javacopts` (rules_java `DEFAULT_JAVACOPTS` includes several `-Xep:…:OFF` and `--should-stop=ifError=FLOW`).
  - This file sets no charset. `-encoding` is a standard javac flag, so it never enters the plugin. Stderr in `BazelJavaBuilder` uses `Charset.defaultCharset()`, which follows the JVM `file.encoding` (the Bazel client pins the server JVM to ISO-8859-1). Zel’s forwarding adds neither `-encoding` nor `-Dfile.encoding`.
  - Also only in the jar: the message bundle, `ErrorProneAnalyzer.createByScanningForPlugins`, `postFlow` after dataflow even when flow errors exist (`runOnFlowErrors` is true), a fatal `ErrorProneError` logged and rethrown so javac ends `ABNORMAL`, and `finish()` timings (total, init, top 10 bug patterns).

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/plugins/processing/AnnotationProcessingModule.java`
- Bazel lines: 111
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - This module never sets processor names, a processor path, javac source output, or `-proc:none`. It only stores `sourceGenDir` and `manifestProto`, turns on when both are non-null, and later writes a path-sorted compilation-unit manifest. Zel forwards the worker flags that feed that module: `--processors`, `--processorpath`, `--generated_sources_output`, and `--output_manifest_proto`.
  - Zel passes each processor class as its own argv string (`JCmd::all`) and each jar as its own path (`JCmd::exec_paths`). It does not comma-join names and does not emit `-processor` or `-proc:none`. Bazel does that join later in `JavaLibraryBuildRequest` (`Joiner.on(',').join`, comma with no space) and rejects a processor argument that already contains a comma in `OptionsParser`. No charset conversion is applied to those names in this module or in Zel; both keep them as plain strings (Rust `String` is UTF-8).
  - Zel’s only comma join of processor names is the progress line `short.join(", ")` (comma plus space, simple class names). This module has no equivalent string.
  - Zel does not strip the source root, mark generated units, or sort and emit the manifest proto. It only passes the output paths through when those artifacts exist.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/plugins/processing/AnnotationProcessingPlugin.java`
- Bazel lines: 69
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`java_create_compile_action`, `java_create_header_action`)
- Verdict: partial
- Differences:
  - The plugin only records each attributed `JCCompilationUnit` once: the exec-root path from `FileObject#getName()` (not the URI), the source-gen root stripped, `generatedByAnnotationProcessor` when the path is under that root, the package, and the top-level class simple names, stored as a `CompilationUnit` proto. Zel has no javac plugin and does not build that proto.
  - Writing the manifest (`AnnotationProcessingModule.emitManifestProto`), javac processing rounds, and processor diagnostics stay inside the JavaBuilder jar. `JavaLibraryBuildRequest` turns off the reduced-classpath fail-fast when processors are present, passes `-processor` or `-proc:none`, and rejects `-processor` and `-processorpath` in javacopts. `SimpleJavaLibraryBuilder.buildGensrcJar` packs `sourceGenDir` only when processors ran. Zel does none of that.
  - Zel’s full compile action only forwards `--processors`, `--processorpath`, `--generated_sources_output`, and `--output_manifest_proto`, and lists the generated-source jar and the manifest as action outputs.
  - The header action forwards `--processors`, `--processorpath`, and `--builtin_processors` to turbine or javac-turbine and does not pass a generated-source jar or a manifest. The plugin is registered only when both the source-gen directory and the manifest path are set.

### `src/java_tools/buildjar/java/com/google/devtools/build/buildjar/javac/statistics/BlazeJavacStatistics.java`
- Bazel lines: 166
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel keeps an in-process AutoValue of one `BlazeJavacMain.compile` call: optional `totalErrorProneTime`, `errorProneInitializationTime`, and `totalProcessorTime`; `bugpatternTiming` and `processorTiming` maps of `Duration`; the `processors` set; `transitiveClasspathLength`, `reducedClasspathLength`, `minClasspathLength`; `transitiveClasspathFallback`; and `auxiliaryData` (`AuxiliaryDataSource.DAGGER` to a cloned proto `byte[]`). A Caffeine weak-key cache registers one builder on the javac `Context`.
  - Those values stay inside the JavaBuilder jar. `ClassloaderMaskingFileManager` loads `com.google.devtools.build.buildjar.javac.statistics.*` from the JavaBuilder classloader so Error Prone and processors share it. `ErrorPronePlugin.finish` writes Error Prone durations and the 10 slowest bugpattern timings (durations, not counts). The two library builders then set classpath lengths and processor simple names. `BazelJavaBuilder.build` writes only diagnostics and compiler output and never reads `statistics()`. `addProcessorTiming`, `totalProcessorTime`, and `addAuxiliaryData` have no callers in this tree.
  - Zel has no copy of this class. `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs` only parses `.jdeps` `requires_reduced_classpath_fallback` and builds the reduced classpath. Spawn `total_time` and `execution_statistics.proto` rusage are process-level, not javac, Error Prone, bugpattern, or processor timings, and Zel does not record classpath lengths.

### `src/java_tools/buildjar/java/com/google/devtools/build/java/turbine/turbine_benchmark.sh`
- Bazel lines: 120
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel uses hyperfine (warmup 3, `RUNS=5`) to time a full non-incremental `//src/main/java/com/google/devtools/build/lib/bazel:BazelServer` build, then an incremental rebuild after `sed` adds a unique public method to `Label.java`. Flags: `--nosystem_rc --nohome_rc`, `--experimental_java_classpath=bazel`, `--disk_cache`, `--experimental_remote_cache_eviction_retries=2`, `--extra_toolchains=//_turbine_benchmark:turbine_benchmark_toolchain_definition`, `--profile`. It requires `-c opt`, copies the runfiles Turbine binary, and compares prebuilt java_tools Turbine (Java 21 toolchain) with HEAD Turbine via `buildozer 'add header_compiler_direct turbine'`. Profiles: `with_prebuilt_turbine.profile.gz` and `with_head_turbine.profile.gz`. The disk cache is kept; `_turbine_benchmark` and the edited file are cleaned up.
  - Zel has no turbine or header-compile benchmark (no hyperfine run, no prebuilt-vs-HEAD swap, no profile outputs). Zel only spawns turbine as the header compiler.

### `src/java_tools/buildjar/java/com/google/devtools/build/java/turbine/update_turbine_pgo.sh`
- Bazel lines: 100
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel rebuilds `//src/java_tools/buildjar/java/com/google/devtools/build/java/turbine:turbine_direct_graal` with `-c opt`, `--//src/java_tools/buildjar/java/com/google/devtools/build/java/turbine:pgo_instrument`, and `--//src/java_tools/buildjar/java/com/google/devtools/build/java/turbine:use_oracle_graalvm`, then collects a Graal profile on `//src/main/java/com/google/devtools/build/lib/skyframe:skyframe_cluster`.
  - Profile path is `_turbine_pgo/profile.<uuid>.iprof` via javacopt `-XX:ProfilesDumpFile`. The instrumented binary is copied to `_turbine_pgo/turbine_instrumented` and wired as `header_compiler_direct` on a Java 21 `default_java_toolchain` (`turbine_benchmark_toolchain`). Only the header jar is built (`build_header_jar` on `JavaInfo.compile_jars`).
  - Collection flags: `--experimental_java_classpath=bazel`, `--extra_toolchains=//_turbine_pgo:turbine_benchmark_toolchain_definition`, `--nocheck_visibility`, `--sandbox_writable_path` of the profile directory, `--min_param_file_size=100000`. Output is moved to `src/java_tools/buildjar/java/com/google/devtools/build/java/turbine/profile.iprof`. Cleanup removes `_turbine_pgo` and the temporary javacopt.
  - Zel has no `profile.iprof`, `pgo_instrument`, `use_oracle_graalvm`, or `turbine_direct_graal` packaging. It only spawns the toolchain header compiler (`_header_compiler` / `_header_compiler_direct`) from `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` (`java_tool_command`, `java_create_header_action`).

### `src/java_tools/buildjar/javatests/com/google/devtools/build/buildjar/VanillaJavaBuilderTest.java`
- Bazel lines: 305
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `hello`: a loose `.java` plus a `.srcjar` (`--sources`, `--source_jars`), `--javacopts` ended by `--`, and `--bootclasspath` `lib/rt.jar` produce an empty diagnostic string, `ok() == true`, and jar entries exactly `META-INF/`, `META-INF/MANIFEST.MF`, `A.class`, `B.class`.
  - `error`: `-Xlint:all` `-Werror` on a switch fall-through yields output containing `possible fall-through`, `ok() == false`, and no output jar.
  - `diagnosticWithoutSource`: `-Xlint:none` and no bootclasspath on `f(0L)` vs `f(int)` yields `note: Some messages have been simplified`, `ok() == false`, and no output jar.
  - `cleanOutputDirectories`: same success and exact jar-entry contract as `hello` after writing an unused `extra.class` (`CAFEBABE`) into a directory that is never passed to the builder.
  - `testDeferredDiagnostics`: two `--sources` using `@Deprecated`, `sun.misc.Unsafe`, and a raw `List` succeed with an empty diagnostic string (suppressed sunapi, deprecation, and unchecked notes).
  - `nativeHeaders`: `--native_header_output` succeeds with empty diagnostics and the header jar contains at least `test_FooWithNativeMethod.h` and `test_BarWithNativeMethod.h`. The class jar is not checked.
  - Sources and the srcjar member are written as UTF-8. There is no non-ASCII source and no assertion of a compiler `-encoding` flag.
  - Exit is `VanillaJavaBuilderResult.ok()` plus the diagnostic string, and on failure the output path does not exist. There is no process status code.
  - Zel’s Java compile tests (`/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs`, `/workspace/zel-cursor/crates/zel-exec/tests/exec_runtime.rs`) only cover reduced classpath, `.jdeps`, and param-file modes with a fake compiler.

### `src/java_tools/import_deps_checker/java/com/google/devtools/build/importdeps/AbstractClassEntryState.java`
- Bazel lines: 125
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no class-entry state for the import-deps checker: no EXISTING, INCOMPLETE, or MISSING variants, no `ClassInfo`, and no `direct` jar-membership flag.
  - No missing singleton (`classInfo` empty, `direct` true), no existing state (`ClassInfo` plus whether the class came from a direct jar), and no incomplete state (class present, ancestor missing, `ResolutionFailureChain` / `missingAncestors()`).
  - No `isMissingState` / `isExistingState` / `isIncompleteState` or the matching `as*` casts.
  - Zel’s only related jdeps code, `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs`, parses `.jdeps` dependency paths and `requires_reduced_classpath_fallback` for reduced-classpath compilation. It does not classify classes or record which jar a class belongs to. `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/jdk/BUILD.java_tools` only names a prebuilt `ImportDepsChecker_deploy.jar`.

### `src/java_tools/import_deps_checker/java/com/google/devtools/build/importdeps/ClassCache.java`
- Bazel lines: 457
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not implement ImportDepsChecker or this class cache. The nearest Java-deps code is `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs`, which only parses `.jdeps` protobuf paths and `requires_reduced_classpath_fallback` to shrink a compile classpath. `--experimental_import_deps_checking` is a no-op graveyard flag in `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`.
  - Bazel eagerly indexes `.class` entry names in boot, input, and regular jars (`computeIfAbsent`, first jar wins), then lazily parses bytecode on `getClassState`. Missing names are `MissingState`; a missing or incomplete superclass or interface becomes `IncompleteState` with a `ResolutionFailureChain`. Super lookups are indirect; a later explicit use upgrades `ExistingState` from indirect to direct. Order is boot, then input jars, then the regular classpath. Boot jars drop `org/jspecify/annotations`. `collectUsedJarsInRegularClasspath` records which regular jars were resolved and whether any use was direct. Zel has none of that.
  - ASM: `ClassInfoBuilder` is an ASM9 `ClassVisitor`. With `populateMembers` it accepts the class with `SKIP_CODE | SKIP_DEBUG | SKIP_FRAMES` and records field and method `name`/`desc`. Otherwise it only copies the class, super, and interface names from `ClassReader`. Zel does not read bytecode.
  - Paths and charset: jars are `java.nio.file.Path` values opened with `new ZipFile(jarPath.toFile())` (no `Charset`; Java’s `ZipFile(File)` decodes entry names as UTF-8). Internal names are the entry path with the final extension stripped. Zel never opens those jars; `.jdeps` dependency paths are protobuf bytes turned into strings with `String::from_utf8_lossy`.

### `src/java_tools/import_deps_checker/java/com/google/devtools/build/importdeps/ClassInfo.java`
- Bazel lines: 98
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not implement the import-deps checker class record. Nothing in `zel-cursor` reads class files for a superclass, interfaces, or members, and there is no ASM (`ClassVisitor` / `Opcodes.ASM9`) usage.
  - Bazel `ClassInfo` holds an ASM internal name, the containing jar as a `java.nio.file.Path`, a `directDep` flag, resolved `superClasses` (`ClassInfo` list: superclass plus interfaces, not raw strings), and `declaredMembers` (`MemberInfo`: non-empty member name plus ASM descriptor). `containsMember` walks that super list; ordering is internal name, jar path, then `compareFalseFirst(directDep)`.
  - The nearest Zel reader, `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs` `parse_jdeps`, only parses a `deps.proto` `Dependencies` message: `dependency[].path` jar path strings (`String::from_utf8_lossy`) and field 5 `requires_reduced_classpath_fallback`. No internal names, descriptors, members, inheritance, or direct-dep bit.

### `src/java_tools/import_deps_checker/java/com/google/devtools/build/importdeps/DepsCheckerClassVisitor.java`
- Bazel lines: 477
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no JVM bytecode scanner and no ASM `ClassVisitor`. Nothing under `/workspace/zel-cursor` walks a class file for superclass, interfaces, fields, methods, annotations, or constant-pool references. `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs` only parses `.jdeps` protos for reduced classpath. `experimental_import_deps_checking` is a flag name only.
  - Bazel is ASM API **ASM9** (`Opcodes.ASM9` on the class, annotation, field, and method visitors in `/workspace/bazel/src/java_tools/import_deps_checker/java/com/google/devtools/build/importdeps/DepsCheckerClassVisitor.java`).
  - Lookups stay in ASM **internal** names (`java/lang/Object`, slashes) via `checkInternalName` and `Type.getInternalName()`. Binary names (`java.lang.Object`) are never produced. A leading `[` is an array descriptor and is not treated as a class. `MethodHandle` and `VarHandle` owners are compared in internal form (`java/lang/invoke/MethodHandle`, `java/lang/invoke/VarHandle`).
  - References Bazel checks, which Zel does not: nullable `superName` (null for `java.lang.Object` and `module-info`); interfaces; exception internal names; descriptors on class, field, method, type, parameter, try-catch, local-variable, and nested annotations; field and method descriptors; `visitTypeInsn`, `visitFieldInsn`, `visitMethodInsn`, `visitMultiANewArrayInsn`, and local-variable descriptors; `LDC` and `invokedynamic` `Type` and `Handle` values. Member checks (`owner`, name, descriptor) are skipped for `MethodHandle.invoke` / `invokeExact` and a fixed `VarHandle` method list, and for `Ljava/lang/Synthetic;` parameter annotations. Generic signatures are ignored. String and boxed-primitive annotation and `LDC` values are accepted and are not recorded as class references.

### `src/java_tools/import_deps_checker/java/com/google/devtools/build/importdeps/ImportDepsChecker.java`
- Bazel lines: 317
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no port of this class. Missing classes, incomplete hierarchy, missing members, manifest labels, the text report, and the jdeps result proto are not implemented in the Zel crates.
  - Vendored `rules_java` only shells out: `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/common/rules/impl/import_deps_check.bzl` runs `java -jar` on `java_toolchain._deps_checker` with `--input`, `--directdep`, `--classpath_entry`, `--bootclasspath_entry`, `--checking_mode=error`, `--jdeps_output`, and `--rule_label`. If `deps_checker` is unset it returns `None` and skips the action. `default_java_toolchain.bzl` never sets `deps_checker`.
  - The jar is named, not shipped. `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/jdk/BUILD.java_tools` points at `java_tools/ImportDepsChecker_deploy.jar`, and that file is not in the tree.
  - `check()` walks each input jar with ASM (`SKIP_DEBUG | SKIP_FRAMES`), skips non-`.class` entries and `META-INF/versions/`, and returns whether the result collector is empty. A runtime failure is printed with `System.err.printf` (`"A runtime exception occurred when processing the class %s in the zip file %s\n"`) using the platform default charset, then rethrown. Zel does none of this.
  - The text report (`computeResultOutput`) is absent in Zel. Bazel emits `Missing <name>`, `Indirectly missing class <name>. Referenced by:`, `Missing member '<name>' in class <owner> : name=<name>, descriptor=<desc>`, a hierarchy under `"The class hierarchies of the classes with missing members:"`, then `===Total===` with `missing=`, `incomplete=`, and `missing_members=`. Internal names become dotted with `replace('/', '.')` only. Labels come from manifest `Target-Label` plus optional `Injecting-Rule-Kind` via `String.format("\"%s %s\"", ...)`. Display names keep only the segment after the last `/` and look like `pkg.Cls (in target)`. Indirect jars without a label are `Path.toString()`. The fix line is a literal ` add_dep <labels> <ruleLabel>\n`, or the "don't carry rule labels" paragraph when the rule label or labels are empty.
  - `emitJdepsProto` always sets `success=true` and one `Dependency` per used regular-classpath jar (`EXPLICIT` or `IMPLICIT`, `path` = `Path.toString()`, plus `rule_label`). Zel's `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs` only reads compile `.jdeps` for reduced classpath: field 1 path via `String::from_utf8_lossy` (invalid UTF-8 becomes U+FFFD) and field 5 `requires_reduced_classpath_fallback`. Kind, rule label, and success are ignored, and nothing writes this checker's proto.
  - Exit is not in this file. `Main.java` uses 199 for `checking_mode=ERROR` and 0 for `WARNING`/`SILENCE`, and writes the report as UTF-8. Zel has no 199 path. `--experimental_import_deps_checking` is a no-op graveyard flag. `--experimental_fix_deps_tool` is hardcoded to `add_dep` on the JavaBuilder compile line only.

### `src/java_tools/import_deps_checker/java/com/google/devtools/build/importdeps/Main.java`
- Bazel lines: 244
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/common/rules/impl/import_deps_check.bzl`
- Verdict: partial
- Differences:
  - Zel has no port of this CLI. The only argv is that vendored `import_deps_check`, called from `bazel_java_import_impl.bzl` for `java_import`.
  - It passes `--input`, `--directdep`, `--classpath_entry`, `--bootclasspath_entry`, `--checking_mode=error`, `--jdeps_output`, and `--rule_label` (`java -jar` on the toolchain checker).
  - It never passes `--output` or `--check_missing_members`. Member checks still run only because the tool default is true.
  - It never uses `WARNING` (the CLI default) or `SILENCE`. Incomplete deps always take the `ERROR` path, exit code 199. The warning path (exit 0) and the silence path (no stderr, jdeps only) are not run.
  - The UTF-8 missing-class report is not written. That write is `asCharSink(..., StandardCharsets.UTF_8)` to `--output`, and only when the check fails and the mode is not `SILENCE`. The empty-file create for a missing `--output` is also skipped. `--jdeps_output` is a binary `Deps.Dependencies` proto, not text.
  - Stderr still comes from the tool (`ERROR:` or `WARNING:`, the input jar list, then the detail string) with no explicit charset. Zel does not format or capture that itself.
  - Zel does not handle the parse-failure path: missing `--input` or `--bootclasspath_entry`, or a path that does not exist, prints the args4j error and usage to stderr and throws `IllegalArgumentException`.
  - The action is skipped when `deps_checker` is null, `jars` is empty, the label is on `_allowlist_java_import_deps_checking`, or tags contain `incomplete-deps`. Nothing in this tree runs it for `aar_import`.
  - Zel's own analysis does not spawn `ImportDepsChecker`. `--experimental_import_deps_checking` is a no-op graveyard flag, and the bootstrap `java_import` in `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/build_rules/java_rules_skylark.bzl` does not call the checker. `BUILD.java_tools` only exports the deploy jar.

### `src/java_tools/import_deps_checker/java/com/google/devtools/build/importdeps/ResolutionFailureChain.java`
- Bazel lines: 83
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not implement missing-ancestor chains or missing-class reporting. Vendored `import_deps_check.bzl` only launches an external ImportDepsChecker jar.
  - Bazel stores a leaf as one internal name, a null start class, and empty parents, and a parent as a start `ClassInfo` plus non-empty parent chains. Missing names are the sorted, distinct union of the parents.
  - `getMissingClassesWithSubclasses` walks to each leaf and maps that single missing name to the child `ClassInfo`. This file builds no message, so there is no string format or charset.

### `src/java_tools/import_deps_checker/java/com/google/devtools/build/importdeps/ResultCollector.java`
- Bazel lines: 143
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not implement this collector. The only import-deps surface is `third_party/bcr/rules_java/9.1.0/java/common/rules/impl/import_deps_check.bzl`, which runs the Java checker jar and does not collect or report missing or incomplete classes.
  - Bazel keeps missing internal names in a set and incomplete states in a map. An incomplete state also adds `missingAncestors()` into the missing-class set. Missing members are stored only when `checkMissingMembers` is true. Indirect deps are a path-to-name multimap.
  - Results are returned sorted (`getSortedMissingClassInternalNames`, incomplete classes by internal name, missing members, indirect deps). `isEmpty()` is true only when all four collections are empty. This class has no diagnostic text and no charset.
  - The only message strings are thrown errors: `"The internal name is invalid. %s"`, a duplicate incomplete state that is not the same object, and `"Unsupported state " + state`.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/AbstractClassCacheTest.java`
- Bazel lines: 104
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no import-deps class-cache test. The only related code is the rules_java action wrapper at `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/common/rules/impl/import_deps_check.bzl` and the prebuilt `ImportDepsChecker_deploy.jar`; neither sets up or asserts this fixture.
  - Missing the shared fixture for `ClassCacheTest` and `DepsCheckerClassVisitorTest`: bootclasspath plus Client, Library, Library_no_members, LibraryAnnotations, LibraryException, LibraryInterface, and LibraryModuleInfo jars, loaded from `classcache.test.*` system properties.
  - Missing the expected internal names under `com/google/devtools/build/importdeps/testdata/`: `Client` and `Client$NestedAnnotation`; `Library` plus `Library$Class1` through `Library$Class12`; annotation nesteds (`ClassAnnotation`, `MethodAnnotation`, `FieldAnnotation`, `ConstructorAnnotation`, `ParameterAnnotation`, `TypeAnnotation`, `AnnotationAnnotation`, `AnnotationFlag`); `LibraryException`; and `LibraryInterface` plus `$Func`, `$One`, `$Two`, `$InterfaceFoo`, `$InterfaceBar`.
  - Missing the `combine` helper that flattens those positive lists for subclasses.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/ClassCacheTest.java`
- Bazel lines: 345
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no import-deps class-cache test. The only related file, `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/common/rules/impl/import_deps_check.bzl`, registers an `ImportDepsChecker` action and does not assert cache states.
  - Missing / incomplete: Bazel asserts a client without superclasses is incomplete, with missing ancestors `Library`, `LibraryInterface`, `LibraryInterface$One`, and `LibraryInterface$Two`, while nested annotations and boot classes (`java/lang/Object`, `java/util/List`) exist. A superclass miss then keeps the subclass incomplete (`Library$Class9`, `Library$Class10`).
  - Direct: `Library` on the direct classpath is `directDep=true` from `libraryJar`; `LibraryInterface` only on the full classpath is `directDep=false` from `libraryInterfaceJar`. With superclasses present, used regular jars are recorded as implicit (`false`).
  - Jar order: the first classpath that contains a class wins. `libraryJar` before `libraryWoMembersJar` yields members `<init>` and `createClass5`; the reverse yields only `<init>`. `populateMembers=false` yields an empty member set. `collectUsedJarsInRegularClasspath` grows as jars are actually resolved.
  - Jspecify: this Bazel test does not assert it. Zel has no class-cache test for it either.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/ClassInfoTest.java`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no import-deps `ClassInfo` or `MemberInfo` test. Bazel asserts `MemberInfo` name, descriptor, and equality (`hashCode`/`()I`, `clear`/`()V`).
  - Bazel asserts `internalName`, `jarPath`, `directDep() == true`, `declaredMembers` (object members ordered via `containsExactly(...).inOrder()`), and `superClasses` containing the parent.
  - Bazel asserts `containsMember` is true for a class's own members and for inherited members (`hashCode` on the child), and false for members the class and its supers do not declare (`size`/`()I`, `clear`/`()V` on the parent).

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/DepsCheckerClassVisitorTest.java`
- Bazel lines: 190
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no bytecode or ASM-visitor test. The nearest Java tests (`/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs`, `/workspace/zel-cursor/crates/zel-exec/tests/exec_runtime.rs`) only parse `.jdeps` protos and reduced classpaths.
  - Bazel runs `DepsCheckerClassVisitor` on `Client`, `Client$NestedAnnotation`, and `Client$InnerClassWithSyntheticConstructorParam` with `ClassReader.SKIP_DEBUG | ClassReader.SKIP_FRAMES`, and asserts `java/lang/invoke/LambdaMetafactory` and `java/lang/Enum` are present.
  - Omitting one library jar at a time must report exactly that jar’s missing classes (`libraryExceptionJarPositives`, `libraryInterfacePositives`, `libraryAnnotationsJarPositives`, `libraryJarPositives`).
  - With member checking on and `libraryWoMembersJar`, missing classes stay empty and the missing members are, in order, `Library$Class1.I`, `Library$Class3.field`, `Library$Class4.createClass5`, and `Library$Class5.create`. With member checking off, the collector is empty.
  - Visiting `module-info.class` (no superclass) must not throw. Zel does not assert that.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/DumpProto.java`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel `DumpProto.main` reads `args[0]` as binary `Deps.Dependencies` (a `.jdeps` proto) through `BufferedInputStream` and `Files.newInputStream`, then `System.out.print`s `TextFormat.printer().printToString`. Input is binary; output is a Java `String` on `System.out` (default charset), with no explicit UTF-8 writer. Zel has no jdeps or proto dump helper that does this.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/ImportDepsCheckerTest.java`
- Bazel lines: 152
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - This file never asserts missing classes, incomplete hierarchies, missing members, a text report, or a report charset. Its only test, `testJdepsProtos`, checks `ImportDepsChecker.check()` plus `emitJdepsProto("empty")`.
  - Jdeps proto: five classpath cases. `check()` is false unless the classpath includes `libraryJar`, `libraryAnnotationsJar`, `libraryExceptionJar`, and `libraryInterfaceJar` (then true). Dependencies are exactly the regular classpath as `Kind.EXPLICIT` with that path string, except the case whose input is `libraryJar` and whose only classpath jar is `libraryAnnotationsJar`, which expects an empty dependency list. Paths must not start with `/`. A second `check()` and `emitJdepsProto` match the first proto. The same proto is produced by `Main.checkDeps` with `--bootclasspath_entry`, `--classpath_entry`, `--input`, `--jdeps_output`, `--rule_label=empty`, `--output` (a temp `.txt` that is not read), and `--checking_mode=silence`.
  - Zel has no import-deps checker unit test. The nearest code is the Starlark action in `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/common/rules/impl/import_deps_check.bzl`, which runs the checker with `--checking_mode=error` and writes `jdeps.proto`. It does not assert missing classes, hierarchies, members, report text, charset, relative paths, explicit kind, or proto stability.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/LazyClassEntryStateTest.java`
- Bazel lines: 101
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no import-deps unit test for lazy class-entry states. The nearest file, `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/common/rules/impl/import_deps_check.bzl`, only wires the ImportDepsChecker action and does not assert state behavior.
  - Bazel `testExistingState`: `ExistingState` created with `direct=true` is existing only, `direct()` is true, `asExistingState()` is the same instance, `asIncompleteState()` and `asMissingState()` throw `IllegalStateException`, and `classInfo` is `java/util/List` with the single member `hashCode ()I`.
  - Bazel `testIncompleteState`: `IncompleteState.create` with a parent chain and an empty child list throws `IllegalArgumentException`. A state built from missing `java/lang/Object` is incomplete only, `asIncompleteState()` is the same instance, the other casts throw `IllegalStateException`, `classInfo` matches the existing-state fixture, `getMissingClassesWithSubclasses()` is empty, and `missingAncestors()` is exactly `java/lang/Object`.
  - Bazel `testMissingState`: `MissingState.singleton()` is missing only, `asMissingState()` is the same instance, and the other casts throw `IllegalStateException`.
  - This Bazel test does not assert a direct-upgrade transition. It only constructs an already-direct existing state. Zel has no test of that either.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/MainTest.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The only Bazel case is `@Ignore` `usage`: `Main.parseCommandLineOptions` with `--bootclasspath_entry`, `--classpath_entry`, and `--input` (no `--jdeps_output`) must throw `IllegalArgumentException` whose message contains `--jdeps_output`. Zel has no test of that CLI parse, exit codes, modes, report text, or charset. `zel-cursor/third_party/bcr/rules_java/9.1.0/java/common/rules/impl/import_deps_check.bzl` only wires the tool (`--checking_mode=error`, `--jdeps_output`); it is not a counterpart test.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/ResolutionFailureChainTest.java`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no import-deps test for missing-ancestor chains. Bazel asserts: `createWithParent` with an empty parent list throws `IllegalArgumentException`; a missing `java/lang/Object` chain reports that class, an empty subclass map, empty parent chains, and a null start class; a one-head `CharSequence` chain keeps `Object` as the only missing class, `CharSequence` as the start class and sole subclass, and the missing-`Object` chain as its only parent; a two-head `String` chain over both of those parents still reports only `java/lang/Object`, both parents, `String` and `CharSequence` as subclasses, and `String` as the start class.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/ResultCollectorTest.java`
- Bazel lines: 158
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no import-deps result-collector test. The only Zel hits are a filename listing in `zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`, the `experimental_import_deps_checking` flag, and a prebuilt `ImportDepsChecker` jar label.
  - Bazel asserts an empty collector has empty sorted missing classes, missing members, incomplete classes, and indirect deps, and `isEmpty()` is true.
  - A missing class (`MissingState`) is reported by internal name; adding a missing member does not drop it; `isEmpty()` becomes false.
  - An incomplete class stores `IncompleteState` with `ClassInfo` and a missing-superclass failure chain, and is returned from `getSortedIncompleteClasses()`.
  - Missing class names are returned from `getSortedMissingClassInternalNames()` (insertion order String, Integer, Object; equality only, not an explicit sort order).
  - Indirect deps collapse duplicate path/name pairs and `getSortedIndirectDeps()` key order is path-sorted (`a` then `b`).
  - `MissingMember` equality covers owner `ClassInfo`, name, descriptor, and `MemberInfo`, including both factory overloads.
  - `MissingMember.compareTo` orders by owner internal name, then member name (`A/B` < `A/C` < `B/B`).

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/golden_test.sh`
- Bazel lines: 91
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no import-deps golden test. There is no `golden_test.sh` and no `java_tools` or `importdeps` test tree under `/workspace/zel-cursor`.
  - Bazel `/workspace/bazel/src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/golden_test.sh` runs the checker as `$@ --jdeps_output`, requires the exit code to equal `expected_exit_code`, then diffs golden stdout (printed with `$PRINT_JDEPS_PROTO`) and golden stderr after stripping the `third_party/bazel/` prefix. The script does not set or check a charset.
  - The only Zel import-deps file, `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/common/rules/impl/import_deps_check.bzl`, is the checker action wrapper, not a golden test.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/Client.java`
- Bazel lines: 120
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Sample client for the import-deps checker. Zel has no fixture that compiles or checks these references.
  - Superclass `Library`; superinterfaces `LibraryInterface`, `LibraryInterface.One`, `LibraryInterface.Two`; overrides `callOne` and `callTwo`.
  - Type-parameter annotation `@TypeAnnotation` on `T`. Class annotation `@ClassAnnotation` with class literals `Library.class` and `LibraryException.class` and a nested `@SuppressWarnings`.
  - Field types `Library.Class1` and `LibraryAnnotations`, annotated `@FieldAnnotation` and `@FieldAnnotation({1, 2, 3})`. Static read `Class1.I`.
  - Constructor `@ConstructorAnnotation`. Method `@MethodAnnotation(name = "method")`, parameter `@ParameterAnnotation(position = 0)`, parameter type `Library.Class2`, thrown type `LibraryException`.
  - Member uses: `new Class3()`, `Class3.field` read and write, method reference `Class4::createClass5` assigned to `LibraryInterface.Func<Class5>` then `Func.get()`, `Class5.create(new Class7())`.
  - Arrays: `Class8[][]` allocation, `clone()`, checkcast back to `Class8[][]`, `Class9[]` store of `new Class10()`.
  - `Class11.foo()` and `Class11.bar()` (default methods from `LibraryInterface.InterfaceFoo` and `InterfaceBar`). Class literal `Class12.class` (b/123020654).
  - Enum `EnumTest` constants plus `ordinal()` and `name()`. JDK calls `Objects.nonNull`, `Integer.valueOf`, `System.out.println`, `String.valueOf`.
  - Nested annotation `NestedAnnotation` with `@Retention(RetentionPolicy.RUNTIME)`, `@Target(ElementType.TYPE)`, and `@AnnotationAnnotation(AnnotationFlag.Y)`.
  - Inner class `InnerClassWithSyntheticConstructorParam` whose constructor takes `@ParameterAnnotation` on a real parameter while the synthetic outer-this parameter must not be treated as `java/lang/Synthetic` (b/78024300).

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/InvokePolymorphic.java`
- Bazel lines: 25
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no import-deps fixture for signature-polymorphic calls. These are `invokevirtual`, not `invokedynamic`.
  - `mh.invoke(true, "hello", 42)` → `java/lang/invoke/MethodHandle.invoke(ZLjava/lang/String;I)Ljava/lang/Object;`. The checker must not look up that call-site descriptor as a real `MethodHandle` member (`invoke` / `invokeExact` are skipped).
  - `vh.setRelease("foo", 1)` → `java/lang/invoke/VarHandle.setRelease(Ljava/lang/String;I)V`. Same skip for the polymorphic `VarHandle` accessor (`setRelease` is on that list).
  - Deps the checker still records, which Zel never exercises here: parameter types `java/lang/invoke/MethodHandle` and `java/lang/invoke/VarHandle` on `void f(...)`, and the declared exception `java/lang/Throwable`.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/JspecifyUser.java`
- Bazel lines: 0
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - None found. The Bazel 9.3.2 blob is empty (`e69de29`, 0 bytes), so it declares no imports, types, members, or annotations. The checker is not shown any `org.jspecify` reference, retention policy, or type-use versus declaration target. Zel has no import-deps JSpecify fixture.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/Library.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `com.google.devtools.build.importdeps.testdata.Library` (top-level class; `Client` extends it). No annotations are declared on this type or its members.
  - `Library.Class1`, including `public static final Class1 I` and the `new Class1()` initializer.
  - `Library.Class2` (method parameter type).
  - `Library.Class3`, including field `public Class4 field`.
  - `Library.Class4`, including `Class5 createClass5()` and `new Class5()` (also used as the method reference `c4::createClass5`).
  - `Library.Class5`, including `public Class6 create(Class7 class7)` and `new Class6()`.
  - `Library.Class6` and `Library.Class7` (return type and parameter of `Class5.create`).
  - `Library.Class8` (array component type `Class8[][]`).
  - `Library.Class9` and `Library.Class10 extends Class9` (array store `Class9[]` ← `new Class10()`).
  - `Library.Class11 implements LibraryInterface.InterfaceFoo, LibraryInterface.InterfaceBar` (`foo()` / `bar()` come from those interfaces).
  - `Library.Class12` (class literal `Class12.class`).

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/LibraryAnnotations.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Every annotation is `@Retention(RetentionPolicy.RUNTIME)`, so javac keeps it in the class file (`RuntimeVisibleAnnotations`, or `RuntimeVisibleTypeAnnotations` for the type-use one). `SOURCE` would be erased and the checker would never see it. The visitor also accepts invisible (`CLASS`) annotations, but this fixture only emits runtime ones.
  - `LibraryAnnotations` itself (client field type `com/google/devtools/build/importdeps/testdata/LibraryAnnotations`).
  - `ClassAnnotation`: declaration target `ElementType.TYPE`; `Class<?>[] friends() default {}` (use site passes `Library.class` and `LibraryException.class`); `SuppressWarnings nested() default @SuppressWarnings("raw")` (nested annotation type, including the default).
  - `MethodAnnotation`: declaration target `ElementType.METHOD`; required `String name()`.
  - `FieldAnnotation`: declaration target `ElementType.FIELD`; `int[] value() default {}` (primitive array; no class beyond the annotation type).
  - `ConstructorAnnotation`: declaration target `ElementType.CONSTRUCTOR`; no members.
  - `ParameterAnnotation`: declaration target `ElementType.PARAMETER` (parameter annotation, not a type-use annotation on the parameter type); `byte position() default -1`.
  - `TypeAnnotation`: targets `{ElementType.TYPE_PARAMETER, ElementType.TYPE_USE}` only, so it is a type annotation (`visitTypeAnnotation` on `Client<@TypeAnnotation T>`), not a declaration annotation; no members.
  - `AnnotationAnnotation`: declaration target `ElementType.ANNOTATION_TYPE`; required `AnnotationFlag value()`.
  - `enum AnnotationFlag { N, Y }`. The client use `AnnotationFlag.Y` is a member check (`visitEnum` on the enum class and constant `Y`). `N` is declared and indexed with the enum, and is not referenced at the use site.
  - `testMissingLibraryAnnotations` expects all nine binary names at once: `LibraryAnnotations` plus `$ClassAnnotation`, `$MethodAnnotation`, `$FieldAnnotation`, `$ConstructorAnnotation`, `$ParameterAnnotation`, `$TypeAnnotation`, `$AnnotationAnnotation`, and `$AnnotationFlag`.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/LibraryException.java`
- Bazel lines: 17
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `com.google.devtools.build.importdeps.testdata.LibraryException` (`public class`; the only type in `testdata_lib_LibraryException`). `Client.method` declares `throws LibraryException`, and `@ClassAnnotation` lists the class literal `LibraryException.class`.
  - Superclass `java.lang.Exception`. No interfaces. The checker walks this ancestor and reports it indirectly missing when the bootclasspath lacks `java.lang.Exception`.
  - No fields, methods, or constructors are declared in the source. The compiler still emits the default constructor `<init>()V`, which the checker records as a member when member population is on.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/LibraryInterface.java`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Top-level `public interface LibraryInterface` (`com/google/devtools/build/importdeps/testdata/LibraryInterface`). `Client` implements it, and `testMissingLibraryInterface` expects this binary name when the interface jar is absent.
  - Nested `interface Func<T>` (`LibraryInterface$Func`) with abstract `T get()`. `Client.method` uses it as `Func<Class5>` and calls `get()`.
  - Nested `interface One` (`LibraryInterface$One`) with abstract `void callOne()`. `Client` implements it and overrides `callOne`.
  - Nested `interface Two` (`LibraryInterface$Two`) with abstract `void callTwo()`. `Client` implements it and overrides `callTwo`.
  - Nested `interface InterfaceFoo` (`LibraryInterface$InterfaceFoo`) with default `void foo() {}`. `Library.Class11` implements it, and `Client.method` calls `eleven.foo()`.
  - Nested `interface InterfaceBar` (`LibraryInterface$InterfaceBar`) with default `void bar() {}`. `Library.Class11` implements it, and `Client.method` calls `eleven.bar()`.
  - Annotations: none declared in this file.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/OneAnnotationValue.java`
- Bazel lines: 20
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `public class com.google.devtools.build.importdeps.testdata.OneAnnotationValue`, implicit superclass `java.lang.Object`.
  - Method `void f()`.
  - Method declaration annotation `@StringAnnotation("hello")`: annotation type `com.google.devtools.build.importdeps.testdata.StringAnnotation`, element `value`, constant `java.lang.String` `"hello"`.
  - Companion type in `StringAnnotation.java` (`java_library` `StringAnnotation`, dep of `OneAnnotationValue`): `public @interface StringAnnotation` with member `String value()`, `@Retention(RetentionPolicy.RUNTIME)`, and no `@Target`. `RUNTIME` keeps the use in the method’s runtime-visible annotations, so `string_annotation_value_test` loads that type. With no `@Target`, this is a declaration annotation on `f()` (`ElementType.METHOD`), not a type-use or type-parameter annotation.
  - Checker resolutions Zel never fixtures: explicit classpath use of `StringAnnotation`; missing `java.lang.Object`; missing `java.lang.annotation.Annotation` (the `@interface` superinterface); `StringAnnotation` incomplete because of those two ancestors (`missing=2`, `incomplete=1`, `missing_members=0`). The string element is not a class dependency, and `value()` is not member-checked.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/StringAnnotation.java`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Annotation type `com.google.devtools.build.importdeps.testdata.StringAnnotation` (`public @interface`, superinterface `java.lang.annotation.Annotation`). Zel has no import-deps fixture for it.
  - `@Retention(RetentionPolicy.RUNTIME)`: runtime-visible meta-annotation. The checker resolves `java.lang.annotation.Retention` and the enum member `java.lang.annotation.RetentionPolicy.RUNTIME`.
  - Member `String value()` with no default. The checker resolves the method descriptor `()Ljava/lang/String;`. This file has no string literal; a string element value is not a class dependency.
  - No `@Target`. Nothing resolves `java.lang.annotation.Target` or `ElementType`, including no `TYPE_USE` or `TYPE_PARAMETER`. With `@Target` omitted, Java applies the annotation to declaration contexts only, not type-use contexts.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/j_p_l/A.java`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No package declaration (default package). The directory name `j_p_l` is the Bazel package, not a Java package. No `java.lang` package-name collision.
  - Package-private `class A`.
  - Field `B b` (simple name in the default package; sibling `B.java`).
  - Field `p.P1.M1 m1`. The checker resolves the nested type as `p.P1$M1`, and through that type `p.P1$M1OrBuilder`, `com.google.protobuf.GeneratedMessage`, and `com.google.protobuf.MessageOrBuilder` (`golden_proto_strictdeps.stderr.txt`).
  - Field `p.P2.M2 m2`. The checker resolves `p.P2$M2` and, through it, `p.P2$M2OrBuilder` plus the same two protobuf supertypes.
  - Zel has no `j_p_l` fixture. The only nearby `class A` is an empty `class A {}` in `/workspace/zel-cursor/crates/zel-exec/tests/exec_runtime.rs`, which declares none of these members or types.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/j_p_l/B.java`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No package declaration (default package). The directory name `j_p_l` is the Bazel package, not a Java package. No `java.lang` package-name collision.
  - Package-private `class B` with an empty body: no fields, methods, constructors, or nested types. The only type this compilation unit declares is `B` (implicit superclass `java.lang.Object`).
  - `A.java` references that type as field `B b`. The checker records `j_p_l/libb-hjar.jar` as an explicit dependency of `j_p_l:a` (`golden_proto_strictdeps.txt`). `B.java` itself names no other type.
  - Target `j_p_l:b` depends on `:p1_java_proto`, but `B.java` does not mention `p.P1` or `p.P1$M1`. Those missing-class results are on `liba-hjar.jar`, not on `B`.
  - Zel has no `j_p_l` fixture and no `class B` declaration in its Java or Rust test sources.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/j_p_l/p1.proto`
- Bazel lines: 22
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no import-deps protobuf fixture for this file: no `package p1`, no `message M1`, and no `option java_package = "p"`.
  - Proto2, empty message, no fields and no `java_outer_classname`. `java_package` is `p` while the proto package is `p1`. With `java_multiple_files` left at its default (false), the outer class is the filename-derived `P1`, so the message is the nested type `p.P1.M1` (binary name `p.P1$M1`).
  - No fields, so proto2 explicit presence (`has*`) is not exercised. This is not proto3 implicit presence.
  - Types the checker resolves that Zel does not test, from `A.java` and `golden_proto_strictdeps.stderr.txt`: nested message `p.P1$M1` (supertype `com.google.protobuf.GeneratedMessage`) and `p.P1$M1OrBuilder` (supertype `com.google.protobuf.MessageOrBuilder`).

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/j_p_l/p2.proto`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Proto2, package `p2`, empty message `M2`, `option java_package = "p"`, and no `java_outer_classname`. No fields, so no proto2 presence (`has*`) and no proto3 implicit presence.
  - Java codegen (no `java_multiple_files`) nests the message in a filename-derived outer class: `p.P2.M2` (`p.P2$M2`), not top-level `p.M2` and not package `p2`.
  - The checker is expected to resolve `p.P2$M2`, `p.P2$M2OrBuilder`, and supertypes `com.google.protobuf.GeneratedMessage` and `com.google.protobuf.MessageOrBuilder` (`A.java` holds `p.P2.M2`; `golden_proto_strictdeps.stderr.txt`). Zel has no fixture for those types, that outer-class naming, or this empty proto2 shape.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/library_no_members/com/google/devtools/build/importdeps/testdata/Library.java`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no import-deps Java fixture. The closest file, `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/common/rules/impl/import_deps_check.bzl`, only launches the checker (`--input`, `--directdep`, `--classpath_entry`, `--checking_mode=error`). It never declares these types.
  - This copy still declares the same type set as `/workspace/bazel/src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/testdata/Library.java`: outer `Library` plus static nested `Class1`–`Class12`, with `Class10 extends Class9` and `Class11` implementing `InterfaceFoo` and `InterfaceBar`.
  - Members present on the full `Library` and omitted here, which `Client` references and `DepsCheckerClassVisitorTest.testMissingMembersInClient` / `golden_library_members_missing.stderr.txt` expect as `missing_members=4`: `Class1.I` (`Lcom/google/devtools/build/importdeps/testdata/Library$Class1;`), `Class3.field` (`Lcom/google/devtools/build/importdeps/testdata/Library$Class4;`), `Class4.createClass5` (`()Lcom/google/devtools/build/importdeps/testdata/Library$Class5;`), `Class5.create(Class7)` (`(Lcom/google/devtools/build/importdeps/testdata/Library$Class7;)Lcom/google/devtools/build/importdeps/testdata/Library$Class6;`).
  - The checker still sees the empty types, including a default `<init>` only on `Library$Class4` (`ClassCacheTest.testLazyClasspathLoadsBootclasspathFirst`). With `checkMissingMembers=false`, the same jar is treated as complete.
  - Source-only: this file imports `LibraryInterface.InterfaceBar` and `InterfaceFoo` and lists `Class11 implements InterfaceBar, InterfaceFoo`. The full file uses qualified `LibraryInterface.InterfaceFoo, LibraryInterface.InterfaceBar` and has no imports.

### `src/java_tools/import_deps_checker/javatests/com/google/devtools/build/importdeps/tests.bzl`
- Bazel lines: 107
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel import-deps test counterpart. The only related file is the action wrapper `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/common/rules/impl/import_deps_check.bzl`, which registers an `ImportDepsChecker` action and does not declare tests.
  - Bazel loads `JavaInfo` and `sh_test`, then defines the `compile_time_jars` rule (`deps` as a `JavaInfo` label list; returns transitive compile-time jars as files and runfiles). Zel has no such test helper.
  - Bazel defines `create_golden_test` (`name`, `golden_output_file`, `golden_stderr_file`, `expect_errors`, `checking_mode`, `has_bootclasspath`, `testdata_pkg`, `import_deps_checker`, `rt_jar`, plus `missing_jar`, `replacing_jar`, `direct_jars`, `check_missing`). It emits an `sh_test` with `srcs = ["golden_test.sh"]`.
  - That test's data are the two golden files, `import_deps_checker`, `rt_jar`, `:DumpProto`, and `testdata_client`, `testdata_lib_Library`, `testdata_lib_LibraryAnnotations`, `testdata_lib_LibraryException`, `testdata_lib_LibraryInterface` (plus `replacing_jar` when set). Zel declares none of these test targets or deps.
  - Test flags are an expected exit code (`199` or `0`), `--checking_mode`, `--check_missing_members`, `--bootclasspath_entry` (`rt_jar`, or `testdata_client` as a fake bootclasspath), `--classpath_entry` (skipping `missing_jar`, substituting `replacing_jar`), `--directdep`, `--input` on `testdata_client`, and `--rule_label=:<name>`. Zel has no golden-test flag setup.

### `src/java_tools/junitrunner/java/com/google/testing/coverage/BitField.java`
- Bazel lines: 215
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `com.google.testing.coverage.BitField`. The nearest coverage code is the vendored Java report merger under `zel-cursor/crates/zel-bazel-tools/resources/tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/`, which stores branch hits as maps, not probe bit fields.
  - Storage is a cloned `byte[]`. The constructor and `getBytes()` both clone, so the caller cannot alias the buffer. `sizeInBits()` is `bytes.length * 8`.
  - Bit order is least-significant-bit first inside each byte: `byteIndex = index / 8`, `bitIndex = index % 8`, `mask = 1 << bitIndex`. Java `/` and `%` truncate toward zero. A negative `index` yields a negative `byteIndex` (`ArrayIndexOutOfBoundsException` or `NegativeArraySizeException` on grow). An index past the end of `isBitSet` returns false instead of throwing.
  - `setBit` grows with `Arrays.copyOf`, which zero-fills. `clearBit` uses the same path with the bit cleared.
  - `and` returns a new field whose length is the minimum of the two lengths, so bits past the shorter field are dropped. `or` returns a new field whose length is the maximum and ORs the shorter prefix into a copy of the longer array. The `or` Javadoc says it returns `this`; the method returns a new `BitField`.
  - `equals(Object)` and `equals(byte[])` are `Arrays.equals` on the raw bytes, so trailing zero bytes make two fields unequal even when the set bits in the shared prefix match. `hashCode` is `Arrays.hashCode(bytes)` (Java's 31-polynomial over the signed bytes). The `byte[]` overload does not override `equals(Object)`.
  - `countBitsSet` uses a 256-entry popcount table filled by `BIT_COUNT_LOOKUP[i] = popcount(i/2)` and `BIT_COUNT_LOOKUP[i+1] = that + 1`. Java `byte` is signed, so the lookup index is `0xFF & b`. Using the raw negative byte as an index throws `ArrayIndexOutOfBoundsException`.
  - `not` inverts each byte with `Integer.valueOf(~bytes[i]).byteValue()`. The `byte` is sign-extended to `int` before `~`; `byteValue()` keeps the low 8 bits, which is still a per-byte invert. `any` is true when any byte is nonzero, including a negative byte whose high bit is set.

### `src/java_tools/junitrunner/java/com/google/testing/coverage/BranchCoverageDetail.java`
- Bazel lines: 86
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `junitrunner` sources and no `com.google.testing.coverage.BranchCoverageDetail` (no per-line `TreeMap` of branch counts plus `BitField` taken bits).
  - Bazel counts branches with `setBranches`/`getBranches` (missing line returns 0) and treats a line as having branches only when that count is greater than 1 (`linesWithBranches`).
  - Taken vs not is a boolean bit per `(line, branchIdx)` (`setTakenBit`/`getTakenBit`). `getExecutedBit` is true only if any bit on that line is set.
  - The nearest Zel types, `BranchCoverageItem` and `BranchCoverage` under `zel-cursor/crates/zel-bazel-tools/resources/tools/test/CoverageOutputGenerator/`, are the LCOV merger (line, block id, branch id, `evaluated`, execution count). They do not use a branch-count map or taken-bit index, and `wasExecuted` is count `> 0` (BRDA) or count `== 2` (BA).

### `src/java_tools/junitrunner/java/com/google/testing/coverage/BranchDetailAnalyzer.java`
- Bazel lines: 130
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `junitrunner` package and no JaCoCo probe walker (`ClassProbesAdapter` / `ClassProbesMapper`, `CRC64.classId`, `ExecutionData.getProbes()`, per-line `BranchExp` evaluation, `setBranches` / `setTakenBit`). The nearest coverage code, `BranchCoverage` in `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/BranchCoverage.java`, only stores already-collected branch counts (`int` line keys, `long` execution counts). It does not read class bytes or map probes, so it is not this analyzer. No Java-vs-Rust index or integer-width mismatch applies.

### `src/java_tools/junitrunner/java/com/google/testing/coverage/BranchExp.java`
- Bazel lines: 133
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no junitrunner `CovExp` tree (`BranchExp`, `ProbeExp`, `NullExp`). Its `BranchCoverage` class is an LCOV execution-count map (line, block id, branch id, hit count), not a probe expression.
  - Bazel stores an ordered `List<CovExp>`. Unset slots are `NullExp.NULL_EXP`, which always evaluates to false. `hasBranches` and `getBranches` drop those slots; `concatenate` keeps them.
  - `setBranchAtIndex` grows the list with `NullExp` fillers up to `index + 1`, then writes that index and clears the eval cache. `add` appends and returns `size - 1`, and does not clear the cache. Indexes are 0-based Java `int`s. This file does not use signed bytes or integer division.
  - `eval` is a short-circuit OR of the children against a `boolean[]` probe vector. Probe ids live in child `ProbeExp`s (`probes[probeId]`), not in `BranchExp`.
  - The result is cached only when it is true (`hasValue = value`) and only for the same array reference (`probes == probesUsed`). A false result is stored but not reused, so the next `eval` recomputes.
  - `zip` pairs equal indexes into a nested `BranchExp` (OR of that pair) and wraps any longer-side remainder as a one-child `BranchExp`. `ensureIsBranchExp` wraps a non-`BranchExp`. `initializeEmptyBranches` starts from an empty list.

### `src/java_tools/junitrunner/java/com/google/testing/coverage/ClassProbesMapper.java`
- Bazel lines: 140
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `java_tools/junitrunner` tree and no JaCoCo `ClassProbesVisitor` / `IFilterContext` implementation. Nearby Zel coverage (`collect_coverage.sh`, `collect_cc_coverage.sh`, and `CoverageOutputGenerator` line/branch LCOV types) never walks class files or maps probes to lines.
  - Bazel builds a `TreeMap<Integer, BranchExp>` (32-bit line numbers, natural order) and returns it from `result()`. The constructor interns the incoming class name with JaCoCo `StringPool` and installs `Filters.all()` for every method.
  - `visitAnnotation` records each annotation descriptor in a `HashSet`. `visitAttribute` records `attribute.type` and does not delegate to `super`. `visitSource` interns the source file name and stores the debug extension as given (it can be null). `visitField` only delegates to `super`.
  - `visit(version, access, name, signature, superName, interfaces)` stores `stringPool.get(name)` in `superClassName`. The superclass argument `superName` is ignored, so `getSuperClassName()` returns the visited class’s own internal name, not its superclass. Filters that consult this context therefore see the wrong type.
  - `visitMethod` returns a `MethodProbesMapper` that, on `visitEnd`, merges that method’s line-to-`BranchExp` map into the class map with `putAll`. A later method overwrites an earlier entry for the same line. Probe ids themselves are not stored here; JaCoCo’s probe visitor assigns them inside the method mapper.
  - `visitTotalProbeCount(int count)` discards the 32-bit probe total. This class never indexes a probe array and does not touch signed bytes.

### `src/java_tools/junitrunner/java/com/google/testing/coverage/CovExp.java`
- Bazel lines: 24
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel declares the public interface `com.google.testing.coverage.CovExp` for a branch-coverage expression: either one probe id or a combination of probes.
  - Its only member is `boolean eval(final boolean[] probes)`, which returns whether that expression holds for the given probe-hit array. The type has no fields, constants, or default methods, and it does no indexing itself.
  - Zel has no junitrunner sources and no `CovExp` (nor the Bazel implementors `ProbeExp`, `BranchExp`, or `NullExp`). Zel coverage code under `com.google.devtools.coverageoutputgenerator` only merges LCOV or gcov source-file reports. `rules_java` and `rules_kotlin` merely name `JacocoCoverageRunner` as a main class, and a bzlmod fixture lists the path `CovExp.java` without the type.

### `src/java_tools/junitrunner/java/com/google/testing/coverage/IllegalStringException.java`
- Bazel lines: 26
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines `com.google.testing.coverage.IllegalStringException`, an unchecked `RuntimeException`. Zel has no junitrunner sources and no type in that package.
  - The only constructor is `IllegalStringException(String message)`, which forwards that string to `RuntimeException(String)`. There is no `(message, cause)` or cause-only constructor, so a cause cannot be attached at construction.
  - The class does not decode a charset and does not parse the string. Its javadoc says it is thrown from `BitField.fromString()` when the input has an odd length or contains a non-hexadecimal character.
  - Zel coverage is LCOV collection and `com.google.devtools.coverageoutputgenerator` types. None of those throw or define this exception, and none reject an odd-length or non-hex bitfield string. The path appears only as a filename in a patch-corpus listing.

### `src/java_tools/junitrunner/java/com/google/testing/coverage/JacocoCoverageRunner.java`
- Bazel lines: 614
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no JaCoCo coverage runner (no `JacocoCoverageRunner`, `JacocoLCOVFormatter`, or `BranchDetailAnalyzer` in Rust or Java). `crates/zel-exec/resources/collect_coverage.sh` and `crates/zel-bazel-tools/resources/tools/test/collect_coverage.sh` only export `JAVA_COVERAGE_FILE=$COVERAGE_DIR/jvcov.dat` and, when `JAVA_RUNTIME_CLASSPATH_FOR_COVERAGE` is set, merge the runtime classpath with SingleJar into `JACOCO_METADATA_JAR`. They assume this runner already exists. The LCOV merger only consumes `.dat` afterward.
  - Bazel turns off JaCoCo’s own writer with `jacoco-agent.output=none` and sets `jacoco-agent.sessionid=default` so a blank session id does not trigger a DNS lookup. A JVM shutdown hook then calls `RT.getAgent().getExecutionData(false)` (do not reset probes). If the agent was never started (`IllegalStateException`), it treats the run as success with an empty dump.
  - The hook writes that opaque `byte[]` in append mode to a unique `*.exec` file, but builds the LCOV report only from this JVM’s bytes (`ByteArrayInputStream`), not from whatever else was appended to the `.exec`. `JAVA_COVERAGE_FILE` is a path template: the extension is stripped and `File.createTempFile` yields a sibling `*.dat` (LCOV) and `*.exec` so `lcov_merger` can collect every `.dat`. A null template becomes a throwaway `coverage*` temp file. `IOException` in the hook prints the stack and `Runtime.halt(1)`.
  - The LCOV writer is UTF-8, `CREATE` and `APPEND` (`newBufferedWriter(..., UTF_8, CREATE, APPEND)`). Class bytes and the exec dump are raw bytes, never decoded. This file does not mask or widen Java’s signed `byte`; probe counters stay inside JaCoCo’s `ExecFileLoader` / `Analyzer`. The only wider integer here is `Unsafe.objectFieldOffset`, a `long`, used to read the Java 9+ application class loader’s URL list.
  - Before the test runs, every classpath `*.jar` is scanned for `*.class.uninstrumented` (stored by entry name, first wins) and for `*-paths-for-coverage.txt` (UTF-8, one path per line, stored verbatim, including `source////class` lines). That preload exists because the shutdown hook has only a few seconds. At report time, if that map is empty, the same suffixes are read again from the metadata jars. Duplicate class entry names are skipped. Branch details are a second pass into a `TreeMap`. The bundle name is the literal `isthisevenused`. The source locator returns `null` and tab width `0`.
  - Metadata paths are UTF-8 lines joined as `javaRunfilesRoot + "/" + line` (literal slash, no normalization). A single `*_deploy.jar` or `*_merged_instr.jar` is one file; otherwise `JACOCO_METADATA_JAR` is that path list. `CLASSPATH_JAR` (and legacy `JACOCO_IS_JAR_WRAPPED`) unwraps a manifest `Class-Path` split on spaces. The main class is `Coverage-Main-Class` from a deploy-jar manifest (skipping names that do not load), else `JACOCO_MAIN_CLASS`, else `IllegalStateException`. It is invoked reflectively with the original args.

### `src/java_tools/junitrunner/java/com/google/testing/coverage/JacocoLCOVFormatter.java`
- Bazel lines: 205
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/test/CoverageOutputGenerator/java/com/google/devtools/coverageoutputgenerator/LcovPrinter.java`
- Verdict: partial
- Differences:
  - Zel has no `JacocoLCOVFormatter`. `LcovPrinter` only reprints an already-parsed tracefile. The runtime merger at `/workspace/zel-cursor/crates/zel-exec/resources/lcov_merger.sh` prints the same record order in awk. Neither reads JaCoCo `IClassCoverage` / `ISourceFileCoverage` / `BranchCoverageDetail`.
  - Bazel record per source, only when a class survived path filtering: `SF:<path>`, then for each method `FN:<firstLine>,<class>::<method> <jvm-desc>` immediately followed by `FNDA:<methodCounter.coveredCount>,<same>`, then `BRDA` lines, then `DA` lines, then `end_of_record`. If there is class coverage but no `ISourceFileCoverage`, the record is only `SF` and `end_of_record`. No `FNF`, `FNH`, `BRF`, `BRH`, `LH`, or `LF`.
  - Zel always emits `SF`, all `FN` lines (TreeMap by function name), all `FNDA` lines (long counts), `FNF`, `FNH`, then `BRDA` or legacy `BA`, `BRF`/`BRH` only when the branch count is greater than 0, `DA` lines, `LH`, `LF`, `end_of_record`. Function names are copied from the input tracefile. Bazel builds `clsName::methodName` plus a space and the JVM descriptor, and uses JaCoCo’s method-counter covered count (hit or miss), not an invocation total.
  - Bazel `DA:<line>,<0|1>` is written only for lines from `getFirstLine()` through `getLastLine()` whose instruction counter total is nonzero. Covered count greater than 0 becomes 1. No checksum. Zel writes `DA:<line>,<full long count>` for every stored line and does not emit the optional checksum its parser accepts.
  - Bazel `BRDA:<line>,0,<branchIdx>,<1|0|->`: block number is always 0, branch index is 0..n-1, taken is 1 or 0 when the line’s executed bit is set, and `-` for every branch on a line that was not executed. Emitted only when `branchCoverageDetail` contains that class binary name. Zel keeps the original block and branch ids and writes the real taken count or `-`, sorted by `BranchCoverageKey`.
  - Bazel never writes `BA`. Zel `LcovPrinter` writes `BA:<line>,<0|1|2>` instead of `BRDA` when `outputLegacyBranches` is true (0 never evaluated, 1 evaluated but not taken, 2 taken). The shell merger accepts `--legacy_branches` and still prints only `BRDA`.
  - Bazel paths are JaCoCo `packageName + "/" + sourceFileName` (forward slash; the default package becomes `/File.java`). With the exec-path set, a class is kept only if some entry is `execPath///` plus an exact match on the `/`-prefixed class path, or the exec path equals the class path or ends with that `/`-prefixed suffix. Otherwise the entry is dropped. The zero-arg constructor emits the raw class path. Zel prints `sourceFileName()` unchanged and does not rewrite separators. Manifest, regex, and `sources_to_replace` filtering happen before `LcovPrinter`.
  - Charset and newlines: this formatter does not choose a charset. Its caller `JacocoCoverageRunner` opens the `PrintWriter` with UTF-8, create and append. `printf` formats use a literal `\n`. `end_of_record` uses `println`, so that one line uses `System.lineSeparator()`. Zel `LcovPrinter` wraps the stream in a UTF-8 `OutputStreamWriter` and forces `'\n'` on every line, including `end_of_record`. The shell merger also prints `\n` but does not pin UTF-8.

### `src/java_tools/junitrunner/java/com/google/testing/coverage/MethodProbesMapper.java`
- Bazel lines: 474
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no JaCoCo method-probes mapper. Its coverage path is LCOV collect/merge (`collect_coverage.sh`, `CoverageOutputGenerator` line and branch records). A fixture path list names this file; that is not an implementation. Bazel builds a per-method control-flow graph and returns `TreeMap<Integer, BranchExp>` from probe ids to line branch expressions.
  - Probe ids are Java `int` (32-bit). This file does no `byte` arithmetic, so signed-byte `0x80..0xFF` does not apply. `ProbeExp` stores that `int` and later evaluates `probes[probeId]` on a `boolean[]` (0-based). Line numbers are `int`; `currentLine` starts at `-1` until `visitLineNumber`.
  - Every real instruction becomes an `Instruction`. The next instruction is linked as branch index 0 (fall-through) unless `visitLabel` sees `LabelInfo.isSuccessor == false` or `visitProbe` runs, both of which set `lastInstruction = null`.
  - A conditional jump records branch index 1 to the target label, wired in `visitEnd`. `visitJumpInsnWithProbe` puts `ProbeExp(probeId)` on branch 1 and records no jump. `visitInsnWithProbe` puts the probe on branch 0 of the new instruction. `visitProbe` (merge successor) puts the probe on branch 0 of the previous instruction, then clears the predecessor.
  - Unprobed `tableswitch`/`lookupswitch`: default is branch 0; each later label increments the branch index only when `LabelInfo.isDone` is false, so duplicate targets do not take a new slot. Probed switches increment the index for every label, including duplicates, then skip already-done labels; `LabelInfo.NO_PROBE` becomes a jump, otherwise `addProbe(id, branch)` with no direct edge. Duplicate case labels therefore consume an index only on the probed path.
  - `visitEnd` wires jumps, then `wireBranchPredecessors` walks the single predecessor chain (iterative) and copies `branchExp` into that predecessor slot, stopping when the predecessor already has branches. `setBranchAtIndex` grows the list with `NullExp` holes so the index is the slot.
  - JaCoCo filter output: `ignore` is inclusive of both endpoints. `merge` is union-find with path compression and no rank; the root zips the other node’s branches (`BranchExp.zip`, pairwise) and the non-root instruction is dropped. `replaceBranches` sorts the unordered target set by 0-based encounter index (`instructionNodeIndex`) and replaces the source expression with that list. Union-find application order follows `HashMap` key order and is not sorted.
  - The line map keeps only non-ignored instructions with `logicalBranches > 1` whose expression is a `BranchExp`. Same-line expressions are `concatenate`d (branch lists appended, holes kept). A branched instruction whose expression is missing or not a `BranchExp` is dropped with no error. `TreeMap` orders keys by line.

### `src/java_tools/junitrunner/java/com/google/testing/coverage/NullExp.java`
- Bazel lines: 28
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines `final class NullExp implements CovExp`: a branch-coverage expression whose `eval(boolean[] probes)` always returns `false` and ignores the probe array.
  - A single shared instance, `public static final NullExp NULL_EXP`, is the only object; the constructor is private.
  - Equality is identity (`Object.equals` / `hashCode` are not overridden). `BranchExp` uses `exp.equals(NullExp.NULL_EXP)` to treat that singleton as an unset slot: `hasBranches()` and `getBranches()` skip it, and `extendBranches` pads with `NULL_EXP`.
  - Zel has no `CovExp`, `NullExp`, or probe-expression tree. Its coverage code (LCOV collectors and `CoverageOutputGenerator` types such as `BranchCoverage`) does not evaluate JaCoCo probes or use a null-expression sentinel.

### `src/java_tools/junitrunner/java/com/google/testing/coverage/ProbeExp.java`
- Bazel lines: 29
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel stores one probe id in a private final Java `int` (32-bit signed) set by `ProbeExp(int id)` with no range check, clamp, or unsigned conversion. Zel has no probe-expression type that stores a probe id.
  - `eval(boolean[] probes)` returns false when `probes` is null, and otherwise returns `probes[probeId]` with no length check, mask, or modulo. A negative id or an id `>= probes.length` throws `ArrayIndexOutOfBoundsException`. Zel does not evaluate a boolean probe array by id.
  - The index is a signed 32-bit `int`, so the largest representable id is `Integer.MAX_VALUE` (2^31−1). `Integer.MIN_VALUE` and any other negative value are invalid indexes, not unsigned ids. Zel has no matching signed-index lookup.
  - The result is the raw `boolean` at that slot (true means that probe was hit). There is no cache, copy, or bit-field packing in this class. Zel’s coverage code (`CoverageOutputGenerator` branch maps, `collect_coverage.sh`) records already-resolved line/branch hits and only mentions `JacocoCoverageRunner`; it does not implement this probe lookup.

### `src/java_tools/junitrunner/java/com/google/testing/junit/junit4/runner/AndFilter.java`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no JUnit runner filter. Vendored rules only name `com.google.testing.junit.runner.BazelTestRunner`; they do not implement this class.
  - Bazel’s `@Deprecated` `AndFilter` extends `org.junit.runner.manipulation.Filter` and stores exactly two `Filter`s.
  - The constructor throws `NullPointerException` (no message) if either argument is null.
  - There is no filter list and no empty-list case: zero filters or more than two cannot be combined here.
  - `shouldRun` returns `filter1.shouldRun(description) && filter2.shouldRun(description)` (short-circuit AND on the same `Description`). It does not match display name, class, or method itself.
  - `describe` returns `String.format("%s && %s", filter1.describe(), filter2.describe())`.

### `src/java_tools/junitrunner/java/com/google/testing/junit/junit4/runner/DynamicTestException.java`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Checked exception: `DynamicTestException extends Exception` (not a `RuntimeException`).
  - One constructor, `DynamicTestException(Description test, Throwable cause)`, calls `super(cause)`. With a non-null cause the detail message is `cause.toString()` and that throwable is the cause; a null cause yields a null message and a null cause. There is no separate message argument.
  - If `test` is not a test (`!test.isTest()`), the constructor throws `IllegalArgumentException` with no message and does not store the description.
  - The JUnit `Description` is stored and returned by final `getTest()`.
  - Class Javadoc names the type `DynamicTestFailureException` while the class is `DynamicTestException`.
  - Zel has no `java_tools` junit runner and no `DynamicTestException`, so none of this failure type, cause wiring, test-description check, or `getTest()` accessor exists.

### `src/java_tools/junitrunner/java/com/google/testing/junit/junit4/runner/Filters.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Deprecated final utility with only `and` and `apply`. It does not define regex, test-size, or shard filter factories, and it does not compile a pattern (`java.util.regex` or otherwise). Include/exclude regex lives in the sibling `RegExTestCaseFilter` (`Pattern.compile` plus `Matcher.find` on `className#methodName`, suites always run). Shard filters live under `runner/sharding/`.
  - `and(delegate1, delegate2)` returns the other filter unchanged when either argument is `Filter.ALL`; otherwise it builds `AndFilter`. `shouldRun` is short-circuit `filter1 && filter2`. A null delegate throws `NullPointerException`. `describe()` is `"%s && %s"`.
  - `apply(request, filter)` wraps the filter in `SuiteTrimmingFilter`, calls `filter.apply` on `request.getRunner()` (mutating that runner; the input request must not be reused), and returns `Request.runner(runner)`. JUnit’s `Filter.apply` throws `NoTestsRemainException` when nothing remains.
  - `SuiteTrimmingFilter` drops empty suites: the delegate must accept the description, a test that passes is kept, and a non-test stays only if some descendant `shouldRun`s. `describe()` forwards to the delegate. A null delegate throws `NullPointerException`.
  - Zel has no JUnit runner and no `Filter` composition or empty-suite trimming. `zel-cursor/crates/zel-exec/src/test_runner.rs` only exports `TESTBRIDGE_TEST_ONLY` (including an explicit empty `--test_filter=`), `TEST_SIZE`, and shard env (`TEST_TOTAL_SHARDS`, `TEST_SHARD_INDEX`, `TEST_SHARD_STATUS_FILE`). It does not compile the filter string.

### `src/java_tools/junitrunner/java/com/google/testing/junit/junit4/runner/RegExTestCaseFilter.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no JUnit `Filter` and no regex test-case matcher. The only related behavior is copying `--test_filter` unchanged into `TESTBRIDGE_TEST_ONLY` (`/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`); the string is never compiled or applied to a test name. There is no `--test_exclude_filter`.
  - Bazel matches the subject with `java.util.regex.Pattern.compile(regex)` at flags `0`, then `Matcher.find()` (unanchored substring). That is not a full match (`matches()`) and not a prefix match (`lookingAt()`). An empty pattern matches every non-suite test. `^` and `$` bind to the whole string because `MULTILINE` is off.
  - Default flags are case-sensitive and not Unicode-class: no `CASE_INSENSITIVE`, `UNICODE_CASE`, `UNICODE_CHARACTER_CLASS`, `DOTALL`, `UNIX_LINES`, `COMMENTS`, `CANON_EQ`, or `LITERAL`. So `NAMETOMATCH` does not match `nameToMatch`, and `\d`, `\w`, `\s`, and `\b` stay ASCII. Embedded flags such as `(?i)` and `(?U)` still work because `Pattern.compile` honors them. No canonical equivalence.
  - The matched string is `className#methodName` when both `getClassName()` and `getMethodName()` are non-null and non-blank after `trim()`. Otherwise it is `Description.getDisplayName()` (null method, blank method, or a display name like `java.lang.Object.hashCode()`).
  - `shouldRun` returns true for every suite (`isSuite()`), including under `exclude`, so children can still be filtered. `include` keeps a match; `exclude` inverts it. `describe()` is `RegEx[pattern]` or `NOT RegEx[pattern]`.
  - Java-vs-Rust regex does not apply: Zel never runs this pattern through the Rust `regex` crate, so differences such as possessive quantifiers, variable-length lookbehind, and Unicode property names are not exercised. The missing piece is the Java `Pattern.find()` filter itself.

### `src/java_tools/junitrunner/java/com/google/testing/junit/junit4/runner/RunNotifierWrapper.java`
- Bazel lines: 101
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines an abstract `RunNotifier` subclass that stores a delegate `RunNotifier` and exposes it through `getDelegate()` so subclasses can change selected behavior.
  - It forwards listener registration unchanged: `addFirstListener`, `addListener`, and `removeListener`.
  - It forwards the full JUnit 4 run lifecycle unchanged: `fireTestRunStarted`, `fireTestStarted` (declared to throw `StoppedByUserException`), `fireTestIgnored`, `fireTestAssumptionFailed`, `fireTestFailure`, `fireTestFinished`, and `fireTestRunFinished`.
  - Failure and assumption-failure events are passed through with no extra aggregation, suppression, or rewriting in this class.
  - `pleaseStop()` is forwarded to the delegate with no local stop flag.
  - This class does not special-case dynamic tests; there is no dynamic-test event method here.
  - Zel has no JUnit notifier wrapper. `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` only runs test processes (setup script, shards, retries, timeouts, XML summaries) and does not forward JUnit listener, start, ignore, assumption-failure, failure, finish, or stop events.

### `src/java_tools/junitrunner/java/com/google/testing/junit/junit4/runner/SuiteTrimmingFilter.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `Filters.java` does not nest a `SuiteTrimmingFilter`. `Filters.apply` constructs this same top-level class (`new SuiteTrimmingFilter(filter)`), so it is that type, not a second class.
  - Bazel’s class is a `public final` `org.junit.runner.manipulation.Filter` decorator. A null delegate throws `NullPointerException`. `describe()` returns the delegate’s description.
  - `shouldRun` returns false when the delegate rejects the description, true for a description that `isTest()`, and for a suite true only when some child recursively `shouldRun`s. A suite with no matching descendant is dropped.
  - Zel has no JUnit suite walker that drops empty suites. `--test_filter` is only copied into `TESTBRIDGE_TEST_ONLY` (`/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`), and `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` `passes_filters` selects whole targets by tag, size, timeout, and language.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/BazelTestRunner.java`
- Bazel lines: 258
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no in-JVM JUnit main. Nothing under `/workspace/zel-cursor/crates` names `BazelTestRunner`. `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` is the outer strategy: it runs `test-setup.sh` plus the test binary, sets env, and maps the process exit. Vendored `rules_java` still launches `com.google.testing.junit.runner.BazelTestRunner` with `-Dbazel.test_suite=`, but Zel does not implement that class.
  - `bazel.test_suite` is required. `System.getProperty` returns Java `null` when unset (only that is "missing"). The missing-property path prints the usage text and `System.exit(2)` immediately, with no "BazelTestRunner exiting…" banner, no 5s stack dump, and no non-daemon wait. The empty string is present: it is not this error. Zel has no suite property and no exit 2 for a missing class.
  - The suite name is passed to `Class.forName(name)` with no trim (initializing load on this class's loader; binary name, inner classes use `$`). `ClassNotFoundException` yields null. Exit 2 and `Class not found: [%s]` happen only when `args.length == 0`. Non-empty args continue with a null suite into `JUnit4Bazel`. `ExceptionInInitializerError` and `NoClassDefFoundError` are not `ClassNotFoundException`; they hit `catch (Throwable)` and become exit 1, or 137 when that throwable itself is an `OutOfMemoryError`.
  - This main does not parse filters. It forwards the Java `String[]` unchanged into `new Config(args)`, which parses `System.getenv()` plus `--test_filter` and `--test_exclude_filter` (`--name=value` with the first `=`, and `indexOf('=') > 0`, or the next argv). A flag with no expression throws `RuntimeException` and becomes exit 1, not 2. `TESTBRIDGE_TEST_ONLY` is copied only when the argv flag is still Java `null`; an explicit `--test_filter=` (empty string) wins. `TESTBRIDGE_TEST_RUNNER_FAIL_FAST` is true only for the string `"1"`. The runner then `Pattern.compile`s those strings and uses `Matcher.find` (unanchored substring) on `class#method`. An empty pattern still counts as a filter, so with sharding `allowNoTests` is set even though `find` matches every name. A bad pattern is a `PatternSyntaxException` and exits 1. Zel only copies `--test_filter` into `TESTBRIDGE_TEST_ONLY` (including an explicit empty value) and appends `spec.args` as-is. It never parses `--test_exclude_filter`, never treats `"1"` as fail-fast, and never applies a Java regex to JUnit descriptions.
  - Exit codes in this file are 0 (`Result.wasSuccessful()`), 1 (any other result, or any non-OOM `Throwable`), 2 (setup failure above), and 137 if the caught throwable is an `OutOfMemoryError` or any `Failure.getException()` is an `OutOfMemoryError`. That check does not walk causes. The comment says `OutOfMemoryException`; the code checks `OutOfMemoryError`. Zel treats pass as exit 0 and not timeout and not a sharding miss; a spawn error is forced to 1; a timeout is forced to 142; a missing shard-status file is forced to 1 and `TestStatus::NoStatus`. `format_exit_status` treats `128 < code <= 192` as a signal, so this runner's 137 is displayed as "Killed" (SIGKILL), not as OOM. Exit 2 is "Exit 2" and still `TestStatus::Failed`.
  - After a run that returns (including class-not-found exit 2 and a thrown `Throwable`), this main always prints `BazelTestRunner exiting with a return value of %d`, a shutdown-hook notice, then starts a daemon named `BazelTestRunner: Print stack traces if JVM exit hangs`. That daemon sleeps 5 seconds with a `nanoTime` deadline (`TimeUnit.NANOSECONDS.sleep`; a negative delay is 0; the interrupt flag is restored) and dumps non-daemon threads, then daemon threads, then deadlocks via `StackTraces.printAll` to the `System.err` captured before the run. If `Boolean.getBoolean("bazel.test_runner.await_non_daemon_threads")` is true (the property must equal `"true"` ignoring case; `"1"` is false), a shutdown hook spins on `Thread.yield()` until every alive non-daemon thread other than main and the hook is gone, with no timeout. It then prints `-- JVM shutdown starting at yyyy-MM-dd HH:mm:ss --` using `SimpleDateFormat` with `Locale.getDefault()` and the JVM default time zone, and `System.exit(code)` (hooks run; it does not `halt`). Zel records the spawn exit and writes logs. It does not wait out non-daemon threads, dump stacks after 5s, or print that banner.
  - This file sets no charset. `printStackTrace`, `printf`, and `println` use `System.err`'s charset (`Charset.defaultCharset()`, not necessarily UTF-8). `%n` is the platform separator. Zel concatenates stdout and stderr as raw bytes and, when it synthesizes XML or strips `Action timed out after `, decodes with `String::from_utf8_lossy` (invalid bytes become U+FFFD). A default-charset stack dump from this runner is therefore corrupted in that XML.
  - Filters, sharding, and XML are not implemented in this file. `runTestsInSuite` only builds `JUnit4Bazel` and calls `runner().run()`, which is what applies the include/exclude regex, the in-process sharding filter, `TEST_PREMATURE_EXIT_FILE`, XML output from `XML_OUTPUT_FILE`, and `System.exit` detection (`halt(121)`). The class comment mentions a security manager, capturing `System.out`/`System.err`, and trapping SIGTERM; this source does none of those. Zel's process strategy sets `XML_OUTPUT_FILE`, `TEST_TOTAL_SHARDS`, `TEST_SHARD_INDEX`, `TEST_SHARD_STATUS_FILE` when `shard_count > 0`, `TESTBRIDGE_TEST_ONLY`, and `TEST_PREMATURE_EXIT_FILE`, and if the process never writes `test.xml` it synthesizes one UTF-8 testcase. It does not select JUnit methods, hash-shard descriptions inside one JVM, or write this runner's per-test XML. There is no path handling in this file; Zel's `Path::to_str().unwrap()` panic on non-UTF-8 paths does not correspond to anything here.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/internal/SignalHandlers.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel installs an in-process handler through `sun.misc.Signal.handle`, which replaces the JVM handler and returns the previous one. `installHandler` wraps that so the new handler runs first and then the saved handler, so repeated installs form a newest-first chain. Zel's test runner (`/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`) never installs a handler.
  - The class itself takes whatever `Signal` the caller passes. The JUnit runner installs only `new Signal("TERM")` from `testRunStarted` (stack-trace dump and XML flush are separate installs). Zel does not catch `SIGTERM` inside the test process.
  - There is no restore or uninstall. Handlers stay for the life of the JVM. Zel's runner has no install and no restore.
  - No thread and no shutdown hook in this class. `sun.misc.Signal` runs the handler on the HotSpot Signal Dispatcher thread, so the handler may do ordinary Java work (print all thread stacks, cancel the run, write JUnit XML). A Rust `libc::signal` handler would be restricted to async-signal-safe calls and would not get that dispatcher thread. Zel's test runner does neither.
  - If the new handler throws, the previous handler is skipped. The previous handler is stored in an `AtomicReference` only after `Signal.handle` returns, so a signal in that window also skips it. Zel has no chain.
  - `Signal.handle` rejects signals the VM will not catch (typically `KILL` and `STOP`) and uses names without the `SIG` prefix. Zel's runner only formats an already-finished exit status (`format_exit_status`); it does not dump threads or flush partial test XML when the process is signaled.
  - The shell wrapper `/workspace/zel-cursor/crates/zel-exec/resources/test-setup.sh` is a different layer: it traps every signal except `SIGCHLD`, forwards it to the child process group, treats `SIGTERM` as a timeout, waits up to 15 seconds, then `SIGKILL`, then `trap -`. It does not chain in-process handlers or write stacks or JUnit XML from the test process. Zel's `signal()` helper in `/workspace/zel-cursor/crates/zel-server/src/sys.rs` replaces the disposition with one async-signal-safe store and is used by the client and daemon, not the test runner.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/internal/StackTraces.java`
- Bazel lines: 148
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel dumps every JVM thread (`ThreadMXBean.dumpAllThreads(true, true)` plus `Thread.getAllStackTraces` only for the daemon bit). Threads that appear only in `ThreadInfo` are dropped. Non-daemon threads are printed first, then daemons. Zel has no junit runner and no thread dump.
  - Each thread line is `"name" Id=<id> <state>`, then optional ` on <lock>`, ` owned by "<owner>" Id=<id>`, `(suspended)`, `(in native)`, `(daemon)`. Frames are Java `StackTraceElement.toString()` lines prefixed with `\tat ` (`class.method(File.java:line)`, `Native Method`, or `Unknown Source`). Zel never formats Java frames. Its test-failure text (`/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`) is Rust `FAIL`/`FAILED`/`TIMEOUT`/`FLAKY` summaries plus the raw test log, not a stack printer. Starlark traces in `/workspace/zel-cursor/crates/zel-starlark/src/eval/thread.rs` are `File "…", line N, column C` evaluation traces, not JVM frames.
  - No frame cap: the comment says this is `ThreadInfo.toString()` without the JDK `MAX_FRAMES` limit (8), so every frame is kept, plus frame-0 `blocked on` / `waiting on`, per-frame `locked` monitors, and locked synchronizers. Zel does not truncate or filter stack frames. It skips an entire test log when it is larger than `--max_test_output_bytes`.
  - Deadlock ids from `findDeadlockedThreads` and `findMonitorDeadlockedThreads` are printed with `Arrays.toString`. Zel has no equivalent.
  - Body lines use a hard-coded `'\n'` (LF). Header and footer (`Starting full thread dump ...`, deadlock lines, `Done full thread dump.`) use `println`, so they also append the `PrintStream` line separator. The embedded `\n` inside `"Starting full thread dump ...\n"` adds an extra blank line. Charset is whatever the caller’s `PrintStream` already uses; this class does not set one. Zel summaries are UTF-8 with a single `b'\n'`. The test log is copied as raw bytes (after stripping the test-setup `-----` header), with `from_utf8_lossy` only when rewriting paths for `--print_relative_test_log_paths`.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/internal/SystemExitDetectingShutdownHook.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel builds a thread named `SystemExitDetectingShutdownHook` and leaves registration to the caller. `JUnit4Runner` adds it with `Runtime.addShutdownHook` only after the premature-exit file is created, and removes it in `finally` after a normal return so a finished test does not trip it. The hook itself sets no priority. The JVM runs shutdown hooks concurrently, so this hook has no defined order against other hooks. Zel's test runner (`/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`) never installs a JVM shutdown hook.
  - On run, Bazel walks `Thread.getAllStackTraces()`. In each thread it scans from the top frame until it sees `java.lang.Runtime.exit`, then keeps that frame and every older caller. Frames above `Runtime.exit` are dropped. Any matching thread is treated as `System.exit` or `Runtime.exit`. Zel does not inspect live thread stacks for `Runtime.exit`.
  - For each matching thread Bazel prints, on the given `PrintStream`, a leading blank line, `System.exit or Runtime.exit was called!`, then one line per kept frame: ` at %s.%s(%s:%d)` (class, method, file, line). Zel prints no such message.
  - If any thread matched, Bazel calls `Runtime.halt(121)` rather than `exit`, so shutdown hooks are not re-entered, and uses 121 as the distinctive status. Zel only records the child process exit code, maps codes 129–192 to signal names in `format_exit_status`, and may write XML `exited with error code N`. It never forces status 121.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/internal/junit4/CancellableRequestFactory.java`
- Bazel lines: 157
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel wraps one JUnit `Request` in a `MemoizingRequest` whose runner is a `CancellableRunner`. A second `createRequest` throws `IllegalStateException("a request was already created")`. Zel has no JUnit `Request` or `Runner` and no single-use cancellable wrapper. Tests are subprocesses (`/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`).
  - Stop flag: an `AtomicReference<CancellationRequest>` starts at `NOT_REQUESTED`. `cancelRun()` CAS-es it to `HARD_STOP`; `cancelRunOrderly()` CAS-es it to `ORDERLY_STOP`. Only the first request wins. If a notifier is already installed, that CAS also calls `pleaseStop()`. Zel has no hard-versus-orderly in-process stop and no CAS that latches the first cancel.
  - `ThreadSafeRunNotifier` keeps a `volatile boolean stopRequested`. `pleaseStop()` only sets that flag and does not call the delegate notifier. The next `fireTestStarted` throws `StoppedByUserException` before the delegate is notified, so later tests are not started. Zel does not stop a suite between cases.
  - If cancel arrives before `run`, the runner still installs the notifier and calls `pleaseStop()`. `ORDERLY_STOP` then returns without `delegate.run`. `HARD_STOP` still enters `delegate.run` so the notifier can throw. Zel does not have that split.
  - `HARD_STOP` wraps `StoppedByUserException` in `RuntimeException("Test run interrupted")`. `ORDERLY_STOP` prints the stack and returns. Any other `StoppedByUserException` is rethrown. Zel never raises or swallows that exception.
  - This class does not call `Thread.interrupt()` and has no timeout. "Interrupted" is only the hard-stop message. Callers outside the file use the two stops: a signal handler calls `cancelRun()` while writing JUnit XML, and the fail-fast listener calls `cancelRunOrderly()` on the first failure.
  - Zel cancels by killing a process group. `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs` sends `SIGTERM` on timeout, then `SIGKILL` after `kill_delay`; `global_cancel` and `CancelFlag` send `SIGKILL` immediately, and a spawn is refused if global cancel is already set. `/workspace/zel-cursor/crates/zel-exec/resources/test-setup.sh` traps signals, treats `SIGTERM` as timeout, forwards it to the child group, waits up to 15 seconds, then `SIGKILL`. `test_runner.rs` records that as `TestStatus::Timeout`. None of that is a cooperative JUnit stop flag.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/internal/junit4/JUnit4TestNameListener.java`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel `JUnit4TestNameListener` is a JUnit4 `RunListener` that records the current test as a `Description` on a `ThreadLocal`. On `testRunStarted` it installs a `TestNameProvider` (via `SettableCurrentRunningTest.setGlobalTestNameProvider`) whose `get()` returns that thread’s `Description`. `testStarted` stores the description; `testFinished` sets it to `null`. The description is stored as-is: this file does not index, substring, or rewrite the name.
  - Zel has no JUnit runner and no per-thread current-test listener. Fallback `test.xml` in `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`synthesize_xml`) names both `<testsuite>` and `<testcase>` from the label after the last `:`, with `classname=""`. Tests that write their own `test.xml` keep those names. That is label slicing for a generated report, not a live JUnit `Description`.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/internal/junit4/JUnit4TestStackTraceListener.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel installs this JUnit 4 `RunListener` at `testRunStarted` and registers a `sun.misc.Signal` handler for `TERM` through `SignalHandlers`. It does not hook `testFailure` or any other failure callback.
  - On `SIGTERM` the handler writes `Dumping stack traces for all threads` and calls `StackTraces.printAll` on the injected stderr `PrintStream`. That dump is a full JVM thread dump (non-daemon threads first, then daemons, with lock info and deadlock ids), not the stack of the failing test.
  - Zel has no `java_tools` JUnit runner and no in-process `SIGTERM` thread dump. `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs` sends `SIGTERM` to the whole process group on timeout, then `SIGKILL` after the kill delay, and records the death as a signal. It never asks the child to print stacks.
  - Zel test-failure handling in `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs` only parses a JUnit `test.xml` the test already wrote. A `<failure>` or `<error>` element sets the case status to failed or error and is then discarded; the body text (the trace) is not stored.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/internal/junit4/JUnit4TestXmlListener.java`
- Bazel lines: 123
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`synthesize_xml`, the writer Zel actually runs). `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/test/generate-xml.sh` is the shell fallback script and is not what the runner writes.
- Verdict: partial
- Differences:
  - This class is a JUnit `RunListener`. It does not format XML itself. `testRunFinished` and the `SIGTERM` handler both call `TestSuiteModel.writeAsXml`, which emits Ant-JUnit XML through `AntXmlResultWriter` and `XmlWriter`. Zel has no JUnit listener, no suite model, and no per-method XML. It writes one synthetic case only when the test left no `test.xml`.
  - Document header and charset: Bazel writes `<?xml version='1.0' encoding='UTF-8'?>` (single quotes) through a UTF-8 `OutputStreamWriter`, then `line.separator` (LF on Linux) and two-space indents. Empty elements are ` />` on the same line. A non-empty end tag is glued to the character data so indentation is not part of the text. `close()` adds a final newline. Zel writes `<?xml version="1.0" encoding="UTF-8"?>\n` and always `\n`, with paired tags and no self-close. Both are UTF-8 with no BOM. The quote style and newlines differ on disk.
  - Elements Bazel emits and Zel does not: a `testsuites` root, then one `testsuite` per suite. Nested suites that still have children are written again as following siblings, not as nested elements, and parent failures are copied onto each descendant `testcase`. Each suite contains `properties` / `property`, the leaf `testcase`s, an empty `system-out`, and an empty `system-err`. A `failure` element carries the stack trace from `Throwable.printStackTrace`. `SKIPPED` and `SUPPRESSED` add an empty `skipped`. `FILTERED` cases are omitted. Dynamic tests (a case whose result has children) are rewritten as their own `testsuite` of leaf cases. Zel is always one `testsuites` / one `testsuite` / one `testcase`, then a `system-out` sibling after `</testcase>`. There is no `properties`, `property`, `failure`, `skipped`, `system-err`, nesting, or dynamic-test suite.
  - Attributes Bazel writes and Zel does not: `testsuite` gets `name` (JUnit display name), `timestamp`, `hostname="localhost"`, `tests`, `failures`, `errors="0"` (fixed; JUnit failures are not errors), `time`, `package=""`, and `id` (0, 1, 2, … across those sibling suites). `property` gets `name` and `value`. `testcase` gets `name` (method name, or the class name when `getMethodName()` is null, with `classname` then taken from the parent display name), `classname`, and `time`. `failure` gets `message` (empty string when the throwable message is null) and `type` (exception class name). Zel’s suite is `name` = label after the last `:`, `tests="1"`, `failures="0"`, `errors` 0 or 1. Its case adds `status="run"`, `duration="0"`, `time="0"`, `classname=""`. A bad exit is an empty `<error message="...">` with no `type`. Bazel does not write `status` or `duration`. Zel does not write `timestamp`, `hostname`, `package`, `id`, or `properties`.
  - Counts and outcomes: Bazel `tests` / `failures` are summed from the real cases. A case with any recorded throwable counts every one of its tests as failed. Assumption failure becomes `testSkipped`. `testIgnored` on a suite with no `@Ignore` is treated as an assumption failure (`testSkipped`); any other ignore is `testSuppressed`. Both statuses render as `<skipped/>`. `SIGTERM` calls `cancelRun()`, then `testRunInterrupted()`: a started case gains `Exception("Test interrupted")`, a not-yet-started pending case gains `Exception("Test cancelled")` and status `CANCELLED`. Those cases are still written. A normal finish does not call `testRunInterrupted`, so pending cases stay `CANCELLED` with no failure element and still appear. Zel never sees JUnit events. `errors="1"` only when the process exit is nonzero or the spawn timed out. The message is `timed out` or `exited with error code N`. `failures` stays 0. No skip, no cancel, no stack trace.
  - Timing: Bazel `time` is monotonic duration in milliseconds divided by `1000.0`, formatted with `String.valueOf` (`0.0` when there is no interval, so 1 ms is `0.001`, and extreme values can be scientific notation). Suite `time` is the span covering the children (`TestInterval.around`), not the sum of case times and not the process wall time. `timestamp` is the start instant from `DateTimeFormatter.appendInstant(3)`, UTC with exactly three fraction digits (`2016-08-20T16:15:34.000Z`), or `""` when there is no interval. Zel takes `duration_ms` and discards it (`let _ = duration_ms`), so `duration` and `time` are the characters `0`. The integer seconds passed in the fake `generate-xml.sh` action log are not written into the file. The shell script would have written that integer into both `duration` and `time`, and would suffix the name with `_shard_N/M`. `synthesize_xml` does neither.
  - Escaping changes the bytes, and for some characters the parsed value. Bazel string attributes go through `XmlEscapers.xmlAttributeEscaper` and are wrapped in single quotes. Content (`failure` text) goes through `xmlContentEscaper`. Both replace `&`, `<`, and `>` with `&amp;`, `&lt;`, and `&gt;`. Attributes also replace `'` with `&apos;`, `"` with `&quot;`, tab with `&#x9;`, LF with `&#xA;`, and CR with `&#xD;`. Content leaves tab, LF, CR, quotes, and apostrophes raw. ASCII controls `U+0000`–`U+001F` other than tab, LF, and CR, plus `U+FFFE` and `U+FFFF`, become `U+FFFD`. Integer and double attributes are not passed through the string escaper. Zel’s closure only does `&`, `<`, `>`, `"` and the attributes use double quotes, so an apostrophe stays a raw `'`. A tab, LF, or CR inside a Zel attribute is a literal whitespace character, which an XML parser normalizes to a space. Bazel’s `&#x9;` / `&#xA;` / `&#xD;` survive as tab, newline, and CR. Zel leaves the other C0 controls and `U+FFFE` / `U+FFFF` in the document, which XML 1.0 forbids. Bazel’s replacement character makes those strings legal. The failure stack trace is content, so Bazel keeps its newlines and escapes `]]> ` as `]]&gt;`. Zel has no stack text.
  - The log is only in Zel’s document. It is UTF-8 lossy (`U+FFFD` for bad bytes) inside `CDATA`, after one trailing `\n` is stripped, under the fixed line `Generated test.log (if the file is not UTF-8, then this may be unreadable):`. `]]>` is replaced with `]]>]]&gt;<![CDATA[`, which inserts an extra `]]>` into the parsed log. The shell script’s split is `]]>]]<![CDATA[>` and keeps the original text, and it turns illegal UTF-8 into `?` rather than `U+FFFD`. Bazel’s listener writes empty `system-out` and `system-err` and does not embed the log.
  - Flush behavior: `writeAsXml` is once-only (`AtomicBoolean`). The `SIGTERM` handler prints `Received SIGTERM; writing test XML` to the listener’s stderr, cancels the run, marks the model interrupted, writes, then prints `Done writing test XML`, or `Could not write test XML` plus the stack if that throws. A later normal finish does not write a second document. Zel does not install a test-process `SIGTERM` handler and does not flush a partial JUnit report. It writes the synthetic file only when `test.xml` is absent after the spawn.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/internal/junit4/MemoizingRequest.java`
- Bazel lines: 50
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `/workspace/bazel/src/java_tools/junitrunner/java/com/google/testing/junit/runner/internal/junit4/MemoizingRequest.java` subclasses JUnit `Request`, stores the delegate, and does no work in the constructor. It caches one `Runner`: the first successful `getRunner()` calls `createRunner` (default `delegate.getRunner()`) and later calls return that same instance.
  - Thread safety is the instance lock on `getRunner()` (`synchronized`). `runnerDelegate` starts null and is written only under that lock. If `createRunner` throws, the field stays null and the next call retries.
  - `createRunner` is a package-private hook so a subclass can replace the runner and still get the one-shot cache (used by `CancellableRequestFactory` and `JUnit4RunnerBaseModule.provideRequest`, which memoizes because the runner is requested twice and may be expensive).
  - Zel has no JUnit request or runner cache. `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` runs tests through `test-setup.sh` and does not memoize a JUnit `Runner`.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/internal/junit4/SettableCurrentRunningTest.java`
- Bazel lines: 25
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines an abstract `CurrentRunningTest` subclass whose only method is `setGlobalTestNameProvider(TestNameProvider)`, so the JUnit4 runner can install a provider into the protected static `testNameProvider` after construction. `JUnit4RunnerModule.provideCurrentRunningTest` is the anonymous implementation that assigns that field.
  - Once installed, `CurrentRunningTest.get()` returns that provider’s `org.junit.runner.Description` for the calling thread, and returns null when no provider was installed (test not run by this runner).
  - `JUnit4TestNameListener` is the caller: on `testRunStarted` it installs a provider that reads a `ThreadLocal<Description>`; `testStarted` stores the description and `testFinished` clears it, so each thread sees its own current test.
  - Zel has no JUnit runner, `TestNameProvider`, or thread-local current-test description. `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` only records process-level results (status, shards, attempts, logs) for a spawned test binary.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/junit4/JUnit4Bazel.java`
- Bazel lines: 135
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`
- Verdict: partial
- Differences:
  - This file is the in-JVM assembler for one JUnit 4 suite. `builder().suiteClass(Class).config(JUnit4InstanceModules.Config).build()` requires both pieces (`IllegalStateException` if either is missing; `NullPointerException` on a null suite, config, or builder), then `JUnit4RunnerModule` is built from `config.options()` (`JUnit4Options.parse` of the process environment plus argv). Zel has no Java runner, suite class, or option parser. It only launches a test binary through `test-setup.sh`.
  - Options it wires: `--test_filter` and `--test_exclude_filter` as `--name=value` or `--name <value>` (a bare flag with no value throws `RuntimeException`); `TESTBRIDGE_TEST_ONLY` fills the include filter only when `--test_filter` was not passed; `TESTBRIDGE_TEST_RUNNER_FAIL_FAST=1` turns on fail-fast. `JUnit4Config` reads `XML_OUTPUT_FILE` when no path was injected, and `com.google.testing.junit.runner.apiVersion` must be `1` or the run throws `IllegalStateException`. Zel copies `--test_filter` (including an explicit empty value) into `TESTBRIDGE_TEST_ONLY` and never parses `--test_exclude_filter`. It stores `test_runner_fail_fast` on the test action and does not export `TESTBRIDGE_TEST_RUNNER_FAIL_FAST`.
  - Sharding it wires: `ShardingEnvironment` plus `ShardingFilters` with the default `ROUND_ROBIN` strategy. Building the memoized `TestSuiteModel` (once, via `MemoizingSupplier`) treats sharding as on when `TEST_TOTAL_SHARDS` is set, collects JUnit `Description`s, creates `TEST_SHARD_STATUS_FILE`, and filters with round-robin, or with hash / a custom `ShardingFilterFactory` class when `-Dtest.sharding.strategy` is set. Index defaults to 0 and total shards to 1 when those variables are absent. Zel only sets `TEST_TOTAL_SHARDS`, `TEST_SHARD_INDEX`, and `TEST_SHARD_STATUS_FILE`, and, when `shard_count > 1` and `incompatible_check_sharding_support` is on, fails the attempt if the binary did not touch that file. It does not split test methods.
  - Filters the constructed `JUnit4Runner` applies: include regex, then exclude regex (`RegExTestCaseFilter`), intersected and applied with `SuiteTrimmingFilter` so a second filter narrows the first (JUnit `ParentRunner` otherwise keeps only the last filter). No remaining tests becomes an `ErrorReportingRunner` ("No tests found matching …"). If a user filter was applied and sharding is active, a shard that then has no tests is a `NoOpRunner` that passes, so the other shards are not failed. The sharding filter is intersected and applied after that. Zel forwards one include string and does not exclude, match JUnit descriptions, or treat an empty shard as success.
  - XML it wires: `AntXmlResultWriter` on the suite model, plus a `JUnit4TestXmlListener` that writes Ant JUnit XML (`testsuites` / per-suite tests, failures, errors, hostname, time, timestamp, properties, `testcase` name and classname, `failure` message and type, `skipped`, system-out/err) to `XML_OUTPUT_FILE`. If that file cannot be opened, the listener prints the stack trace and writes to a discarding stream. The suite name is the suite class's canonical name; a top-level description that is not a suite (an `@Ignore` class) builds an empty model. Zel writes `test.xml` only when the process did not: one `<testcase>`, `failures="0"`, `errors` 0 or 1, `duration` and `time` fixed at 0, message `exited with error code N` or `timed out`, and the log in CDATA. It does not record per-method results.
  - Exit and cancellation it wires: `runner()` always passes `Collections.emptySet()` for initializers, so this facade adds none. The runner prints `JUnit4 Test Runner`, overwrites `TEST_PREMATURE_EXIT_FILE` before `JUnitCore.run`, deletes it in `finally` (a delete error is printed and does not replace the result), and installs `SystemExitDetectingShutdownHook` for that window. Fail-fast adds `StopOnFailureRunListener`, which calls `CancellableRequestFactory.cancelRunOrderly()` on the first failure. The process codes that consume this builder (`BazelTestRunner`) are 0 on `Result.wasSuccessful()`, 137 if any failure is `OutOfMemoryError`, 1 for any other test failure, and 2 for a missing `bazel.test_suite` or a runner failure. Zel sets the premature-exit path and maps the child exit code (timeout forced to 142) to PASSED, FAILED, TIMEOUT, or FLAKY. It does not cancel a JUnit request, detect `System.exit`, or special-case 137 and 2.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/junit4/JUnit4Config.java`
- Bazel lines: 140
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`build_test_env`); `bazel run` also writes the same variables in `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - System property `com.google.testing.junit.runner.apiVersion` is read from the JVM `Properties` at construction, default `"1"` when absent. `getJUnitRunnerApiVersion` applies `String.trim()` (only code points ≤ U+0020), then `Integer.parseInt`, so `"1"`, `"01"`, `"+1"`, and `" 1 "` are version 1. Anything else, including non-integers (`"3.14"`, `"I love pesto"`) and other integers (`"13"`), throws `IllegalStateException` whose text is `Unsupported JUnit Runner API version com.google.testing.junit.runner.apiVersion=<trimmed> (must be \"1\")` (literal backslashes before the quotes). Zel has no JVM system properties and no API-version gate.
  - `getXmlOutputPath` returns a non-null constructor `Path` and ignores `XML_OUTPUT_FILE`. If that path is null, it uses `System.getenv("XML_OUTPUT_FILE")`: unset returns null; any set value, including `""`, becomes `FileSystems.getDefault().getPath(...)` with no trim, normalization, or existence check. The public constructors that take a `Path` force fail-fast to false; the fail-fast constructor forces the path to null so the env var is used. Zel only writes the variable (`<attempt>/test.xml` in `build_test_env`, `{log_prefix}/test.xml` on `bazel run`) and never resolves it back with that override.
  - Include and exclude regexps and the fail-fast flag are stored as passed in and returned unchanged, including null filters. This class does not read `TESTBRIDGE_*`. Zel exports `TESTBRIDGE_TEST_ONLY` from `--test_filter`, including an explicit empty filter, but never sets `TESTBRIDGE_TEST_RUNNER_FAIL_FAST`. `--test_runner_fail_fast` is only parsed for aquery (`"true"` → fail-fast). There is no `--test_exclude_filter`, and Zel does not apply include or exclude regexps in a JUnit runner.
  - Charset: this class does not transcode. `getProperty` sees an already decoded Java string. `System.getenv` is decoded with `sun.jnu.encoding`, not `file.encoding` (the Bazel server’s `-Dfile.encoding=ISO-8859-1` does not change getenv, and this runner is a separate JVM). Zel inserts env values as Rust `String`s (UTF-8 bytes). ASCII paths match. A non-ASCII `XML_OUTPUT_FILE` is re-decoded by this Java getenv path, so a JVM whose JNU charset is not UTF-8 sees a different path than the UTF-8 string Zel wrote. Zel never reads the variable back. Putting the API version in the environment would not be seen by `getProperty`.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/junit4/JUnit4InstanceModules.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel keeps a final, uninstantiable holder (`private JUnit4InstanceModules()`) for stateful, non-static legacy Dagger modules so they can be named on the component builder and still show up in module includes. Zel's test runner (`/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`) has no in-process module holder.
  - Nested `Config` stores the runner argv as `List<String>` via `Arrays.asList(args)` in `public Config(String... args)`. Zel passes `TestSpec.args` into the spawned process and never wraps them in an overridable runner module.
  - Package-private `options()` is the only provider. It calls `JUnit4Options.parse(System.getenv(), args)`, so include/exclude filters and fail-fast come from live env plus argv (`--test_filter` / `--test_exclude_filter`, and `TESTBRIDGE_TEST_ONLY` / `TESTBRIDGE_TEST_RUNNER_FAIL_FAST` when the flag is absent). Zel only exports `TESTBRIDGE_TEST_ONLY` from `spec.test_filter` at spawn time and does not parse those flags inside a JUnit runner.
  - `Config` does not build `JUnit4Config` itself. `BazelTestRunner` injects it with `JUnit4Bazel.builder().suiteClass(suite).config(new Config(args)).build().runner()`. `Builder.createModule()` throws `IllegalStateException` if `config` is unset, then does `new JUnit4RunnerModule(config.options())`, and that module's `config()` builds `JUnit4Config` from fail-fast, include filter, and exclude filter. Zel never constructs that graph.
  - The builder field is replaceable at test time: if `module` is already set, `build()` skips `createModule()` (`JUnit4BazelMock.testModule` installs a `TestModule`). Zel has no equivalent hook to swap the in-process JUnit runner module.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/junit4/JUnit4Options.java`
- Bazel lines: 134
- Zel counterpart: none (nearest test-option code: `/workspace/zel-cursor/crates/zel-options/src/parser.rs`, `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`, `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`, `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`)
- Verdict: partial
- Differences:
  - Bazel parses the test-process argv for only `--test_filter` and `--test_exclude_filter`, each as `--flag=value` or `--flag value`. The value is the raw substring after the first `=` (`indexOf('=') > 0`, so a token whose first character is `=` is not this form; `--test_filter=foo=bar` stores `foo=bar`; `--test_filter=` stores `""`). The two-token form consumes the next argv entry even if it looks like a flag. A known flag with no following argument throws `RuntimeException` (`No filter expression specified after ` + the flag). Any other token, including `--unknown=foo`, is kept in order in `unparsedArgs`. A later flag overwrites an earlier one. Zel has no runner argv parser, no `--test_exclude_filter`, and no `unparsedArgs` list.
  - Include and exclude default to null. `TESTBRIDGE_TEST_ONLY` is copied into the include filter only when that filter is still null, so a command-line value including `""` beats the environment, and an environment value including `""` is used when the flag was omitted. The exclude filter is never taken from the environment. Zel exports `TESTBRIDGE_TEST_ONLY` from the last `--test_filter` (`exec_phase.rs` `opt_last`, including an explicit empty string) in `test_runner.rs`, but `parser.rs` drops an empty value from `Options.test_filter` (`if !raw_val.is_empty()`). Zel never reads `TESTBRIDGE_TEST_ONLY` back and never applies an exclude regexp.
  - Fail-fast defaults to false and is true only when `TESTBRIDGE_TEST_RUNNER_FAIL_FAST` is exactly the string `"1"` (`"1".equals(...)`: a missing variable, `"true"`, `"TRUE"`, `"yes"`, `"1 "`, and `" 1"` are all false). There is no integer parse. Zel’s `--test_runner_fail_fast` defaults to false and its boolean converter accepts `true`/`1`/`yes`/`t`/`y` case-insensitively, but `test_runner.rs` never sets `TESTBRIDGE_TEST_RUNNER_FAIL_FAST`. `aquery.rs` treats fail-fast as on only when the stored display equals `"true"`, so a stored `"1"` is false there while this runner would treat the env value `"1"` as true.
  - This class does not parse sharding or any integer. Zel shards separately (`effective_shard_count` in `exec_phase.rs`; `TEST_TOTAL_SHARDS` / `TEST_SHARD_INDEX` in `test_runner.rs`). That is not a substitute for this parser. Zel’s `forced=k` uses a decimal `i64` parse (`'08'` fails; a leading zero is not octal). Bazel’s `ShardingStrategyConverter` uses `Integer.decode` (leading-zero octal, `0x` / `#` hex). That integer difference is outside this file and does not change `JUnit4Options`.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/junit4/JUnit4Runner.java`
- Bazel lines: 277
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`
- Verdict: partial
- Differences:
  - Zel’s runner is the outer spawn loop (`runs_per_test` × shards × flaky attempts through `test-setup.sh`). It does not run JUnit 4 in-process: no `JUnitCore`, no `Request`/`Runner`, no `Initializer`s, no `"JUnit4 Test Runner"` banner, and no API-version check that rejects anything other than `junit.runner.api.version=1`.
  - Include filter: Bazel compiles `--test_filter` / `TESTBRIDGE_TEST_ONLY` as a regex and matches `class#method` with `Matcher.find()` (display name if class or method is blank). Suites always pass; `SuiteTrimmingFilter` then drops suites whose children do not match. Zel only exports `TESTBRIDGE_TEST_ONLY` when `test_filter` is set and never evaluates the regex.
  - Exclude filter: Bazel intersects `RegExTestCaseFilter.exclude` (`--test_exclude_filter`) with the include filter before the request is run. Zel has no exclude filter and no intersection.
  - Empty-filter outcome: if the regex removes every test, Bazel returns an `ErrorReportingRunner` whose failure is `No tests found matching <filter.describe()>`. If a regex was applied and sharding is on, a shard that then has nothing left becomes `NoOpRunner` (`nothingToDo`, empty `run`) so the other shards are not failed. Zel has neither the error request nor the no-op shard.
  - Filter application order: Bazel applies the regex first, then a second, stricter filter that is the regex intersected with the shard filter, because `ParentRunner` keeps only the last filter. Zel does not apply filters to a runner at all.
  - Sharding selection: when `TEST_TOTAL_SHARDS` is set, Bazel builds a filter (default round-robin over sorted test descriptions; `test.sharding.strategy=HASH` or a custom `ShardingFilterFactory` class) and runs only that shard’s cases. Building the filter also creates or touches `TEST_SHARD_STATUS_FILE` before any test runs, including an empty shard. Zel only sets `TEST_TOTAL_SHARDS`, `TEST_SHARD_INDEX`, and `TEST_SHARD_STATUS_FILE` when `shard_count > 0`, and leaves case selection to the test binary.
  - Shard-support check: Zel, when `shard_count > 1` and `check_sharding_support`, fails the attempt if `test.shard` is missing and stops later shards with status `NO STATUS`. Bazel’s JUnit runner advertises support by touching that file itself; Zel never touches it.
  - XML: this loop does not write XML itself. It registers listeners; the production set includes `JUnit4TestXmlListener`, which records each case (start, failure plus exception, assumption skip, `@Ignore` suppressed vs suite assumption), writes Ant XML on `testRunFinished`, and on `TERM` hard-cancels the run, marks it interrupted, and writes partial XML. Zel only notices whether `test.xml` already exists. If not, it writes one synthetic suite named after the target, `duration`/`time` fixed at `0`, `failures="0"`, and a single `<error>` of `timed out` or `exited with error code N` plus the log in CDATA. It copies the last attempt’s XML upward and parses it. It logs a `generate-xml.sh` spawn without running that script.
  - Premature exit: Bazel reads `TEST_PREMATURE_EXIT_FILE`, truncates it to empty before `core.run`, and deletes it in `finally`. A write failure aborts with `Could not write exit file at …`. A delete failure is printed and does not replace the test result. If the process dies before `finally`, the file remains. Zel sets the variable to `test.exited_prematurely` and lists it as an output, but never creates, truncates, or deletes that file.
  - `System.exit` detection: Bazel installs `SystemExitDetectingShutdownHook` for the run and removes it in `finally`. If any thread’s stack contains `java.lang.Runtime.exit`, it prints `System.exit or Runtime.exit was called!` plus frames and `Runtime.halt(121)`. Zel has no shutdown hook and no exit code 121.
  - Fail-fast: when `TESTBRIDGE_TEST_RUNNER_FAIL_FAST=1`, Bazel adds `StopOnFailureRunListener`, which on `testFailure` calls `cancelRunOrderly()`. The wrapped notifier then refuses to start the next test; `StoppedByUserException` is printed and swallowed (`ORDERLY_STOP`), so the current result stands and later cases do not run. The request is always cancellable, including a hard stop from the XML listener. Zel’s `TestSpec` has no fail-fast flag and `build_test_env` never sets `TESTBRIDGE_TEST_RUNNER_FAIL_FAST`. A failed shard or run does not stop the remaining shards or runs; only a sharding-unsupported result breaks the loop.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/junit4/JUnit4RunnerBaseModule.java`
- Bazel lines: 60
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines an uninstantiable abstract legacy Dagger module, `JUnit4RunnerBaseModule`, whose two static factories are the shared bindings for real runs and runner integration tests. Callers are `JUnit4Bazel` (`provideRequest`) and `JUnit4RunnerModule` / `JUnit4RunnerTest` (`provideTextListener`). Zel has no Guice or Dagger test-runner module and no Java JUnit runner.
  - `provideTextListener(PrintStream)` wraps the runner stdout in a `PrintStream` using UTF-8 and `autoFlush=false`, then returns `org.junit.internal.TextListener` on that stream. If UTF-8 is missing it throws `IllegalStateException` ("UTF-8 must be supported as per the java language spec") wrapping `UnsupportedEncodingException`. Zel does not attach a JUnit text listener or force runner console output through a UTF-8 `PrintStream`.
  - `provideRequest(Class<?>)` builds `Request.aClass(suiteClass)` and wraps it in `MemoizingRequest` so the JUnit `Runner` is created once. The runner asks for the `Runner` twice (model before filtering, execution after filtering), and the extra memoization is kept because users may be on JUnit older than 4.11, where `Request.aClass` itself memoizes. Zel does not construct or memoize a JUnit `Request` or `Runner`. `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` only spawns a test binary through `test-setup.sh` and records process-level status, shards, attempts, and logs.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/junit4/JUnit4RunnerModule.java`
- Bazel lines: 148
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel’s package-private legacy Dagger module takes `JUnit4Options` and supplies the in-process JUnit 4 runner graph. Zel’s test path (`/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`) only launches a test binary through `test-setup.sh` and never constructs this module.
  - `provideXmlStream` opens `JUnit4Config.getXmlOutputPath()` as a `FileOutputStream`. On a null path or `FileNotFoundException` it prints the stack and returns a discarding `OutputStream` (`write` is a no-op, null buffers throw `NullPointerException`, `toString` is `"null OutputStream"`) so a missing XML file does not fail the run. Zel only exports `XML_OUTPUT_FILE` and, if the test writes no `test.xml`, emits a minimal XML document from the process result.
  - `config()` builds `JUnit4Config` from `options.getTestRunnerFailFast()`, `getTestIncludeFilter()`, and `getTestExcludeFilter()`. Zel forwards a single `TESTBRIDGE_TEST_ONLY` string and does not apply separate include and exclude filters or fail-fast inside a Java runner.
  - `shardingEnvironment()` and `shardingFilters()` bind a `ShardingEnvironment` and `ShardingFilters` with `ShardingFilters.DEFAULT_SHARDING_STRATEGY`, which filter JUnit requests in-process. Zel only sets shard environment variables and checks `TEST_SHARD_STATUS_FILE`.
  - `setOfRunListeners` installs an unmodifiable set of four `RunListener`s: `JUnit4TestStackTraceListener` on `System.err` with a real signal-handler installer, `JUnit4TestXmlListener` (suite-model supplier, `CancellableRequestFactory`, real signal handlers, the XML stream, `System.err`), `JUnit4TestNameListener` over a `SettableCurrentRunningTest` that writes the global `testNameProvider`, and `JUnit4RunnerBaseModule.provideTextListener(System.out)`. Zel has none of these listeners, the current-test name provider, or signal-handler wiring.
  - `cancellableRequestFactory()` and `clock()` bind a new `CancellableRequestFactory` and `TestClock.systemClock()`. `stdout()` binds `System.out`. Zel has no equivalents.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/junit4/JUnit4TestModelBuilder.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel builds an in-process `TestSuiteModel` from the JUnit `Request`: `get()` calls `request.getRunner().getDescription()` and, when that root `isSuite()`, passes the whole `Description` tree to `TestSuiteModel.Builder.build(suiteName, root)`. That walk happens before command-line filters. Zel has no JUnit runner and no description tree; `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` only records the spawned process result.
  - A root that is not a suite (a class with no test methods, including a class annotated `@Ignore`) is built as an empty model via `builder.build(suiteName)`, with no children attached. Zel has no `@Ignore` or empty-suite model.
  - `Builder.build` (invoked from this supplier) is the sharding hook: if sharding is enabled it collects every `isTest()` description, touches the shard status file, and installs the sharding filter on the model. Empty suites are then pruned. `JUnit4Bazel` memoizes this supplier because walking the runner is expensive. Zel never selects individual JUnit cases onto a shard; it only sets shard environment variables on the test process.
  - Include and exclude regexes (`--test_filter` and the exclude filter) are applied after this model exists, in `JUnit4Runner.applyFilters`, intersected with the model’s sharding filter. A user filter plus sharding that leaves a shard with no tests becomes a no-op runner so the other shards do not fail. Zel only exports `TESTBRIDGE_TEST_ONLY` and does not filter or intersect a JUnit suite.
  - Zel’s `test_suite` expansion in `/workspace/zel-cursor/crates/zel-rules/src/resolver.rs` selects Bazel targets, not methods inside a JUnit suite.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/junit4/ProvideXmlStreamFactory.java`
- Bazel lines: 98
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (fallback `std::fs::write` of `synthesize_xml`); shell writer `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/test/generate-xml.sh`
- Verdict: partial
- Differences:
  - Bazel does not open the file in this class. `get()` returns a `LazyOutputStream` that, on the first `write`, calls `JUnit4RunnerModule.provideXmlStream`. A null `configSupplier` throws `IllegalStateException` at construction. Zel has no lazy factory, no double-checked lock, and no null-config failure.
  - The path is `JUnit4Config.getXmlOutputPath()`: the configured `Path`, or else `XML_OUTPUT_FILE` via `FileSystems.getDefault().getPath`. A null path never creates a file. Zel always sets `XML_OUTPUT_FILE` to `<attempt_dir>/test.xml` (`test_runner.rs` `build_test_env`) and only writes when that file is absent.
  - Open is `new FileOutputStream(path.toFile())`, the one-argument constructor: create or truncate, not append. Parent directories are not created; `FileNotFoundException` is printed and replaced with a discarding stream (`write(int)` drops the byte; `write(byte[])` throws `NullPointerException` if the array is null). `close` and `flush` do nothing until the delegate exists. Zel `std::fs::write` also truncates (create, not append) and then closes, but errors are ignored (`let _ =`), and an existing `test.xml` is left untouched. `test-setup.sh` does `mkdir -p` on the parent of `XML_OUTPUT_FILE` before the test runs. `generate-xml.sh` truncates with `cat >"${XML_OUTPUT_FILE}"`.
  - The `FileOutputStream` is raw bytes. No charset is applied at open, so this path does not change bytes. UTF-8 is applied later by `XmlWriter` (`OutputStreamWriter(..., UTF_8)`, no BOM). Zel writes a Rust `String`, which is UTF-8 with no BOM, so valid Unicode matches Java UTF-8. The fallback path does change bytes: `String::from_utf8_lossy` on the test log replaces invalid UTF-8 with U+FFFD (`EF BF BD`) before those bytes are embedded in the XML. Java's stream would pass through already-encoded writer bytes and would not do that replacement.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/model/AntXmlResultWriter.java`
- Bazel lines: 202
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`synthesize_xml`); also `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/test/generate-xml.sh`. `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs` only parses `test.xml`.
- Verdict: partial
- Differences:
  - Bazel writes a full JUnit tree from `TestResult`: a `testsuites` root, one `testsuite` per direct child, and further suites that themselves have children emitted as later sibling `testsuite` elements (not nested). Leaves become `testcase`. Filtered leaves are omitted. Zel’s writers always emit one suite and one case named from the target (`label` after `:`, or `TEST_BINARY` with `./`/`../` stripped and an optional `_shard_N/M` suffix). `test_status.rs` does not write XML.
  - Suite attributes, in order, single-quoted: `name`, `timestamp` (start instant, ISO-8601 with exactly 3 fractional digits via `appendInstant(3)`, or `""` if there is no interval), `hostname="localhost"`, `tests` (`getNumTests()`), `failures` (`getNumFailures()`), `errors="0"` always, `time` (`toDurationMillis()/1000.0` through `String.valueOf`, so 1 ms is `0.001`; `"0.0"` if the interval is null), `package=""`, `id` (int, starting at 0, incremented per suite in this document). `synthesize_xml` writes only `name`, `tests="1"`, `failures="0"`, and `errors` 0 or 1. It drops `duration_ms` and hardcodes `time="0"`. `generate-xml.sh` sets `duration` and `time` to the passed seconds string. Neither writes `timestamp`, `hostname`, `package`, or `id`, and both count a bad exit as `errors` rather than `failures`.
  - Every suite gets a `properties` element and one `property` with `name` and `value` per `result.getProperties()` entry, then empty `system-out` and `system-err` (` />`). Zel writes no properties. It puts the log in `system-out` as CDATA after the testcase, prefixed with `Generated test.log (if the file is not UTF-8, then this may be unreadable):`, and writes no `system-err`.
  - A testcase has `name`, `classname` (`getClassName()`), and `time` in the same seconds form. It does not set `status` or `duration`, even though `Status.wasRun()` exists for `run|notrun`. Each ancestor throwable plus the case’s own throwables becomes `failure` with `message` (null becomes `""`), `type` (exception class name), and `printStackTrace` as character data. `SKIPPED` and `SUPPRESSED` also get an empty `skipped` element; failures are not removed. Zel emits at most one empty `error` (`timed out`, or `exited with error code N` in `synthesize_xml`; only a nonzero exit in `generate-xml.sh`), with no `type`, no stack trace, and no `skipped`. The testcase is forced to `classname=""`, `status="run"`.
  - Parent failures are concatenated down the suite ancestry and repeated on every descendant testcase. Zel has no suite tree and no inherited failures.
  - Declaration and charset come from `XmlWriter`: `<?xml version='1.0' encoding='UTF-8'?>`, UTF-8 `OutputStreamWriter`, two-space indent, newline before each start tag, empty elements self-closed as ` />`, no extra whitespace before end tags, trailing platform EOL. Attribute values go through `XmlEscapers`: `& < > ' "` plus tab/newline/CR as `&#x9;` / `&#xA;` / `&#xD;`; other C0 controls and code points above U+FFFD become U+FFFD. Character content (stack traces) escapes `& < >` and those illegal controls, and leaves quotes and tab/newline/CR raw. Integer attributes are unescaped. Zel’s declaration uses double quotes. `synthesize_xml` only replaces `& < > "` in names and the error message (not `'`, controls, or noncharacters) and builds a Rust string. The log is CDATA; `]]>` becomes `]]>]]&gt;<![CDATA[`, which differs from `generate-xml.sh`’s `]]>]]<![CDATA[>`. `generate-xml.sh` rewrites illegal UTF-8 and XML characters to `?` before CDATA. `synthesize_xml` uses `from_utf8_lossy` and does not strip XML-illegal controls. `parse_test_xml` unescapes only `&lt; &gt; &quot; &apos; &amp;` (so `&#x9;` / `&#xA;` / `&#xD;` stay literal), skips CDATA and `system-out` text, ignores `timestamp`, `hostname`, `package`, `id`, `properties`, and failure `message`/`type`/body, and reads `time` as seconds times 1000, rounded. It treats `failure` as failed and `error` as error, so Bazel’s `errors="0"` plus `failure` elements do not match Zel’s fallback, which uses `error`.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/model/TestCaseNode.java`
- Bazel lines: 300
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs` (`TestCase`, filled by `parse_test_xml`) and `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`TestResult` for the whole spawned action)
- Verdict: partial
- Differences:
  - Bazel is a live JUnit leaf. Name comes from `Description.getMethodName()`; if that is null, the name becomes `getClassName()` and the class name becomes the parent suite’s display name. Dynamic children use `getDisplayName()` and the parent display name as the class name. Zel copies optional `name` and `class_name` from XML `name` and `classname` and does not do that fallback.
  - Bazel status is a CAS state machine: `INITIAL`→`FILTERED`, `PENDING`→`CANCELLED` until start, `STARTED`→`INTERRUPTED` if execution ends while running, plus `SKIPPED`, `SUPPRESSED`, `CANCELLED`, `INTERRUPTED`, and `FINISHED`→`COMPLETED`. `pending` only from `INITIAL`; `started` from `INITIAL` or `PENDING`; `skipped` and `finished` only from `STARTED`; `suppressed` from `INITIAL` or `PENDING`; interrupt of a started case becomes `INTERRUPTED`, of an unstarted case `CANCELLED`. Zel’s case status is only `Passed`, `Failed`, `Error`, or `Skipped`, counted from `<failure>`, `<error>`, `<skipped>`, and child statuses. The XML `status` attribute only sets `run` for `run` / `notrun`. The process `TestStatus` (`PASSED`, `FLAKY`, `TIMEOUT`, `FAILED`, `INCOMPLETE`, and the rest) is for the whole action, not this per-case machine.
  - Bazel timing is a nullable `TestInterval`. Every successful transition other than `PENDING` creates `(now, now)` or updates the end instant, so `getRuntime()` stays null while `INITIAL` or only `PENDING`. Dynamic children inherit that interval. Zel stores one optional `run_duration_millis` from the XML `time` attribute (`seconds * 1000`, rounded). It has no start/end interval on the case. Action-level `TestResult` has `duration_ms`, `start_time_ms`, and `times_ms`. The generated fallback `test.xml` writes `duration` and `time` as 0.
  - Bazel keeps a concurrent queue of `Throwable`s plus a `Description`→throwable-list map. Interrupt adds `Exception("Test interrupted")`; cancel adds `Exception("Test cancelled")`. `testFailure` and `dynamicTestFailure` record the throwable and, from `INITIAL` or `PENDING`, force `FINISHED`. `numFailures` is the number of dynamic tests that failed when there are no global failures, otherwise it equals `numTests`. A dynamic child fails if the parent has any global failure or that child has its own. Zel drops failure and error bodies (`skip_element`) and keeps only the status enum. It has no throwables, messages, stacks, dynamic-failure map, synthetic interrupt/cancel exceptions, `numTests` / `numFailures`, per-case properties, or test integrations. `failed_logs` are log paths on the action, not case failures.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/model/TestInterval.java`
- Bazel lines: 75
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (also `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs`, `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`)
- Verdict: partial
- Differences:
  - Bazel keeps one interval as a wall `Instant` plus a monotonic `Duration` (`nanoTime`). Duration is `end.monotonic.minus(start.monotonic).toMillis()`; `getStartMillis` / `getEndMillis` and the ISO timestamp use wall time only. Zel measures the attempt with `Instant::elapsed().as_millis()` and takes the suite start once from `SystemTime` since the Unix epoch. It has no per-case interval and no separate wall end.
  - Positive sub-millisecond truncation matches: Java `Duration.toMillis` and Rust `Duration::as_millis` both drop leftover nanos. That conversion does not change a positive elapsed millisecond count. Reported numbers still change afterward.
  - Wall start before 1970: Java `Instant.toEpochMilli()` returns a negative long. Zel uses `duration_since(UNIX_EPOCH).unwrap_or(0)`, so the start becomes 0.
  - Overflow: `Duration.minus` and `toMillis` throw `ArithmeticException` when the delta does not fit in a `long` of milliseconds. Zel casts `as_millis()` (`u128`) with `as u64`, which wraps past `u64::MAX`. A `nanoTime` span stays inside a Java `long`, so this does not show up for normal test lengths.
  - The constructor throws `IllegalArgumentException` ("Start must be before end") when the start monotonic time is greater than the end. Equal endpoints are allowed and last 0 ms. Zel's `elapsed()` is always non-negative and does not reject an inverted interval.
  - `withEndMillis` replaces the whole end `TestInstant` and rechecks monotonic order. Zel never updates an end instant.
  - `around` builds the parent as the earlier monotonic start and the later monotonic end, so the parent duration includes gaps between children and keeps the wall time of whichever endpoint won the monotonic comparison. Zel's summary end is `start_time_ms + max(attempt duration)`, and the BEP total is the sum of attempt durations. Several attempts or gaps produce a different number, and the published end always equals wall start plus monotonic duration. Bazel can report a 100 ms duration with equal wall start and end millis when the wall clock steps backward.
  - `startInstantToString` is UTC ISO-8601 with exactly three fraction digits (`2016-08-20T16:15:34.000Z`), independent of zone and locale. Zel does not emit that timestamp. XML runtime is `String.valueOf(millis / 1000.0)` (`100` ms → `"0.1"`). The log line formats integer millis as `({:.1}s)`, so `1499` ms prints as `1.5s`.
  - When the test does not write `test.xml`, `synthesize_xml` ignores `duration_ms` and writes `time="0"` and `duration="0"`. Bazel writes the measured seconds.
  - Parsed `time` attributes go through `(seconds as f64 * 1000).round()` to `i64`. `0.0005` s becomes 1 ms. Bazel had already truncated to whole millis before formatting, so a value below 0.5 ms would have been 0.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/model/TestNode.java`
- Bazel lines: 86
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs` (`TestCase`); process-level outcome is `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`TestResult`)
- Verdict: partial
- Differences:
  - Bazel’s node is a live JUnit suite entry: a non-null `Description` (null throws), abstract `getChildren()`, and abstract `isTestCase()` (case vs suite via `Description.isTest()`). Zel’s `TestCase` is a parsed `test.xml` tree (`child`, `kind` of case/suite/decorator/unknown, name, class name, duration). It has no `Description` and is not updated while a test runs.
  - Bazel records outcomes on the node with `TestInstant`: `testSkipped`, `testSuppressed` (`@Ignore` / `@Suppress`, separate from skip), `testInterrupted`, `testFailure(Throwable)`, and `dynamicTestFailure(Description, Throwable)` for a generated case or suite. `getResult()` caches one `TestResult` from abstract `buildResult()`. Zel only stores `Passed` / `Failed` / `Error` / `Skipped` (rolled up from `<failure>`, `<error>`, `<skipped>`) and a `run` flag from XML `status="run"|"notrun"`. It does not keep throwables, interruption, suppression, or dynamic-test failures, and it does not lazily build a per-node result.
  - This class does not filter children. The runner’s `TestResult.Status` still separates `FILTERED` (user filter), `SKIPPED` (assumption), `SUPPRESSED`, `CANCELLED`, `INTERRUPTED`, and `COMPLETED`. Zel collapses “not run” into `Skipped` plus `run`. Its `--test_filter` is only copied to `TESTBRIDGE_TEST_ONLY`; it does not trim a node tree.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/model/TestResult.java`
- Bazel lines: 246
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs` (JUnit XML `TestCase` / `CaseStatus`); target outcome is a different enum in `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`TestStatus` / `TestResult`). No junit-runner `TestResult` type.
- Verdict: partial
- Differences:
  - Bazel `Status` is `SKIPPED` (assumption / test chose not to run), `FILTERED` (user filter), `SUPPRESSED` (`@Ignore` / `@Suppress`), `CANCELLED` (never started: signal or timeout), `INTERRUPTED` (started, not finished: signal or timeout), and `COMPLETED` (finished, including failures). Only `INTERRUPTED` and `COMPLETED` have `wasRun() == true`, which is the XML `status="run"|"notrun"` bit. Zel `CaseStatus` is only `Passed`, `Failed`, `Error`, and `Skipped`. Zel `TestStatus` is `NoStatus`, `Passed`, `Flaky`, `Timeout`, `Failed`, `Incomplete`, `FailedToBuild`, `BlazeHaltedBeforeTesting` (numeric values skip 6); `is_passed` is true for `Passed` and `Flaky`.
  - Zel sets `run` only from an XML `status` of `run` or `notrun`. Any other `status` value is ignored. A `notrun` case with no `<skipped/>` still becomes `CaseStatus::Passed` when there are no `<failure>` or `<error>` tags. There is no separate filtered, suppressed, cancelled, interrupted, or completed case state.
  - Bazel keeps `List<Throwable> failures`, plus `numTests` and `numFailures`, and a `childResults` tree. Zel counts `<failure>` and `<error>` while parsing and then drops the message and body. Parent status is `Error` if any error (including a child’s), else `Failed` if any failure, else `Skipped` only if this element itself had `<skipped/>`, else `Passed`. It does not store throwables, `numTests`, or `numFailures`, and it ignores the suite `tests` attribute.
  - Bazel timing is a nullable `TestInterval` (`getRunTimeInterval()`). The builder may set it once, including to null; a second set throws `IllegalStateException`. Zel stores `run_duration_millis` from the XML `time` attribute as seconds × 1000, rounded to `i64`; a missing `time` stays unset, and a non-numeric `time` fails the parse. That is a duration, not a start/end interval. Action-level `duration_ms`, `times_ms`, and `start_time_ms` on `test_runner::TestResult` are process timing, not the JUnit case interval.
  - Bazel also requires `name`, `className`, a `properties` map, a `TestIntegration` set, and the child list. The builder rejects a second assignment and a null for every field except `runTime` (`IllegalStateException` / `NullPointerException`). Zel has optional `name`, `class_name`, and `result` strings on `TestCase` and no properties map, integrations, or single-assignment builder.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/model/TestSuiteModel.java`
- Bazel lines: 408
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel builds a one-shot in-memory suite tree from JUnit `Description`s (`Builder.build` throws if called twice), attaching root properties and indexing suites and cases in a `ConcurrentHashMap`. Zel has no live suite model. `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` `TestResult` and `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs` `TestCase` are filled after the test process exits.
  - When sharding is on, Bazel collects leaf tests, touches the shard file, builds a `Filter`, drops tests that should not run, and prunes empty suites. Zel only checks sharding support on the test spawn and runs shard counts as separate attempts.
  - Bazel updates the tree during the run: `testRunStarted` marks cases pending, `testStarted` records the clock and thread-local property and integration exporters, and `testFinished` records the end time without clearing those exporters.
  - `testRunInterrupted` timestamps the whole root as interrupted. Zel’s cancel path kills the build or spawn. It does not mark a suite node interrupted.
  - Failures on a suite are applied to its children. `DynamicTestException` is recorded as a dynamic failure. An unknown case (for example Mockito) is inserted under the suite with the same test class and then failed, with timing known to be wrong. A missing suite throws `IllegalStateException`. Skipped and suppressed are separate transitions, and cases can export XML properties. Zel does none of this.
  - `write` / `writeAsXml` emit XML exactly once via `AtomicBoolean.compareAndSet`. Later calls are no-ops, and `toString` uses that same path, so stringifying the model consumes the write. Zel copies the test’s `test.xml` (last attempt wins) or runs `generate-xml.sh` when the test wrote none, then parses it. There is no once-only writer on a suite model.
  - Null `Description`s are ignored so a thrown exception cannot make JUnit 4 drop the listener. Zel has no JUnit listener or description lookup.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/model/TestSuiteNode.java`
- Bazel lines: 129
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs` (`TestCase` with `CaseType::TestSuite`, built by `parse_test_xml` / `P::suite` / `P::contained`)
- Verdict: partial
- Differences:
  - Bazel is a live JUnit parent: `addTestSuite` / `addTestCase` append to an `ArrayList`, `getChildren` returns an unmodifiable view, and `isTestCase()` is false. `testFailure`, `dynamicTestFailure`, `testInterrupted`, `testSkipped`, and `testSuppressed` are forwarded to every child. Zel never builds this node. It only parses a finished `test.xml` into a `TestCase` tree (`testsuite`, `testcase`, `testdecorator`).
  - `buildResult` sums `numTests` and `numFailures` from each child's `getResult()`, keeps the suite's own throwable list empty, and stores the child results in order. Zel has no counts. `contained` tallies direct `<failure>` / `<error>` elements plus child `Failed` / `Error` statuses only to choose one status, and drops the counts.
  - Suite status is hardcoded `Status.SKIPPED`. Zel sets `Error` if any error, else `Failed` if any failure, else `Skipped` only when a `<skipped>` element was seen on that node, else `Passed`. Child `Skipped` statuses do not mark the parent skipped.
  - Timing is the union of non-null child intervals (`TestInterval.around`: earliest monotonic start, latest monotonic end) and stays null if every child interval is null. Zel copies the suite `time` attribute (`seconds * 1000`, rounded) and does not fold child durations into the parent. A missing `time` leaves `run_duration_millis` unset.
  - The result name is `Description.getDisplayName()`, `className` is `""`, constructor properties are copied, and integrations are an empty set. Zel sets the suite name only from the `name` attribute, leaves `class_name` unset on suites, and has no properties or integrations.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/model/XmlResultWriter.java`
- Bazel lines: 29
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`synthesize_xml`)
- Verdict: partial
- Differences:
  - Bazel defines a Java interface with one method, `writeTestSuites(XmlWriter writer, TestResult result)`, that writes a structured `TestResult` through a supplied `XmlWriter` and declares `IOException`. It does not build XML itself.
  - Zel has no `XmlResultWriter`, `XmlWriter`, or `TestResult` type. `synthesize_xml` returns one synthetic `<testsuites>` string from the target name, exit code, timeout flag, and test log when the test did not write `XML_OUTPUT_FILE`.
  - `duration_ms` is unused. The generated `testcase` always has `duration="0"` and `time="0"`, and the suite is always `tests="1"`.
  - A non-zero exit or timeout becomes a single empty `<error message="timed out">` or `<error message="exited with error code N">`, with `errors="1"` and `failures="0"`. There is no walk of nested suites, cases, failures, skips, or properties.
  - The log is placed in `<system-out>` CDATA after stripping one trailing newline and splitting `]]>`. Attribute text escapes `&`, `<`, `>`, and `"`. The suite `name` is the label after the last `:`, and `classname` is empty.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/model/XmlWriter.java`
- Bazel lines: 221
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`synthesize_xml`) and `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`xml_escape_attr`, `xml_write`)
- Verdict: partial
- Differences:
  - Bazel is a streaming element writer (`OutputStream` / `Writer`, element stack, `startDocument` / `startElement` / `endElement` / `writeAttribute` / `writeCharacters` / `close`). Zel has no equivalent type. `synthesize_xml` only fills a fixed JUnit skeleton. `xml_write` only emits an element tree of attributes and child elements (no text nodes, no attribute-vs-content escaper, no open-element state).
  - Charset: `new OutputStreamWriter(outputStream, UTF_8)` encodes Java UTF-16 `char`s as UTF-8. `CharEscaper` walks UTF-16 code units, safe range inclusive `\u0000`–`\uFFFD`. Supplementary characters are left as surrogate pairs and encoded to UTF-8 by the writer. Unpaired surrogates are in the safe range, so they are copied through and the UTF-8 encoder replaces them (replacement character). `\uFFFE` and `\uFFFF` are outside the safe range and become the character U+FFFD, not a character reference. Rust `str` is already Unicode scalar values in UTF-8: no unpaired surrogates and no U+FFFE/U+FFFF, and non-BMP characters are emitted directly. `synthesize_xml` is called with `String::from_utf8_lossy`, so invalid log bytes become U+FFFD before XML is built. `XmlWriter` does not do that conversion.
  - Content escaping (`XmlEscapers.xmlContentEscaper`): `&` → `&amp;`, `<` → `&lt;`, `>` → `&gt;`. Tab, LF, and CR stay raw. Other C0 controls (`0x00`–`0x1F`) become the character U+FFFD. Quotes are not escaped. `writeCharacters` skips `null` and `""` but still closes the open start tag. Zel has no content escaper. `synthesize_xml` puts the log in CDATA and only splits `]]>`; C0 controls in the log or in a target name are written raw. Its name/message escaper is only `&`, `<`, `>`, `"`.
  - Attribute escaping (`xmlAttributeEscaper`, string overload only): content rules plus `'` → `&apos;`, `"` → `&quot;`, tab → `&#x9;`, LF → `&#xA;`, CR → `&#xD;` (hex, uppercase `A`/`D`). Other C0 controls still become the character U+FFFD. `null` writes an empty quoted value. `int` and `double` use `String.valueOf` with no escaper (`NaN`, `Infinity`, `-Infinity` pass through). Element and attribute names are not escaped. `xml_escape_attr` uses decimal `&#9;` / `&#10;` / `&#13;`, decimal `&#N;` for every other char below U+0020, and does not escape `'`. It never emits a raw U+FFFD for controls. `synthesize_xml` does not escape `'`, whitespace, or other controls, and it ignores `duration_ms` (always `duration="0" time="0"`).
  - Quotes: declaration is `<?xml version='1.0' encoding='UTF-8'?>` (single quotes, XML 1.0, no `standalone`). Attributes are single-quoted (` name='value'`). `synthesize_xml` uses double quotes and `version="1.0"`. Query XML uses `<?xml version="1.1" encoding="UTF-8" standalone="no"?>` and double-quoted attributes. Attribute order is call order in Bazel; `xml_write` emits `BTreeMap` key order.
  - Newlines: `EOL` is `System.getProperty("line.separator", "\n")`. The declaration has no newline of its own; each start tag is prefixed with `EOL` plus indent, and `close()` appends one more `EOL` after auto-closing any open elements, then closes the writer. End tags of elements that already have content are written as `</name>` with no preceding newline or indent, so `writeCharacters` text is exact. `synthesize_xml` and `xml_write` always use `\n`. `xml_output` applies `line_terminator` only to the declaration; `xml_write` still hardcodes `\n` after every tag.
  - Indentation and empty elements: two spaces per open-element depth, including before the root. A still-open element ends as ` />` (space before the slash). After `writeCharacters`, the same element is `>` … `</name>` even when the text was empty. `xml_write` indents with four spaces and self-closes childless elements as `/>` with no space. `synthesize_xml` uses two-space steps for `testsuites` / `testsuite` / `testcase`, puts `<error …></error>` inline (explicit close, not self-close), and indents `system-out` with six spaces.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/sharding/HashBackedShardingFilter.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel is a JUnit `Filter`. The constructor stores `shardIndex` and `totalShards` and throws `IllegalArgumentException` when `shardIndex < 0` or `totalShards <= shardIndex`. Zel has no JUnit runner and no equivalent filter.
  - `shouldRun` always returns true for a suite (`description.isSuite()`). For a test it hashes `description.getDisplayName()`, takes that modulo `totalShards`, and runs the test only when the adjusted residue equals `shardIndex`. Zel never reads a JUnit display name and never assigns individual cases to a shard.
  - The hash is Java `String.hashCode()`: 32-bit wrapping `h = 31 * h + utf16CodeUnit`. Java `%` truncates toward zero, so a negative hash yields a negative remainder; Bazel then adds `totalShards` once. If the result is still outside `[0, totalShards)` it throws `IllegalStateException`. Rust's default hasher is SipHash over `u64` and is not stable across processes, so the same display name would not land on the same shard, and a `u64` remainder is never negative, so that adjustment would not apply. Rust `%` on `i32` does match Java's sign-of-dividend rule, but Zel does not use it here.
  - Zel does implement that same Java `hashCode` (`string_hash` in `/workspace/zel-cursor/crates/zel-query/src/java_order.rs` and `java_string_hash` in `/workspace/zel-cursor/crates/zel-server/src/commands/help.rs`: UTF-16 code units, wrapping `i32`). Those are used for query-set iteration order and `help flags-as-proto` `HashMap` order, not for test sharding.
  - `describe()` returns the fixed string `"hash-backed sharding filter"`. Zel has no such description.
  - Zel's test sharding (`/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`, `effective_shard_count` in `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`) only launches one process per shard and sets `TEST_TOTAL_SHARDS`, `TEST_SHARD_INDEX`, and `TEST_SHARD_STATUS_FILE`. Which cases run is left to the test binary. `test_tmpdir_hash` fingerprints the executable path plus shard and run for the temp directory; it is not a case-assignment hash.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/sharding/RoundRobinShardingFilter.java`
- Bazel lines: 111
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel sorts the JUnit `Description` collection with `DescriptionComparator` on `getDisplayName()` (`String.compareTo`) before numbering, so a shuffled suite still gets a stable shard assignment.
  - After that sort, each test is stored at a 0-based index. `shouldRun` keeps a test only when `index % totalShards == shardIndex`.
  - Duplicate `Description` keys share one map entry: the later index overwrites the earlier one, so both run on the shard of the later index.
  - The constructor throws `IllegalArgumentException` (no message) when `shardIndex < 0` or `totalShards <= shardIndex`.
  - A non-test in the input collection throws `IllegalArgumentException` naming `getDisplayName()`. `shouldRun` returns true for every suite (`isSuite()`) and throws if a test description was not in the constructor collection.
  - `describe()` returns the string `round robin sharding filter`.
  - Zel never filters JUnit cases by display name, index, or modulo. `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` only starts one process per shard (`shard` in `0..shards`) and sets `TEST_TOTAL_SHARDS`, a 0-based `TEST_SHARD_INDEX`, and `TEST_SHARD_STATUS_FILE`. Which cases run is left to the test binary. Log directories use 1-based `shard_{n}_of_{total}`.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/sharding/ShardingEnvironment.java`
- Bazel lines: 94
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`
- Verdict: partial
- Differences:
  - `isShardingEnabled` is true whenever `TEST_TOTAL_SHARDS` is non-null, including an empty string. Zel never reads that variable; `build_test_env` writes it only when `spec.shard_count > 0`.
  - `getShardIndex` uses `TEST_SHARD_INDEX`, returns 0 when unset, and otherwise calls `Integer.parseInt` with no trim. A bad value throws `NumberFormatException`. Zel writes `shard` with `u32` `Display` and never parses the env string, so there is no default of 0 and no throw on a malformed value.
  - `getTotalShards` returns 1 when `TEST_TOTAL_SHARDS` is unset and otherwise `Integer.parseInt`s it. A set but unparsable value still counts as sharding enabled, then throws. Zel does not parse; `shard_count == 0` leaves the variable unset and still runs one iteration via `shard_count.max(1)`. `shard_count == 1` exports `TEST_TOTAL_SHARDS=1`.
  - `Integer.parseInt` accepts an optional sign and ASCII digits in signed 32-bit range, so `+3` and `-1` are valid and values above `2147483647` fail. Zel emits unsigned decimal only. `i32` parse matches that grammar, but `test_runner.rs` does not use it, so a bad shard env never aborts the way this class does.
  - `touchShardFile` reads `TEST_SHARD_STATUS_FILE`, no-ops if unset, otherwise `createNewFile` and, if the file already exists, `setLastModified(now)`. Failure throws `RuntimeException` (`Error writing shard file …`). Zel points the variable at `attempt_dir/test.shard` when `shard_count > 0`, then only checks `exists()` after exit 0 when `shard_count > 1` and `check_sharding_support`. It does not create the file or update its mtime. `shard_count == 1` exports the path and skips the check. A missing file becomes exit code 1 plus a log line.
  - `getTestShardingStrategy` returns the JVM system property `test.sharding.strategy` (`-Dtest.sharding.strategy=…`), which may be null. `test_runner.rs` does not read that property.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/sharding/ShardingFilters.java`
- Bazel lines: 111
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel builds a JUnit `Filter` in-process. `createShardingFilter` calls `getShardingFilterFactory().createFilter(descriptions, shardingEnvironment.getShardIndex(), shardingEnvironment.getTotalShards())`. Zel never assigns individual JUnit cases to a shard.
  - The built-in enum `ShardingStrategy` implements `ShardingFilterFactory`. `HASH` ignores the description list and returns `HashBackedShardingFilter(shardIndex, totalShards)`. `ROUND_ROBIN` returns `RoundRobinShardingFilter(testDescriptions, shardIndex, totalShards)`. `DEFAULT_SHARDING_STRATEGY` is `ROUND_ROBIN`. The one-arg constructor uses that default; the two-arg constructor stores a caller-supplied factory.
  - The strategy string is `ShardingEnvironment.getTestShardingStrategy()`, which is the JVM system property `test.sharding.strategy` (documented as `-Dtest.sharding.strategy=round_robin`). A null property returns the stored default factory. Zel has no such property and does not select hash versus round robin.
  - A non-null string is first parsed with `ShardingStrategy.valueOf(strategy.toUpperCase(Locale.ENGLISH))`, so `hash` and `round_robin` (any ASCII case) select those enum factories.
  - If that parse throws `IllegalArgumentException`, the string is treated as a custom class name. Bazel loads it with `Thread.currentThread().getContextClassLoader().loadClass(strategy)`, narrows it with `asSubclass(ShardingFilterFactory.class)`, and instantiates it with the public no-arg constructor (`getConstructor().newInstance()`). `ReflectiveOperationException` or `IllegalArgumentException` from that path is wrapped as `RuntimeException("Could not create custom sharding strategy class " + strategy, e2)`. `asSubclass` throws `ClassCastException`, which is not in that catch, so a loaded class that is not a `ShardingFilterFactory` propagates as `ClassCastException`. Zel does not load a named factory class.
  - Zel sharding is only how many processes to spawn. `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` `effective_shard_count` accepts `--test_sharding_strategy` values `explicit` (use the `shard_count` attribute), `disabled` (0 shards), and `forced=k` (k shards, rejecting a negative k), and rejects any other string. `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` sets `TEST_TOTAL_SHARDS`, `TEST_SHARD_INDEX`, and `TEST_SHARD_STATUS_FILE` on each shard process and expects the test binary to filter itself.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/sharding/api/ShardingFilterFactory.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines a public SPI, `ShardingFilterFactory`, whose implementations must have a public no-argument constructor so a runner can load a custom strategy by class name.
  - `createFilter(Collection<Description> testDescriptions, int shardIndex, int totalShards)` returns a JUnit `Filter` that selects which test descriptions belong on this shard. `shardIndex` is 0-based and must satisfy `0 <= shardIndex < totalShards`.
  - Zel has no JUnit filter factory and does not partition a suite by `Description`. It only spawns one process per shard and sets `TEST_TOTAL_SHARDS` and `TEST_SHARD_INDEX`, then checks that the runner touched `TEST_SHARD_STATUS_FILE`.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/sharding/api/WeightStrategy.java`
- Bazel lines: 29
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines the public Java interface `com.google.testing.junit.runner.sharding.api.WeightStrategy` for JUnit sharding filters. Zel has no `junitrunner` tree and no `WeightStrategy` type.
  - The only method is `int getDescriptionWeight(org.junit.runner.Description description)`, which must return the integer weight stored on that test description. Zel never reads a weight off a JUnit `Description`.
  - The contract is per-test weighting so a sharding filter can balance tests by weight. Zel sharding is process-level only: `shard_count`, `--test_sharding_strategy` (`explicit`, `disabled`, `forced=k`), and a `TEST_SHARD_STATUS_FILE` touch check. It does not partition individual tests by weight.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/sharding/testing/FakeShardingFilters.java`
- Bazel lines: 78
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `junitrunner` Java sources and no `FakeShardingFilters`, `ShardingFilters`, or `ExplicitDescriptionFilter`.
  - Bazel’s test double subclasses `ShardingFilters`, takes the descriptions that should run as varargs, and calls `super(null, null)`.
  - It copies those descriptions into an unmodifiable `HashSet` and `createShardingFilter` returns a filter built from the suite’s descriptions plus that set.
  - `shouldRun` always returns true for suites, throws `IllegalArgumentException` with `"Not in the suite: "` plus the description when the description is not in the suite, and otherwise returns whether the description is in the explicit run set.
  - `describe()` returns the fixed string `"explicit description filter"`.
  - Zel’s sharding tests (`/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`, `/workspace/zel-cursor/crates/zel-server/tests/build_test_run_coverage.rs`) only cover process shards: `TEST_TOTAL_SHARDS`, `TEST_SHARD_INDEX`, `TEST_SHARD_STATUS_FILE`, `--test_sharding_strategy`, and the missing-status failure. They do not select individual JUnit `Description`s.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/sharding/testing/RoundRobinShardingFilterFactory.java`
- Bazel lines: 34
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel provides a final test helper, `RoundRobinShardingFilterFactory`, in package `com.google.testing.junit.runner.sharding.testing`, that implements `ShardingFilterFactory`.
  - `createFilter(Collection<Description> testDescriptions, int shardIndex, int totalShards)` returns `new RoundRobinShardingFilter(testDescriptions, shardIndex, totalShards)`.
  - Zel has no `junitrunner` Java sources and no sharding filter factory or round-robin sharding test that builds a JUnit `Filter` from descriptions, shard index, and total shard count.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/sharding/testing/ShardingFilterTestCase.java`
- Bazel lines: 286
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Abstract JUnit 3 base (`ShardingFilterTestCase`) for every sharding-filter test. Subclasses supply `createShardingFilterFactory()`. It builds one `Filter` per shard with `createFilter(descriptions, shardIndex, numShards)` over six descriptions `Test.test0` through `Test.test5`.
  - Requires a complete partition: across all shards, each description runs exactly once (same multiset, order ignored). Checked for 1 shard, 5 shards (more tests than shards), 6 shards (equal), and 7 shards (more shards than tests).
  - Same exact-once check with duplicate descriptions: the six descriptions are listed twice (12 entries) and split across 7 shards, so identical descriptions are counted by frequency, not uniqueness.
  - Requires stable assignment: two simulations of the same filters produce equal filter-to-description maps. A second helper rotates a deque so each shard would see a different order, then still walks the original list, so that path does not actually reorder `shouldRun` checks.
  - A suite description (`testSuite` containing a child suite) must pass `shouldRun` for shard 0 of 1. Suites are not partitioned as tests.
  - Unused-by-these-methods helper `assertThrowsExceptionForUnknownDescription`: `shouldRun` on `Object.unknown` must throw `IllegalArgumentException`.
  - Zel only tests process-level sharding (`TEST_SHARD_INDEX` / `TEST_TOTAL_SHARDS`, attempt counts, `TEST_SHARD_STATUS_FILE` / `NO STATUS`, coverage merge) in `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`, `/workspace/zel-cursor/crates/zel-server/tests/build_test_run_coverage.rs`, and `/workspace/zel-cursor/crates/zel-exec/tests/coverage.rs`. It does not assign JUnit `Description`s to shards or check partition, duplicate frequency, suite inclusion, or unknown-description rejection.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/sharding/testing/StubShardingEnvironment.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines `StubShardingEnvironment` in `com.google.testing.junit.runner.sharding.testing`, extending `ShardingEnvironment`. Zel has no `junitrunner` package and no `ShardingEnvironment` type.
  - `shardingEnabled` defaults to `false`. `isShardingEnabled()` returns that field, and `setIsShardingEnabled(boolean)` writes it. Zel has no equivalent controllable test stub.
  - `touchShardFile()` is an empty no-op. Zel has no matching override.
  - `getShardIndex()`, `getTotalShards()`, and `getTestShardingStrategy()` each throw `UnsupportedOperationException`. Zel does not implement those methods.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/CharEscaper.java`
- Bazel lines: 183
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`xml_escape_attr`, used by `xml_write`) and `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`synthesize_xml`). The XML safe range and replacement table that this class applies are configured in `/workspace/bazel/src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/XmlEscapers.java` and used by `XmlWriter`.
- Verdict: partial
- Differences:
  - Bazel walks UTF-16 code units (`length` / `charAt`). A supplementary character is two units, each tested on its own. With the XML safe max `\uFFFD`, both surrogates (`U+D800`–`U+DFFF`) are inside the safe range, so they are copied through, including unpaired surrogates. `xml_escape_attr` iterates `s.chars()` (Unicode scalar values); a non-BMP character is one `char` and is emitted as UTF-8. `&str` cannot hold an unpaired surrogate. `synthesize_xml` uses `str::replace` on the same UTF-8 string.
  - A unit is left unchanged only when it has no map entry and `safeMin <= c <= safeMax` (inclusive). `safeMax < safeMin` makes the range empty, so every unit is unsafe. `escape(String)` returns the original string instance when nothing needs escaping, and throws `NullPointerException` on null. The XML setup uses `\u0000`–`\uFFFD`, so only `\uFFFE` and `\uFFFF` are out of range; `escapeUnsafe` replaces each with the single character `U+FFFD`, not a character reference. A null unsafe replacement passes those units through; `""` deletes them. Zel has no safe window and no `U+FFFD` substitution: `xml_escape_attr` copies `\uFFFE`, `\uFFFF`, `U+FFFD`, and other noncharacters unchanged, and turns other C0 controls into decimal references (`&#0;` … `&#31;`). `synthesize_xml` leaves controls and those noncharacters raw.
  - Replacements are a `char[][]` indexed by code unit, sized to `max(key)+1`. An explicit entry wins inside or outside the safe range. A zero-length replacement deletes that unit. The array is copied as-is and not escaped again. An empty map is a zero-length array; a null map throws. XML content replacements are `&` → `&amp;`, `<` → `&lt;`, `>` → `&gt;`, and C0 controls other than tab, LF, and CR → `U+FFFD`. The attribute escaper adds `'` → `&apos;`, `"` → `&quot;`, tab → `&#x9;`, LF → `&#xA;`, CR → `&#xD;`. `XmlWriter` writes attributes in single quotes and runs element text through the content escaper. `xml_escape_attr` is a fixed match: `&`, `<`, `>`, `"`, LF → `&#10;`, CR → `&#13;`, tab → `&#9;` (decimal, not hex). It does not escape `'`, and query XML only escapes attribute values inside double quotes; names and element text are not escaped. `synthesize_xml` replaces only `&`, `<`, `>`, and `"` in double-quoted attributes. Its log is CDATA with `]]>` split to `]]>]]&gt;<![CDATA[`, so `<`, `&`, quotes, controls, and noncharacters in the log are not escaped or replaced with `U+FFFD`.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/CurrentRunningTest.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel exposes `CurrentRunningTest.get()`, which returns the JUnit `Description` of the test running on the calling thread when Bazel's runner has installed a static `TestNameProvider`, and `null` otherwise (for example a test started directly from an IDE).
  - The provider is installed only by Bazel's runner, which registers a `JUnitCore` `RunListener` for test start and stop. The listener stores the description in a `ThreadLocal` on `testStarted` and clears it on `testFinished`. This class itself only forwards to that provider and is documented as JUnit 3 and JUnit 4 compatible.
  - Zel has no Java junit runner and no API that looks up the current test case for the calling thread. `crates/zel-exec/src/test_runner.rs` only launches whole test processes through `test-setup.sh` (shards, attempts, timeouts, XML) and never records which test method is running inside a process.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/Factory.java`
- Bazel lines: 20
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines a public generic marker `Factory<T>` in `com.google.testing.junit.runner.util` that extends `java.util.function.Supplier<T>` and adds no methods of its own, so the only contract is the inherited `T get()`.
  - The JUnit runner uses that type for dependency-injection factories. `ProvideXmlStreamFactory` implements `Factory<OutputStream>`, and its `create(Supplier<JUnit4Config>)` returns `Factory<OutputStream>`.
  - Zel has no `junitrunner` Java tree and no `Factory<T>` / `Supplier<T>` marker. Test execution lives in `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (environment, attempts, `XML_OUTPUT_FILE`, fallback `test.xml`), which does not expose this factory type.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/FakeTestClock.java`
- Bazel lines: 70
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs` (`ManualClock`)
- Verdict: partial
- Differences:
  - Bazel stores two clocks: monotonic `Duration` (starts at `Duration.ZERO`) and a wall `Instant` offset (starts at `Instant.EPOCH`). `wallTime()` is `wallTimeOffset.plus(monotonic)`. `now()` returns the parent `TestInstant` pair. Zel’s `ManualClock` keeps one `u64` nanosecond counter. `nanos()` is that counter. `millis()` is the fixed base `1_704_164_645_000` (2024-01-02 03:04:05 UTC) plus `nanos / 1_000_000`. There is no separate monotonic `Duration` and no settable wall offset.
  - Bazel `advance(Duration)` adds only to the monotonic counter and returns `this`. `setWallTimeOffset(Instant)` replaces the wall offset so wall time can diverge from monotonic time. Zel `advance(Duration)` adds `d.as_nanos() as u64` onto that single counter (truncating cast, wrapping add) and returns nothing. Wall millis always tracks the same elapsed nanos.
  - Bazel `now()` is synchronized and calls `advance(autoIncrementStep)` before `super.now()`. `autoIncrementStep` is private, starts at `Duration.ZERO`, and this file has no `setAutoIncrementStep` even though the comment names it, so a read does not move time unless `advance` was called. Zel does not auto-increment on read.
  - Bazel synchronizes `advance` and `now`. `setWallTimeOffset` is unsynchronized. Zel guards the counter with a `Mutex`.
  - Zel `ManualClock` implements `UiClock` for UI tests. It does not implement the junit-runner `TestClock` (`monotonicTime` / `wallTime` / `TestInstant`).

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/MemoizingSupplier.java`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel implements `MemoizingSupplier<T>` as a `java.util.function.Supplier<T>` that stores a delegate and, on the first successful path through `get()`, caches that call’s result for every later `get()`. Zel has no supplier-memoizing type (no `java_tools` / junit-runner util, and no generic supplier cache).
  - Thread safety is double-checked locking: a `volatile boolean initialized` plus `synchronized (this)`, so the delegate is invoked at most once. `initialized = true` is written before `instance = delegate.get()`, so another thread can observe `initialized` and read `instance` before that write is published.
  - A null return is cached: the boolean, not a null `instance`, marks completion, so a deliberate null is returned again instead of recomputing.
  - Exceptions are not retried. If `delegate.get()` throws, `initialized` is already true and `instance` is still unset, so the exception escapes once and later `get()` calls return null without calling the delegate again.
  - The supplier is documented as not serializable (it does not implement `Serializable`).

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/TestClock.java`
- Bazel lines: 109
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (test timing only; no `TestClock`)
- Verdict: partial
- Differences:
  - Bazel keeps two clocks on one sample: `wallTime()` is `Instant.ofEpochMilli(System.currentTimeMillis())` (epoch millis, not monotonic) and `monotonicTime()` is `Duration.ofNanos(System.nanoTime())` (monotonic, not a wall time). `now()` stores both on `TestInstant`. Zel has no paired sample. Attempt length is `std::time::Instant::now()` / `elapsed().as_millis()` around the spawn only (`run_attempt`). The reported start is one `SystemTime::now().duration_since(UNIX_EPOCH)` taken once at the start of `run_test_in`, before runs, shards, and attempts.
  - On Linux both monotonic sources are `CLOCK_MONOTONIC` and both wall sources are `CLOCK_REALTIME`, so the clock kind matches. Reported times still diverge because Zel does not read them together. Every BEP attempt start is that single early `start_time_ms`, and the summary end is `start + max(attempt duration_ms)`, not a wall reading at the end. A realtime step (NTP) changes Bazel's reported start/end millis (`getStartMillis` / `getEndMillis` from the wall `Instant`) while the duration stays the `nanoTime` delta. Zel's duration stays monotonic too, but its end timestamp does not move with the wall clock.
  - Bazel durations stay in nanoseconds until `Duration.toMillis()` (truncate toward zero) on the monotonic delta. Zel drops sub-millisecond time immediately with `as_millis() as u64`. Both can report 0 for a sub-millisecond run. Java `Duration` from `nanoTime` may be negative; Rust `Instant::elapsed()` is non-negative.
  - Wall millis: `currentTimeMillis` can be negative before the epoch. Zel maps a pre-epoch `SystemTime` to 0 (`unwrap_or(0)`), and the cache-status reload uses `max(0)`, so a pre-epoch start is reported as 0.
  - Suite duration is `times.iter().max()`, not the monotonic span from the first sample to the last. The BEP summary total is the sum of attempt durations. Those two reported numbers can disagree with each other and with `end - start`.
  - `synthesize_xml` takes `duration_ms` and ignores it (`let _ = duration_ms`), writing `duration="0"` and `time="0"`.
  - Bazel also has an injectable `TestClock`, a process-wide `systemClock()`, and `TestInstant.UNKNOWN` (`Instant.EPOCH`, `Duration.ZERO`). Zel has none of those.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/TestIntegration.java`
- Bazel lines: 283
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel models an external link attached to a test result (`TestIntegration`) with nine fields: contact email, component id, name, URL, icon URL, icon name, description, foreground color, and background color. Zel has no type, builder, or export API for that link.
  - `ExternalLinkAttribute` maps each field to an XML attribute name (`name`, `url`, `contact_email`, `component_id`, `description`, `icon_name`, `icon_url`, `background_color`, `foreground_color`) via `name().toLowerCase()`. Zel does not emit those attributes.
  - `builder()` defaults only the optional icon, description, and color fields to `""`. Contact email, component id, name, and URL stay unset (`null`) until their setters run. `build()` does not require name or URL.
  - `getAttributeValueMap()` returns an `EnumMap` of all nine attributes, including nulls for unset fields. Zel's test XML path (`crates/zel-exec/src/test_runner.rs`, `synthesize_xml`) writes a minimal JUnit document and does not record user properties or external integrations.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/TestIntegrationsExporter.java`
- Bazel lines: 60
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel provides a process-wide `TestIntegrationsExporter.INSTANCE` whose `newTestIntegration(TestIntegration)` forwards an external test-result link into the JUnit runner so it can be written into the test XML. Zel has no exporter, callback, or `TestIntegration` type.
  - The only storage hook is an injectable `Callback.exportTestIntegration`. The protected constructor exists so test infrastructure can supply a fake. `DefaultCallback` does not write XML itself; it calls `TestIntegrationsRunnerIntegration.getCallbackForThread()` and forwards the same object. With no runner installed, that thread-local callback is a no-op, so the export is dropped.
  - The object carries name, URL, contact email, component id, description, icon name, icon URL, and foreground/background colors (`TestIntegration.ExternalLinkAttribute`). The runner stores it on the test-case node (`TestCaseNode.exportTestIntegration`). Zel never records those fields.
  - Zel’s fallback writer, `synthesize_xml` in `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`, emits a fixed `testsuites` / `testsuite` / `testcase` document (optional `error`, plus `system-out` from the log). It has no properties or external-link elements. `parse_test_xml` in `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs` keeps suite/case name, time, classname, status, and failure/error/skipped counts, and skips every other element.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/TestIntegrationsRunnerIntegration.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel keeps a per-thread `ThreadLocal<Callback>` that starts as `NoOpCallback.INSTANCE`, so `exportTestIntegration` is always callable.
  - `setTestCaseForThread` swaps that callback and returns the previous one. A null argument calls `ThreadLocal.remove()` so the next get falls back to the no-op.
  - `getCallbackForThread` returns only the callback installed on the current thread. A TODO notes this fails when the test runs on a different thread than the runner.
  - `NoOpCallback.exportTestIntegration` ignores the `TestIntegration`. The real callback is installed by the JUnit runner (`TestSuiteModel`) and consumed by `TestIntegrationsExporter`.
  - Zel has no Java JUnit runner and no thread-local export of a `TestIntegration` into the current test case. Its only test hook (`--test_force_command_panic` in `/workspace/zel-cursor/crates/zel-server/src/daemon.rs`) forces a command panic for daemon isolation. `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` only runs tests and reads `test.xml`.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/TestNameProvider.java`
- Bazel lines: 28
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines the public interface `com.google.testing.junit.runner.util.TestNameProvider`, whose contract is to expose the description of the test that is currently running.
  - Its only method is `Description get()`, returning `org.junit.runner.Description` for that current test.
  - Zel has no `junitrunner` sources and no type that supplies the JUnit `Description` of the test in progress.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/TestPropertyExporter.java`
- Bazel lines: 166
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel exposes a process-wide `TestPropertyExporter.INSTANCE` whose `DefaultCallback` forwards `exportProperty` and `exportRepeatedProperty` to the per-thread runner callback from `TestPropertyRunnerIntegration.getCallbackForThread()`, so a test can push name/value pairs into the JUnit XML only from the thread running that test. Zel has no exporter, callback, or thread binding.
  - `exportProperty(name, value)` stores one value per name (the fake `Map` overwrites the same key). `exportRepeatedProperty` appends another value and returns the stored key. Zel never records test properties.
  - Repeated names are `name` plus an index starting at `INITIAL_INDEX_FOR_REPEATED_PROPERTY` (1000): the first export of a name is `name1000`, the next `name1001`, and the per-name counter is incremented under a lock. Zel has no repeated-property naming.
  - `createFake(Map)` and `createFake(Callback)` build an exporter that writes into a caller-supplied map or callback instead of the runner. Zel has no test double for this API.
  - The class does not validate names or write XML itself; it only delegates. The documented `IllegalArgumentException` for an invalid name lives in the runner callback, not here.
  - Nearest Zel XML writers only emit a single `testsuites`/`testsuite`/`testcase` report (optional `error` and `system-out`): `synthesize_xml` in `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` and `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/test/generate-xml.sh`. Neither writes `<property>` elements. `parse_test_xml` in `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs` skips unknown elements, and `TestCase` has no property fields.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/TestPropertyRunnerIntegration.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel keeps a `ThreadLocal<TestPropertyExporter.Callback>` whose initial value is a process-wide `NoOpCallback`. `setTestCaseForThread` swaps that callback for the calling thread, returns the previous one, and `null` calls `ThreadLocal.remove()` so the next lookup falls back to the no-op. `getCallbackForThread` is package-private and is what `TestPropertyExporter.DefaultCallback` calls.
  - The no-op `exportProperty(name, value)` drops the pair. `exportRepeatedProperty(name, value)` returns `name` unchanged and does not append a numeric suffix or store a value.
  - `/workspace/bazel/src/java_tools/junitrunner/java/com/google/testing/junit/runner/model/TestSuiteModel.java` installs the current `TestCaseNode` as that callback in `testStarted`, so a test thread can write properties into the JUnit XML for the case that is running.
  - Zel has no `junitrunner` Java tree and no per-thread test-property hook. `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` only launches a test process, sets `XML_OUTPUT_FILE`, and can write a fallback `test.xml` when the test does not. It never records a current test case or accepts `exportProperty` / `exportRepeatedProperty` calls from inside a running test.

### `src/java_tools/junitrunner/java/com/google/testing/junit/runner/util/XmlEscapers.java`
- Bazel lines: 141
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`xml_escape_attr`) and `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`synthesize_xml`)
- Verdict: partial
- Differences:
  - Bazel builds two escapers from one map. Shared rules: `&` `<` `>` become `&amp;` `&lt;` `&gt;`; every C0 control in `U+0000`–`U+001F` except tab, LF, and CR becomes the literal character U+FFFD (not a character reference and not deleted). The content escaper stops there, so `'`, `"`, tab, LF, and CR are copied raw (`XmlWriter.writeCharacters`). The attribute escaper adds `'` → `&apos;`, `"` → `&quot;`, tab → `&#x9;`, LF → `&#xA;`, CR → `&#xD;` (uppercase hex, no leading zeros). `XmlWriter.writeAttribute` then wraps that result in single quotes, so the apostrophe escape is required for well-formedness.
  - Query XML only has attribute escaping, and only inside double quotes (` {k}="{escaped}"`). There is no content escaper. `'` is left raw. Tab, LF, and CR become decimal `&#9;` `&#10;` `&#13;`, not `&#x9;` `&#xA;` `&#xD;`. Every other scalar below `U+0020`, including NUL, becomes `&#N;` (decimal). Bazel replaces those controls with U+FFFD in both content and attributes.
  - `synthesize_xml` uses one replacer for attribute text (`name`, `message`): only `&` `<` `>` `"`. Apostrophes, tab, LF, CR, and other C0 controls stay literal inside double-quoted attributes, so a newline or NUL in a name or message is not escaped. The log is not passed through a content escaper; it is placed in CDATA after only splitting `]]>` into `]]>]]&gt;<![CDATA[`. Characters illegal in XML 1.0 Char (C0 controls other than tab/LF/CR) therefore remain in the CDATA, where Bazel’s content escaper would have written U+FFFD. `&` and `<` inside the log are also left raw, which CDATA allows and the content escaper would not.
  - UTF-16: Bazel’s `CharEscaper.escape` walks Java `char` code units. The safe range is inclusive `U+0000` through `U+FFFD`; only `U+FFFE` and `U+FFFF` fall outside it and each becomes one U+FFFD. Surrogates `U+D800`–`U+DFFF` are inside the safe range and have no explicit replacement, so supplementary characters and unpaired surrogates are copied unchanged. U+FFFD itself is safe and is not rewritten. Neither Zel function maps `U+FFFE` or `U+FFFF` to U+FFFD. Zel iterates Unicode scalars (`str::chars`), so it cannot preserve or pass through an unpaired surrogate the way the Java escaper does.

### `src/java_tools/junitrunner/javatests/com/google/testing/coverage/JacocoLCOVFormatterUninstrumentedTest.java`
- Bazel lines: 211
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `JacocoLCOVFormatter` and no test of uninstrumented class path matching. Closest coverage tests (`/workspace/zel-cursor/crates/zel-exec/tests/coverage.rs`, `/workspace/zel-cursor/crates/zel-server/tests/build_test_run_coverage.rs`) cover LCOV merge, filter, and report location, not this formatter.
  - Bazel drives a mocked bundle (`Foo` / package `com/example` / source `Foo.java`) through `createVisitor` + `visitBundle` + `visitEnd` and checks the writer text.
  - Simple Unix exec path `/parent/dir/com/example/Foo.java` must appear in the report.
  - Simple Windows exec path `C:/parent/dir/com/example/Foo.java` must appear in the report.
  - Mapped Unix path `/some/other/dir/Foo.java////com/example/Foo.java` must emit the prefix before `////` (`/some/other/dir/Foo.java`).
  - Mapped Windows path `C:/some/other/dir/Foo.java////com/example/Foo.java` must emit `C:/some/other/dir/Foo.java`.
  - An exec path that does not match the class (`/path/does/not/match/anything.txt`) must produce empty output.
  - An empty exec-path set must produce empty output.
  - The no-arg constructor (no exec paths) must not prune: output is non-empty and uses the class path as-is.
  - An exact exec path `com/example/Foo.java` must appear in the report.
  - This Bazel test does not assert LCOV record lines (`SF`/`DA`/`FN`/`BRDA`) or a charset. Zel also has no test of those for this formatter.

### `src/java_tools/junitrunner/javatests/com/google/testing/coverage/TestClass.java`
- Bazel lines: 22
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Public class `com.google.testing.coverage.TestClass` with `public static void main(String[] args)` whose only statement prints `Coverage run success!` to stdout.
  - No branches: no conditionals, loops, switches, or exception handlers. The single executable line is that `println`.
  - It is the only source of `java_library` `jar+with+plus`, so the jar path contains `+`. `jacoco_runner_integration_tests.sh` sets `JACOCO_MAIN_CLASS` to this class, runs `JacocoCoverageRunner` with that jar, and requires the success log and no `java.nio.file.NoSuchFileException`.
  - Zel has no `junitrunner` Java tree, no `TestClass`, and no fixture that prints that string or exercises a Jacoco main class packaged in a jar whose name contains `+`.

### `src/java_tools/junitrunner/javatests/com/google/testing/coverage/jacoco_runner_integration_tests.sh`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel requires `TEST_SRCDIR` and exits 1 when it is unset. Zel has no test of that guard.
  - Bazel loads a unit-test framework from `$1`, then runs the suite `deploy_jar` with `errexit` off. Zel has no such shell suite.
  - Bazel resolves the JaCoCo runner from `$2` and a jar whose path contains `+` from `$3`, both under `$PWD`.
  - Bazel launches `"${JACOCO_RUNNER} --main_advice=com.google.testing.coverage.JacocoCoverageRunner"` with `--main_advice_classpath` set to that plus-named jar.
  - Bazel exports `JACOCO_MAIN_CLASS=com.google.testing.coverage.TestClass` for that run.
  - Bazel requires the process to succeed, the log to omit `java.nio.file.NoSuchFileException` (the `+` decoded as a space), and the log to contain `Coverage run success!`.
  - The script does not check `.exec` bytes, LCOV records, or path rewriting. Zel’s coverage tests (`/workspace/zel-cursor/crates/zel-exec/tests/coverage.rs`, `/workspace/zel-cursor/crates/zel-client/tests/exec_e2e.rs`, `/workspace/zel-cursor/crates/zel-server/tests/build_test_run_coverage.rs`) only cover shell `collect_coverage` and LCOV merge or combined reports, not `JacocoCoverageRunner` or a `+` in a jar name.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/junit4/runner/RegExTestCaseFilterTest.java`
- Bazel lines: 126
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel `RegExTestCaseFilter` always runs suite descriptions (`include("doNotMatch")` still returns true for a suite that has a child test). Zel has no JUnit description filter.
  - Include matches the formatted `class#method` name with `Pattern.matcher(...).find()`, so a substring hits: quoted `java.lang.Object#nameToMatch`, unanchored `Object#nameToMatch`, and bare method `nameToMatch` all run `java.lang.Object#nameToMatch`. Zel does not match test names.
  - A fully anchored pattern `^java.lang.Object#nameToMatch$` also matches that same `class#method` string. Zel does not compile or anchor a test regex.
  - A null method name falls back to the display name: `java.lang.Object$` includes `Description.createSuiteDescription(Object.class)`. An unexpected name with no `#` is matched as the raw display name (`Pattern.quote("java.lang.Object.hashCode()")`). Zel does not format or special-case those names.
  - `exclude("nameToMatch")` drops a matching test; `include("doNotMatch")` drops `nameToMatch`. Zel has no include/exclude negation on test cases.
  - `include("")` runs every non-suite test (empty pattern `find()` succeeds). Zel only exports an empty `--test_filter=` as `TESTBRIDGE_TEST_ONLY=` and does not treat it as a match-all regex.
  - Regex syntax is real and case-sensitive: `[Nn]ameToMatch` matches, `java\\.lang\\.Object#nameToMatch` matches, `NAMETOMATCH` does not. Zel forwards the filter string unchanged and never evaluates it.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/ProgramThatCallsSystemExit.java`
- Bazel lines: 32
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel’s fixture is a final `ProgramThatCallsSystemExit` with a private constructor. `main` builds `SystemExitDetectingShutdownHook.newShutdownHook(System.err)`, registers that thread with `Runtime.addShutdownHook`, then calls `System.exit(0)` so the hook runs during JVM shutdown.
  - The hook scans every thread for a `java.lang.Runtime.exit` frame, prints `System.exit or Runtime.exit was called!` plus the stack from that frame to stderr, and `Runtime.halt(121)` so the process does not return 0. `system_exit_detecting_test.sh` runs this jar and checks both the exit code 121 and a stack trace that ends in `ProgramThatCallsSystemExit.main`.
  - Zel has no junitrunner Java tree, no `SystemExitDetectingShutdownHook`, and no program that calls `System.exit` to force a shutdown-hook stack trace. `/workspace/zel-cursor/crates/zel-server/tests/build_test_run_coverage.rs` (`test_run_test_target_full_env_and_setup_script`) only asserts that `TEST_PREMATURE_EXIT_FILE` is present. `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` sets that variable to `test.exited_prematurely` and lists the file as a declared output. `/workspace/zel-cursor/crates/zel-exec/resources/test-setup.sh` only makes the path absolute. None of those install a JVM shutdown hook, print a `System.exit` stack trace, or fail the process with exit code 121.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/TestListener.java`
- Bazel lines: 121
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines a package-private JUnit 4 `RunListener` (`com.google.testing.junit.runner.TestListener`) that prints each test-state change. Zel has no `junitrunner` Java tree and no `RunListener` that records or prints these callbacks.
  - `testRunStarted` prints `Found N test case.` / `Found N test cases.` from `Description.testCount()`, with singular wording only for 1.
  - `testRunFinished` prints a success line (`Successfully finished running … in <runtime> ms.`) or a failure line (`Finished running … in <runtime> ms.`), then `There was 1 failed test.` or `There were N failed tests.` from `Result.getFailureCount()`.
  - After the run, an ignore count of 1 prints `1 test case was ignored.`; a count greater than 1 prints `N test cases were ignored.`; zero ignores print nothing.
  - `testStarted` prints `Test case started: ` plus `Description.getDisplayName()` to stdout.
  - `testFailure` prints `Failure in <header>: <message>\n<stack trace>` to stderr via `Failure.getTestHeader()`, `getMessage()`, and `getTrace()`.
  - `testAssumptionFailure` prints only `Test case assumed false condition: <message>` to stderr, with no stack trace.
  - `testIgnored` prints `Test case <methodName> ignored.` to stderr from `Description.getMethodName()`.
  - Zel’s test-event path (`crates/zel-events` `BepEvent::TestResult`, `crates/zel-exec/src/test_runner.rs`) records a process-level result and can fall back to `test.xml`. It does not emit these stdout/stderr lines or handle JUnit start, failure, assumption-failure, or ignore callbacks.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/TestRunner.java`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel provides a `main` for the junitrunner's own javatests. With no arguments it throws `IllegalArgumentException` ("Must specify at least one argument (source files of the tests to run)!"). Zel has no `junitrunner` Java tree and no program that accepts those source-file arguments.
  - Each argument is a path under `com.google.testing.junit.runner`: `/` becomes `.`, a trailing `.java` is stripped, and the class is loaded with `Class.forName(PACKAGE + "." + className)`. Zel never maps source paths to classes in that package.
  - All resolved classes are combined into one `Request.classes(...)`, run through `JUnitCore` with a `TestListener`, and the process exits 0 if `Result.wasSuccessful()` and 1 otherwise. Zel's `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` only spawns an already-built test binary (via `test-setup.sh`), records the process exit code, and handles logs, XML, shards, and timeouts. It does not call `JUnitCore` or `Request`.
  - Zel's Starlark `java_test` in `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/build_rules/java_rules_skylark.bzl` defaults `main_class` to stock `org.junit.runner.JUnitCore`, which expects fully qualified class names as arguments and does not do this package-relative path rewrite or this runner's listener.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/antxmlresultwriter_integration_test.sh`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The script runs `XmlOutputExercises` (`-Dbazel.test_suite=...XmlOutputExercises`), allows the expected test failure (`|| true`), and `diff -wu`s `${TEST_TMPDIR}/test.xml` against the golden `XmlOutputExercises.ant.xml`. Zel has no Ant-XML writer and no golden diff for that document.
  - Before the diff, `sed` blanks every single-quoted attribute whose name contains `time` (`time` and `timestamp` become `time=''` / `timestamp=''`) and deletes stack-frame lines. The script therefore locks attribute presence and quoting, not the numeric runtime or the JDK stack text.
  - The golden locks `<?xml version='1.0' encoding='UTF-8'?>`, a bare `<testsuites>`, and one `<testsuite>` per outer class plus each Enclosed inner class. Suite attributes, single-quoted: `name`, `timestamp`, `hostname='localhost'`, `tests`, `failures`, `errors='0'` (assertion failures count as failures, errors stay 0), `time`, `package=''`, and sequential `id` starting at 0. Each suite has an empty `<properties />`, `<system-out />`, and `<system-err />`.
  - Counts and cases are fixed: outer suite `tests='7' failures='1'`; `FailureTest` 1/1; `OtherTests` 1/0 (`testToString`); `EqualsHashCodeTest` 2/0 (`testHashCode`, `testEquals`); `ComparabilityTest` 3/0. Each `<testcase>` has `name`, `classname` (inner class with `$`), and `time`. The failure is `<failure message='This is an expected error. The test is supposed to fail.' type='java.lang.AssertionError'>` plus the exception text.
  - The writer formats suite and case `time` as seconds (`millis/1000.0`, or `0.0` if there is no interval) and `timestamp` as the start instant (or empty). Those values are stripped before the compare.
  - This fixture’s message has no `&`, `<`, `>`, or quotes, so the script does not assert XML escaping. Zel’s only test XML, `synthesize_xml` in `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`, is a fallback when the test writes no `test.xml`: one suite named from the target, `tests="1" failures="0" errors="0|1"`, one case with `classname="" status="run" duration="0" time="0"` (`duration_ms` is ignored), an empty `<error message="...">` instead of `<failure type="...">`, and the log in `<system-out>` CDATA (with `]]>` split). It escapes `& < > "` in the name and message, and it does not emit hostname, timestamp, package, id, properties, system-err, nested suites, or per-method cases.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/deploy_jar_integration_tests.sh`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel integration-tests `_deploy.jar` when it is launched with `java -jar` and no runfiles wrapper. Zel has no junitrunner deploy-jar integration script, and its built-in `java_binary` / `java_test` analysis writes a 0-byte dummy and does not build a deploy jar or wrapper.
  - The script exits immediately if `TEST_SRCDIR` is unset, then sources Bazel’s bash runfiles library (`bazel_tools/tools/bash/runfiles/runfiles.bash`) and the unit-test framework passed as `$1`. Zel has no equivalent harness.
  - It clears `TEST_PREMATURE_EXIT_FILE`, then locates the JDK: if `TEST_WORKSPACE` is `_main` it strips an `external/` prefix from the javabase (`$2`); otherwise it prefixes the javabase with `$TEST_WORKSPACE` (Blaze). The testbed jar is `$PWD/$3`. Zel does not implement this workspace-relative javabase resolution for a deploy-jar test.
  - `test_Warning` runs `java -jar $TESTBED_JAR` with no `-Dbazel.test_suite` and requires the log to match `The test suite Java system property .* is required but missing`. It then requires that process to exit successfully (`wait` failure fails the suite). Zel never emits that warning: there is no `bazel.test_suite` check, and the bundled Starlark `java_test` in `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/build_rules/java_rules_skylark.bzl` only defaults `main_class` to `org.junit.runner.JUnitCore` and launches `java … -jar $DEPLOY` with `jvm_flags`, without injecting or requiring a test-suite system property.
  - The suite is registered as `deploy_jar`. Zel has no `deploy_jar` test suite and no test that a fat jar’s classpath still warns and exits cleanly when the test-suite property is omitted.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/internal/SignalHandlersTest.java`
- Bazel lines: 77
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The fake installer accepts only `TERM` (`sun.misc.Signal("TERM")`). `install` replaces the current handler and returns the previous one. Zel’s `install_handler` in `/workspace/zel-cursor/crates/zel-server/src/sys.rs` calls `signal(2)`, discards the previous handler, and installs one process-wide recorder.
  - `testOneHandlerCanHandleSignal` installs one `TERM` handler and asserts `handle(TERM)` runs when the signal is delivered. Zel records `LAST_SIGNAL` / `SIGNAL_COUNT` only; it has no per-signal `SignalHandler` list.
  - `testHandlersCanBeChained` installs `handler1` then `handler2` on `TERM` and asserts both `handle(TERM)`. `SignalHandlers.installHandler` runs the newest handler first, then the previous handler. The test does not use `InOrder`, so Mockito does not check that order. Zel does not chain handlers: a later `install_handler` for the same signal replaces the earlier one.
  - The installer contract says a handler that throws skips the rest of the chain unless it catches the exception. These two tests do not cover that. Zel has no handler chain to skip.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/internal/junit4/CancellableRequestFactoryTest.java`
- Bazel lines: 216
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no JUnit runner and no `CancellableRequestFactory` tests. The only nearby Zel name is the flag `experimental_cancel_concurrent_tests` (cancel sibling `--runs_per_test` invocations). That is not a JUnit `Request` cancel.
  - This file never calls `cancelRunOrderly()` and never names `ORDERLY_STOP`. Both cancel tests call `cancelRun()`, which the factory maps to `HARD_STOP` (`stop(true)`): `pleaseStop()`, then a `StoppedByUserException` rethrown as `RuntimeException` with message `"Test run interrupted"`.
  - `testCancelRunAfterStarting`: a `RunnerSuite` of a blocking `FakeRunner` and a second runner. The first test counts down a start latch and waits up to 1s. A single-thread executor runs `JUnitCore.run`. After the start latch (1s timeout), `cancelRun()` runs and the continue latch is released. `future.get(10s)` must throw `ExecutionException` whose cause is that `RuntimeException`, caused by `StoppedByUserException`. The `secondTestRan` flag is never asserted.
  - `testCancelRunBeforeStarting`: `cancelRun()` before `JUnitCore.run`. `core.run` throws the same `RuntimeException` / `StoppedByUserException` pair, and the test body stays false.
  - `testNormalRun`: without cancel, the body runs, `runCount` is 1, `failureCount` is 0.
  - `testFailingRun`: the body throws a `RuntimeException`; `runCount` is 1, `failureCount` is 1, and the recorded failure is that same instance. `FakeRunner` also maps `AssumptionViolatedException` to `fireTestAssumptionFailed` and any other `Throwable` to `fireTestFailure`, then always `fireTestFinished`. No test covers assumptions or `ORDERLY_STOP` (swallow `StoppedByUserException`, print it, and return so later tests do not run).

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/internal/junit4/JUnit4TestNameListenerTest.java`
- Bazel lines: 177
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `java_tools` junitrunner and no `JUnit4TestNameListener` or `TestNameProvider` current-test tracking.
  - Bazel installs a listener over `SettableCurrentRunningTest` and asserts the provider starts null.
  - `testRunStarted` on a suite leaves the current name null. `testStarted` sets it to that `Description`. `testFinished` clears it. A second method repeats the set-then-clear cycle. `testRunFinished(null)` leaves it null.
  - Display name is `methodName(canonicalClassName)`, asserted as `testJUnit4Listener_hasExpectedDisplayName(com.google.testing.junit.runner.internal.junit4.JUnit4TestNameListenerTest)`.
  - Current name is thread-local: a worker thread starts null, then sees its own description after `testStarted`, while the caller still sees the first description. Finishing the first description clears only that thread. The worker still sees the second description until it finishes, then both threads are null.
  - Unpaired callbacks: a duplicate `testStarted` for the same description keeps that description. Starting a second description replaces it. `testFinished` of the first description clears the current name even though the second was last started, and finishing the second stays null.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/internal/junit4/JUnit4TestXmlListenerTest.java`
- Bazel lines: 292
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no JUnit 4 `RunListener` (no `JUnit4TestXmlListener`, signal-time XML flush, or `TestSuiteModel` callbacks). Nearby code only sets `XML_OUTPUT_FILE` and, if the process never wrote `test.xml`, synthesizes one suite from the process exit (`/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` `synthesize_xml`, `/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/test/generate-xml.sh`). Process-wrapper SIGTERM kills the process group and does not flush in-process JUnit XML. `test_status.rs` only parses `<skipped/>` and `<failure>` after the fact.
  - On `testRunStarted`, exactly one handler is installed for signal name `TERM` (`sun.misc.Signal("TERM")`). Delivering that signal must write UTF-8 stderr that contains both `SIGTERM` (`Signal.toString()`) and `Done writing test XML`. The stderr `PrintStream` is created with charset `UTF-8` and auto-flush; the check is `ByteArrayOutputStream.toString("UTF-8")`. The asserted substrings are ASCII, so UTF-16 code units versus Unicode scalars would not change them.
  - That same SIGTERM handler must call, in order, `cancelRun()`, then `testRunInterrupted()`, then `writeAsXml` on the listener’s XML stream. The test does not assert the failure branch (`Could not write test XML` plus a stack trace).
  - After a passing empty `@Test` run through `JUnitCore`, stderr length is 0 bytes (`errStream.size()`, not a character count), `writeAsXml` is invoked once on that stream, and `cancelRun` is never called.
  - A test method that hits `assumeTrue(false)` is reported as skipped on that method’s `Description` (first child of the suite), then `writeAsXml`, with empty stderr and no `cancelRun`.
  - A `@BeforeClass` `assumeTrue(false)` is reported as skipped on the suite `Description` itself, not on the child test, then `writeAsXml`. That covers the JUnit 4 `ParentRunner` behavior of firing `testIgnored` for a suite skipped by an assumption when the suite has no `@Ignore`.
  - `Assert.fail()` is reported as `testFailure` with that method’s `Description` and any `Throwable`, then `writeAsXml`. The test does not require a particular exception type or message.
  - `@Test` plus `@Ignore` is reported as `testSuppressed` on that method’s `Description` (not `testSkipped`), then `writeAsXml`.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/internal/junit4/MemoizingRequestTest.java`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no JUnit 4 `Request` wrapper and no `MemoizingRequest` (no `junitrunner` tree under `/workspace/zel-cursor`). Nothing constructs a request around a delegate without calling that delegate.
  - `testConstructorDoesNoWork`: constructing `MemoizingRequest` must not invoke any method on the wrapped `Request`.
  - `testMemoizesRunner`: the first `getRunner()` calls the delegate’s `getRunner()` once and caches the `Runner`; a second `getRunner()` returns that same instance and does not touch the delegate again. The production method is `synchronized` and calls `createRunner` only while the cache is null.
  - `testOverridingCreateRunner`: a subclass can replace `createRunner(Request)` so both `getRunner()` calls return that stub instance and the delegate is never called. Default `createRunner` is `delegate.getRunner()` and is package-visible so tests and subclasses can override it. Rust has no equivalent virtual hook on a JUnit request type.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/junit4/JUnit4BazelMock.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines a final `JUnit4BazelMock` whose `builder()` returns a `Builder`. Zel has no `java_tools` JUnit runner sources and no matching test double.
  - `Builder` extends `JUnit4Bazel.Builder<Builder>`.
  - `testModule(JUnit4RunnerTest.TestModule)` throws `NullPointerException` on null, assigns the module to `this.module`, and returns `this`. The method is annotated `@CanIgnoreReturnValue`.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/junit4/JUnit4ConfigTest.java`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - This test only asserts `JUnit4Config.getJUnitRunnerApiVersion()`. It never reads `XML_OUTPUT_FILE` and never mentions `file.encoding`, `Charset.defaultCharset()`, or `sun.jnu.encoding`. Zel’s locale-derived `sun.jnu.encoding` string in `info` (`/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`) and the `XML_OUTPUT_FILE` env set in `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` are a different layer; Rust has no JVM system properties.
  - Property `com.google.testing.junit.runner.apiVersion` equal to `"1"` must return `1`. Both tests set that property to `"1"` (the test does not omit the property). Zel has no JUnit runner API-version property or parser.
  - A non-numeric value (`"I love pesto"`) must throw `IllegalStateException` whose message contains that string.
  - A non-integer (`"3.14"`) must throw `IllegalStateException` whose message contains `"3.14"`.
  - An integer other than 1 (`"13"`) must throw `IllegalStateException` whose message contains `"13"`. Zel has no “must be exactly 1” rule.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/junit4/JUnit4OptionsTest.java`
- Bazel lines: 128
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/parser.rs` (CLI `--test_filter` only). Env is written later in `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`.
- Verdict: partial
- Differences:
  - Empty argv leaves the include filter null and the unparsed-args list empty. Zel’s default `test_filter` is `None`, but it has no unparsed-args list; non-flags become `targets`.
  - `--bar baz` leaves the include filter null and keeps those tokens, in order, as unparsed args. Zel rejects `--bar` as an unrecognized option and never records an unparsed-args array.
  - `--test_filter foo` and `--test_filter=foo` both set the include filter to `foo` and leave unparsed args empty. Zel accepts both forms and stores `foo`.
  - `--bar --test_filter=foo --baz` sets the include filter to `foo` and keeps unparsed args as `--bar`, then `--baz`. Zel fails on the first unknown flag and does not skip unknown tokens around a known filter.
  - `--test_filter=foo --test_filter=bar` keeps only `bar`. Zel also last-wins (`allow_multiple` is false; `opt_last` pops the last raw value).
  - A bare `--test_filter` with no value throws `RuntimeException`. Zel rejects it too, as `OptionsParsingError` (`Expected value after --test_filter`), not a runner exception.
  - `--test_exclude_filter foo` and `--test_exclude_filter=foo` set the exclude filter to `foo`. Zel has no `test_exclude_filter` flag or exclude-filter field.
  - `--unknown=foo` is retained as an unparsed arg and is not an error. Zel returns an unrecognized-option error.
  - `TESTBRIDGE_TEST_ONLY=foo` with an empty argv sets the include filter to `foo`. Zel’s parser never reads that variable; it only writes `TESTBRIDGE_TEST_ONLY` from an already parsed `--test_filter` when launching a test.
  - `--test_filter=bar` overrides `TESTBRIDGE_TEST_ONLY=foo`. Zel has no env-versus-flag override, because it does not parse that variable as input.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/junit4/JUnit4RunnerTest.java`
- Bazel lines: 550
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (generic spawn only; no JUnit4 runner)
- Verdict: partial
- Differences:
  - Bazel applies an include regex (`testThatAlwaysFails`, and `testThatAlways(Passes|Fails)`) inside the runner and runs only matching JUnit descriptions. Zel copies `--test_filter` into `TESTBRIDGE_TEST_ONLY` and never selects cases.
  - Bazel intersects an exclude regex (`CallsSystemExit`) with the include filter; with include `test` the sample suite still runs both tests and the failure is `SampleFailingTest.testThatAlwaysFails`. Zel has no `--test_exclude_filter`.
  - An include filter that matches nothing becomes one synthetic failure whose message contains `No tests found` (run count 1, failure count 1). Zel does not turn an unmatched filter into that failure.
  - With sharding on, Bazel builds a sharding filter, calls `touchShardFile`, and runs only descriptions the filter keeps (`testShardingIsSupported`: 2 runs, exactly 1 failure, `getNumTestCases() == 2`). Zel launches `shard_count` processes with `TEST_TOTAL_SHARDS`, `TEST_SHARD_INDEX`, and `TEST_SHARD_STATUS_FILE`, and fails a passing shard only when that file is missing and `shard_count > 1`. It does not shard JUnit descriptions.
  - Include filter and sharding filter are applied together (intersection), then the shard file is touched. A filter plus a shard that contains none of the filtered tests is a success with run/failure/ignore counts all 0, and the shard file is still touched. Sharding with no user filter and a filter that rejects every test is a `No tests found` failure, and the shard file is still touched. Zel does not make that empty-shard pass-versus-fail distinction.
  - `cancelRun()` during `testStarted` aborts with `RuntimeException` message `Test run interrupted` and cause `StoppedByUserException`, after `testRunStarted`, `testStarted`, and `testFinished`. Zel cancellation kills the test process group.
  - Passing and failing runs print `JUnit4 Test Runner` plus JUnit text-listener output, including `AssertionError: expected` and a UTF-8 message `expected:<Test [Japan].> but was:<Test [日\u672C].>`, and listeners see the start/failure/finish order. Zel records the process log and, if the test writes no `test.xml`, synthesizes XML from the exit code.
  - API version property `com.google.testing.junit.runner.apiVersion=2` throws `IllegalStateException` starting with `Unsupported JUnit Runner API version`. Zel has no JUnit runner API version check.
  - This test does not assert `TEST_PREMATURE_EXIT_FILE`. Zel exports that path from `test_runner.rs` and does not fail the run when the file is left behind.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/junit4/JUnit4TestModelBuilderTest.java`
- Bazel lines: 195
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - When sharding is enabled (`isShardingEnabled` true, `getTotalShards` 2), `JUnit4TestModelBuilder.get()` calls `ShardingEnvironment.touchShardFile()` for a two-method JUnit4 class built with `ShardingFilters` and `DEFAULT_SHARDING_STRATEGY`.
  - When sharding is disabled, the same `get()` never calls `touchShardFile()`.
  - A class with a top-level `@Ignore` and two `@Test` methods builds a model with `getNumTestCases() == 0` (the ignored class is not a suite of those methods).
  - A single class with `testOne` then `testTwo` yields exactly one top-level suite node whose description equals `Request.getRunner().getDescription()`, two children in that child order, both leaves with empty children, and `getNumTestCases() == 2`.
  - A `@RunWith(Suite.class)` suite of a one-test class yields one top suite node, one nested suite child, one leaf whose description matches the inner test and whose children are empty, and `getNumTestCases() == 1`.
  - Zel has no JUnit4 runner or `TestSuiteModel` builder. `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` only checks that a sharded action fails if the process does not touch `TEST_SHARD_STATUS_FILE`, and `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs` only parses an already-written `test.xml` tree.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/junit4_testbridge_integration_tests.sh`
- Bazel lines: 164
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`, `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs`, `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - Bazel runs a JUnit4 testbed and checks stdout. With `TESTBRIDGE_TEST_ONLY` unset the suite fails and the log matches `Failures: 2`; `TESTBRIDGE_TEST_ONLY=testPass` passes and the log matches `OK.*1 test`; unsetting the variable again restores `Failures: 2`. Zel only copies `--test_filter` into `TESTBRIDGE_TEST_ONLY`. It has no JUnit filter and no `Failures:` / `OK.*1 test` summary.
  - The same filter is per process. Bazel re-runs the testbed after `declare +x TESTBRIDGE_TEST_ONLY` and expects the full failing suite again. Zel has no runner-side filter state to clear.
  - `test_Junit4ExitCodes` runs only when `SUITE_PARAMETER` is `bazel.test_suite`. Unfiltered `JUnit4TestbridgeOomExercises` exits 137 with `Failures: 2`. `TESTBRIDGE_TEST_ONLY=testFailAssertion` exits 1 with `Failures: 1`. `testFailWithOom` exits 137 with `Failures: 1`. `testPass` exits 0 with `OK.*1 test`. Zel forwards the process exit code and treats 0 as pass and any other code as fail. It does not map an assertion failure to 1 or an `OutOfMemoryError` to 137. A timeout is rewritten to 142.
  - `TESTBRIDGE_TEST_ONLY=testFailOnce` plus the testbed argument `--test_filter testPass` passes. The reverse fails (`Failures: 1`). The JUnit runner uses the command-line `--test_filter` and ignores the environment variable when both are set. Zel never appends `--test_filter` to the test argv. When both exist, Zel’s own `--test_filter` overwrites `TESTBRIDGE_TEST_ONLY` before the test starts. An empty `--test_filter=` still exports `TESTBRIDGE_TEST_ONLY=`.
  - `TESTBRIDGE_TEST_RUNNER_FAIL_FAST` unset or set to `0` yields `Failures: 2`. Set to exactly `1`, the suite stops after the first failure (`Failures: 1`). Zel parses `--test_runner_fail_fast` into the aquery action key and does not export `TESTBRIDGE_TEST_RUNNER_FAIL_FAST` or stop a JUnit suite early.
  - Suite `Junit4UndeclaredTestCaseFailures` fails even when the individual cases pass. The log contains `unnecessary Mockito stubbings`, and `test.xml` matches `tests='2' failures='1'`. Zel’s pass/fail decision uses the process exit code only. `parse_test_xml` reads `<failure>`, `<error>`, and `<skipped>` children and ignores `tests` and `failures` attributes, so a suite counted failed only by `failures='1'` stays `Passed` in the tree. If the test writes no `test.xml`, Zel synthesizes one case with `tests="1"` `failures="0"` and `errors` 0 or 1.
  - After the filtered runs, the script deletes `${TEST_TMPDIR}/test.xml` so a failure report is not picked up as the shell test’s own result.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/model/AntXmlResultWriterTest.java`
- Bazel lines: 167
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Passing cases are separate elements `<testcase name='testCase1'` and `<testcase name='testCase2'` (single-quoted attributes). Zel’s wrapper XML (`synthesize_xml`, `generate-xml.sh`, and the Windows writer) emits one double-quoted `<testcase>` named from the target or test binary.
  - A case left `FILTERED` (created, never started) is omitted; only the completed case is written. Zel has no filtered-case writer and always emits that single wrapper case.
  - `testsuite` `time` is the monotonic duration as Java `String.valueOf(millis/1000.0)`: 1 ms is exactly `0.001` even after the wall clock is moved back one hour. `generate-xml.sh` and the Windows writer store the process duration in whole seconds on both `time` and `duration`. `synthesize_xml` ignores `duration_ms` and writes `time="0"` and `duration="0"`.
  - `testsuite` `timestamp` is the wall-clock start, and `ISO_DATE_TIME` must parse back to `Instant.ofEpochMilli(1560786184600)` (`2019-06-17T15:43:04.600Z`), not a later backward-shifted wall time. Zel’s writers emit no `timestamp`. `test_status.rs` reads only `name` and `time` on `testsuite`.
  - `testsuite` `name` is the JUnit class display name `com.google.testing.junit.runner.model.TestCaseNodeTest$TestSuite` (`$` left raw). Zel names the suite from the binary or target label.
  - A started-then-skipped case and a never-started suppressed case each produce one `testcase` with exactly one empty `<skipped>` child. Suppressed is written; filtered is not. Zel’s writers never emit `<skipped>`.
  - This file does not assert escaping. Every asserted string is ASCII BMP, so the UTF-16 versus Unicode-scalar distinction does not change the expected XML.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/model/TestCaseNodeTest.java`
- Bazel lines: 165
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no JUnit-runner `TestCaseNode` state machine (`pending` / `started` / `finished` / `testFailure` / `testInterrupted` / `testSkipped` / `testSuppressed`). The nearest model, `TestCase` in `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs`, is a parsed `test.xml` tree (`Passed` / `Failed` / `Error` / `Skipped`, one `run_duration_millis`).
  - A node built from a JUnit test `Description` reports `isTestCase() == true`.
  - A node that is never `pending()` has status `FILTERED` and a null run-time interval.
  - After `pending()` only, status is `CANCELLED` and the interval stays null.
  - `pending()` then `testInterrupted` at wall epoch-ms 1 yields `CANCELLED`, start millis 1, duration 0.
  - `pending()` then `testFailure` at that same instant yields `COMPLETED` (not a failure status), start millis 1, duration 0.
  - `pending()` then `started` with no finish or interrupt yields `INTERRUPTED`, start millis 1, duration 0 (marked as an unexpected terminal state).
  - `pending()`, `started` at epoch-ms 1, then `testInterrupted` 1 ms later yields `INTERRUPTED`, duration 1.
  - The same start plus `testSkipped` 1 ms later yields `SKIPPED`, duration 1.
  - The same start plus `finished` 1 ms later yields `COMPLETED`, duration 1.
  - `started`, then `testFailure` at +1 ms, then `finished` at +2 ms stays `COMPLETED`; duration is 2 (start to finish), and the failure does not change the status.
  - `started`, then `testFailure` at +1 ms, then `testInterrupted` at +2 ms yields `INTERRUPTED` (interrupt wins over the earlier failure), duration 2.
  - `pending()` then `testSuppressed` without `started` yields `SUPPRESSED`, start millis equal to that instant, duration 0.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/model/TestInstantUtil.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel’s test helper builds a `TestInstant` from a `java.time.Instant` wall time whose monotonic stamp is `Duration.ofMillis(111111 + wallTime.toEpochMilli())`, so the monotonic value is deliberately not the wall epoch and tests can assert it is not used as absolute time. Zel has no such factory.
  - `advance` shifts both clocks by the same `Duration`: `wallTime().plus(duration)` and `monotonicTime().plus(duration)`. Zel has no paired advance.
  - Java `Instant` is an epoch timeline; `TestInstant.monotonicTime` is a `Duration` (from `System.nanoTime` on the real clock) used only for elapsed time. Zel records attempt length with monotonic `std::time::Instant::elapsed()` (`duration_ms`, also stored as `wall_time_ms` in `zel-cursor/crates/zel-exec/src/test_runner.rs`) and a separate wall `SystemTime` epoch millis once per `run_test_in` (`start_time_ms`). Those are not one value, are not offset by 111111 ms, and `Instant` cannot be turned into wall time the way Java’s pair can.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/model/TestIntervalTest.java`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel stores a pair of `TestInstant`s. `getStartMillis`/`getEndMillis` return `wallTime().toEpochMilli()` (123456 then 234567, and a zero-length interval where both are 123456). Zel keeps a wall epoch on the suite (`start_time_ms` from `SystemTime` in `run_test_in`) and a separate monotonic elapsed on each attempt. It never records both clocks on one interval.
  - `toDurationMillis` is the monotonic delta, not the wall delta: 100 ms for wall 50→150, and 0 when the endpoints are equal. Zel’s attempt `duration_ms` is also monotonic elapsed, but the result duration is `max` of those attempt times.
  - `testToDurationOnNonMonotonicWallTime` freezes the wall clock (start and end both epoch 123456) while monotonic time moves 50 ms → 150 ms. Endpoints stay 123456 and `toDurationMillis` is still 100. Zel has no case that a stuck or rewound wall clock must leave the reported duration equal to the monotonic span. `synthesize_xml` ignores `duration_ms` and always writes `time="0"` and `duration="0"`.
  - Construction throws `IllegalArgumentException` with message `Start must be before end` when monotonic start is after monotonic end. Zel does not reject an inverted interval.
  - `startInstantToString` formats the wall start as UTC ISO-8601 with three fractional digits: epoch `1471709734000` becomes `2016-08-20T16:15:34.000Z`. Zel test timing does not format a test-case start instant that way.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/model/TestSuiteNodeTest.java`
- Bazel lines: 97
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs` (`TestCase` with `CaseType::TestSuite`)
- Verdict: partial
- Differences:
  - A suite is not a test case (`isTestCase()` is false). Building the suite and `addTestCase` does not call the child. Zel only stores `kind = TestSuite` versus `TestCase`; it has no live child registration.
  - `testInterrupted(instant)` is forwarded once to the child with that same instant. Zel has no interrupt event and no clock instant on a suite.
  - `testSkipped(instant)` is forwarded once to the child. Zel sets `Skipped` only when that element itself contains `<skipped>`; a skipped child does not mark the suite skipped.
  - `testSuppressed(instant)` is forwarded once to the child. Zel has no suppressed or ignored status separate from skip.
  - `testFailure(throwable, instant)` is forwarded once, with the same exception and instant. Zel derives `Failed` or `Error` by counting `<failure>` and `<error>` tags after XML parse; it does not keep the throwable or instant.
  - `dynamicTestFailure(description, throwable, instant)` is forwarded once, and the suite does not touch the `Description`. Zel has no dynamic-test failure path.
  - A property map passed to the suite constructor is returned exactly by `getResult().getProperties()`. `TestCase` has no properties field; XML `<properties>` and unknown attributes are dropped.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/model/XmlWriterTest.java`
- Bazel lines: 222
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel `XmlWriter`. Closest writers are `xml_escape_attr` / `xml_write` in `/workspace/zel-cursor/crates/zel-query/src/format.rs` and `synthesize_xml` in `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`.
  - The declaration must be `<?xml version='1.0' encoding='UTF-8'?>` (single quotes, 1.0, no `standalone`). Query XML emits `<?xml version="1.1" encoding="UTF-8" standalone="no"?>`. `synthesize_xml` uses version 1.0 but double quotes.
  - Attributes are single-quoted (`name='value'`, `count='7'`, `size='large'`, `time='1.0'`). Both Zel writers use double quotes. The int overload prints `String.valueOf(7)` as `7`. The double overload prints `String.valueOf(1.0)` as `1.0`.
  - The only attribute escape this test asserts is `>`: `a > b` becomes `name='a &gt; b'`. Content uses the same rule: `foo]]>bar` becomes `foo]]&gt;bar` (`>` is always escaped, not only the `]]>` sequence). `synthesize_xml` replaces `>` only among `& < > "`, and it handles `]]>` inside CDATA by splitting (`]]>]]&gt;<![CDATA[`) rather than escaping element text.
  - Character data keeps a raw newline and does not reindent. Query XML never writes text nodes. `synthesize_xml` leaves newlines in the log raw inside CDATA.
  - Quote and whitespace escapes (`'` to `&apos;`, `"` to `&quot;`, tab/newline/CR to `&#x9;` / `&#xA;` / `&#xD;`) are not asserted here. Query XML escapes `"` but not `'`, and uses decimal `&#9;` `&#10;` `&#13;`. Bazel replaces other C0 controls with U+FFFD. Zel does not.
  - The writer is an `OutputStreamWriter` in UTF-8. The fixture `"z\u0080\u0800\u010000"` is UTF-16 code units, not Unicode scalars. A Java `\u` escape is exactly four hex digits, so this is U+007A, U+0080, U+0800, U+0100, `'0'`, `'0'` (UTF-8 lengths 1+2+3+2+1+1). It is not U+10000, despite the comment `1+2+3+4 bytes`. A supplementary character would be the surrogate pair `\uD800\uDC00`. `CharEscaper` walks code units. Surrogates sit inside the safe range ending at U+FFFD, so a well-formed pair is written through as one 4-byte UTF-8 sequence. This test never supplies a surrogate. Zel `chars()` yields scalars only, and a Rust string cannot hold an unpaired surrogate.
  - Document shape Zel does not produce: header plus trailing EOL, empty `<DocumentName />` (space before `/>`), one to three attributes on a self-closing tag, two-space indent, a nested empty child closed on the same line as its parent, and two siblings where only the open parent is closed by `close()`. Query XML uses four-space indent and `/>` with no space.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/non_daemon_exit_test.sh`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel shells an integration test of the JUnit4 test-bridge protocol: it requires `TEST_SRCDIR`, sources a unit-test framework from `$1`, and runs the testbed at `$2` against `com.google.testing.junit.runner.testbed.NonDaemonThreadTest` via `-D$3=…`.
  - `test_JVMDoesNotExitImmediately` starts that testbed in the background with `-Dbazel.test_runner.await_non_daemon_threads=true`, sleeps 1s (shorter than the testbed’s 5s non-daemon sleep), and fails if the process has already exited.
  - It then sleeps 7s and fails if the process is still alive, so a non-daemon thread must hold the JVM and the process must exit once that thread finishes.
  - After the check it deletes `${TEST_TMPDIR}/test.xml` so a failing JUnit XML result is not treated as a suite failure, then calls `run_suite "non_daemon_exit_test"`.
  - Zel has no `java_tools/junitrunner`, no `NonDaemonThreadTest`, and no `bazel.test_runner.await_non_daemon_threads` handling. Its test-process exit path does not wait on leftover non-daemon JVM threads or assert that timing.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/sharding/HashBackedShardingFilterTest.java`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The factory builds `HashBackedShardingFilter(shardIndex, totalShards)` and ignores the description collection. Membership is `displayName.hashCode() % totalShards`, then `mod += totalShards` when Java’s toward-zero `%` leaves a negative remainder, and the test runs iff that value equals `shardIndex`. Suites always run. The constructor throws `IllegalArgumentException` when `shardIndex < 0` or `shardIndex >= totalShards`. `describe()` is `"hash-backed sharding filter"`.
  - Inherited cases require a complete, partitioned, order-stable assignment for 6 descriptions `test0`…`test5` (display names `testN(org.junit.Test)`) at 1, 5, 6, and 7 shards, the same with those descriptions duplicated at 7 shards, and `shouldRun(suite) == true` on shard 0 of 1. There are no literal hash fixtures; the modulo is whatever `String.hashCode` yields.
  - Java `String.hashCode` is a signed 32-bit polynomial over UTF-16 units (`h = 31*h + unit`, wrap). Rust `std` hashing is not that function. Zel implements the Java algorithm (`java_order.rs` `string_hash`, `help.rs` `java_string_hash`, `strategy.rs` `java_hash`) only for map iteration and filters, and never applies it as `hash % totalShards` to choose tests.
  - Zel sharding only sets `TEST_SHARD_INDEX` / `TEST_TOTAL_SHARDS` and runs the whole test binary once per shard.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/sharding/RoundRobinShardingFilterTest.java`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel builds one JUnit `Filter` per shard with `RoundRobinShardingFilterFactory` over six descriptions (`test0`–`test5`). Zel only splits a test target into processes and sets `TEST_TOTAL_SHARDS`, `TEST_SHARD_INDEX`, and `TEST_SHARD_STATUS_FILE` (`zel-cursor/crates/zel-exec/src/test_runner.rs`); it never assigns individual test cases.
  - `testShardingIsBalanced`: 3 shards each run 2 tests; 4 shards run 2, 2, 1, and 1. Assignment is sort-by-display-name, then `index % totalShards == shardIndex`, so the remainder stays on the earlier shards. Zel does not balance or modulo-assign cases.
  - `testShouldRun_throwsExceptionForUnknownDescription`: `shouldRun` throws `IllegalArgumentException` for a description that was not passed in when the filter was built. Zel has no such check.
  - The round-robin factory also drives the inherited suite: each test runs exactly once for 1, 5, 6, and 7 shards (including more shards than tests); duplicate descriptions stay partitioned; suite descriptions always run; the same cases stay on the same shard when description order is rotated. Zel has none of that.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/sharding/ShardingEnvironmentTest.java`
- Bazel lines: 43
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`
- Verdict: partial
- Differences:
  - `testTouchShardingFile` builds a missing file `shard_file_123` under a unique temp dir, calls `ShardingEnvironment.touchShardFile(File)`, and requires the file to exist afterward. That method creates the file with `createNewFile`, or if it already exists updates its mtime with `setLastModified`, and on failure throws `RuntimeException("Error writing shard file " + path)`. Zel never creates or updates `test.shard`. It only sets `TEST_SHARD_STATUS_FILE` to `<attempt>/test.shard` when `shard_count > 0`, and the test process is expected to touch it.
  - This test does not parse `TEST_TOTAL_SHARDS`. The class under test does: sharding is on iff the variable is set, and `getTotalShards` returns 1 when unset and `Integer.parseInt` otherwise. Zel writes `TEST_TOTAL_SHARDS` from `spec.shard_count` only when `shard_count > 0`. A missing touch is a status only when `exit_code == 0`, the attempt did not time out, `shard_count > 1`, and `check_sharding_support`: exit code becomes 1, `sharding_unsupported` is set, the log gets the "did not advertise support" message, and the unit status is `TestStatus::NoStatus`, which stops later shards. `shard_count == 1` skips that check.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/sharding/ShardingFiltersTest.java`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - A null strategy keeps the factory passed into `ShardingFilters`. This test passes `ShardingStrategy.ROUND_ROBIN` with shard index 0 and total shards 5 over 6 descriptions, and requires the result to be a `RoundRobinShardingFilter` with the same `testToShardMap`, `shardIndex`, and `totalShards`. Zel has no JUnit `Filter` and no round-robin test-to-shard map.
  - A non-enum strategy string is a `ShardingFilterFactory` binary name. The test returns `ShardingFiltersTest$TestFilterFactory` and expects `createShardingFilter` to load that class. Zel does not load a user class.
  - The loader uppercases the strategy with `Locale.ENGLISH` and accepts `HASH` and `ROUND_ROBIN`. This file only exercises the null-plus-injected-`ROUND_ROBIN` path and the custom-class path, not the `"HASH"` or `"ROUND_ROBIN"` strings. Zel accepts neither name.
  - A string that is neither enum nor a loadable no-arg `ShardingFilterFactory` fails with `RuntimeException("Could not create custom sharding strategy class " + strategy)`. The test does not cover that failure. Zel’s `--test_sharding_strategy` only accepts `explicit`, `disabled`, and `forced=k` (case-insensitive); anything else, including `HASH`, `ROUND_ROBIN`, and a class name, is `Not a valid test sharding strategy` (`/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`).
  - The strategy value in the runner is the JVM property `test.sharding.strategy`. Zel’s flag only chooses a shard count and, when the count is greater than 0, sets `TEST_TOTAL_SHARDS`, `TEST_SHARD_INDEX`, and `TEST_SHARD_STATUS_FILE`. It does not set `test.sharding.strategy` or partition test descriptions.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/stack_trace_integration_tests.sh`
- Bazel lines: 153
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel integration-tests the JUnit runner’s console stack traces when a test process is sent SIGTERM (the timeout signal, with SIGKILL expected later). Zel has no `java_tools`/`junitrunner` and no first-party JVM test runner that prints those traces.
  - The script drives a testbed (`$1`) with a suite system property (`$2`), a JUnit version (`$3`), and `testenv.sh`. JUnit 3 filters are `Suite#method`; other versions use `--test_filter=method`. Zel has no equivalent harness.
  - `expect_thread_dumps_in_log` requires at least one log pair of `Starting full thread dump` / `Done full thread dump`, at most N starts, and equal start and end counts so every dump finishes. Zel does not emit or check those JVM full-thread-dump markers on test interrupt.
  - `test_ShutdownHook` synchronizes on a FIFO while `testSneakyShutdownHook` hangs in a shutdown hook after reporting success (`OK.*1 test`, `Entered shutdown`). After SIGTERM and a 3s wait it requires `INTERRUPTED TEST: SIGTERM` plus frames `Shutdown.runHooks`, `StackTraceExercises.handleHook`, and `Thread.sleep`, at most two thread dumps, and a successful process exit. Zel has no shutdown-hook interrupt dump.
  - `test_SlowSuite` hangs in `SuiteMethodTakesForever` during suite creation (`Entered suite creation`). SIGTERM must log `Execution interrupted while running 'TestSuite creation'` and exactly one completed thread dump. Zel has no suite-creation interrupt dump.
  - `test_PrematureExit` runs `testNotSoFastBuddy` (`System.exit`) with `TEST_PREMATURE_EXIT_FILE` set and requires the log `Hey, not so fast there` and a still-readable premature-exit file. Zel’s test runner only sets the `TEST_PREMATURE_EXIT_FILE` environment variable; it does not implement this JVM `System.exit` contract or its integration test.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/system_exit_detecting_test.sh`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel runs a jar that installs `SystemExitDetectingShutdownHook` and calls `System.exit(0)`, then requires the process status to be 121 (`Runtime.halt(121)`), because a normal exit would deadlock inside the shutdown hook. Zel’s test runner only records the raw status and treats any non-zero code as failure. There is no code 121 and no `halt`.
  - On stderr the hook scans every thread for a `java.lang.Runtime.exit` frame and prints `System.exit or Runtime.exit was called!` plus frames from that frame downward. The shell test rewrites line numbers with `sed 's/:[0-9][0-9]*/:XXX/'` and diffs that against `testdata/system_exit_detecting_test_stack.txt`, which must be `Runtime.exit` → `System.exit` → `ProgramThatCallsSystemExit.main`. Zel does not scan JVM stacks.
  - `Runtime.halt` is not what the hook detects (halt skips shutdown hooks). The test only covers `System.exit` / `Runtime.exit`.
  - The script requires `TEST_SRCDIR`, sources the unit-test framework from `$1`, resolves `java` through runfiles (`external/` stripped when `TEST_WORKSPACE=_main`, otherwise `${TEST_WORKSPACE}/${JAVABASE}`), and turns `errexit` off so `$?` can be asserted.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/testbed/ExampleObject.java`
- Bazel lines: 64
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines `com.google.testing.junit.runner.testbed.ExampleObject`, the subject of `XmlOutputExercises`. Zel has no `ExampleObject`.
  - It implements `Comparable<ExampleObject>` and stores a non-null `String data`, rejecting null in both the constructor and `setData` via `Preconditions.checkNotNull`.
  - `getData` returns that string. `compareTo` orders instances by `data.compareTo`. `equals` treats same instance as equal, rejects null and other classes, and otherwise compares `data`. `hashCode` is `data.hashCode()`. `toString` returns `data`.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/testbed/Fifo.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines a non-instantiable `Fifo` (`private Fifo(){}`) that synchronizes Java test-bed tests with shell tests through a named FIFO.
  - It reads the FIFO path from the `test.fifo` system property and throws `IllegalStateException("No fifo specified")` when that property is unset.
  - `waitUntilDataAvailable()` blocks by copying the FIFO's bytes to a null output stream (`Files.asByteSource(new File(FIFO)).copyTo(ByteStreams.nullOutputStream())`), returning only once data has been read.
  - Zel has no equivalent fixture; the path appears only as a filename inside a patch-corpus file list.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/testbed/InternationalCharsTest.java`
- Bazel lines: 28
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - JUnit 3 fixture `testThatAlwaysFails` always fails with `assertEquals("Test Japan.", "Test \u65E5本.")`. Zel has no testbed that emits this failure.
  - The actual value mixes a BMP escape with a raw source character. `\u65E5` is U+65E5 (`日`); `本` is stored in the file as UTF-8 `E6 9C AC` (U+672C). Both are one Java UTF-16 code unit and one Rust Unicode scalar, so scalar vs UTF-16 length does not diverge (2 and 2). UTF-8 on the wire is 3 bytes each. JUnit 3 `ComparisonFailure` compacts the common prefix and suffix into `expected:<Test [Japan].> but was:<Test [日本].>`. `utf8_test_log_test.sh` requires that exact string in the log.
  - The runner prints that text through a `PrintStream` forced to UTF-8 (`JUnit4RunnerBaseModule.asUtf8PrintStream`), independent of `file.encoding` (Bazel sets ISO-8859-1) and of `sun.jnu.encoding`. A platform-default stream would turn both kanji into `?`. Zel's `info` character-encoding line only reports those three properties. It does not run this suite.
  - Zel copies spawn stdout and stderr into `test.log` as raw bytes. That path does not build the JUnit 3 compact message and does not lock a UTF-8 `PrintStream` against a non-UTF-8 default charset.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/testbed/JUnit4TestbridgeExercises.java`
- Bazel lines: 40
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines a JUnit4 suite with `testPass` (empty success), `testFailOnce` (`Assert.fail()`), and `testFailAgain` (`Assert.fail()`). An unfiltered run is two failures (`Failures: 2`).
  - `TESTBRIDGE_TEST_ONLY=testPass` selects only the passing method (`OK.*1 test`). Clearing the variable restores both failures. A direct `--test_filter` overrides the environment. Zel only copies `--test_filter` into `TESTBRIDGE_TEST_ONLY` in `build_test_env`; nothing applies that filter to individual JUnit methods.
  - `TESTBRIDGE_TEST_RUNNER_FAIL_FAST=1` stops after the first failure (`Failures: 1`). Unset or `"0"` still reports `Failures: 2`. Zel records `fail_fast` on the analysis action and never exports `TESTBRIDGE_TEST_RUNNER_FAIL_FAST`.
  - The runner writes `${TEST_TMPDIR}/test.xml`. Ant XML emits per-testcase `name`, `classname`, `time`, `<failure message type>` plus stack trace, omits `FILTERED` cases, and writes a `<properties>` block. Zel's `synthesize_xml` is one `<testcase>` named after the target, `failures="0"`, `errors` 0 or 1, `<error message>` with no type or stack, and no `<properties>` or per-method results.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/testbed/JUnit4TestbridgeOomExercises.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel ships a JUnit4 suite with `testPass` (empty success), `testFailAssertion` (`Assert.fail()`), and `testFailWithOom` (`throw new OutOfMemoryError("testing")`).
  - `BazelTestRunner` maps a failure whose exception is `OutOfMemoryError` to exit 137, a normal assertion failure to 1, a runner failure to 2, and a clean run to 0. An `OutOfMemoryError` thrown by the runner itself also exits 137. Zel’s `run_test` treats every nonzero exit, including 137, as `TestStatus::Failed` and only special-cases timeouts (exit 142).
  - The integration script checks that an unfiltered run exits 137 with `Failures: 2`, `TESTBRIDGE_TEST_ONLY=testFailAssertion` exits 1, `testFailWithOom` exits 137, and `testPass` exits 0. Zel does not implement that filter-plus-exit-code matrix.
  - Zel `format_exit_status` treats 137 as signal 9 (`"Killed"`, 128+9). It does not record a distinct test OOM status. Zel’s own `ExitCode::OOM_ERROR` (33, `CRASH_OOM`) is the Zel process crash code, not a test-subprocess code for a thrown `OutOfMemoryError`.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/testbed/Junit4UndeclaredTestCaseFailures.java`
- Bazel lines: 36
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel ships a JUnit4 fixture run with `MockitoJUnitRunner.Strict`. Its only method, `passesButHasUnnecessaryStubs`, stubs `mockList.add("")` and never calls it, so the method body does not throw.
  - Strict Mockito still fails the run for unnecessary stubbings. The integration script requires a non-zero exit, a log line containing `unnecessary Mockito stubbings`, and JUnit XML with `tests='2' failures='1'`.
  - `TestSuiteModel.testFailure` treats a failure whose `Description` is not a declared test as a dynamically added case: it builds a new `TestCaseNode`, attaches it to the suite, and records the throwable. The suite fails even though the declared method passed.
  - Zel has no path that turns a runner-only failure into an extra failed testcase. `parse_test_xml` only classifies `<failure>`, `<error>`, and `<skipped>` already present in XML.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/testbed/NonDaemonThreadTest.java`
- Bazel lines: 39
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `testNonDaemonThread` starts a thread, calls `setDaemon(false)`, and returns without `join`. The thread sleeps 5000 ms and, on `InterruptedException`, restores the interrupt flag. The JVM therefore stays up until that non-daemon thread finishes.
  - `non_daemon_exit_test.sh` runs this class with `-Dbazel.test_runner.await_non_daemon_threads=true` and checks the process is still alive after 1 s and gone after a further 7 s. `BazelTestRunner.awaitAllNonDaemonThreadsToFinish` installs a shutdown hook that spin-waits (`Thread.yield`) until every other non-daemon thread is dead, then `System.exit`s.
  - Zel has no `bazel.test_runner.await_non_daemon_threads` hook. Nothing waits out leftover non-daemon threads before process exit.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/testbed/StackTraceExercises.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no stack-trace fixture. Bazel’s `StackTraceExercises` is a JUnit 3 `TestCase` whose failures are documented as not meant to turn continuous builds red.
  - `testSneakyShutdownHook` registers a shutdown hook that calls `handleHook`.
  - `handleHook` prints `Entered shutdown hook`, flushes stdout, blocks on `Fifo.waitUntilDataAvailable()`, then sleeps 15000 ms, wrapping any `Exception` in an `Error`.
  - `testNotSoFastBuddy` prints `Hey, not so fast there` and calls `System.exit(0)`.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/testbed/SuiteMethodTakesForever.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel provides a JUnit 3 `suite()` fixture that prints and flushes `Entered suite creation`, blocks on `Fifo.waitUntilDataAvailable()`, then sleeps one hour so a harness can send SIGTERM during suite construction.
  - If that sleep finishes, `suite()` throws `IllegalStateException` with the message `Expected to be interrupted before finishing the suite creation`.
  - The fixture exists so the JUnit runner’s stack-trace path can be checked when TERM arrives during suite creation, and the file comment says failures here must not fail continuous builds.
  - Zel applies numeric test timeouts (`short` 60s, `moderate` 300s, `long` 900s, `eternal` 3600s, plus `--test_timeout`) and a process-wrapper SIGTERM-then-SIGKILL path, but it has no suite-construction hang fixture, no FIFO handshake, and no stack-trace capture for a signal during `suite()`.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/testbed/XmlOutputExercises.java`
- Bazel lines: 111
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel locks an Ant JUnit document: `<?xml version='1.0' encoding='UTF-8'?>`, a `<testsuites>` root, and five sibling `<testsuite>` elements (`id` 0–4) for the `Enclosed` outer class plus `$FailureTest`, `$OtherTests`, `$EqualsHashCodeTest`, and `$ComparabilityTest`. Zel’s `synthesize_xml` writes one suite named from the target label.
  - The fixture records 7 cases and 1 failure. Each case has the inner-class `classname` and an empty `time`. Zel always emits `tests="1"`, `classname=""`, `status="run"`, `duration="0"`, `time="0"`, and ignores `duration_ms`.
  - `testFail` is a `<failure message='This is an expected error. The test is supposed to fail.' type='java.lang.AssertionError'>`. The outer suite is `tests='7' failures='1' errors='0'`. Zel reports a nonzero exit or timeout as `failures="0" errors="1"` with an empty `<error>` and never an `AssertionError` type.
  - Every suite has empty `<properties />`, `<system-out />`, and `<system-err />`, plus `hostname='localhost'`, empty `timestamp`, `time`, and `package`. Zel puts the log in `<system-out>` CDATA and omits properties, system-err, hostname, timestamp, package, and id.
  - The golden file is ASCII UTF-8. `$` in class names is raw. The failure message has no `&`, `<`, `>`, or quotes. The writer behind it escapes content `& < >`, and attributes also `' "`, tab, newline, and CR as `&#x9;`, `&#xA;`, `&#xD;`, and replaces other C0 controls with U+FFFD. Zel attribute escaping is only `& < > "`.
  - Zel splits a CDATA `]]>` as `]]>]]&gt;<![CDATA[`. Bazel’s `generate-xml.sh` splits it as `]]>]]<![CDATA[>`. The Ant fixture uses no CDATA.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/utf8_test_log_test.sh`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The script sources `unittest.bash`, requires `TEST_SRCDIR`, unsets `TEST_PREMATURE_EXIT_FILE`, and runs `TestbedBinary` with `-Dbazel.test_suite=com.google.testing.junit.runner.testbed.InternationalCharsTest`. Zel has no shell test that launches that class.
  - The script requires that run to fail. `expect_log` fixed-string greps `$TEST_log` for `expected:<Test [Japan].> but was:<Test [日本].>`. That text stays intact only because `asUtf8PrintStream` wraps the text listener in `PrintStream(..., StandardCharsets.UTF_8)`. Java’s default charset is not UTF-8 here (Bazel starts the JVM with `-Dfile.encoding=ISO-8859-1`), so a default-charset `PrintStream` would replace or mis-encode 日本. Zel does not do this check.
  - Zel `run_attempt` writes `test.log` as the test process’s raw stdout plus stderr. Rust `String` and `println!` are already UTF-8, so there is no `Charset.defaultCharset()` conversion on that path.
  - After the expected failure, the script deletes `${TEST_TMPDIR}/test.xml` so the intentional JUnit failure XML is not reported as the shell test’s own failure. Zel keeps a failing `test.xml` (or synthesizes one).
  - When Zel rewrites a log or embeds it in synthesized XML, it decodes with `String::from_utf8_lossy` (invalid bytes become U+FFFD). That is log-to-XML filtering, not an assertion that 日本 is stored as UTF-8 in the log.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/util/TestIntegrationsExporterTest.java`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no test for `TestIntegrationsExporter` exporting a test integration to the thread callback.
  - Bazel saves the previous thread callback in `@Before` via `TestIntegrationsRunnerIntegration.setTestCaseForThread` and installs a Mockito mock, then restores the previous callback in `@After`.
  - `testExportTestIntegration` builds a `TestIntegration` with contact email `test@testmail.com`, component id `1234`, name `Test`, url `testurl`, description `Test description.`, foreground color `white`, and background color `rgb(47, 122, 243)`.
  - It calls `TestIntegrationsExporter.INSTANCE.newTestIntegration` and verifies the mock callback’s `exportTestIntegration` receives that same integration object.

### `src/java_tools/junitrunner/javatests/com/google/testing/junit/runner/util/TestPropertyExporterTest.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - This Bazel test never uses the name `name1000`. Repeated properties are exported under the literal `"propertyName"`, and a separate case treats a single space `" "` as a valid name.
  - `@Before` installs a Mockito `TestPropertyExporter.Callback` with `TestPropertyRunnerIntegration.setTestCaseForThread` and keeps the previous callback; `@After` restores that previous callback.
  - `testExportProperty` calls `exportProperty("propertyName", "value")` and verifies the same pair on the thread callback.
  - `testExportRepeatedProperty` calls `exportRepeatedProperty("propertyName", "value")` and verifies that repeated export on the callback.
  - `testExportProperty_emptyNameIsValid` exports the name `" "` with value `"value"` and verifies the callback accepts it.
  - `testExportRepeatedProperty_emptyNameIsValid` does the same for `exportRepeatedProperty(" ", "value")`.
  - Zel has no `exportProperty` / `exportRepeatedProperty` coverage and no thread-callback install/restore for this exporter.

### `src/java_tools/singlejar/java/com/google/devtools/build/singlejar/ConcatenateStrategy.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `ConcatenateStrategy`. `crates/zel-analysis/src/java_native.rs` only spawns the toolchain `single_jar`. `crates/zel-bzlmod/src/archive.rs` extracts zip members and does not merge duplicate names.
  - Bytes are copied in read order through a reused 4096-byte buffer. There is no charset decode, sort, endian swap, or line dedup. `finish` writes nothing.
  - `appendNewLine` defaults to true. `lastByteCopied` starts as `0x0A`, so the first stream is never prefixed. Before each later stream, if the previous last byte is not `0x0A`, one `0x0A` is inserted (`out.write('\n')`, not `\r\n`). A file ending in `\r` only still gets that extra `0x0A`. A file ending in `\n` does not. No trailing newline is added after the last stream.
  - An empty stream does not change `lastByteCopied`. `"a"`, `""`, `"b"` becomes `a\nb`. `"a\n"` + `"b"` stays `a\nb`. `ConcatenateStrategy(false)` is raw concatenation with no inserted bytes (`protobuf.meta`). The default strategy is what `DefaultJarEntryFilter` uses for `META-INF/services/*` (including the directory name), `META-INF/spring.handlers`, `META-INF/spring.schemas`, and `reference.conf`.
  - One instance is not thread-safe and is kept per entry name. Duplicate lines are kept. `skipEmpty` stays false, so a merge that wrote zero bytes is still emitted as an empty zip entry.
  - This class does not set DOS time, CRC, or method. `ZipCombiner.finish` stamps the merged entry with the `Date` passed to `customMerge`, or the current time if that date is null. With the default filter and `normalize=true`, that date is the DOS epoch (1980-01-01). Non-merged duplicates are first-wins. `.class` files are copied, not concatenated, at DOS epoch plus 2 seconds.
  - The vendored `MergeJars.java` is not this strategy. It joins `META-INF/services/` bodies with `"\n\n"`, uses the platform charset, always writes a trailing `\n`, emits service names in `TreeMap` order, and stamps `StableZipEntry` dates. It does not raw-concatenate `protobuf.meta` or newline-merge `spring.handlers`, `spring.schemas`, or `reference.conf`.

### `src/java_tools/singlejar/java/com/google/devtools/build/singlejar/CopyEntryFilter.java`
- Bazel lines: 33
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `CopyEntryFilter` is a final, `@Immutable` `ZipEntryFilter`. `accept(filename, callback)` ignores the name and always calls `callback.copy(null)`, so every entry is requested as a plain copy with the source timestamp left unchanged.
  - It never skips, renames, or custom-merges any name (manifests, `META-INF`, services, directories, or duplicates).
  - `ZipCombiner.getAction` turns a `COPY` into `SKIP` once that name is already in the output, so the first entry of each name is copied and later same-named entries are dropped. That first-wins rule is in the combiner, not in this filter.
  - Zel has no jar combiner and no zip-entry filter. `zel-analysis` only launches the Java toolchain’s external `single_jar` executable.

### `src/java_tools/singlejar/java/com/google/devtools/build/singlejar/DefaultJarEntryFilter.java`
- Bazel lines: 124
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Entries failing `allowed(filename)` are `skip()`’d before any merge or copy rule. Zel has no singlejar entry filter; `java_native.rs` only spawns the toolchain `single_jar` binary (`--normalize`, `--dont_change_compression`, `--exclude_build_data`, `--no_strip_module_info`).
  - Constructors: `(normalize, allowedPaths)`, `(normalize)` (defaults to `ANY_PATH`), and no-arg (normalize `true`). When `normalize` is true, non-class timestamps are `ZipCombiner.DOS_EPOCH` and `.class` timestamps are `DOS_EPOCH_PLUS_2_SECONDS` (1980-01-01 00:00:02, one ZIP 2-second tick after the DOS epoch so javac still prefers `.class` over a same-named source). When false, both dates are null.
  - Exact-name newline concatenation: `META-INF/spring.handlers`, `META-INF/spring.schemas`, and `reference.conf` (any directory, not only `META-INF`).
  - Prefix `META-INF/services/` (including the directory entry itself) is newline-concatenated, and this check runs before signing-file skips, so `META-INF/services/foo.SF` is merged rather than dropped.
  - Skip `META-INF/MANIFEST.MF` and every name ending in `.SF`, `.DSA`, or `.RSA` anywhere in the archive, not only under `META-INF` (a source TODO says signing files should be limited to `META-INF/`).
  - `.class` entries are copied with `classDate` (checked before `protobuf.meta`, so `protobuf.meta.class` is a class copy). Exact `protobuf.meta` is concatenated with `ConcatenateStrategy(false)` (no inserted newlines; binary protobuf). Everything else is `copy(date)`.
  - Duplicate policy: services, Spring handlers/schemas, `reference.conf`, and `protobuf.meta` are custom-merged; manifests and signature files are skipped; other names are copied and left to `ZipCombiner`.

### `src/java_tools/singlejar/java/com/google/devtools/build/singlejar/JarUtils.java`
- Bazel lines: 17
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The file is only the Apache header and `package com.google.devtools.build.singlejar;`. It declares no type, so there is no jar-path helper and no manifest helper.
  - It sets no charset for entry names or manifest bytes.
  - It has no path-separator policy: `\` is not rewritten to `/`, and `.`, `..`, and repeated separators are not normalized.
  - Zel’s zip code does not fill that gap. `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` `normalize` splits only on `/` (`\` stays inside a segment), drops empty and `.` segments, resolves `..`, and makes absolute paths relative. `parse_zip` decodes central-directory names with `String::from_utf8_lossy` and does not read or merge `META-INF/MANIFEST.MF`.

### `src/java_tools/singlejar/java/com/google/devtools/build/singlejar/ZipCombiner.java`
- Bazel lines: 596
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no singlejar combiner. `java_resource_jar_action` only spawns the toolchain `single_jar` binary. Nothing in Zel implements the combining behavior below.
  - The combiner writes one 32-bit ZIP (STORE and DEFLATE only) through `ZipWriter` with Zip64 disabled. `OutputMode` is `DONT_CARE` (default), `FORCE_DEFLATE`, or `FORCE_STORED`. Callers must `finish()` or `close()`. Input damage is passed through.
  - `DONT_CARE` copies the raw compressed bytes and keeps the source method. `FORCE_DEFLATE` deflates non-DEFLATE entries (raw `Deflater`, `DEFAULT_COMPRESSION`, nowrap, 8192-byte buffer) and copies already-deflated entries raw. The “directories stay stored” comment is not implemented on this path. `FORCE_STORED` inflates non-STORE entries with a nowrap `Inflater` and sets the compressed size to the uncompressed size; CRC and uncompressed size stay as copied from the source entry.
  - `addFile` always buffers the whole uncompressed payload, sets CRC32 and size, then deflates it unless the mode is `FORCE_STORED` (so `DONT_CARE` still deflates new files). `addDirectory` always writes a STORE entry with CRC 0 and sizes 0, requires a trailing `/`, and copies the given extra-field list.
  - Merge inputs that are DEFLATE are inflated first. `finish()` drops the entry when the buffer is empty and `skipEmpty()` is true, otherwise writes a new entry (name and time only; no source extra fields, comment, or attributes).
  - `addDirectory`, `addFile`, and non-merge entries from `addZip` are written immediately, in central-directory order. Merged names are not written until `finish()`, which walks the `actions` `HashMap`, so their relative order is hash order and they all land after the entries already written.
  - Duplicates are exact name matches. `addFile` throws `IllegalArgumentException` if the name is already present; `addDirectory` throws `IllegalStateException`. A `COPY` whose name is already in the output is changed to `SKIP`, so the first copy wins. A `RENAME` is skipped when the new name is already present; that skip is not stored, and a stored `RENAME` is re-filtered on every later sighting of the original name. Within one input zip, `ZipReader` already collapses duplicate names (last central-directory record, first position) before the combiner sees them.
  - A non-null `Date` overrides the entry time. A null date on `addDirectory`, `addFile(String, Date, InputStream)`, and merged output uses `new Date()` (now). A null date on copy or rename keeps the source time. `DOS_EPOCH` is exposed and never applied as a default. `ZipWriter` rejects a time of -1.
  - The output `ZipWriter` and the default `ZipReader` use `UTF_8`. Entries created here start with flags 0, so the general-purpose UTF-8 bit (bit 11) is not set. Copied and renamed entries keep the source flags except the data-descriptor bit, which `ZipWriter` clears. Rename changes the name only.

### `src/java_tools/singlejar/java/com/google/devtools/build/singlejar/ZipEntryFilter.java`
- Bazel lines: 128
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines a thread-safe `ZipEntryFilter` whose `accept(String filename, StrategyCallback callback)` decides, once per unique entry name, how every entry of that name is combined. Zel has no singlejar filter type.
  - `accept` is documented to be called exactly once per name. A rename is the exception: the original name is not treated as already encountered.
  - `StrategyCallback` requires exactly one of `skip`, `copy`, `rename`, or `customMerge` per name. A second call throws `IllegalStateException`. The callback may defer the work.
  - `skip()` drops the current entry and every later entry with the same name.
  - `copy(Date date)` keeps the first entry, drops later duplicates, and overwrites the entry timestamp when `date` is non-null.
  - `rename(String filename, Date date)` stores the first entry under a new name, drops later entries of the original name, and overwrites the timestamp when `date` is non-null.
  - `customMerge(Date date, CustomMergeStrategy strategy)` merges every entry of that name with one strategy instance. The same strategy object must not be passed twice. A non-null `date` sets the output timestamp; a null `date` means the current time.
  - `merge(InputStream, OutputStream)` is called for each same-named entry, then `finish(OutputStream)` writes the single merged entry once. Each instance given to `customMerge` is not called from two threads at once.
  - `skipEmpty()` defaults to `false`, so an empty merge result is still written. Returning `true` skips that empty result.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/AdjustSfx.java`
- Bazel lines: 83
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/third_party/ijar/zip.cc` (read-side preamble only)
- Verdict: partial
- Differences:
  - Bazel rewrites the archive (`zip -A`): it copies the raw prefix through the physical central-directory start, then emits a new central directory. Zel ijar only adds the preamble when seeking local headers and never writes adjusted offsets back. `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` only reads zips.
  - Delta is `adjustOffset = actualCenStart - recordedCentralDirectoryOffset`, with `actualCenStart = actualCenEnd - centralDirectorySize` (both Java `long`s). Non-Zip64 `actualCenEnd` is the physical end-of-central-directory from a backward scan for little-endian signature `0x06054b50` whose little-endian `u16` comment length at EOCD+20 accounts for every byte through EOF. ijar uses the same subtraction: `in_offset_ = physicalCd - (fileStart + recordedCdOffset)`.
  - Zip64 `actualCenEnd` is the locator’s absolute little-endian `u64` at locator+8 (`0x07064b50`, 20 bytes before the physical EOCD) when `isZip64()` and that offset is `> 0`. An unadjusted self-extracting Zip64 file still stores the pre-preamble offset there, so `adjustOffset` becomes 0 and the copy stops at the old directory. ijar first tries the physical slot `eocd - 20 - 56` (preamble-safe) and documents that the absolute locator offset does not work if bytes were prepended without updating it.
  - On a nonzero delta, every entry’s local-header offset, the central-directory offset, and (if Zip64 and the locator offset is `> 0`) the Zip64 EOCD offset increase by `adjustOffset`. Local-file headers are copied unchanged. `CentralDirectory.write` then overwrites the Zip64 EOCD offset with `adjustedCdOffset + rewrittenDirectorySize`.
  - Rewritten multi-byte fields are little-endian. ijar’s Zip64 parse reads version-made-by, version-needed (`get_u2be`), and disk number (`get_u4be`) big-endian, then the remaining counts and offsets little-endian. Those big-endian fields match Bazel only while they are zero.
  - `archive.rs` uses `eocd - cdSize` only when that value’s low 32 bits equal the recorded offset. A preamble that is not a multiple of 2^32 fails that check and the reader follows the unadjusted offset. Zip64 absolute offset is `u64` LE at `eocd - 20 + 8` with no preamble added.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/CentralDirectory.java`
- Bazel lines: 40
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`parse_zip`, `find_zip_eocd`; test-only `build_zip`)
- Verdict: partial
- Differences:
  - This class only writes. It walks `file.getEntries()`, calls `CentralDirectoryFileHeader.write` for each, stores the summed size with `setCentralDirectorySize`, and does not scan an end-of-central-directory record. Zel’s production zip path is the reader. The only Zel writer of a central directory is the test helper `build_zip`.
  - Little-endian is not implemented in this file. The headers it calls emit little-endian through `ZipUtil`. Zel reads with `u16le` / `u32le` / `u64le` and `build_zip` writes with `to_le_bytes`.
  - ZIP64 trailers are written only when `file.isZip64() && allowZip64`: the Zip64 end-of-central-directory offset is set to `centralDirectoryOffset + centralDirectorySize`, then the Zip64 end record and locator are written, and the classic end record always follows (0xFFFF / 0xFFFFFFFF sentinels when `allowZip64` and a field overflows). `build_zip` never writes a Zip64 end record or locator and stores the entry count, directory size, and offset as `u16` / `u32`. `parse_zip` follows a locator at `eocd - 20` only when the classic total is 0xFFFF or the offset is 0xFFFFFFFF. It does not treat a 0xFFFFFFFF directory size as a Zip64 trigger, and it keeps the 32-bit end-of-central-directory size instead of replacing it from the Zip64 record.
  - Entry comments and the archive comment are encoded by the callees with `ZipFileData`’s charset. `find_zip_eocd` uses the `u16` comment length only to require `eocd + 22 + clen == file length` and does not decode the archive comment. `parse_zip` skips each central-directory comment and decodes entry names with `String::from_utf8_lossy`. `build_zip` writes a zero comment length. Names are the raw UTF-8 of the Rust string, with the general-purpose flag left at 0.
  - Vendored ijar `WriteCentralDirectory` writes little-endian Zip64 trailers only when the entry count, directory size, or offset exceeds 16/32-bit limits, and it writes a zero-length comment.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/CentralDirectoryFileHeader.java`
- Bazel lines: 265
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`parse_zip`; test-only writer `build_zip`)
- Verdict: partial
- Differences:
  - Signature and the 46-byte fixed header match: `0x02014b50`, flags at offset 8, name/extra/comment lengths at 28/30/32, local-header offset at 42. Zel’s error text is the same (“does not start with 02014b50”, “Unexpected end of file while reading Central Directory File Header.”).
  - Bazel copies the full 16-bit general-purpose flags through on read and write. Zel reads that word and does not use it. Bit 11 (UTF-8, `0x0800`) is never tested in this class or in Zel.
  - Bazel decodes and encodes both the name and the comment with the caller `Charset`. Zel decodes only the name with `String::from_utf8_lossy` and skips the comment bytes. The test writer stores `name.as_bytes()` with flags left at 0.
  - Bazel parses the whole extra blob into `ExtraDataList`. `write` strips extended-timestamp and Info-ZIP Unix extra before emitting. Zel walks extra records only when compressed size, uncompressed size, or local offset is `0xFFFFFFFF`, and only consumes id `0x0001`.
  - ZIP64 on read matches for those three fields: extra id `0x0001`, then uncompressed size, compressed size, and local offset, 8 bytes each, and only for fields that are `0xFFFFFFFF`. Neither side reads a ZIP64 disk-start number. Bazel’s writer emits that extra and writes `0xFFFFFFFF` into the 32-bit slots, and always writes disk-start 0. Zel has no production central-directory writer; `build_zip` writes 32-bit sizes only, extra length 0, and no ZIP64 extra.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/CountingInputStream.java`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel wraps any `InputStream` and keeps a running total in `long count`, returned by `getCount()`. Zel has no counting filter stream. The nearest zip cursor is `pos: usize` in `parse_zip`, which advances by header lengths.
  - `count` and `mark` are signed 64-bit `long` (`mark` starts at `-1`). Zel’s cursor and entry offsets are unsigned `usize`. There is no “mark unset” sentinel.
  - `read()` and `read(byte[], int, int)` return Java `int`. EOF (`-1`) adds `0`; a successful read adds `1` or the `int` count. One call therefore adds at most `Integer.MAX_VALUE` bytes. Zel slice lengths are `usize` with no 32-bit per-read cap.
  - `skip(long)` adds the full `long` returned by the inner stream, so one skip can move the counter by more than 2³¹−1. Zel does not skip through a counting wrapper.
  - `mark(int readlimit)` stores `count` as `long` but forwards a 32-bit `readlimit`. `reset()` throws `IOException` `"Mark not supported"` when `!in.markSupported()`, then `"Mark not set"` when `mark == -1`, then sets `count = mark`. Both methods are `synchronized`; `read` and `skip` are not.
  - Java `long` addition wraps silently. After 2⁶⁴−1 counted bytes, `count` is `-1` and collides with the unset-mark sentinel. Zel casts with `as u64` / `as usize`; a value above `usize` truncates instead of wrapping a signed counter.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/CountingOutputStream.java`
- Bazel lines: 54
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/protobuf/33.4/java/core/src/main/java/com/google/protobuf/CodedOutputStream.java`
- Verdict: partial
- Differences:
  - Bazel is a `FilterOutputStream` whose only state is a running byte count. Zel has no equivalent wrapper; the nearest writers fold the count into a protobuf buffer or a `ByteString` builder.
  - Bazel stores `long count` and returns it from `getCount()`. Zel `totalBytesWritten` and `ByteString.Output.size()` are `int`.
  - Neither side checks overflow. Bazel `count++` and `count += len` wrap only past `Long.MAX_VALUE`. Zel `totalBytesWritten++` wraps at `Integer.MAX_VALUE`.
  - Bazel adds the count only after `out.write` returns, so a throw leaves `count` unchanged even if the delegate already emitted a prefix. `OutputStreamEncoder.write` adds `totalBytesWritten` when bytes enter the buffer and again before/around `out.write`; `getTotalBytesWritten` is documented as unreliable if a write fails midway. The protobuf count also includes bytes still sitting in the buffer.
  - Bazel counts the call’s length and overrides both array `write`s so `write(byte[])` calls `out.write(b)` directly. Zel’s encoder counts copied or flushed lengths in `write(byte[], int, int)` and `write(ByteBuffer)`.
  - First-party gzip in `/workspace/zel-cursor/crates/zel-util/src/deflate.rs` stores the uncompressed length as `data.len() as u32` (modulo 2^32). That is a 32-bit truncated size, not Bazel’s full `long` total.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/EndOfCentralDirectoryRecord.java`
- Bazel lines: 104
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`find_zip_eocd`, `parse_zip`); writer also in ijar `zip.cc`
- Verdict: partial
- Differences:
  - Bazel fixed layout is 22 bytes: signature `0x06054b50`, disk number, central-directory disk, entries-on-disk, total entries, central-directory size, offset, comment length, then the comment. Zel uses the same signature and the fields at +10, +12, +16, and +20, and requires `i + 22 + comment_length == file length` while scanning backward. It never reads disk number, central-directory disk, or entries-on-disk.
  - Bazel `read` stores the comment bytes. A short read yields `Unexpected end of file while reading End of Central Directory Record.` Zel only uses the comment length to accept a candidate; a missing record is `Zip file '…' is malformed. It does not contain an end of central directory record.`
  - Bazel sets `maybeZip64` when any signed field is `-1`: a `0xFFFF` disk or entry field, or a `0xFFFFFFFF` size or offset. Zel treats ZIP64 only when total entries is `0xFFFF` or the offset is `0xFFFFFFFF`, and only if the 20 bytes immediately before the record are locator signature `0x07064b50` pointing at `0x06064b50`. A `0xFFFFFFFF` central-directory size, or a `0xFFFF` disk field, does not trigger that path. On success Zel replaces the total entry count from ZIP64 offset 32 and the offset from offset 48, and leaves the 32-bit size unchanged.
  - Bazel `create` always writes both disk numbers as 0. Entry count, size, and offset become the all-ones sentinel only when `allowZip64` is true and the value is strictly above `0xFFFF` or `0xFFFFFFFF`; otherwise they are truncated. The same entry count is written at both entries-on-disk and total entries. This method does not write a ZIP64 record or locator. `archive.rs` has no end-of-central-directory writer.
  - ijar widens disk numbers to 32 bits and counts, size, and offset to 64 bits. It rejects non-zero disk numbers or mismatched per-disk and total entry counts as multi-disk JARs. When writing, counts above `0xFFFF` or a size or offset above `0xFFFFFFFF` cause a ZIP64 record and locator, then a classic record with sentinels, disk numbers 0, and comment length 0.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/ExtraData.java`
- Bazel lines: 103
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/third_party/ijar/zip.cc`
- Verdict: partial
- Differences:
  - Bazel `ExtraData` is a generic record: little-endian 16-bit header id at offset 0, little-endian 16-bit payload length at offset 2, then the payload (`FIXED_DATA_SIZE` 4). Zel has no `ExtraData` type. The same layout is only walked inline in `ProcessCentralDirEntry`.
  - Zel only interprets header id `0x0001`, reading up to three little-endian u64s (uncompressed size, compressed size, local-header offset) when the central-directory fields are `0xffffffff`. `ExtraData` stores any id and does not decode a ZIP64 payload.
  - Construction in Bazel rejects a payload longer than `0xffff` and the buffer constructor rejects a short or truncated record. Zel advances `extra_p` by `data_size` with no per-record remaining-length check inside the loop.
  - Zel’s writers set `extra_field_length` to 0 and copy an empty extra blob. They do not emit an id, a length, and a payload the way `ExtraData(short id, byte[] data)` does.
  - The local-header extra field in Zel is a raw slice copied through, not split into id and payload. Bazel exposes `getId` via signed 16-bit `ZipUtil.get16`; Zel uses an unsigned `u2`.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/ExtraDataList.java`
- Bazel lines: 171
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`parse_zip` extra walk); also ijar `ProcessCentralDirEntry`
- Verdict: partial
- Differences:
  - Bazel keeps every extra record in a `LinkedHashMap` keyed by header id and can add, remove, and reserialize the whole list. Zel only scans the central-directory extra bytes to fill ZIP64 uncompressed size, compressed size, and local-header offset when those fields are `0xFFFFFFFF`. There is no id map and no writer.
  - Duplicate ids: `entries.put` replaces the value and leaves the key at its first insertion slot, so the last record is the one `get` and `getBytes` emit, still in the first record’s position. Zel applies a ZIP64 body only while a field is still `0xFFFFFFFF`, so the first ZIP64 record that supplies a field wins and a later duplicate is ignored.
  - Truncation: `ExtraDataList(byte[])` throws `IllegalArgumentException` ("incomplete extra data entry in buffer") when fewer than 4 bytes remain or the declared payload extends past the buffer. Zel stops the walk when `e + 4 > extra.len()` and leaves a 1–3 byte tail unread. A ZIP64 read that runs past the slice returns `"Truncated zip file"` from `u64le`. ijar adds `data_size` with no remaining-length check.
  - The byte-array constructor rejects `buffer.length > 0xffff`, and `add` rejects when the combined length would exceed `0xffff`. Zel’s extra length is the central-directory `u16`; there is no second 65535 check.
  - Header ids `0x5455` (`EXTENDED_TIMESTAMP`) and `0x7875` (`INFOZIP_UNIX_NEW`) are declared here and unused by the parser. Zel zip code only special-cases id `1`.
  - Local-header extra is not parsed by this class. Zel `zip_entry_bytes` skips `lextra` and clamps the data start when the extra block runs past the buffer.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/LocalFileHeader.java`
- Bazel lines: 107
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/third_party/ijar/zip.cc` (`WriteLocalFileHeader`, `ProcessLocalFileEntry`)
- Verdict: partial
- Differences:
  - Both use signature `0x04034b50` and the 30-byte local header. Bazel fills version, flags, method, DOS time, CRC, sizes, name, and extra from the entry. Zel hardcodes version 10, flags 0, method stored (later patched), DOS date 2010-01-01, CRC and sizes 0 then patched, extra length 0, and a raw C-string name.
  - Bazel never writes a data descriptor (`0x08074b50`); CRC and sizes stay in the local header. Zel’s writer also never emits one, though `EstimateSize` reserves 12 bytes per entry. Zel’s reader, when general-purpose bit 3 is set, skips a 12-byte descriptor or 16 bytes if the signature is present, and does not skip a ZIP64 20- or 24-byte descriptor.
  - Bazel copies `entry.getFlags()` unchanged, so UTF-8 bit 11 is only present if the entry already set it, and the name is encoded with `ZipFileData`’s charset. Zel accepts bit 11 on input but writes flags 0 on every local and central header.
  - Bazel ZIP64: if `ZIP64_SIZE` or `ZIP64_CSIZE` is set, it requires `allowZip64` or throws `ZipException`, strips extra id `0x0001`, stores `0xffffffff` in each overflowing 32-bit size, and appends extra `0x0001` with the 8-byte uncompressed size and/or compressed size (uncompressed first). It also drops extended-timestamp and Info-ZIP Unix extras, and does not put a local-header offset in that extra. Zel writes no ZIP64 extra on local headers and caps output at 4GiB. Its ZIP64 write path is only the EOCD and locator. On read, a local-header `0xffffffff` size is taken from the central directory, not from the local extra.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/PackageZipper.java`
- Bazel lines: 270
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/third_party/ijar/zip_main.cc`
- Verdict: partial
- Differences:
  - Order: Bazel writes `A-server.jar` first, then `String.compareTo`-sorted package entries, then `install_base_key` last. The sorted set is built from `build-label.txt` (from the server jar's `build-data.properties` `build.label`, else `no_version`), basenames of regular files, `embedded_tools/*.zip` members prefixed with `embedded_tools/` (directories skipped), and `.tar`/`.tar.gz`/`.tgz` members under their archive names (directories skipped). Zel's zipper writes inputs in argv or `@filelist` order, keeps directories, and does not sort, pin first or last names, generate `build-label.txt`, or unpack nested zips or tars.
  - Compression: Bazel always raw-deflates (`nowrap`), level 9 or level 1 with `--fast`, and stores only when the deflated size is at least the raw size and the raw size is greater than 0, so empty payloads stay deflated. Without `--fast` it first repacks the server jar as a name-sorted STORED archive of non-directory entries (version 20, DOS epoch, CRC) so the outer entry can compress across classes. Zip64 is allowed, names are UTF-8, and the output channel is `force(true)` before exit. Zel compresses only when `C` is set, at `Z_DEFAULT_COMPRESSION`, and stores unless the deflated size is strictly smaller. Empty files and directories are stored with CRC 0. There is no server-jar stored repack.
  - Timestamps: Bazel sets every entry to `ZipUtil.DOS_EPOCH` (1980-01-01 00:00:00 in the local zone) and version-needed 20, and does not copy external attributes. Zel stamps every local and central header with a fixed DOS date of 2010-01-01 (`kDefaultTimestamp`), version-needed 10, and copies the file mode into the external attributes.
  - Duplicates: neither tool rejects repeated names. Bazel can emit the same name more than once, and `Collections.sort` keeps equal names in insertion order. Zel writes every input even when zip paths collide.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/Zip64EndOfCentralDirectory.java`
- Bazel lines: 77
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`; writer/reader also in ijar `zip.cc`
- Verdict: partial
- Differences:
  - Bazel `read` consumes a fixed 56-byte record, checks signature `0x06064b50`, and stores central-directory size (offset 40), central-directory offset (offset 48), and total entries (offset 32) as unsigned 64-bit values. `archive.rs` only consults the ZIP64 record when the classic EOCD entry count is `0xFFFF` or the offset is `0xFFFFFFFF`, then loads total entries at `z64+32` and the offset at `z64+48`. It leaves the central-directory size as the classic EOCD `uint32`, so a ZIP64 size is never promoted to `uint64`.
  - Bazel `create` writes the size field as `FIXED_DATA_SIZE - 12` (44), version made-by and version needed as `0x2d`, disk numbers as 0, and both entry counts as `uint64` from `getNumEntries()`, then the `uint64` central-directory size and offset. It appends no ZIP64 extensible data sector. `archive.rs` does not write a ZIP64 end-of-central-directory record.
  - Neither Bazel path uses the record-size `uint64` to skip an extensible data sector: `read` always takes exactly 56 bytes; `create` always emits size 44. `archive.rs` also ignores the size field and only finds the record through a locator assumed to sit 20 bytes before the classic EOCD.
  - ijar matches the 56-byte fixed size and the written size value `ZIP64_EOCD_FIXED_SIZE - 12`, reads the entry counts, central-directory size, and offset as `uint64`, and comments that an extensible data sector is not read. Its writer uses version made-by `0x0300` and version needed `0`, and it only emits the ZIP64 record when entries exceed `0xFFFF` or the directory size or offset exceeds `0xFFFFFFFF`.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/Zip64EndOfCentralDirectoryLocator.java`
- Bazel lines: 66
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/third_party/ijar/zip.cc`; also minizip `zip.c`
- Verdict: partial
- Differences:
  - Bazel is a 20-byte record (`SIGNATURE` `0x07064b50`): disk-with-ZIP64-EOCD at offset 4, ZIP64 end-of-central-directory offset as an unsigned little-endian 64-bit value at offset 8, total disk count at offset 16. ijar uses the same signature, size 20, and field order.
  - `create` always writes disk-with-EOCD `0` and total disks `1`, and copies `file.getZip64EndOfCentralDirectoryOffset()` unchanged. ijar writes the same constants. Minizip writes disk `0` and total disks `1`, but stores `zip64eocd_pos_inzip - add_position_when_writing_offset`, so a prepended prefix changes the stored end-record offset.
  - `read` checks the signature, sets ZIP64, and stores only the end-record offset. It never reads the disk-with-EOCD field or the total disk count. ijar reads all three and rejects the archive when the disk-with-EOCD is not 0 or the total disk count is greater than 1 (a count of 0 is accepted). Minizip requires disk-with-EOCD exactly 0 and total disks exactly 1, so a total of 0 fails there and passes in ijar.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/ZipEntryInputStream.java`
- Bazel lines: 119
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`zip_entry_bytes`)
- Verdict: partial
- Differences:
  - Bazel is a shared-stream `InputStream`: after the local-header signature check it skips the name and extra field, then `read`/`skip` stop after `rem` bytes (`getSize()`, or `getCompressedSize()` when `raw`). Zel has no stream, raw mode, `available`, `skip`, `mark`, or `reset`. It always materializes the central-directory uncompressed size and drops `csize`.
  - Stored and any non-`DEFLATED` method: Bazel copies `rem` bytes from the live file stream. A short `read` that returns `-1` does `rem -= len`, so the remaining count increases, and `-1` is returned as EOF. Zel treats every method other than 8 and 9 as stored and returns a clamped slice, so a short entry is a successful shorter `Vec` with no EOF signal.
  - Deflated: Bazel wraps only method 8 in `InflaterInputStream` with `Inflater(true)` and an 8192-byte buffer, and does not limit compressed input to `compressedSize`. Zel does the same for method 8 via `inflate_limit` (raw deflate, no compressed-size bound, no CRC check). It also inflates method 9 (Deflate64). Bazel’s `Compression` is only stored (0) and deflated (8); method 9 fails earlier in `setMethod` with a `NullPointerException` and never reaches this stream.
  - Bazel’s effective cap is `rem` (`long`). `available()` clamps to `Integer.MAX_VALUE`. Zel caps inflate at `size` and, on `"Deflate output exceeds size limit"`, turns the error into `Ok` and truncates the stashed partial to `size`. A stored deflate block is truncated correctly; a back-reference that crosses the limit is filled by repeatedly copying `out[start]`, so those tail bytes differ from Java’s inflater, which stops after a correct prefix of `size`.
  - `close()` is empty: it does not close the shared `ZipReader` stream and does not call `Inflater.end()`. Zel reads each entry from an immutable buffer, and the inflater’s lookahead never moves a shared file position.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/ZipFileData.java`
- Bazel lines: 288
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`Entry`, `parse_zip`, `find_zip_eocd`)
- Verdict: partial
- Differences:
  - Bazel keeps entries in a `LinkedHashMap<String, ZipFileEntry>`: name lookup, and a later `addEntry` with the same name replaces the value. Zel keeps a `Vec<Entry>` with no name index, so duplicate central-directory names are all retained.
  - Both follow central-directory order. Bazel’s order is `LinkedHashMap` insertion order and a replacement stays at the original key position. Zel appends every header in read order and has no replace-in-place.
  - Bazel stores the archive comment as a charset-decoded `String` (default `""`), rejecting comments longer than `0xffff` bytes. Zel uses the EOCD comment length only to accept a signature, and drops both the archive comment and each entry’s comment bytes.
  - Bazel records charset, central-directory size and offset, expected versus actual entry counts (`numEntries` increments even when a name is overwritten), `maybeZip64` / `isZip64`, and the Zip64 end-of-central-directory offset. Zel keeps none of those on the entry list; Zip64 is a parse-local flag used to fix the entry count and central-directory offset.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/ZipFileEntry.java`
- Bazel lines: 444
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`
- Verdict: partial
- Differences:
  - Bazel stores the entry name as a Java `String` and only rejects null. Zel decodes central-directory name bytes with `String::from_utf8_lossy` and ignores general-purpose bit 11. The test writer `build_zip` writes `name.as_bytes()` with flags 0.
  - Bazel `time` is epoch milliseconds, default `-1`, with no DOS conversion in this class. Zel never reads the central-directory time or date and `build_zip` writes four zero bytes.
  - Bazel CRC is a `long`, default `-1`, and `setCrc` rejects values outside `0..=0xFFFFFFFF`. Zel does not load the central-directory CRC, and `zip_entry_bytes` does not check it. `build_zip` does write an IEEE CRC-32 of the uncompressed bytes.
  - Bazel `Compression` is only `STORED` (0, version `0x0a`) and `DEFLATED` (8, version `0x14`); `fromValue` returns null otherwise, and the constructor defaults to `STORED`. Zel keeps a raw `u16`: method 8 is inflate, 9 is inflate64, and every other method, including stored, is a raw copy of `size` bytes.
  - Bazel marks ZIP64 when uncompressed size, compressed size, or local-header offset is `>= 0xFFFFFFFF`, using separate features (version `0x2d`). Negative sizes throw. Zel replaces a field only when that 32-bit value is exactly `0xFFFFFFFF`, reading extra id `0x0001` in uncompressed, compressed, offset order, then discards compressed size. `build_zip` writes 32-bit sizes only and emits no ZIP64 extra.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/ZipReader.java`
- Bazel lines: 378
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`find_zip_eocd`, `parse_zip`)
- Verdict: partial
- Differences:
  - Bazel opens a `RandomAccessFile` and seeks a channel. Zel `extract` reads the whole file, then `parse_zip` walks that buffer. Null `file` or `charset` throws in Bazel; Zel has no charset argument.
  - EOCD search matches in substance. Both walk backward for `PK\x05\x06` and accept a hit only when the 16-bit comment length equals the bytes left in the file, and both scan the whole file (no 64 KiB cap). Bazel uses a 64-byte window with a 3-byte overlap. Zel steps one byte. A file shorter than 22 bytes and a file with no valid EOCD both surface the same `"Zip file '<name>' is malformed..."` text.
  - After the EOCD, Bazel stores the comment in the caller charset and treats the archive as maybe-ZIP64 if any of the six EOCD fields is all-ones. Zel only tries ZIP64 when total entries are `0xFFFF` or the CD offset is `0xFFFFFFFF`.
  - Both then look for the ZIP64 locator 20 bytes before the EOCD and the ZIP64 EOCD at the locator’s 64-bit offset. A bad locator or record is ignored. Bazel, once the locator parses, sets ZIP64 and replaces the CD size, the CD offset, and the entry count. Zel copies the entry count and CD offset, and leaves the CD size as the 32-bit EOCD value. A ZIP64 archive whose only sentinel is the CD size (or a disk field) is opened as ZIP64 by Bazel and as Zip32 by Zel, so a CD past 4 GiB is read from the truncated offset.
  - The non-ZIP64 offset rule is the same: `eocd - cdSize`, and if its low 32 bits equal the recorded offset, read headers until `cdSize` bytes are consumed. Otherwise both read the recorded entry count from the recorded offset. Zel adds `computed >= 0`, so a CD size larger than the EOCD offset cannot take the size-based path.
  - Preamble / SFX is only on the Bazel side. `createForAdjustSfx` sets `isUnadjustedSfx` and `AdjustSfx` then adds `actualStart - recordedOffset` to each local-header offset. Zel has no SFX flag. `strictEntries` forces the count-based read even for Zip32. Zel has no such switch.
  - Bazel inserts into a `LinkedHashMap` keyed by name (last name wins) and `size()` counts every `addEntry`, including overwrites. Zel keeps a `Vec` and preserves duplicate names. Names and comments are decoded with the constructor charset (default UTF-8). Zel uses `from_utf8_lossy` and drops comments.
  - ijar is a different reader (mapped file, comment scan capped at 64 KiB + 22). It does not implement this class’s windowed scan, `strictEntries`, or `isUnadjustedSfx`.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/ZipUtil.java`
- Bazel lines: 234
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/third_party/ijar/zip.cc` (endian helpers in `common.h`)
- Verdict: partial
- Differences:
  - Zel has no `ZipUtil`. Nothing in Zel's own Rust code converts DOS times or encodes zip names. `java_native.rs` only spawns the toolchain `single_jar`. Overlapping code is vendored ijar, protobuf `zip_writer.cc`, rules_pkg `build_zip.py`, minizip, and rules_jvm_external `StableZipEntry`.
  - DOS range is local-zone `GregorianCalendar`: epoch 1980-01-01 00:00:00 through 2107-12-31 23:59:59. `unixToDosTime` throws `IllegalArgumentException` outside that range. `dosToUnixTime` does not validate; a lenient `Calendar` normalizes illegal fields.
  - Packing is PKZIP: second/2, minute, hour, day, month+1, year−1980. Round-trip drops odd seconds, so 23:59:59 becomes 23:59:58. The zone is the JVM default, including DST gaps.
  - ijar writes one constant, `30<<25 | 1<<21 | 1<<16` (2010-01-01 00:00:00 DOS). Protobuf writes time 0 and date `1<<5 | 1` (1980-01-01) and does not range-check. `StableZipEntry` names `DOS_EPOCH` but it is 2010-01-01 in the system zone; `.class` names add 2000 ms. rules_pkg clamps with `max(315532800, timestamp)` in UTC and has no 2107 ceiling. Minizip uses the same bit layout, subtracts 1980 or 80, and does not throw.
  - Little-endian stores are low byte first for 2, 4, and 8 bytes. `get16` and `get32` are signed. `getUnsignedLong` throws `ZipException` when bit 63 is set, because a Java `long` cannot hold that unsigned value. ijar returns unsigned types and does not reject bit 63. Minizip fills every written byte with `0xff` when bits remain; this file truncates.
  - `arrayStartsWith` treats a null array as false and a null target as true. `readFully` throws `IndexOutOfBoundsException` on a negative length and returns the short count, including 0, on EOF. None of the Zel zip writers have those helpers.
  - This file does not encode names. ijar accepts UTF-8 bit 11 on read but both writers store flag 0, and a name longer than 65535 is truncated by `put_u2le`. rules_pkg sets `flag_bits |= 0x800` on every entry.

### `src/java_tools/singlejar/java/com/google/devtools/build/zip/ZipWriter.java`
- Bazel lines: 227
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no port of this raw `OutputStream` filter. `ZipWriter` does not compress. `putNextEntry` requires time, CRC-32, uncompressed size, and compressed size already set (`-1` throws `IllegalArgumentException`), clears general-purpose bit 3, records the local-header offset on the caller’s entry, and writes the local header. `closeEntry` rejects a compressed-size mismatch. `finish` appends the central directory and leaves the underlying stream open. `setComment` rejects comments longer than `0xffff` bytes. `startPrefixFile` / `endPrefixFile` allow a leading prefix only before any entry.
  - ZIP64 is opt-in. The two-argument constructor forces `allowZip64` false. With it true, sizes at `0xffffffff` become a `0x0001` extra (uncompressed then compressed) and a ZIP64 end record plus locator are written when the archive is ZIP64. No Zel writer implements that switch.
  - UTF-8 flag bit 11 is never set here. Names and comments are encoded with the constructor `Charset`. Extended-timestamp (`0x5455`) and Info-ZIP Unix-new extras are dropped. DOS date/time comes from `unixToDosTime` (local zone; out-of-range times throw). No data descriptor is written after the payload.
  - ijar `OutputZipFile` writes the local header with CRC and sizes zero, optionally deflates, then back-patches. Flags are always 0. Every entry gets DOS 2010-01-01. ZIP64 is only the EOCD and locator when the entry count, directory size, or offset overflows. Local sizes stay 32-bit and output is capped at 4GB. No prefix, no archive comment, and `Finish` closes the file.
  - rules_pkg always sets bit `0x800`, stamps a Unix time clamped to 1980-01-01, and compresses through Python `zipfile`. Protobuf writes stored entries only, flags 0, DOS date 1980-01-01, 32-bit sizes, no ZIP64, no comment, and no prefix.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/singlejar/ConcatenateStrategyTest.java`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The test drives `ConcatenateStrategy` through `merge` (`appendNewLine=true`) and `mergeNoNewLine` (`false`), UTF-8, then `finish`. Zel has no `ConcatenateStrategy`. The only in-tree jar merge that concatenates text is vendored `MergeJars.java`, and only for `META-INF/services/*`: `String.join("\n\n", bodies)` plus a trailing `"\n"`, platform default charset, service names in `TreeMap` order. There is no flag to turn newline insertion off.
  - `testSingleInput`: both modes leave a lone `"a"` as `"a"`. `MergeJars` would emit `"a\n"`.
  - `testTwoInputs`: both modes leave `"a\n"` + `"b"` as `"a\nb"` because the previous last byte is already `0x0A`. `MergeJars` inserts `"\n\n"` between the full strings and a final `"\n"`, so this input becomes `"a\n\n\nb\n"`.
  - `testAutomaticNewline`: `"a"` + `"b"` is `"a\nb"` when insertion is on and `"ab"` when it is off. `MergeJars` always produces `"a\n\nb\n"`.
  - `testAutomaticNewlineAndEmptyFile`: `"a"`, `""`, `"b"` is `"a\nb"` with insertion on and `"ab"` with it off. An empty stream does not change `lastByteCopied`. `MergeJars` joins the empty string, producing `"a\n\n\n\nb\n"`. Neither mode adds a newline after the last stream; `MergeJars` always does.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/singlejar/CopyEntryFilterTest.java`
- Bazel lines: 36
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `testSingleInput` builds a `CopyEntryFilter`, calls `accept("abc", callback)`, and asserts the recording callback received exactly `["copy"]`. The filename is ignored; every name is requested as a plain copy.
  - Zel has no `CopyEntryFilter` and no test that an entry name is accepted as a copy. `java_resource_jar_action` only spawns the toolchain `single_jar`. `archive.rs` reads and extracts zip members and does not implement a copy-entry strategy.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/singlejar/DefaultJarEntryFilterTest.java`
- Bazel lines: 110
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `testServiceInput` expects `META-INF/services/any.service` to be `customMerge` with the date `ZipCombiner.DOS_EPOCH`. Zel has no `DefaultJarEntryFilter`. `java_native.rs` only spawns the toolchain `single_jar`.
  - `testOtherSkippedInputs` expects `a.SF`, `a.DSA`, and `a.RSA` to be `skip` with an empty date list.
  - `testClassInput` expects `a.class` to be `copy` with `DOS_EPOCH_PLUS_2_SECONDS`.
  - The same filter tests are also absent: `abc` copies at `DOS_EPOCH`, `protobuf.meta` custom-merges at `DOS_EPOCH`, `JarFile.MANIFEST_NAME` is skipped, and `META-INF/spring.handlers`, `META-INF/spring.schemas`, and `reference.conf` custom-merge at `DOS_EPOCH`.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/singlejar/FakeZipFile.java`
- Bazel lines: 173
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `FakeZipFile` records expected entries, and `assertSame(byte[])` walks a `ZipInputStream` in that order and requires `getNextEntry()` to be null afterward.
  - Each entry checks the method (`DEFLATED` or `STORED`), the name, an optional timestamp, extra-field bytes (a null expected extra must match a null actual extra), and content through `PlainByteValidator` (UTF-8, null content treated as empty) or a custom `ByteValidator`.
  - After the entries it reads the end-of-central-directory disk and total entry counts as unsigned shorts at `data.length - 14` and `data.length - 12` and requires both to equal `entries.size()`, assuming the EOCD is at the end and there is no zip comment. A `preamble` prefix can be skipped, but this file never assigns `preamble`.
  - Zel's `build_zip` and `minimal_zip` only synthesize archives for extraction tests. They do not assert entry order, compression method, timestamps, extra fields, a preamble, or EOCD counts of an existing byte array.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/singlejar/RecordingCallback.java`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `RecordingCallback` implements `ZipEntryFilter.StrategyCallback` and stores invocations in public `List<String> calls` and `List<Date> dates`.
  - `copy(Date date)` appends `"copy"` and the date, including a null date.
  - `rename(String filename, Date date)` appends `"rename"` and the date. The new filename is not stored.
  - `customMerge` appends `"customMerge"` and the date. The strategy instance is not stored.
  - `skip()` appends `"skip"` and does not append a date, so `calls` and `dates` can diverge.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/singlejar/SingleJarTests.java`
- Bazel lines: 27
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel’s suite is an empty JUnit 4 class annotated `@RunWith(ClasspathSuite.class)`. It declares no `@Test` methods.
  - `ClasspathSuite` discovers and runs every test class in `com.google.devtools.build.singlejar`.
  - Zel has no `SingleJarTests` and no `ClasspathSuite`. First-party code only spawns the toolchain `single_jar` binary.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/singlejar/SlowConcatenateStrategy.java`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Package-private final `SlowConcatenateStrategy` implements `ZipEntryFilter.CustomMergeStrategy` and is marked `@NotThreadSafe`. It exists only as a test double.
  - `merge` copies the input with `in.read()` and `out.write(nextByte)`, one byte per call, and inserts no separator bytes.
  - `finish` is empty: all bytes are already written.
  - Vendored `MergeJars` joins `META-INF/services/` bodies with `"\n\n"`, uses the platform charset, always writes a trailing newline, and copies other entries with `ByteStreams.copy`. It is not a one-byte, no-separator test strategy.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/singlejar/ZipCombinerTest.java`
- Bazel lines: 800
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel zip combiner. The nearest code only launches the toolchain `single_jar`. `archive.rs` only extracts repository zips.
  - First-wins copy: adding the same zip twice keeps one `hello.txt`. `addFile("hello.txt")` then the same name from a zip still emits that one entry. The filter is invoked only on the first sighting of the name.
  - A `copy` consumes the name, so later entries of that name skip the filter. An identity `rename` does not consume it that way. Renaming onto a name already written skips that entry after the filter runs. After a `skip`, later same-name entries are not filtered.
  - `ConcatenateStrategy` joins two `hello.txt` payloads as `Hello World!\nHello World!`. `SlowConcatenateStrategy` joins them with no newline. A single slow merge of the byte `0xFF` (`-1`) round-trips. The filter must call the callback exactly once: a second call or no call throws `IllegalStateException`.
  - Default mode keeps the input method. `FORCE_DEFLATE` deflates both a deflated and a stored entry. `FORCE_STORED` stores both. A stored empty `META-INF/` (size, compressed size, and CRC 0) plus a deflated manifest is treated as equivalent to `JarOutputStream` except for extra-field bytes and general-purpose bits 3 and 11.
  - Plain copies are checked at `ZipUtil.DOS_EPOCH`. A custom merge date applies only to the merged name; the copied sibling stays at the DOS epoch.
  - Copied names are written as they are seen; merged names are emitted later, so a merge yields `hello2.txt` then the concatenated `hello.txt`. One hundred `addFile` calls stay in insertion order.
  - A four-byte non-zip and a 32-byte partial local header both fail with `ZipException` "It does not contain an end of central directory record". The first still leaves an empty output zip.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/singlejar/ZipFactory.java`
- Bazel lines: 105
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Fluent in-memory zip builder. `addFile(String, String)` encodes with ISO-8859-1 and stores compressed. `addFile(String, byte[])` clones the array and stores compressed. `addFile(String, String, boolean)` keeps the compressed flag. `addEntry` does not copy the byte array.
  - `toByteArray` writes a `ZipOutputStream`. Compressed entries use `DEFLATED`. Uncompressed entries use `STORED` and set size plus CRC32. Every entry time is `ZipCombiner.DOS_EPOCH`. `IOException` becomes `RuntimeException`.
  - Zel `build_zip` writes raw local and central headers, uses a stored-block deflate body when `deflate` is true, stamps DOS time 0, and sets external attributes. It is not this factory.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/singlejar/ZipTester.java`
- Bazel lines: 412
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `ZipTester` walks a ZIP byte stream and fails with `IOException` diagnostics instead of relying on `java.util.zip.ZipFile`.
  - It recognizes only local-file (`0x04034b50`), data-descriptor (`0x08074b50`), central-directory (`0x02014b50`), and end-of-central-directory (`0x06054b50`) markers, and rejects any other signature with the offset.
  - Local entries are limited to stored (method 0, version 10) and deflate (method 8, version 20) and to flags 0 or the size-masked bit (`1 << 3`). Stored entries must have equal compressed and uncompressed sizes; deflate entries are inflated with a raw `Inflater`, then CRC32, compressed size, and uncompressed size are checked, including the 12-byte data descriptor when sizes are masked.
  - After the local entries it checks that each central-directory record matches the local name, offset, flags, method, and DOS time, then checks the EOCD: disk numbers 0, entry counts equal and equal to `entries.size() % 0x10000`, and an empty comment.
  - Zel archive extraction and gzip CRC checks do not parse local headers, data descriptors, or the central directory the way `ZipTester.validate()` does.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/zip/AdjustSfxTest.java`
- Bazel lines: 77
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `testAdjustSfx` writes a stored `foo.txt` (`"hello"`, DOS epoch, preset CRC and sizes), prepends a 5-byte preamble, and runs `AdjustSfx.main` so local-header, central-directory, and Zip64 end-of-central-directory offsets are shifted. It then opens the rewritten file with a normal `ZipReader` and checks one entry, the name, CRC, size, and bytes.
  - Zel has no `AdjustSfx` rewriter. `parse_zip` only reads a central directory at `eocd - cdSize` when the low 32 bits already match the recorded offset, so a 5-byte unadjusted prefix does not qualify and is never rewritten.
  - `scripts/bzlmod_harness/gen.py` writes `z_sfx.zip` as a shell stub plus `small.zip` and nothing consumes or adjusts it.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/zip/ExtraDataListTest.java`
- Bazel lines: 113
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The test checks null-safety, construction from `ExtraData` records and from a raw buffer, little-endian layout (`id` + `u16` size + payload) for `0xcafe` and `0xbeef`, `contains` / `get` / `remove`, re-append order, and `getByteStream` reading two 7-byte records then EOF (`-1`). It never asserts truncation or duplicate header ids.
  - The class under test rejects a buffer longer than 65535 and a header or payload that does not fit, and stores records in a `LinkedHashMap` keyed by header id, so a repeated id replaces the earlier record.
  - Zel has no id-keyed extra-field list. ijar does not reject a short subfield, and a second ZIP64 block updates a size only while it is still `0xFFFFFFFF`. Minizip copies the extra blob truncated to the caller buffer. Protobuf's zip writer writes extra-field length 0.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/zip/ExtraDataTest.java`
- Bazel lines: 77
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`
- Verdict: partial
- Differences:
  - The Bazel file tests a standalone `ExtraData` record (`0xcafe` id, little-endian header, offset parse, `getId` / `getLength` / `getData` / `getBytes`). Zel has no `ExtraData` type and no tests of that API.
  - Both use a 4-byte little-endian header (u16 id, u16 payload size). Zel only walks that header when compressed size, uncompressed size, or local-header offset is `0xFFFFFFFF`, and only reads payload bytes when the id is ZIP64 (`1`).
  - Short header (fewer than 4 bytes): Bazel throws `IllegalArgumentException` with `"incomplete extra data entry in buffer"`. Zel’s `while e + 4 <= extra.len()` stops and ignores a trailing 1–3 bytes.
  - Short payload (declared size past the buffer): Bazel throws the same exception. Zel does `e += 4 + sz` without checking that `sz` fits. A non-ZIP64 id is skipped; a ZIP64 id that then needs an 8-byte field fails later in `u64le` as `"Truncated zip file"`.
  - ijar `ProcessCentralDirEntry` reads the little-endian id and size and does not reject a short header or a short payload.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/zip/PackageZipperTest.java`
- Bazel lines: 152
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - JUnit4 suite for `PackageZipper.main`. Zel’s only first-party zipper is ijar, which does not take `<output_zip> <server_jar> <install_base_key> [file...]`.
  - `writesEntriesWithLocalDosEpochTimestamp` asserts the archive names are exactly `A-server.jar`, `build-label.txt`, `foo.txt`, and `install_base_key`, that every entry’s DOS time is `0x00210000` (1980-01-01), and that the local-header modification time at offset 10 matches. Zel’s ijar writer stamps January 1, 2010 and never emits `build-label.txt` or `install_base_key`.
  - `optimizedBundleRepackagesServerJarWithStoredEntries` checks the default (non-`--fast`) path: the outer `A-server.jar` entry is `DEFLATED`, and the nested jar is rewritten with `STORED` entries, the same 1980-01-01 DOS time, byte-identical payloads, and sorted names `a.txt`, `build-data.properties`, `empty.txt`, `z.txt` (the fixture’s `empty-dir/` is dropped).
  - `fastBundleKeepsServerJarAsIs` passes `--fast` and asserts the embedded `A-server.jar` bytes equal the input jar. Zel’s zipper has no `--fast` package mode.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/zip/SlowZipReader.java`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `SlowZipReader` subclasses `ZipReader` and overrides `getStreamAt` so ZIP section reads come from a deliberately short `InputStream`.
  - `read(byte[], int, int)` returns at most one byte (or `-1` / `0`), so parsers must handle data that is not fully available in one call.
  - `skip(long)` ignores the requested count and always skips one byte via `super.skip(1)`.
  - Constructors only forward `File`, optional `Charset`, and `strictEntries` to `ZipReader`.
  - Zel’s ZIP code reads a complete `&[u8]` buffer. There is no one-byte read or skip wrapper.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/zip/ZipFileDataTest.java`
- Bazel lines: 121
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - JUnit4 test of the mutable zip index `ZipFileData`. Zel has no such type. `parse_zip` only reads an on-disk central directory.
  - `testNulls` rejects nulls on every public constructor and instance method. Zel has no equivalent API.
  - `testCharset` stores `UTF_8`. Zel decodes entry names with `String::from_utf8_lossy` and has no per-archive charset.
  - Comment tests round-trip `"foo"` as a string and as UTF-8 bytes. Zel uses the EOCD comment length only to confirm the record ends at EOF and never keeps the comment text.
  - A `0x100ff`-byte comment throws `ZipException` `File comment too long. Is 65791; max 65535.` A `u16` comment length cannot express 65791.
  - Zip64 stays off at the 32-bit sentinels and turns on only above them: size and offset `0xffffffff` stay false and `0x100000000` become true; expected entries `0xffff` stay false and `0x10000` become true. `parse_zip` treats on-disk `total == 0xFFFF` or `cd_off == 0xFFFF_FFFF` as a cue to read the Zip64 locator, and it never checks central-directory size against `0xffffffff`.
  - `setZip64EndOfCentralDirectoryOffset(0)` forces Zip64, including offset 0. Zel stores no such offset.
  - 65535 `addEntry` calls leave Zip64 false, and the 65536th sets it. `setZip64(true)` also sets `isMaybeZip64()`. Zel has no `maybeZip64` flag.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/zip/ZipFileEntryTest.java`
- Bazel lines: 212
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`
- Verdict: partial
- Differences:
  - This test never covers name encoding. Names are only `"foo"`. `setName` stores a Java `String` and does not set UTF-8 bit 11. Zel always does `String::from_utf8_lossy` and ignores general-purpose flags. ijar copies the name into a C string truncated at `PATH_MAX - 1` and every writer stores flags 0.
  - This test never calls `setTime`. The field is epoch milliseconds, default `-1`. Zel never reads the central-directory time. ijar writes a fixed DOS stamp for 2010-01-01. `StableZipEntry` uses 2010-01-01 in the system zone and adds 2000 ms for `.class`.
  - `setCrc(-1)` and `setCrc(0x100000000L)` throw `IllegalArgumentException` `"invalid entry crc-32"`. `0xffffffff` is not tested. The field defaults to `-1`. Zel does not read or check the zip CRC; a wrong CRC still extracts. CRC checks exist only for gzip and for `Data::Deflated`.
  - `setSize(0xfffffffeL)` leaves version at `0x0a`. `setSize(0xffffffffL)` and `setSize(0x100000000L)` raise version to `0x2d`. The same threshold applies to compressed size and local-header offset. Negative sizes throw. Zel replaces a field from extra id 1 only when that field equals `0xFFFFFFFF`. `0xFFFFFFFE` stays the 32-bit value. There is no negative-size rejection. ijar writes per-entry sizes as `u4` with extract version 10, not 45.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/zip/ZipReaderTest.java`
- Bazel lines: 690
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`
- Verdict: partial
- Differences:
  - EOCD acceptance matches: scan backward for `PK\x05\x06` and keep it only when the comment length equals the bytes after the 22-byte record. Empty, 3-byte, and 4-byte files use the same malformed-archive text. Zel only tests a valid zip plus one trailing byte.
  - An embedded `PK\x05\x06` inside the comment is ignored. Zel never returns the archive comment, so the 120-byte comment cases are absent.
  - Java finds that 120-byte comment by sliding a 64-byte window with a 3-byte overlap, and a signature at index 47 seeks because the comment-length field crosses the window. Zel indexes the full slice, so that bounds error cannot occur.
  - The preamble half does not match. `createForAdjustSfx` sets the central-directory start to the discovered EOCD minus the directory size. Zel trusts `eocd - cdSize` only when its low 32 bits equal the recorded offset; a 5-byte stub makes them differ. ijar does handle this gap.
  - ZIP64 with EOCD offset `0xffffffff` and extra id `0x0001` is the path Zel implements. Zel does not check version-needed 45, CRC, empty comment, or method.
  - Non-sentinel EOCD fields mean Java’s `isMaybeZip64` is false, and Zel also skips the locator unless the count is `0xFFFF` or the CD offset is `0xFFFFFFFF`.
  - Zel does not treat disk number, CD disk, entries-on-disk, or CD size sentinels as ZIP64, and it does not copy the ZIP64 directory size at record offset 40.
  - With ZIP64 disallowed, the EOCD count is truncated to `0x00ff` while the directory still holds `0x100ff` entries. `strictEntries` stops at `0x00ff`. The default reader, and Zel, read by directory size and see `0x100ff`. Zel has no `strictEntries` switch.
  - This file has no duplicate-name case. Java replaces by name in a `LinkedHashMap` while `size()` still counts every record. Zel appends every central-directory record.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/zip/ZipTests.java`
- Bazel lines: 27
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Empty JUnit 4 aggregator annotated `@RunWith(ClasspathSuite.class)` with no `@Test` methods. `ClasspathSuite` discovers every test class on that classpath.
  - The Bazel `java_test` sets `test_class` to this class and depends on every `*.java` in the directory. Separate `PackageZipperTest_behind_utc` and `PackageZipperTest_ahead_of_utc` targets run `PackageZipperTest` with `TZ` set to `America/Los_Angeles` and `Europe/Berlin`.
  - Zel has no `ClasspathSuite` and no `com.google.devtools.build.zip` test suite.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/zip/ZipUtilTest.java`
- Bazel lines: 177
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`
- Verdict: partial
- Differences:
  - No Zel tests for these 24 cases. `u16le`/`u32le`/`u64le` are unsigned and return `"Truncated zip file"` on a short buffer. `ZipUtil` does not bounds-check, and every test uses offset 0.
  - Unsigned layouts match (`{0x34,0xff}` → 65332, `{0xff,0xff}` → 65535). The signed Java results are absent: `get16` of `{0x34,0xff}` is `(short)-204`, and `get32` of `{0x34,0xff,0xff,0xff}` is `-204`.
  - No Zel encoder for the signed patterns `-3532` → `{0x34,0xf2}` or sign-extension of a negative int to eight `0xff` high bytes. Minizip saturates a field that does not fit to `0xff`; `ZipUtil` keeps the low bytes.
  - DOS time is not read. The fixture `(20<<25)|(2<<21)|(14<<16)|(3<<11)|(7<<5)|(15>>1)` is 2000-02-14 03:07:14 in the JVM default zone, so the expected millis move with the zone. rules_pkg forces UTC.
  - Odd-second round trip is missing: 03:07:15 must come back as 03:07:14. One second before local `DOS_EPOCH` throws. That epoch equals `315532800` only in UTC. rules_pkg clamps instead of throwing. `MAX_DOS_DATE` (local 2107-12-31 23:59:59) must be accepted; one second later must throw. No Zel path rejects 2108-01-01.
  - `dosToUnixTime(0)` is lenient and normalizes to before 1980, and `unixToDosTime` then throws. The equality assertion is not reached. ijar stamps 2010-01-01 with no zone. Protobuf stamps 1980-01-01 00:00:00. `StableZipEntry` uses 2010-01-01 in the system zone plus 2000 ms for `.class`.

### `src/java_tools/singlejar/javatests/com/google/devtools/build/zip/ZipWriterTest.java`
- Bazel lines: 490
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/resources/third_party/ijar/zip.cc`
- Verdict: partial
- Differences:
  - UTF-8 flag: every test constructs the writer with `UTF_8`, which only selects the charset. Nothing sets or asserts bit 11. ijar and protobuf always write flags 0. rules_pkg always sets `0x800`.
  - Timestamps must match a DOS-legal time (year 1980–2107, day 1–29, even second). ijar stamps 2010-01-01. Protobuf uses 1980-01-01 and time 0. rules_pkg stores a caller UTC time.
  - The writer does not compress. One test writes a raw DEFLATED payload from a nowrap `Deflater`. ijar deflates only when the result is smaller. Protobuf is stored-only. rules_pkg does not accept a precompressed payload.
  - 65535 empty entries with Zip64 on report `0xffff` from both readers. `0x100ff` entries report `0x100ff`. With `allowZip64=false`, a Zip64 reader sees `0x00ff` and a non-Zip64 reader sees `0x100ff`. ijar has no force-Zip32 mode and upgrades instead of truncating the 16-bit count. Its ZIP64 version-needed is 0.
  - Sizes `0xffffffff` and `0x1000000ff` round-trip through `java.util.zip.ZipFile` with extra id `0x0001` and version 45. `putNextEntry` without Zip64 throws `ZipException` containing `Writing an entry of size` and `without Zip64 extensions is not supported.` ijar stores sizes as `u4` and caps output at 4GB.
  - Archive and entry comments persist. ijar, protobuf, and rules_pkg write comment length 0.
  - A write before an entry throws and leaves the file empty. A compressed-size mismatch throws on `closeEntry`. Version 1 or 2 for STORED is raised to `0x0a`. Flag `0x0008` is cleared. Internal and external attributes are kept. A `#!/bin/bash` prefix stays executable, and a prefix after an entry is rejected. After `finish()`, further writes throw `IllegalStateException`. ijar `Finish()` is idempotent.
