# Bazel vs zel code differences: third party

Category rule: `third_party/` vendored code and patches.

- Failure entries: 934
- Missing: 930
- Partial: 4
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `third_party/grpc-java/compiler/src/java_plugin/cpp/java_generator.cpp`
- Bazel lines: 1248
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no protoc plugin that emits a `*Grpc` Java class (package from the protobuf Java outer class, `SERVICE_NAME`, `@Generated` / `@GrpcGenerated`, and `@Deprecated` when the service option is set).
  - [P2] No double-checked method descriptors: `METHOD_*` / `get*Method()`, unary / client-streaming / server-streaming / bidi types, `setSafe` for `NO_SIDE_EFFECTS`, `setIdempotent` for `IDEMPOTENT`, lite vs normal marshallers (`ProtoLiteUtils` vs `ProtoUtils`), and a schema descriptor only for the normal flavor.
  - [P2] No async, blocking, or future client stubs, `ImplBase`, method-handler classes, or `bindService` / `getServiceDescriptor` (including the optional gRPC version string when `disable_version` is false).
  - [P2] No Java identifier rewriting (`MixedLower` keyword suffix, `ToAllUpperCase`) or javadoc emission (leading comments, HTML escapes, `<pre>` bodies).

### `third_party/grpc-java/compiler/src/java_plugin/cpp/java_generator.h`
- Bazel lines: 75
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no Java gRPC protoc generator that writes a service interface from a protobuf service descriptor into an output stream.
  - [P2] There is no normal-versus-lite flavor switch and no flag that disables the generated runtime version check.
  - [P2] There are no helpers that return the Java package of a file's gRPC services or the outer class name that wraps one service.
  - [P3] There is no check or fail path that writes `CHECK FAILED: <file>:<line>:` and the message to stderr, then aborts the process.

### `third_party/grpc-java/compiler/src/java_plugin/cpp/java_plugin.cpp`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No zel protoc plugin entry point that registers a Java gRPC code generator and advertises only `FEATURE_PROTO3_OPTIONAL`.
  - [P2] Generator parameters are not parsed: a `lite` key does not switch the flavor from normal to lite, and a `noversion` key does not disable version emission. Unknown keys are ignored and the `error` string is never set.
  - [P2] No per-service output path: the Java package is not turned into a directory (`.` to `/`, trailing `/` when non-empty) and `{package dir}{service class name}.java` is not opened for each service.
  - [P2] `GenerateService` is never invoked with the service, flavor, and version flag, and the plugin does not always return success after writing those files.

### `third_party/grpc/bazel/cc_grpc_library.bzl`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no `cc_grpc_library` macro. It does not reject `srcs` unless the list has length 1 (`fail` on attribute `srcs` with the singleton-list message).
  - [P2] It does not create a private `generate_cc` target named `_<name>_grpc_codegen` with `plugin = //third_party/grpc:cpp_plugin`, `protoc = //third_party/protobuf:protoc`, `generate_mocks` (default false), the same `srcs`, and the macro’s common `**kwargs`.
  - [P2] It does not wrap that target in a `cc_library` of the given `name` with both `srcs` and `hdrs` set to `:<codegen>`, `deps` equal to the caller’s `deps` plus `//third_party/grpc:grpc++_codegen_proto`, and kwargs formed by `extra_cc_library_kwargs` then overwritten by `**kwargs`.
  - [P1] There is no `generate_cc` rule and no C++ gRPC stub or mock codegen. A native `cc_library` is reported as a rule class with no implementation. `zel-remote` gRPC is a remote-execution client, not this macro.

### `third_party/grpc/bazel/generate_cc.bzl`
- Bazel lines: 174
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No zel rule generates C++ protobuf or gRPC stubs from `ProtoInfo` (`check_deps_sources`, `transitive_imports`).
  - [P2] Missing plugin outputs `{stem}.grpc.pb.h`, `{stem}.grpc.pb.cc`, and `{stem}_mock.grpc.pb.h` when `generate_mocks` is set, and the no-plugin `{stem}.pb.h` / `{stem}.pb.cc` pair.
  - [P2] Missing the `protoc` action: `--cpp_out=<flags>:<genfiles+proto_root>` or plugin args, `--proto_path` for each transitive import plus the output directory, source paths with the file root stripped, mnemonic `CppGrpcGenSource`, plugin as a tool, and `use_default_shell_env`.
  - [P2] Missing package-relative path stripping (`workspace_root` + package) that fails when a proto is outside the package, `output_to_genfiles`, and a `DefaultInfo` of the declared outputs.
  - [P1] `proto_library` is an analysis error with no outputs. `cc_proto_library_*_suffixes` and the proto toolchain flags do not run this generator.

### `third_party/grpc/bazel/protobuf.bzl`
- Bazel lines: 250
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no implementation of these gRPC protoc helpers. `proto_compiler` and `cc_proto_library_*_suffixes` are only flag defaults, and vendored protobuf `.bzl` files are not a Rust port of this module.
  - [P2] Missing `get_proto_root`: a non-empty `workspace_root` becomes `"/" + workspace_root`, otherwise `""`.
  - [P2] Missing generated-name calculation: strip a trailing `.proto` or fail, then apply a format string such as `"{}.pb.h"`.
  - [P2] Missing include-directory resolution for `--proto_path`: virtual imports keep `<root>/_virtual_imports/<first segment>`; generated files drop `root.path`; `external/<repo>` paths stop at the repository directory; otherwise the result is `root.path` or `"."`.
  - [P2] Missing plugin arguments: `--plugin=protoc-gen-<name>=<path>` and `--<name>_out=`, with flags joined by commas before `dir_out`, and `generate_mock_code=true` appended when mocks are requested. The default plugin name is `PLUGIN`.
  - [P2] Missing staging: direct `ProtoInfo` sources in another package that are not virtual imports are copied with `cp` under mnemonic `CopySourceProto`; same-package and virtual-import files are used in place. Transitive imports are not collected as includes.
  - [P2] Missing proto arguments and declared outputs: virtual imports are stripped to the path after the include directory; other protos under the genfiles directory drop that prefix; virtual-import outputs keep the path from `_virtual_imports` onward, and other outputs use the basename.
  - [P2] Missing `--<lang>_out` directory: mixed virtual and real sources fail with `Proto sources must be either all virtual imports or all real`. An all-virtual set uses the first file's include directory, drops one trailing `workspace_root` when present, and sets `import_path` to the path starting at `_virtual_imports`. Otherwise the directory is `genfiles_dir` plus `workspace_root` when set, and `import_path` is unset.
  - [P2] Missing virtual-import detection: a file counts only when it is not a source file and its path contains the virtual-folder marker (default `/_virtual_imports/`).

### `third_party/grpc/build_defs.bzl`
- Bazel lines: 172
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no `java_grpc_library` or `_java_grpc_gensource`. Bazel fails when `srcs` has more than one entry, and prints (does not fail) when a proto's package differs from the rule. It runs `protoc` with `//third_party/grpc-java:grpc-java-plugin` and `--grpc-java_out=enable_deprecated={true|false}:{name}.jar` (`enable_deprecated` defaults to false). Include flags are `-I{logical}={path}` from `ProtoInfo.transitive_imports`, where the logical path is `short_path` in the main repo, the segment after `/_virtual_imports/<dir>/` when that marker is present, otherwise the path with the `workspace_root` prefix stripped. Direct proto sources are positional args. Inputs are those sources plus imports, tools are protoc and the Java plugin, mnemonic is `JavaGrpcGenSource`, and `use_default_shell_env` is true. A second action copies that jar to `%{name}.srcjar`.
  - [P2] `java_grpc_library` then builds a `java_library` named `name` whose `srcs` are the private `{name}_srcs` target tagged `avoid_dep`. Deps are `//third_party:javax_annotations`, `//third_party:jsr305`, `//third_party/grpc-java:grpc-jar`, `//third_party:guava`, `@com_google_protobuf//:protobuf_java`, plus the caller's `deps`. `visibility` applies only to the library. `constraints` is accepted and ignored. Other kwargs go to both the genrule and the library.
  - [P3] Zel has no `grpc_cc_library`. Bazel wraps `cc_library` with `defines = ["GRPC_ARES=0"]`, `hdrs` equal to `hdrs` plus `public_hdrs`, `deps` plus `//third_party/<each external_dep>`, `includes` of `.` and `include`, and `linkopts` of `-pthread` except an empty list on `:windows`. `language` equal to `C` in any case adds `-std=c99` and `-Wimplicit-function-declaration`, or `/we4013` on `:windows`. C++ leaves `copts` empty. `standalone` is accepted and ignored. `alwayslink` defaults to `0`. `testonly`, `visibility`, and `data` are forwarded.

### `third_party/ijar/classfile.cc`
- Bazel lines: 2030
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] `StripClass` rewrites a JVM class file into an interface class. Zel has no class-file parser or stripper. Header jars are a Turbine or JavacTurbine spawn from `java_create_header_action` in `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`. The vendored ijar tree under `/workspace/zel-cursor/crates/zel-bazel-tools/resources/third_party/ijar` has zip I/O only. Its `BUILD` still lists `classfile.cc`, and that source is absent.
  - [P2] The magic must be `0xCAFEBABE`, or the process aborts. An unknown constant-pool tag copies the input bytes and still returns keep. A class-level annotation of type `Lkotlin/Metadata;` also copies the bytes unchanged.
  - [P2] A class that has an `EnclosingMethod` attribute returns keep false and writes nothing, so the caller omits that entry. Every other class is rewritten. Private fields, `<clinit>`, private methods, and package-private synthetic methods that are not bridges are omitted. Surviving members lose `Code`, `StackMapTable`, `LineNumberTable`, `LocalVariableTable`, `LocalVariableTypeTable`, `SourceFile`, `Synthetic`, `BootstrapMethods`, and `SourceDebugExtension`.
  - [P2] Kept attributes are `Exceptions`, `Signature`, `Deprecated`, `EnclosingMethod`, `InnerClasses`, `AnnotationDefault`, `ConstantValue`, runtime-visible and runtime-invisible annotations, parameter annotations, and type annotations, `MethodParameters`, `NestHost`, `NestMembers`, `Record`, `PermittedSubclasses`, and the opaque blobs `Scala`, `ScalaSig`, `ScalaInlineInfo`, `TASTY`, and `TurbineTransitiveJar`. `InnerClasses` is written last. A non-anonymous entry stays when the inner class is already referenced, its name was taken from a descriptor or signature, or its outer class is this class, and that pull then keeps the outer class and the inner name. `NestMembers` keeps a class only when it is already referenced or named in those signatures. The output constant pool is the reachable subset, and `long` and `double` still occupy two slots.
  - [P2] Any other attribute is skipped. `com.android.tools.r8.SynthesizedClass` and `SynthesizedClassV2` are silent. Each other unknown name is printed once on stderr. A bad constant-pool index, a type-annotation target outside `0x00`, `0x01`, and `0x10`–`0x17`, or an illegal annotation element tag aborts. A signature byte that `Expect` rejects calls `exit(1)`.

### `third_party/ijar/common.h`
- Bazel lines: 117
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ijar cursor codec. These helpers read or write with no length check and advance the pointer: `get_u1`/`put_u1` one byte; `get_u2be`/`put_u2be` and `get_u4be`/`put_u4be` big-endian; `get_u2le`/`put_u2le`, `get_u4le`/`put_u4le` little-endian; `get_u8le`/`put_u8le` little-endian 64-bit as the low 32 bits then the high 32 bits. There is no big-endian 64-bit pair.
  - [P3] `put_n` copies `n` bytes with `memcpy` and advances the destination by `n`. `u8` is `unsigned long long`; `u4`/`u2`/`u1` are `uint32_t`/`uint16_t`/`uint8_t`. On Windows this header sets `PATH_MAX` to 4096 and `mode_t` to `int`.
  - [P3] `verbose` is only an `extern bool`. `/workspace/zel-cursor/crates/zel-analysis/src/build_options_table.rs` stores `use_ijars` as a boolean defaulting to true, and `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` reads Zip fields with `from_le_bytes` while extracting repository archives. Neither strips class files nor writes jars through this pointer API.

### `third_party/ijar/dummy_zlib_client.cc`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `ComputeCrcChecksum` always returns 0. With `compute_crc` set, `OutputZipFile::FinishFile` then rejects every non-empty entry as `"Error calculating CRC32 checksum."`; a zero-length entry is accepted with CRC 0.
  - [P3] `TryDeflate` always returns 0 and does not rewrite the buffer. A non-empty entry with `compress` set then fails as `"Error compressing files."`; an empty entry stays method STORED.
  - [P3] `Decompressor::UncompressFile` always returns null and `GetError` always returns null, so a compressed input entry fails in `InputZipFile::ProcessFile` with no decompressor error string. The constructor and destructor allocate nothing.
  - [P3] Zel has no no-op ijar zlib client. Its ijar tree only vendors the real `zlib_client.cc`, and `zel-util` CRC-32/DEFLATE always compute a real checksum and a raw deflate stream.

### `third_party/ijar/ijar.cc`
- Bazel lines: 514
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] Zel has no interface-jar tool. Vendored `third_party/ijar` keeps the zip library and zipper (`zip_main.cc`) and omits `ijar.cc` and `classfile.cc`. Nothing in the Rust tree strips class files or stamps jars.
  - [P2] No CLI: `-v`, `--strip_jar`, `--nostrip_jar` (default strip), `--target_label`, `--injecting_rule_kind`, input jar, optional output. A missing output whose input ends in `.jar` becomes `<name>-interface.jar`; otherwise stderr and exit 1. Extra arguments or a missing flag value print usage and exit 1.
  - [P2] Strip mode keeps `.class` except `META-INF/TRANSITIVE/*.class`. `module-info.class`, `META-INF/*.kotlin_module`, `kotlin/*.kotlin_builtins`, and `*.tasty` are copied stored with a CRC. Other classes go through `StripClass` and are dropped when it returns false. Every other entry is omitted.
  - [P2] Copy mode (`--nostrip_jar`) stores every entry with a CRC, skips the original `META-INF/` and `META-INF/MANIFEST.MF`, and rewrites the manifest. With both an existing manifest and a label, main-section `Target-Label` and `Injecting-Rule-Kind` lines are removed and the new attributes are inserted before the first blank line. A label with no manifest writes `Manifest-Version: 1.0`, `Created-By: bazel`, the label, and the optional rule kind. No label copies the existing manifest unchanged. Strip mode writes that generated manifest only when a label is set, plus an empty `META-INF/` entry.
  - [P2] An output with zero entries gets a stored `dummy` member so javac will accept it. Open, process, and finish failures print to stderr and abort. Verbose mode logs each entry and the output/input size percentage.

### `third_party/ijar/mapped_file.h`
- Bazel lines: 83
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Rust type memory-maps an existing file and exposes open success, a C-string error, the mapped byte buffer, and the file length.
  - [P3] No prefix discard that unmaps a leading byte count from a mapped input, and no `Close` that returns an int status.
  - [P3] No output mapping that reserves an estimated size and then `Close(size)` commits a shorter final length.

### `third_party/ijar/test/A.java`
- Bazel lines: 71
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no copy of this fixture and no class-file rewriter. Vendored ijar is only the zip reader and writer. `use_ijars` and header compilation spawn an external turbine or javac tool and never strip a `.class`.
  - [P3] The empty `static {}` is removed, so the interface class has no `<clinit>`.
  - [P3] `privateField` and `privateMethod` are removed, including the code constant `foofoofoofoo`. `protectedField` and generic `protectedMethod` stay. Every `Code` attribute is removed. The input has two private members and five method bodies. The interface jar has neither.
  - [P3] `private class PrivateInner` is still emitted. The interface jar has five entries: `A`, `A$PrivateInner`, `A$PublicInner`, `A$MyAnnotation`, and `A$RuntimeAnnotation`.
  - [P3] `public static final long L1 = 123L` keeps `ConstantValue`, so a dependent compiles to `ldc2_w 123`.
  - [P3] The generic method keeps `Signature`. `@Deprecated` and `throws IOException` stay, so compiling a subclass still warns and still catches `IOException`.
  - [P3] `MyAnnotation` keeps default element values for a string, an enum, a class, a string array, and a nested `@Deprecated`. `@Retention(RUNTIME)` on `RuntimeAnnotation` stays, so `isAnnotationPresent` is true at runtime.

### `third_party/ijar/test/AnnotatedClass.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `AnnotatedClass` fixture and no class-file rewriter. Vendored ijar under `/workspace/zel-cursor/crates/zel-bazel-tools/resources/third_party/ijar` is the zip reader and writer only. `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` only spawns the toolchain header compiler.
  - [P3] The source is a package-private `final` class. `@Annotations.RuntimeInvisible` (default CLASS retention) and `@Annotations.RuntimeVisible` (`RUNTIME`) are applied to the class, the package-private `String field`, the private constructor, `public static void publicStaticMethod`, its `String param`, and the local `local`.
  - [P3] `publicStaticMethod` sets `local` to `"Hello " + param + "!"` and prints it. That body is a `Code` attribute, and the local-variable annotations are type annotations inside `Code`. ijar drops `Code`, so the concatenation, the print, and both local annotations are removed. The descriptor `(Ljava/lang/String;)V` and both method and parameter annotation attributes remain.
  - [P3] The private constructor and its annotations are dropped (`ACC_PRIVATE`). The class, the non-private field, and both class and field annotation attributes remain. `StripVerifyingVisitor` accepts those annotations and rejects any remaining `Code` or private member.

### `third_party/ijar/test/Annotations.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel’s ijar test fixture is a non-instantiable package-private `Annotations` holder (private constructor) with three nested marker annotations. Zel has no matching types, no `third_party/ijar/test` sources, and no `classfile.cc` / `ijar.cc` even though the vendored ijar `BUILD` still names them.
  - [P3] `RuntimeInvisible` has no `@Retention`, so retention is `CLASS`, and `@Target` is `ANNOTATION_TYPE`, `CONSTRUCTOR`, `FIELD`, `LOCAL_VARIABLE`, `METHOD`, `PARAMETER`, and `TYPE`. `AnnotatedClass` applies it on a type, field, constructor, method, parameter, and local variable.
  - [P3] `RuntimeVisible` uses the same target set with `@Retention(RetentionPolicy.RUNTIME)`, so it is stored as a runtime-visible annotation rather than a class-file-only one.
  - [P3] Public `ParametersOnlyAnnotation` also has default `CLASS` retention, but `@Target` is only `PARAMETER`. `UseRestrictedAnnotation` puts it on a method, and `testRestrictedAnnotations` expects that compilation against the produced interface jar to fail, which checks that ijar kept the `@Target` meta-annotation.

### `third_party/ijar/test/B.java`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no client of an ijared `A` that fails to compile unless `protectedMethod` keeps its generic `Signature` (`String str = protectedMethod("foo")` is illegal if the return type erases to `Object`).
  - [P3] Nothing requires `deprecatedMethod()` to still throw `IOException` (the `catch` would not compile if the `Exceptions` attribute is dropped) or to emit a javac deprecation warning.
  - [P3] Nothing checks that `A.L1` stays a compile-time constant: `l(A.L1)` must compile to `ldc2_w` of `123`, which only happens when `ConstantValue` survives.
  - [P3] Nothing instantiates `A.PublicInner` from a subclass constructor, so retention of that inner class is untested.
  - [P3] Nothing places `A.MyAnnotation` on a method or `A.RuntimeAnnotation` (`RetentionPolicy.RUNTIME`) on a public field and then asserts `Class.forName("B").getField("k").isAnnotationPresent(RuntimeAnnotation.class)`, throwing `AssertionError("RuntimeAnnotation got lost!")` on failure and printing `B.main() OK` to stderr on success.

### `third_party/ijar/test/DeprecatedParts.java`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel’s ijar keeps both the JVM `Deprecated` attribute (from a Javadoc `@deprecated` tag) and the runtime-visible `@Deprecated` annotation so a later compile still emits mandatory deprecation warnings. Zel has no class-file rewriter that preserves either.
  - [P3] The fixture places both forms on a public static field, an instance field, an instance method, a static method, a nested annotation type, a nested class, a constructor, and a nested interface. Compiling a client against the interface jar must succeed and report at least 16 mandatory deprecation warnings. Zel has no equivalent fixture or check.
  - [P3] Parameter `@Deprecated` is omitted because javac does not honor it. Zel does not record that exception.

### `third_party/ijar/test/DumbClass.java`
- Bazel lines: 24
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Package-private generic class `DumbClass<V>`. Zel has no type with this shape.
  - [P3] Public instance field `inner` whose type is the private member class `Inner`, so a private nested type is part of the public API.
  - [P3] `Inner` is a non-static private member class with one package-private field `V v` and no declared methods. The compiler adds the outer `this` and the constructors.
  - [P3] This source is compiled into `ijar_testlib`. `PrivateNestedClass` then compiles `new DumbClass<String>().inner` against the interface jar, which requires both the public field and the private nested class to survive stripping. Zel has no ijar class stripper and no such client compile.

### `third_party/ijar/test/EnclosingMethod.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no type that compiles to an anonymous class carrying an `EnclosingMethod` attribute. The vendored ijar `BUILD` names `classfile.cc` and `ijar.cc`, but those sources are absent, and no Rust code reads or writes that attribute.
  - [P3] Package-private `EnclosingMethod` contains package-private member class `Inner`.
  - [P3] `Inner.run` is `public <T> T run(Callable<T>)`. It returns `callable.call()`. Any `Exception` yields `null`. An `Error` propagates.
  - [P3] `Inner.asCallable` is `private <T> Callable<T> asCallable(final Callable<T>)`. It returns an anonymous `Callable` whose `call()` declares `throws Exception` and returns `Inner.this.run(callableToWrap)`. Nothing in this file calls `asCallable`, so the anonymous class is only a compile-time class file.
  - [P3] That class file is `EnclosingMethod$Inner$1`. Its `EnclosingMethod` attribute names `EnclosingMethod$Inner.asCallable` with the erased descriptor `(Ljava/util/concurrent/Callable;)Ljava/util/concurrent/Callable;`. ijar then omits that class and the private method, and keeps `Inner.run` with its body removed. Zel does not perform that strip.

### `third_party/ijar/test/GenDuplicatedUnsupportedAttribute.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no program that writes a JAR at `args[0]` containing `unsupportedattributes/Test1.class` and `unsupportedattributes/Test2.class`.
  - [P3] Each class is version 52, `ACC_PUBLIC|ACC_SUPER`, extends `java/lang/Object`, and has one class attribute `some_unknown_type` whose body is the big-endian int 1234. The attribute is emitted once per class. The two class files are what repeat it.
  - [P3] Each class has public `<init>()V` (`aload_0`, `invokespecial Object.<init>`, `return`, max stack 1, max locals 1) and public static `main([Ljava/lang/String;)V` that loads `System.err`, the string `Hello`, and calls `PrintStream.println`, with max stack 2 and max locals 1.
  - [P3] The archive is a `JarOutputStream`: a default `META-INF/MANIFEST.MF` (`Manifest-Version: 1.0`) plus DEFLATE entries. A missing path argument throws before any write.

### `third_party/ijar/test/GenDynamicConstant.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel writes `args[0]` as a JAR (default `JarOutputStream` manifest `META-INF/MANIFEST.MF`, then deflated entry `dynamicconstant/Test.class`). Zel has no generator for that JAR.
  - [P3] The class is major version 16, `ACC_PUBLIC|ACC_SUPER`, name `dynamicconstant/Test`, superclass `java/lang/Object`, no interfaces or signature.
  - [P3] `public static int get()` has a `Code` attribute whose body is `iconst_5; ireturn` (`max_stack` 1, `max_locals` 0).
  - [P3] `public static int run()` has a `Code` attribute that `ldc`s a `CONSTANT_Dynamic` named `const` with descriptor `I`, then `ireturn`. The bootstrap is `H_INVOKESTATIC` `java/lang/invoke/ConstantBootstraps.invoke` with descriptor `(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/Class;Ljava/lang/invoke/MethodHandle;[Ljava/lang/Object;)Ljava/lang/Object;` and one argument, an `H_INVOKESTATIC` handle to `dynamicconstant/Test.get()I`. Zel emits no `BootstrapMethods` attribute and no dynamic constant.

### `third_party/ijar/test/GenJarWithManifestSections.java`
- Bazel lines: 64
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no tool that writes `args[0]` as a jar. `JarOutputStream(OutputStream)` first emits a deflated `META-INF/MANIFEST.MF` (timestamp = now) containing only `Manifest-Version: 1.0`. A second entry of the same name is then written with `setTime(0)` (DOS-clamped to 1980-01-01).
  - [P3] That second manifest always has `Manifest-Version: 1.0`, named section `foo` with `Foo: bar`, and named section `baz` with `Another: bar` (section order follows `HashMap`). If `args.length > 1`, the main attributes also get `Target-Label` set to `args[1]`; further arguments are ignored. A missing `args[0]` throws `ArrayIndexOutOfBoundsException`.
  - [P3] After the manifest, the jar gets a deflated `java/lang/String.class` at `setTime(0)`, whose bytes are the running JVM’s `/java/lang/String.class` resource.

### `third_party/ijar/test/GenKeepForCompile.java`
- Bazel lines: 205
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not write a JAR to a caller-supplied path. Bazel writes `args[0]` as a JAR (default manifest) with one entry, `functions/car/CarInlineUtilsKt.class`.
  - [P3] That class is Java 8, public final, superclass `java/lang/Object`, source `CarInlineUtils.kt`, with a runtime-visible `kotlin.Metadata` annotation: `mv` `[1, 1, 11]`, `bv` `[1, 0, 2]`, `k` `2`, and `d1`/`d2` naming `customName`, `Lfunctions/car/Car;`, `formatYear`, and `Lkotlin/Function1;`.
  - [P3] The only member is private static final `customName(Car, Function1) -> String`, with a signature over `Function1<? super Integer, String>`, mandated parameter `$receiver`, runtime-invisible `@NotNull` on the method and both parameters, and a body that null-checks both arguments then returns `formatYear(year) + ' ' + make + ' ' + model`, including the `$i$f$customName` local.

### `third_party/ijar/test/GenKotlinModule.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no program that writes a jar to the path in `args[0]` with `META-INF/bar.kotlin_module` containing UTF-8 `hello`, `kotlin/kotlin.kotlin_builtins` containing UTF-8 `goodbye`, and `java/lang/String.class` copied from the running JVM's `String.class` resource.
  - [P3] Those three entries have zip time 0. `JarOutputStream` also writes a default `META-INF/MANIFEST.MF` and deflates the entries. Zel does not produce this ijar kotlin-module fixture.

### `third_party/ijar/test/GenMetaInfTransitive.java`
- Bazel lines: 44
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no program that writes a jar to the path in `args[0]` with `META-INF/TRANSITIVE/java/lang/String.class` and `java/lang/String.class`, both copied from the running JVM's `String.class` resource.
  - [P3] Both entries have zip time 0. `JarOutputStream` also writes a default `META-INF/MANIFEST.MF` and deflates the entries. Zel does not produce this ijar META-INF/TRANSITIVE fixture.

### `third_party/ijar/test/GenModuleInfo.java`
- Bazel lines: 44
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel tool writes a JAR at `args[0]` with deflated entries `module-info.class` (UTF-8 `hello`), `foo/module-info.class` (UTF-8 `goodbye`), and `java/lang/String.class` copied from the running JVM's `/java/lang/String.class`.
  - [P3] Those three entries are written with zip time 0. `JarOutputStream` also adds a default `META-INF/MANIFEST.MF` that this generator does not timestamp.

### `third_party/ijar/test/GenScalaTasty.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no program that writes a jar to the path in `args[0]` with `Bar.tasty` containing UTF-8 `hello` and `java/lang/String.class` copied from the running JVM's `String.class` resource.
  - [P3] Both entries have zip time 0. The one-argument `JarOutputStream` writes no `META-INF/MANIFEST.MF`, adds JAR extra-field magic on the first entry (`Bar.tasty`), and deflates the entries. Zel does not produce this ijar Scala TASTy fixture.

### `third_party/ijar/test/GenSourceDebugExtension.java`
- Bazel lines: 84
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no program that writes a jar to the path in `args[0]` with entry `sourcedebugextension/Test.class`. `JarOutputStream` also writes a default `META-INF/MANIFEST.MF` and deflates the entries.
  - [P3] That class is major version 52, `ACC_PUBLIC|ACC_SUPER`, name `sourcedebugextension/Test`, superclass `java/lang/Object`. It carries `SourceFile` `dispatch.clj` and a `SourceDebugExtension` whose text is the Clojure SMAP (`SMAP`, `dispatch.java`, `Clojure`, `*S Clojure`, `*F`, `+ 1 dispatch.clj`, `clojure/pprint/dispatch.clj`, `*L`, `144#1,8:144`, `*E`).
  - [P3] It has a public `<init>()V` that calls `java/lang/Object.<init>` (`max_stack` 1, `max_locals` 1) and a public static `main([Ljava/lang/String;)V` that loads `java/lang/System.err` and calls `println` with `"Hello"` (`max_stack` 2, `max_locals` 1). Zel does not produce this ijar SourceDebugExtension fixture.

### `third_party/ijar/test/GenZipWithEntries.java`
- Bazel lines: 64
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no CLI that takes an entry count and an output path and writes a JAR containing a default `META-INF/MANIFEST.MF` plus `Test1.class` through `TestN.class`, each a Java 8 public class extending `java/lang/Object` with only a public no-arg constructor.

### `third_party/ijar/test/IjarTests.java`
- Bazel lines: 498
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] ijar keeps a private nested class that a public API exposes. Compiling `PrivateNestedClass.java` against `interface_ijar_testlib.jar` with `-Xlint:deprecation` must succeed. Zel has no class-file rewriter and no such compile check.
  - [P3] `@Target` and other annotations stay on the interface class. Compiling `UseRestrictedAnnotation.java` against that jar must fail. Zel does not preserve annotations or fail that compile.
  - [P3] Deprecation stays on the stripped class. Compiling `UseDeprecatedParts.java` must succeed and emit at least 16 mandatory warnings whose English text starts with `[deprecation]` or contains `has been deprecated`. Zel does not keep the `Deprecated` attribute or the `@Deprecated` annotation.
  - [P3] `EnclosingMethod` stays so an annotation processor on `package-info.java` can call `getElementsAnnotatedWith(Override.class)` and the compile still succeeds. Zel does not keep that attribute.
  - [P3] Every `.class` in the interface jar has no `ACC_PRIVATE` class, field, or method, no `Source` attribute, and no method `Code`. Zel does not strip bodies or private members.
  - [P3] A class with `EnclosingMethod` (local or anonymous) is omitted. `LocalAndAnonymous$1`, `$2`, and `$1LocalClass` are dropped. `LocalAndAnonymous`, `$NestedClass`, `$InnerClass`, and `$PrivateInnerClass` remain. The outer class’s `InnerClasses` keeps only those three named inners, each with outer name `LocalAndAnonymous`. Zel does not drop those entries or rewrite `InnerClasses`.
  - [P3] A path whose final component is `module-info.class` is copied unchanged and stored uncompressed, including the non-class payloads `hello` and `goodbye` next to `java/lang/String.class`. Zel does not copy `module-info` through.
  - [P3] `META-INF/*.kotlin_module` and `kotlin/*.kotlin_builtins` are copied unchanged (`hello`, `goodbye`) beside `String.class`. A `*.tasty` entry is copied the same way. Zel does not pass those suffixes through.
  - [P3] Any other non-`.class` entry is dropped, including `META-INF/TRANSITIVE/`. The transitive fixture’s interface jar contains only `java/lang/String.class`. Zel does not apply that filter.
  - [P3] With `--target_label` and `--injecting_rule_kind`, the stripper writes `META-INF/` then `META-INF/MANIFEST.MF` first: `Manifest-Version: 1.0`, `Created-By: bazel`, `Target-Label: //foo:foo`, and `Injecting-Rule-Kind: foo_library`. An empty input still gets `Target-Label: //empty`. The manifest entry’s modified time is 2010-01-01 00:00:00 in the JVM default zone. Zel has no ijar manifest writer.
  - [P3] `--nostrip_jar` copies every non-manifest payload byte for byte, still stored uncompressed, and still puts `META-INF/` and `META-INF/MANIFEST.MF` first. An existing `Created-By: test-code` is kept. A missing manifest is created with `Created-By: bazel`. A new `Target-Label` is appended. An existing `Target-Label` or `Injecting-Rule-Kind` line is replaced. Named sections stay (`foo` / `Foo=bar`, `baz` / `Another=bar`). A second `--nostrip_jar` pass is byte-identical, including zip metadata. Zel does not implement `--nostrip_jar`.

### `third_party/ijar/test/LocalAndAnonymous.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Compiled with `-source 8 -target 8`, this file yields seven classes. ijar drops the three that carry `EnclosingMethod`: anonymous `LocalAndAnonymous$1`, local `LocalAndAnonymous$1LocalClass`, and anonymous `LocalAndAnonymous$2`. It keeps `LocalAndAnonymous`, static `NestedClass`, member `InnerClass`, and private `PrivateInnerClass`. Zel has no class-file stripper, so it neither drops those entries nor keeps that set.
  - [P3] On the outer class, `InnerClasses` starts with all six inners. The local and anonymous entries have outer name null (`outer_class_info_index` 0). After ijar only the three member types remain, each with outer `LocalAndAnonymous`. Anonymous entries are skipped because `inner_name_index` is 0. The named local class is dropped because its outer index is 0 and stripped method bodies no longer reference it. Zel does not rewrite `InnerClasses`.
  - [P3] `PrivateInnerClass` has a private constructor and is constructed from `f()`. ijar still emits that class file, and it drops the private constructor and the package-private synthetic constructor used to instantiate it. Zel does not.

### `third_party/ijar/test/Object.java`
- Bazel lines: 18
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Compiled with `-source 8 -target 8`, this package-private `java.lang.Object` is class-file version 52 with only `ACC_SUPER`, superclass index 0, no interfaces or fields, and one package-private `<init>()V` whose body is a bare `return`. Bazel ijar treats superclass 0 as absent, writes 0 back, keeps that constructor, and drops `Code`, `LineNumberTable`, and `SourceFile`. The regression only requires ijar to exit 0.
  - [P3] Zel has no class-file rewriter. Its vendored ijar tree is the zip reader and `zipper`; `classfile.cc` and `ijar.cc` are named by the BUILD and are not in the tree. Header compilation only builds a turbine command line and does not strip this class.
  - [P3] Nothing in Zel compiles a package-private `java.lang.Object` or checks that an interface jar can be produced from it.

### `third_party/ijar/test/PrivateMembersClass.java`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] ijar drops every `ACC_PRIVATE` member of this package-private `final` class: field `privateField`, the private constructor, `privateStaticMethod`, and `privateMethod`. It keeps `public static void main()V` and removes its `Code` attribute, along with `SourceFile`, line numbers, and local variables. Zel has no class-file rewriter that does this.
  - [P3] The four private member types stay in the interface jar. They are not local or anonymous (no `EnclosingMethod`), so they are not dropped. `InnerClasses` keeps each named nested type whose outer class is this one — `PrivateInnerClass`, `PrivateStaticInnerClass`, `PrivateInnerInterface`, and `PrivateInnerAnnotation` — and writes the original inner access flags, including `ACC_PRIVATE` (`ACC_STATIC` on the last three). That flag is only in `InnerClasses`; the class-file header does not have it. `classfile.cc` still notes that private inner classes are not omitted. Package-private `print()` remains with no body, so the strings `"private inner"` and `"private static inner"` leave the constant pool. The private synthetic enclosing-instance field on the non-static inner class is dropped. Zel does not retain these types this way.
  - [P3] `StripVerifyingVisitor` accepts the stripped jar: no private fields or methods, no `Code`, no `Source`, and it does not check `InnerClasses` for private nested types. Zel has neither this fixture nor that check. The ijar sources vendored in Zel are only the zip writer.

### `third_party/ijar/test/PrivateNestedClass.java`
- Bazel lines: 20
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Compiling this package-private client against the ijar of `DumbClass` must succeed. Its static initializer only reads `new DumbClass<String>().inner`. Zel has no class-file rewriter and no equivalent compile check.
  - [P3] ijar keeps the private member class `DumbClass$Inner` (only local and anonymous classes with `EnclosingMethod` are omitted) and keeps the public field whose type is that class, including the generic signature of `DumbClass<V>`. Zel does not.
  - [P3] ijar still drops the private field `Inner.v` and the package-private synthetic constructor used to instantiate that nested class. This client never uses either. Zel does not implement that stripping.

### `third_party/ijar/test/StripVerifyingVisitor.java`
- Bazel lines: 224
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The ASM7 visitor records `Private class found %s` when `ACC_PRIVATE` is set on the class, `Private field %s found in %s` for a private field, and `Private method %s found in %s` for a private method. Other access flags are allowed. Zel has no class-file walk that checks this.
  - [P3] `visitSource` always records `Source entry found %s %s`, including a null source or debug string. Zel does not reject a `SourceFile` or `SourceDebugExtension` entry.
  - [P3] `visitCode` records `Code found for method %s of %s`. A private method with a body produces both the private-method error and the code error. Frames, instructions, locals, line numbers, and maxs are ignored after that. Zel has no check that interface-jar methods have no code.
  - [P3] Outer class, class/field/method/parameter annotations, annotation defaults, custom attributes, and inner classes are accepted and not inspected (annotation visitors return null). Leftover and private inner classes are explicitly not verified. Zel has no equivalent allowance list.

### `third_party/ijar/test/TypeAnnotationTest2.java`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel compiles this source against the ijar of `typeannotations2/Util`. `javac` must accept `Util.castNullable(null)` from package-private `void m(Object o)`. That requires the interface method `<T extends @Nullable Object> @NonNull T castNullable(T)` to keep its `Signature` and the `RuntimeVisibleTypeAnnotations` constant-pool entries for `@Nullable` on the type-parameter bound and `@NonNull` on the return type.
  - [P3] Zel has no class-file rewriter and does not compile this client. The vendored ijar sources stop at zip I/O and never read or write `RuntimeVisibleTypeAnnotations` or `RuntimeInvisibleTypeAnnotations`.

### `third_party/ijar/test/UseDeprecatedParts.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Compiling this client with `-Xlint:deprecation` against the ijar of `DeprecatedParts` must succeed and emit at least 16 mandatory warnings (`[deprecation]` or `has been deprecated`). Zel has no interface-jar rewriter and no compile that checks those warnings.
  - [P3] The 16 uses are both `@Deprecated` and Javadoc `@deprecated`: annotation types on fields, deprecated constructors, static and instance fields, instance and static methods, nested superclasses, and superinterfaces. Zel has no client that exercises that set.

### `third_party/ijar/test/UseRestrictedAnnotation.java`
- Bazel lines: 22
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel compiles this source on the classpath of `interface_ijar_testlib.jar` and requires `CompilationTask.call()` to return false. Method `m` carries `@Annotations.ParametersOnlyAnnotation`, whose `@Target` is only `ElementType.PARAMETER`, so javac must reject it.
  - [P3] That rejection holds only if ijar kept the CLASS-retention `@Target` (`RuntimeInvisibleAnnotations`) on `ParametersOnlyAnnotation`. Dropping `@Target` would make the annotation legal on a method and the test would fail. Zel has no class-file rewriter that preserves or strips that meta-annotation (vendored ijar is zip/platform only; `classfile.cc` is absent) and no compile that asserts this misuse fails.

### `third_party/ijar/test/WellCompressed1.java`
- Bazel lines: 505
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Package-private `WellCompressed1` has a private constructor and one `public static final String DATA`: 32266 spaces from 482 concatenated literals. Zel has no type with that field.
  - [P3] That constant makes the class file at least one 4KiB page larger uncompressed than compressed, so `test_output_bigger_than_input` can require ijar not to crash when the interface jar is bigger than the input jar. Zel has no such class or check.

### `third_party/ijar/test/WellCompressed2.java`
- Bazel lines: 316
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `test_output_bigger_than_input` compiles this public default-package class with `javac -g` together with `WellCompressed1`, packs the classes into a jar, and requires ijar to finish with an interface jar strictly larger than that input.
  - [P3] The class has an implicit public constructor and 100 `public static final String` fields, `DATA_001` through `DATA_100`. Each value is the same 172 spaces, from literals of length 36, 68, and 68, which `javac` folds into one constant-pool string so the class deflates far below its uncompressed size.
  - [P3] Zel has no fixture or test that an interface jar may grow past a highly compressed input. Its vendored ijar sources omit the class-file rewriter this input is meant to exercise.

### `third_party/ijar/test/ZipCount.java`
- Bazel lines: 36
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no program that takes a zip path and an expected count, opens the file with `java.util.zip.ZipFile`, and compares `size()` to `Integer.parseInt` of the second argument.
  - [P3] `size()` is the JDK central-directory entry count, including directories and a ZIP64 total above 65535. A match exits 0 with no output. A mismatch throws `AssertionError` with the message `expected %d entries, saw %d`.
  - [P3] A missing argument, a non-`int` expectation, or a zip the JDK cannot open fails with `ArrayIndexOutOfBoundsException`, `NumberFormatException`, or `IOException`/`ZipException`. Zel has none of these CLI outcomes.

### `third_party/ijar/test/ijar_test.sh`
- Bazel lines: 637
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The suite runs the `ijar` binary (JDK `javac`/`java`/`jar`/`javap`, plus `zip`, `unzip`, and an entry counter). With one path, the output name is the input with `-interface.jar` substituted for `.jar`. Zel has no `ijar` or `classfile` tool and no such runner. Vendored C++ `zip.cc` is only the zip layer.
  - [P3] On `A`: the interface jar has 5 class entries, including private nested classes. Private members, `<clinit>`, method bodies, and the code constant `foofoofoofoo` are gone. `B.java` still compiles, the `@Deprecated` warning remains, and `ConstantValue` `123` is still inlined as `ldc2_w`. A second `ijar` pass is byte-identical. The jar contains only `.class` files, every entry is stamped `Fri Jan 01 00:00:00 UTC 2010`, and a `jar`-built input and a `zip`-built input produce the same interface jar.
  - [P3] A well-compressed input may yield a larger interface jar. A class with 16384 methods (body over 64 KiB) is accepted, and running `ijar` on that output succeeds. Stored (`jar c0`) and default compression of the same classes match, including class files padded to 1 MiB and 15 MiB and an added ~2.2 GiB `zeroes.data`. Two hundred classes padded to 15 MiB each (output size bound above 2 GiB) must succeed.
  - [P3] A jar with no classes still produces a non-empty interface jar that `javac` accepts on the classpath. A central directory whose file count is wrong still yields 2 entries. Adding one byte, or shortening the file by two, fails with a non-zero status and the log text `missing end of central directory record`. A separate corrupt zip must abort with status 134.
  - [P3] Preserved attributes: `RuntimeVisibleTypeAnnotations` (a client still compiles), `MethodParameters`, `NestMembers`, `NestHost`, `Record`, and `PermittedSubclasses`. Lambda and dynamic-constant classes lose every method body (the lambda class goes from 4 bodies to 0). `java.lang.Object` compiled with `-source 8 -target 8` is accepted. `SourceDebugExtension` is dropped and does not log `skipping unknown attribute`. A repeated unknown attribute logs `skipping unknown attribute: "some_unknown_type"` once. `--strip_jar` still keeps the two Kotlin `Intrinsics.checkParameterIsNotNull` calls.
  - [P3] `test_inner_class_argument` never calls `ijar`; it only compiles packages `a`, `b`, and `c` and jars `a` and `b`. `test_inner_class_pruning` compiles three different `a.A` classes, then runs `ijar` three times on `one/one.jar` only and requires those three outputs to compare equal.
  - [P3] The leading TODO is not implemented: a non-interface source edit is never checked for an unchanged interface jar. The harness also resolves the JDK through Bazel `_main` versus Blaze runfiles and deletes `TEST_TMPDIR` on exit.

### `third_party/ijar/test/invokedynamic/ClassWithLambda.java`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Compiling this class yields exactly four `Code` attributes: the default constructor, `hash`, `m`, and the private static synthetic `lambda$m$0` for `x -> x.apply(x)`. `String::hashCode` is a second `invokedynamic` (`REF_invokeVirtual` `String.hashCode`) and adds no fifth body. The constant `HELLO` is inlined as `ldc "Hello World"`, so there is no `<clinit>`. ijar must accept the class and leave `javap -c -private` with zero `Code` attributes. An unknown constant-pool tag copies the class through unchanged and would leave all four bodies. Zel has no class-file rewriter and no such check.
  - [P3] ijar drops `Code` and `BootstrapMethods`, drops the private field `HELLO` and the private lambda method, and keeps the package-private `hash` and `m` methods with empty bodies. Zel does not strip those attributes or members.
  - [P3] Nested interface `Rec extends Function<Rec, Rec>` is recorded in `InnerClasses` and has no methods. ijar keeps that attribute. Zel has no equivalent fixture.

### `third_party/ijar/test/methodparameters/Test.java`
- Bazel lines: 20
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Compiled with `javac -parameters`, `methodparameters.Test` has two public static methods, `foo(String fooParam)` and `bar(final String barParam)`. ijar keeps the JVMS §4.7.24 `MethodParameters` attribute, including each parameter’s UTF-8 name and access flags (`ACC_FINAL` only on `barParam`). Zel has no class-file rewriter, so those names and flags are not kept on an interface jar.
  - [P3] `test_method_parameters_attribute` runs ijar and requires `javap -v methodparameters.Test` to still print `MethodParameters`. Zel has no equivalent fixture or check.

### `third_party/ijar/test/nestmates/NestTest.java`
- Bazel lines: 24
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No class declares a public outer type with a public static nested class whose empty instance method is invoked from `main`, which is what makes javac emit `NestMembers` on the outer class and `NestHost` on the nested class.
  - [P3] Nothing checks that an interface jar still exposes `NestMembers` on the outer class and `NestHost` on the nested class.

### `third_party/ijar/test/package-info.java`
- Bazel lines: 14
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] This is an unnamed-package `package-info` compilation unit whose only content is the doc comment `Empty package-info`. It has no `package` declaration, no annotations, and no types, so javac emits `package-info.class` with no runtime-visible annotations. Zel has no such compilation unit.
  - [P3] `IjarTests` compiles this source with `-Xlint:deprecation` and classpath `interface_ijar_testlib.jar`. A processor that supports `*` and the latest source version calls `roundEnv.getElementsAnnotatedWith(Override.class)` and returns false, and that compile must succeed. The scan completes every unnamed-package element in the interface jar and fails if ijar dropped `EnclosingMethod`. Zel has no such processor round and no class-file rewriter.

### `third_party/ijar/test/records/RecordTest.java`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no fixture for a public Java 16 record `RecordTest` with components `foo` (`List<String>`) and `bar` (`Map<String, Integer>`), which is what places a `Record` attribute on the class.
  - [P3] Zel has no interface-jar stripper that copies that attribute (component name, descriptor, and nested attributes). The Bazel test only checks that `javap -v` still prints `Record` after ijar.

### `third_party/ijar/test/sealed/SealedTest.java`
- Bazel lines: 22
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] This fixture is a package-private `sealed class SealedTest` that permits only the static final nested types `SealedTest.Foo` and `SealedTest.Bar`, both of which extend `SealedTest` and declare no members. A Java 17 compile puts a `PermittedSubclasses` attribute on `SealedTest` naming `SealedTest$Foo` and `SealedTest$Bar`.
  - [P3] Bazel ijar reads and rewrites that attribute, and `test_sealed_attribute` requires `javap -v SealedTest` on the interface jar of `sealed.jar` to still show `PermittedSubclasses`. Zel has no class-file rewriter, no `PermittedSubclasses` handling, and no sealed fixture or check.

### `third_party/ijar/test/testenv.sh`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel bootstraps runfiles v3 (five fallbacks for `runfiles.bash`, exit 1 if none resolve), then `rlocation`s and sources `io_bazel/src/test/shell/unittest.bash`, exiting 1 if that source fails. Zel has no ijar shell-test harness that loads that framework.
  - [P3] After lowercasing `uname -s`, Linux defines `statfmt` as `stat -c "%s"` and `MD5SUM=md5sum`. Every other OS uses `stat -f "%z"` and `MD5SUM=/sbin/md5`. Those are the size and digest commands `ijar_test.sh` and `zip_test.sh` use. Zel does not define them.

### `third_party/ijar/test/typeannotations2/NonNull.java`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Public marker annotation with `RetentionPolicy.RUNTIME` and `@Target({TYPE_USE, TYPE_PARAMETER})`. Runtime retention puts uses in `RuntimeVisibleTypeAnnotations`. Zel has no annotation with those targets or that retention, and no class-file rewriter.
  - [P3] `Util.castNullable` annotates the generic return type `@NonNull T`. ijar keeps that method type annotation and the constant-pool UTF-8 descriptor `LNonNull;` (kept when the attribute is written via `slot()`), so `javap -v Util` still shows `RuntimeVisibleTypeAnnotations` and `javac` of a caller against the interface jar succeeds. Zel does not strip class files or retain those constant-pool refs.
  - [P3] The cast `(@NonNull T)` is a type annotation inside `Code` and is dropped with the method body. Zel has no equivalent strip.

### `third_party/ijar/test/typeannotations2/Nullable.java`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] This file declares a public marker annotation `Nullable` with no elements. It is retained with `RetentionPolicy.RUNTIME` and targeted at `ElementType.TYPE_USE` and `ElementType.TYPE_PARAMETER`, so uses such as a type-parameter bound become `RuntimeVisibleTypeAnnotations` entries whose constant-pool type is `LNullable;`.
  - [P3] Bazel’s ijar test compiles this annotation with the other `typeannotations2` sources and requires the interface jar to keep those runtime-visible type annotations so a client still compiles. Zel has no class-file rewriter, no copy of that annotation, and no equivalent check.

### `third_party/ijar/test/typeannotations2/Util.java`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] After ijar, public static `castNullable` still has signature `<T:Ljava/lang/Object;>(TT;)TT;` and `RuntimeVisibleTypeAnnotations`: method type-parameter bound `0x12` (parameter 0, bound 0, empty path) for `@Nullable` on `T extends Object` (`LNullable;`), and method return `0x14` (empty path) for `@NonNull` (`LNonNull;`). `javap -v` must print `RuntimeVisibleTypeAnnotations`, and `javac` of `TypeAnnotationTest2` (`Util.castNullable(null)`) must succeed against that interface jar. Zel has no class-file rewriter that keeps those attributes or runs that check.
  - [P3] The body `return (@NonNull T) value` is an unchecked identity, so null is returned unchanged. That cast is a `Code` type annotation (target `0x47`). ijar drops `Code` and therefore the cast annotation; its type-annotation reader aborts on target `0x47` if it is parsed. Zel has no such method and no code-stripping path for this fixture.

### `third_party/ijar/zip.cc`
- Bazel lines: 1163
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`
- Verdict: partial
- Differences:
  - [P2] Zel has no zip writer. ijar writes entries with extract version 10, general-purpose flags 0, DOS time `30<<25 | 1<<21 | 1<<16` (2010-01-01 00:00:00), external attributes preserved, and extra length 0. It deflates only when the result is smaller than the stored payload, back-patches CRC and sizes, and emits a ZIP64 end record plus locator only when the entry count exceeds 65535 or the central-directory size or offset exceeds `0xFFFFFFFF`. Output is capped at 4GB. `Finish` is idempotent.
  - [P2] ijar rejects multi-disk archives when either classic disk number is non-zero, the per-disk entry count differs from the total, or the ZIP64 locator’s disk is non-zero or its total disk count is greater than 1. Zel never reads those fields and still extracts the archive.
  - [P2] ijar finds a ZIP64 end record at `eocd - 76` before using the locator, so a prepended preamble still resolves, and it keeps a ZIP64 field only when it matches the classic value or that value is the 16/32-bit sentinel. Zel consults only the absolute locator offset, and only when the classic entry count is `0xFFFF` or the classic directory offset is `0xFFFFFFFF`.
  - [P2] ijar places local headers at `physical_central_directory - recorded_offset + local_header_offset`. Zel uses the recorded local-header offset as a file offset. For a non-ZIP64 archive it reads the central directory at `eocd - cd_size` only when the low 32 bits of that sum equal the recorded offset.
  - [P2] ijar ignores the end-of-central-directory entry count and stops the directory walk on a central-directory signature, a digital signature, a ZIP64 end record, or a classic end record. Zel stops after the recorded count, or after `cd_size` bytes on the adjusted-start path, and any other signature is `Malformed Central Directory File Header; does not start with 02014b50`.
  - [P2] ijar copies entry names as raw bytes, truncated to `PATH_MAX - 1` and cut at the first NUL. Zel decodes every name with lossy UTF-8 and does not read the UTF-8 flag.
  - [P2] ijar rejects a local general-purpose bit other than bits 1, 2, 3, and 11, and a compression method other than store or deflate. Zel ignores those flags, inflates method 9 as deflate64, and for every other method copies `uncompressed_size` bytes.
  - [P2] ijar requires a non-zero, non-`0xFFFFFFFF` local size to match the central directory, and a stored entry to have equal compressed and uncompressed sizes. It then copies or inflates `compressed_size` bytes and does not check the CRC. Zel ignores the local sizes and CRC, and inflates until `uncompressed_size` bytes, truncating output that would exceed that limit.
  - [P2] ijar walks extra subfields with no remaining-length check, so a short ZIP64 payload consumes the following bytes. Zel stops when fewer than 4 bytes remain and returns `Truncated zip file` when a ZIP64 integer extends past the extra slice.
  - [P3] ijar passes each accepted entry’s name, external attributes, and bytes to a processor callback. Zel materializes files, directories, and symlinks, forces directory mode `0o755`, defaults other modes to `0o644`, and rejects a symlink target longer than 4096 bytes.

### `third_party/ijar/zip.h`
- Bazel lines: 169
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `zipattr_is_dir` is true only when DOS external-attribute bit `0x10` is set. It ignores the Unix mode and a trailing slash. Zel has no ijar attribute helper. `parse_zip` in `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` treats a name ending in `/` as a directory with mode `0755`. Otherwise a Unix-made entry (version-made-by high byte 3) with a nonzero mode is a directory only if the high 16 bits include `S_IFDIR` (`0o040000`), and a symlink if they include `S_IFLNK`. The DOS `0x10` bit is used only in the remaining case, which also forces mode `0644`.
  - [P3] `zipattr_to_perm` is always `(attr >> 16) & 0777`, including when `attr` is 0. Zel keeps the unmasked high 16 bits for those Unix entries, substitutes `0755` or `0644` in the other cases, and on extract applies `(mode & 0o777) | 0o400` to files. Directories are created with `create_dir_all` and are not chmod'd from the zip attribute.
  - [P3] `ZipBuilder::NewFile` returns a builder-owned buffer. `FinishFile` defaults both deflate and CRC32 off. `WriteEmptyFile` is `NewFile(name, 0)` then `FinishFile(0)`. `Finish` runs at most once, the destructor calls it if it was skipped, and it writes the central directory, so `GetSize` is short until then. `Create` returns null and leaves `errno` set, and the output is capped at `estimated_size`. `EstimateSize` returns 0 on error. Zel does not build zips this way. Interface jars are produced by spawning the toolchain `single_jar`.
  - [P3] `ZipExtractor::ProcessNext` returns false at end of archive or on error, and `GetError` is null when there is no error. `Accept` can skip an entry. `CalculateOutputLength` is the size of an uncompressed zip of the accepted files, or 0 with a non-empty error. `Create` returns null and sets `errno`. Zel reads the whole file with Java `ZipReader` end-of-central-directory rules and extracts to a directory. It has no accept/skip processor, no rewind, and no uncompressed output-size estimate.

### `third_party/ijar/zip_main.cc`
- Bazel lines: 500
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no zipper CLI. The `v`/`x`/`c`/`f`/`C` mode string, the mutual exclusion of create and extract, `-d exdir`, `@filelist` newline lists, and `zip_path=file` (including an empty file after `=`) are all absent.
  - [P2] Create mode is absent: stat each input (mode `0666` and non-directory when the file side is empty), skip directories when flattening, use the basename or else the given path with a trailing `/` on directories, reject paths longer than `PATH_MAX`, print `d|f <mode> <path>` when verbose, and write entries with optional compression.
  - [P2] Extract and list mode is absent: accept every entry or an exact name set, treat external attribute `0` as mode `0777` and as a directory when the name ends in `/`, skip directories and use basenames when flattening, join the output root with `/`, abort when the normalized path leaves that root, and create directories or write file bytes with the zip permissions.
  - [P2] The C++ source is only embedded as `@bazel_tools` bytes. `archive.rs` extracts zip for module downloads and does not implement this program.

### `third_party/ijar/zlib_client.cc`
- Bazel lines: 162
- Zel counterpart: `/workspace/zel-cursor/crates/zel-util/src/deflate.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel `TryDeflate` runs zlib `deflateInit2` at `Z_DEFAULT_COMPRESSION`, raw window `-MAX_WBITS`, memLevel 8, and `Z_DEFAULT_STRATEGY`, and overwrites the input only when `deflate(..., Z_FINISH)` returns `Z_STREAM_END` inside a buffer of the original length. Any other result leaves the input bytes unchanged and returns the original length. Zel `deflate` always returns a new fixed-Huffman LZ77 stream (64-entry hash chain, 32KiB window), and that stream can be longer than the input.
  - [P2] Bazel `Decompressor` inflates with `inflateInit2` window `-MAX_WBITS` and `Z_SYNC_FLUSH`, starting from a 256KiB buffer that doubles up to `INT32_MAX` (2147483647). Past that it returns null and `GetError()` is `ijar does not support decompressing files larger than 2047MB.` Zel `inflate` has no output cap.
  - [P2] Inflate init or stream errors return null, and `GetError()` is `inflateInit: %d` or `zlib returned error code %d during inflate.` (`Z_DATA_ERROR`, `Z_BUF_ERROR`, `Z_STREAM_ERROR`, `Z_NEED_DICT`, and any other code). The message stays until overwritten. Zel returns `Err` strings (`unexpected end of deflate stream`, `truncated stored block`, `invalid Huffman code`, `distance too far back`) and has no separate error query. Zel also accepts a stored block whose length complement does not match, which zlib rejects.
  - [P3] A successful `UncompressFile` returns the compressed byte count as `u4` (`next_in - buffer`, so trailing input is not consumed) and a pointer to the decompressor’s single buffer, which the next call overwrites. Zel returns an owned `Vec<u8>` and a `usize` count.

### `third_party/ijar/zlib_client.h`
- Bazel lines: 71
- Zel counterpart: `/workspace/zel-cursor/crates/zel-util/src/deflate.rs`
- Verdict: partial
- Differences:
  - [P2] `TryDeflate` raw-deflates in place and keeps the original bytes and length when the compressed result is larger than the input or when `deflateInit2` fails. `deflate` always returns a new buffer and can be larger than the input.
  - [P2] Bazel uses zlib `Z_DEFAULT_COMPRESSION` (level 6, raw window, memLevel 8, default strategy). Zel writes one fixed-Huffman block with a 64-step hash chain, so the compressed bytes differ.
  - [P2] `Decompressor` reuses one buffer from 256 KiB, doubling to `INT32_MAX`, then fails instead of decompressing a larger file. `inflate` grows an unbounded `Vec` and has no that limit.
  - [P3] Inflate failure is a null `DecompressedFile` plus a sticky message in a `4 * PATH_MAX` buffer (`GetError`). Zel returns `Err(String)` and does not keep an error across calls.
  - [P2] A stored block with a bad length complement is a zlib data error. Zel skips those two bytes and does not check them.

### `third_party/jarjar/java/com/tonicsystems/jarjar/AbstractDepHandler.java`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no dependency-handler base that turns a `PathClass` pair into strings at `JAR` (classpath) or `CLASS` (class name) level, drops pairs already seen, and forwards each unique pair once.
  - [P3] Zel has no empty `handleStart`/`handleEnd` callbacks on that handler, and no failure path when the level is neither `JAR` nor `CLASS`.

### `third_party/jarjar/java/com/tonicsystems/jarjar/DepFind.java`
- Bazel lines: 84
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `DepFind` walks two classpaths relative to `user.dir` (overridable via `setCurrentDirectory`). The `to` path is indexed with `ClassHeaderReader` into a map of internal class name to `entry.getSource()` (duplicate names keep the last source). Zel has no classpath class-name index.
  - [P3] The `from` path is then fully parsed with ASM `ClassReader.accept(..., SKIP_DEBUG)` and a `DepFindVisitor` that reports against that index. `DepHandler.handleStart()` runs after the index is built and `handleEnd()` runs only after the from-scan finishes. Zel has no jarjar-style dependency report between two classpaths.
  - [P3] A bad entry is skipped after `Error reading <name>: <message>` on stderr, and both the entry stream and the `ClassPathIterator` are always closed. A `RuntimeIOException` from either iterator is rethrown as its cause `IOException`, which skips `handleEnd` and also skips `handleStart` if it occurs while indexing `to`. Zel does not implement that scan or its error policy.
  - [P3] Both iterators are opened with a null exclude filter. Zel does not provide this finder. `rules_kotlin` only shells out to an external `jarjar` binary to apply shade rules.

### `third_party/jarjar/java/com/tonicsystems/jarjar/DepFindVisitor.java`
- Bazel lines: 75
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file visitor that, on `visit`, records the current internal class name with its archive source and, for every referenced type whose name is in the known-class map under a different source, reports that cross-archive edge to a dependency handler.
  - [P3] Zel does not leave the type unmapped (`map` returns null, so the walk does not rewrite names) or wrap a handler `IOException` in a runtime I/O exception.

### `third_party/jarjar/java/com/tonicsystems/jarjar/DepHandler.java`
- Bazel lines: 34
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no dependency-handler callback that receives a `PathClass` edge (`from`, `to`) during a classpath scan.
  - [P3] There is no `CLASS` versus `JAR` reporting level for those edges.
  - [P3] There are no scan start and end hooks that can fail with an I/O error around that callback.

### `third_party/jarjar/java/com/tonicsystems/jarjar/EmptyClassVisitor.java`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ASM `ClassVisitor` fixed at API level `ASM9` with a null delegate.
  - [P3] Nothing returns a no-op `MethodVisitor`, `AnnotationVisitor`, or `FieldVisitor` from `visitMethod`, `visitAnnotation`, and `visitField`, so there is no sink that forces those members to be walked and then discarded.
  - [P3] Bazel still uses the inherited null returns for type annotations, record components, and modules (those visits are skipped). That split is absent because the visitor itself is absent.

### `third_party/jarjar/java/com/tonicsystems/jarjar/ExcludeProcessor.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No jar processor drops a zip entry when `struct.name` is an exact member of a caller-supplied exclude set and otherwise keeps the entry unchanged (no rename or rewrite of name, data, or time).
  - [P3] No verbose path writes `Excluding <entry name>` to stderr when an entry is dropped.
  - [P3] The second-pass jar strip that deletes those exact paths (keep-excluded `.class` files, including already-renamed names) is not implemented.

### `third_party/jarjar/java/com/tonicsystems/jarjar/JarJarMojo.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Maven Mojo entry point that shades `fromJar` into `toJar` from either an inline `rules` string or a `rulesFile`.
  - [P3] The check that exactly one of `rules` or an existing `rulesFile` is set (otherwise "Exactly one of rules or rulesFile is required") is absent.
  - [P3] Parsing those rules, running the jar processor with verbose from the Mojo and skip-manifest forced on, then stripping the output jar and wrapping `IOException` as `MojoExecutionException`, is absent.

### `third_party/jarjar/java/com/tonicsystems/jarjar/JarJarTask.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Ant task collects `<rule>`, `<zap>`, and `<keep>` elements in order, rejecting a rule that lacks `pattern` or `result` and a zap or keep that lacks `pattern`.
  - [P3] No execution path builds the processor with `skipManifest` forced false, runs it over the jar, then strips the destination archive and turns an I/O failure into a build failure.
  - [P3] Cleanup that clears the accumulated pattern list after the base helper reset has no counterpart.

### `third_party/jarjar/java/com/tonicsystems/jarjar/Keep.java`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel’s `Keep` is an empty `PatternElement` subclass: a jarjar rule kind that stores only a pattern string and no rename result, so keep rules stay distinct from zap and rewrite rules. Zel has no jarjar rule model and no such type.

### `third_party/jarjar/java/com/tonicsystems/jarjar/KeepProcessor.java`
- Bazel lines: 129
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] For each non-multi-release `.class` entry, the internal name (path without `.class`) is a root when it matches a keep wildcard (`.` rewritten to `/`; patterns that contain `/` are rejected). ASM with `EXPAND_FRAMES` records dependencies, then the class is removed from its own set. `getExcludes()` is every class that was seen and is outside the transitive closure of those roots. Zel does not build this graph or this removable set.
  - [P3] Edges whose binary name starts with `java/` or `javax/` are dropped. String constants that are dotted for-name values (non-empty, only `.` or a Java identifier part) and array-for-name values matching `[L…;` are extra edges. `map` always returns null, so the class bytes are not rewritten. Zel does not.
  - [P3] `process` always returns true, so this step never drops the entry. A class-parse failure is printed to stderr as `Error reading <name>: <message>` and then ignored. `isEnabled()` is true only when the keep wildcard list is non-empty. Zel does not.

### `third_party/jarjar/java/com/tonicsystems/jarjar/Main.java`
- Bazel lines: 106
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No JarJar CLI that dispatches `strings`, `find`, and `process`, or prints `help.txt` to stderr (UTF-8, platform newlines) and returns on an empty or unknown command.
  - [P3] No `strings` mode that requires a classpath and dumps its strings to a UTF-8 buffered stdout.
  - [P3] No `find` mode that requires a dependency level plus two classpaths (third argument), parses the level with `Locale.ROOT` uppercasing, and writes dependency text to UTF-8 stdout.
  - [P3] No `process` mode that requires a rules file, input jar, and output jar, parses rules, remaps the jar with `verbose` and `skipManifest` taken from boolean system properties, then strips the output jar.

### `third_party/jarjar/java/com/tonicsystems/jarjar/MainProcessor.java`
- Bazel lines: 123
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no jarjar entry processor that splits patterns into zap, rule, and keep lists and builds a `PackageRemapper` from the rules.
  - [P3] No processor chain in the Bazel order: manifest (with `skipManifest`), optional keep, zap, class remapping, resource remapping, then service remapping.
  - [P3] No per-entry rename map, verbose `Renamed` / `Removed` stderr logging, or `strip` pass that deletes keep-excludes under their renamed `.class` paths.

### `third_party/jarjar/java/com/tonicsystems/jarjar/ManifestProcessor.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Jar-entry processor: a name equal to `META-INF/MANIFEST.MF`, ignoring case, is the manifest. Every other entry is kept and its bytes are left unchanged (`return true`).
  - [P3] When `skipManifest` is true, that entry is dropped (`return false`) and is not parsed.
  - [P3] Otherwise the bytes are parsed with `java.util.jar.Manifest`. Only the main-attribute `Main-Class` is rewritten through `PackageRemapper.mapValue`. `struct.data` is replaced with `Manifest.write` and the entry is kept. This processor does not change `struct.name`.
  - [P3] `mapValue` on a dotted `Main-Class` with no `/` turns dots into slashes, applies the package rules, then turns slashes back into dots. A value that already contains both `.` and `/` is left unchanged when `mapPath` does not change it. A `[L…;]` array `Class.forName` form is remapped as a descriptor. Verbose mode prints `Changed "…" -> "…"` on stderr when the value changes.
  - [P3] `Manifest.write` re-serializes even when `Main-Class` is absent or unchanged: UTF-8, CRLF, 72-byte continuation lines, version attribute first, then the other main attributes, then per-entry `Name:` sections. Per-entry attributes are not remapped. Invalid manifest bytes throw `IOException`.
  - [P3] Zel has no jarjar processor and no code that rewrites a JAR `Main-Class` under package rules or drops `META-INF/MANIFEST.MF` for `skipManifest`.

### `third_party/jarjar/java/com/tonicsystems/jarjar/PackageRemapper.java`
- Bazel lines: 139
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No ASM `Remapper` that rewrites internal type names with the first matching jarjar wildcard, caching hits and storing identity mappings as null so ASM keeps the original key.
  - [P3] No `mapPath` that rewrites only a resource path’s directory by swapping the filename for the `RESOURCE` sentinel, keeping a leading `/`, restoring the original filename, and returning the original path uncached when a rule consumes the sentinel.
  - [P3] No `mapValue` that rewrites string constants: `[L…;` array `Class.forName` forms via descriptor mapping, dotted or slashed class names when the string is not mixed dot-and-slash, otherwise resource paths; non-strings are left to `Remapper.mapValue`, and verbose mode prints changes to stderr.

### `third_party/jarjar/java/com/tonicsystems/jarjar/PathClass.java`
- Bazel lines: 40
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No value type pairing a class-file path with a binary class name.
  - [P3] No accessors that return those two strings unchanged.
  - [P3] No `toString` of `classPath + "!" + className`.

### `third_party/jarjar/java/com/tonicsystems/jarjar/PatternElement.java`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no jarjar pattern-element type that stores a pattern string and builds wildcards from a list: only `Rule` elements supply a result (other elements use an empty result), patterns containing `/` are rejected, and `.` in the pattern is rewritten to `/` before the wildcard is constructed.

### `third_party/jarjar/java/com/tonicsystems/jarjar/ResourceProcessor.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] JarJar `ResourceProcessor` is a `JarProcessor` that, for every entry `EntryStruct.isClass()` rejects, replaces `struct.name` with `PackageRemapper.mapPath` and leaves class entries unchanged.
  - [P3] `isClass()` is true only when the name ends in `.class` and does not start with `META-INF/version`, so multi-release class files are path-remapped as resources.
  - [P3] The processor never changes entry bytes or timestamps and always returns true, so it never drops an entry.
  - [P3] Zel has no jarjar pipeline that remaps non-class JAR entry paths through package rules.

### `third_party/jarjar/java/com/tonicsystems/jarjar/Rule.java`
- Bazel lines: 29
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no jarjar rule type that subclasses a pattern element and stores a mutable replacement `result` string.
  - [P3] `setResult` and `getResult` are absent, so nothing holds the rewrite result that Ant-style parsing and wildcard construction read from a `Rule`.

### `third_party/jarjar/java/com/tonicsystems/jarjar/RulesFileParser.java`
- Bazel lines: 94
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no parser for jarjar rules text. Bazel reads a UTF-8 file or string, drops `#` comments, skips blank lines, and builds `rule` (pattern plus result), `zap`, or `keep` elements. Fewer than two tokens, a `rule` with no result, or an unknown verb throws `Error on line N: [...]`, and `N` counts only non-blank lines. Extra tokens are ignored. The only zel references invoke an external `jarjar_runner` from Starlark and do not parse these files.

### `third_party/jarjar/java/com/tonicsystems/jarjar/ServiceProcessor.java`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no jarjar processor. Nothing rewrites `META-INF/services/` entry names or their provider lines through a package remapper. `java_native.rs` only spawns the toolchain `single_jar`.
  - [P3] An entry whose name starts with `META-INF/services/` (including the directory entry itself) keeps that prefix and replaces the suffix with `PackageRemapper.mapValue(suffix)`. Other entry names are left unchanged.
  - [P3] Those bodies are decoded as UTF-8 (malformed bytes become U+FFFD), split with `String.lines()` (`\n`, `\r\n`, and `\r`; a final terminator does not add an extra empty line), and each full line, including blanks and `#` comments, is passed through `mapValue`.
  - [P3] The lines are joined with `\n` and a trailing `\n` is always written, then encoded as UTF-8. An empty body becomes a single `0x0A`. Content can change when no rule matches because of line-ending normalization, the forced trailing newline, and UTF-8 replacement.
  - [P3] `process` always returns true, so this step never drops an entry and does not merge two service files that remap to the same name.
  - [P3] Vendored `MergeJars.java` only concatenates `META-INF/services/` bodies on jar merge. It does not remap the service filename or the provider class names.

### `third_party/jarjar/java/com/tonicsystems/jarjar/StringDumper.java`
- Bazel lines: 113
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no jarjar `strings` dump and no class-file walker that prints constant strings.
  - [P3] `run` keeps one `DumpStringReader` for the whole classpath. Each entry is parsed with `ClassReader.accept` and parsing options `0` (code and line numbers included). The entry stream is closed, and the writer is flushed after every entry. The classpath iterator is closed in a `finally`. A `RuntimeIOException` from the iterator is rethrown as its `IOException` cause. An `IOException` from `openStream` or `close` propagates.
  - [P3] Any other failure while reading one class is caught and the walk continues. Stderr gets `Error reading ` plus the entry name, `: `, and `getMessage()` (which can be null).
  - [P3] Only non-empty strings are printed. A new internal class name emits one header with `/` replaced by `.`. The same name in a later class does not repeat that header, including across classpath entries. Each string is a tab, then `line: ` when `line >= 0`, then a quoted literal. The only escapes are `\n`, `\r`, `\b`, `\f`, `\t`, `\"`, and `\\`. Every other character is copied raw. Lines use the `PrintWriter` line separator.

### `third_party/jarjar/java/com/tonicsystems/jarjar/StringReader.java`
- Bazel lines: 120
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel’s ASM9 `ClassVisitor` records the internal class name and the latest `visitLineNumber` (line starts at and is reset to `-1` per class, and is not cleared at method entry). `visitString` is invoked for `String` field constants, annotation element values and enum constant names (including nested annotations), method `LDC` strings, and `invokedynamic` bootstrap arguments that are strings. Field, method, and parameter annotations share that visitor.
  - [P3] Zel has no class-file or jarjar string walker that reports those constants with class name and source line.

### `third_party/jarjar/java/com/tonicsystems/jarjar/TextDepHandler.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Jarjar `find` writes each dependency pair with `PrintWriter.println` as `from -> to` to the supplied writer. Zel has no jarjar dependency-text reporter that emits that line.

### `third_party/jarjar/java/com/tonicsystems/jarjar/Wildcard.java`
- Bazel lines: 162
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Package patterns are not compiled: `**` becomes a reluctant `(.+?)` capture (greedy when it is the final capture), `*` becomes `([^/]+)`, and the regex is anchored at both ends.
  - [P3] Pattern `**`, a pattern containing `***`, and any pattern character outside Java identifier parts plus `/*-` are not rejected.
  - [P3] Result templates are not parsed: `@` plus digits selects a capture, literal `.` is rewritten to `/`, `@` not followed by a digit is rejected, and a placeholder index above the capture count is rejected.
  - [P3] `matches` and `replace` do not accept a value only when the regex matches and every character is a Java identifier part or `/` or `-`, with a trailing `package-info` exempt from that character check.

### `third_party/jarjar/java/com/tonicsystems/jarjar/Zap.java`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no jarjar pattern-element type whose only state is the inherited pattern string and whose runtime type selects a zap (delete matching classes) rather than a rename rule or a keep.

### `third_party/jarjar/java/com/tonicsystems/jarjar/ZapProcessor.java`
- Bazel lines: 49
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A class entry (`name` ends with `.class` and does not start with `META-INF/version`) is omitted when its name minus the six-character `.class` suffix matches any zap wildcard. `process` returns false, which stops the jarjar chain and drops the entry. Zel never applies zap rules, so those classes stay in the jar.
  - [P3] Every non-class entry is kept, including resources whose path matches a zap pattern and multi-release classes under `META-INF/versions/`. Names and bytes are not rewritten. Zel has no keep-versus-drop split of that kind.
  - [P3] Zap patterns are compiled once: a `/` in the pattern is rejected, `.` is rewritten to `/`, and matching uses jarjar wildcards (`*` is one path segment, `**` is a reluctant `.+`). The first match wins. An empty zap list drops nothing. Zel does not compile or match those patterns.

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/AntJarProcessor.java`
- Bazel lines: 111
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] This Ant `Jar` task drops every input directory (`zipDir` is empty). Each file is read fully into one reused `EntryStruct` (`data`, `name`, `time`) and handed to `JarProcessor.process`. `false` omits the entry. `true` writes the struct’s name, bytes, and time, which the processor may have changed, and passes `fromArchive` through unchanged.
  - [P3] A unix mode of `0` is replaced with `ZipFileSet.DEFAULT_FILE_MODE` before that write. Any other mode is kept.
  - [P3] Unless `filesonly` was set on this task, each kept entry causes missing parents of the post-process name to be written once, ancestors first, as `dir + "/"` with `ZipFileSet.DEFAULT_DIR_MODE` and a single `JarMarker` extra field. A dropped entry adds no parents. An input directory that contains no kept file never appears. The seen-directory set lives until `reset` or `cleanUp`.
  - [P3] `setFilesonly` stores its own flag in addition to Ant’s. `reset` and `cleanUp` clear that flag, `verbose`, and the directory set. `verbose` is otherwise unused here. The processor reference is left in place for a later `execute`.
  - [P3] Zel has no Ant jar task and no jarjar writer that filters or rewrites entries and then synthesizes those JAR-marked parent directories.

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/ClassHeaderReader.java`
- Bazel lines: 195
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never parses a Java class-file stream for its header: magic `0xCAFEBABE` (a short first read is ignored, then a mismatch is a format error), constant-pool offsets for tags 1 and 3–12 (long/double consume an extra index; any other tag fails), then access flags, this class, superclass (`null` when the class index is 0), and interfaces.
  - [P3] The Bazel reader also stops at the interface list (versions unchecked), grows its buffer in 2048-byte chunks, fails with EOF when the requested header is short, and always closes the input stream. Nothing in zel does that.

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/ClassPathEntry.java`
- Bazel lines: 28
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no classpath-entry type that exposes a container source path (`getSource`, which may fail with I/O), an entry name (`getName`), and an openable byte stream of that entry (`openStream`).

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/ClassPathIterator.java`
- Bazel lines: 271
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no iterator that splits a delimited classpath, resolves relative entries against a parent directory, accepts only a single trailing `/*` (non-recursive `.jar` expansion), and yields `.class` entries from `.jar`, `.zip`, and directory trees, with zip handles held until `close`.

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/EntryStruct.java`
- Bazel lines: 40
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no mutable JAR-entry record holding the entry bytes, name, and timestamp.
  - [P3] Zel has no class-file test: the name must end with `.class`, and a name that starts with `META-INF/version` (including multi-release `META-INF/versions/…`) is not a class file.
  - [P3] Zel has no directory-entry test that is true exactly when the name ends with `/`.

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/GetNameClassWriter.java`
- Bazel lines: 49
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No ASM `ClassVisitor` (API `ASM9`) that wraps a `ClassWriter` built with the caller-supplied flags, records the internal class name from `visit`, and forwards version, access, name, signature, superclass, and interfaces to that writer.
  - [P3] No `getClassName` that returns the name stored by `visit` (unset until then) and no `toByteArray` that returns the wrapped writer's bytecode, which `JarTransformer` uses with `COMPUTE_MAXS` after the visitor chain runs.

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/IoUtil.java`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no helper that opens a file as a `ZipOutputStream` over an 8192-byte buffer and a truncating `FileOutputStream` (default deflate). Closing that stream finishes the ZIP central directory after flushing the buffer.
  - [P3] Zel has no helper that wraps an `OutputStream` in a non-auto-flushing `PrintWriter` over an 8192-byte buffer and a charset `OutputStreamWriter` (malformed input replaced; write failures latched on the writer instead of thrown).

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/JarProcessor.java`
- Bazel lines: 33
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no jar-entry processor hook that receives a mutable entry (name, timestamp, and bytes), may rewrite it in place, returns true to let the processor chain continue, returns false to stop the chain (callers then omit the entry), and fails with an I/O error.

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/JarProcessorChain.java`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no jar-entry processor chain that copies the processor list at construction and runs each processor in order on the same mutable entry, stopping and returning false when any processor returns false, and returning true only after every processor accepts the entry (I/O errors propagate).

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/JarTransformer.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no jar-entry processor that rewrites class entries through an ASM visitor (`COMPUTE_MAXS`, `EXPAND_FRAMES`), leaves the original bytes in place when `ClassReader` throws, and otherwise replaces the bytes and renames the entry to the visited class name (`.` to `/`, plus `.class`) only when the visitor is not a remapper or that remapper actually changed something. Non-class entries are unchanged, and the entry is always kept.

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/JarTransformerChain.java`
- Bazel lines: 36
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-visitor chain that shallow-clones a `RemappingClassTransformer` array, links each transformer to the next from the tail (`chain[i-1].setTarget(chain[i])` on the shared instances), and on `transform` points the last transformer at the downstream `ClassVisitor` and returns the first so visits run through the whole chain. An empty chain throws on `transform`; a one-element chain only retargets that transformer. Because the head is what is returned, only that transformer's `didRemap` is visible to `JarTransformer`.

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/RemappingClassTransformer.java`
- Bazel lines: 120
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ASM `ClassRemapper` that starts on an empty visitor, swaps that visitor in `setTarget` while clearing a remap flag, and sets the flag when a delegated `map`, descriptor, type, type array, method descriptor, value, signature, method name, invokedynamic name, or field name differs from its input, which `JarTransformer` uses to skip rewriting class bytes and the entry path when nothing changed.

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/RuntimeIOException.java`
- Bazel lines: 27
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no unchecked exception that wraps a single `IOException` as its cause (message from that exception's `toString()`, `serialVersionUID` 0) so classpath iteration and dependency visitors can leave `Iterator.next` and ASM `map` without a checked `throws`, and callers can unwrap the original `IOException`.

### `third_party/jarjar/java/com/tonicsystems/jarjar/util/StandaloneJarProcessor.java`
- Bazel lines: 99
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no JAR rewriter that reads every zip entry (name, timestamp, and bytes), drops entries whose processor returns false, stable-sorts the rest by transformed name, silently drops duplicate directory names and directories that are not a prefix of a later file, rejects duplicate file names with `Duplicate jar entries: `, and writes the survivors with the original timestamp and an unknown compressed size.

### `third_party/java/proguard/proguard6.2.2/bin/proguard.sh`
- Bazel lines: 24
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Resolves the script path with `readlink -f`, then `readlink`, then `$0`, and sets `PROGUARD_HOME` to the parent of that directory.
  - [P3] Runs `java -jar "$PROGUARD_HOME/lib/proguard.jar"` and forwards every argument unchanged. Zel has no launcher that finds `lib/proguard.jar` from a ProGuard home and invokes it that way.

### `third_party/java/proguard/proguard6.2.2/bin/proguardgui.sh`
- Bazel lines: 27
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No launcher resolves its own path with `readlink -f`, then `readlink`, then `$0`, and sets the install root to the parent of that directory.
  - [P3] Nothing starts `java -DsuppressSwingDropSupport=true -jar <root>/lib/proguardgui.jar` and forwards all arguments. Zel only references the ProGuard shrinker jar for Android builds.

### `third_party/java/proguard/proguard6.2.2/bin/retrace.sh`
- Bazel lines: 24
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No launcher resolves its own path with `readlink -f`, then `readlink`, then `$0`, sets the home directory to the parent of that path, and runs `java -jar "$PROGUARD_HOME/lib/retrace.jar"` with the original arguments.

### `third_party/java/proguard/proguard6.2.2/buildscripts/build.sh`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no driver that `cd`s to the script directory (unquoted `dirname`, failure ignored) and builds ProGuard 6.2.2 from the sibling module scripts.
  - [P3] Required modules `core`, `retrace`, `gui`, and `annotations` run in that order; the first failure aborts with exit status 1.
  - [P3] `gradle` and `ant` then run with their statuses discarded. After the required modules succeed, the script’s exit status is the status of `wtk`.

### `third_party/java/proguard/proguard6.2.2/buildscripts/functions.sh`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Sourced ProGuard build helpers (`set -o pipefail`, `SRC`/`OUT`/`LIB`, Java 1.8, jar paths for proguard, retrace, proguardgui, and annotations) are absent.
  - [P3] `download` skips an existing file; otherwise it creates the parent directory and fetches the URL with `wget -O` or `curl -L -o`.
  - [P3] `compile` runs `javac` (`-nowarn -Xlint:none`, `-source`/`-target` 1.8, optional classpath) on `_*.java` plus the named class file, then `groovyc` on the matching `.groovy` path, then copies `*.properties`, `*.png`, `*.gif`, and `*.pro` into `out` with `--parents`. Compiler output is indented and a failure returns 1. The Kotlin `kotlinc` path is commented out.
  - [P3] `createjar` and `updatejar` pass each top-level `out` entry to `jar` as `-C out <name>`. A `META-INF/MANIFEST.MF` selects `jar -cfm` / `jar -ufm`; otherwise `jar -cf` / `jar -uf`. Only `createjar` creates the destination parent directory.

### `third_party/java/proguard/proguard6.2.2/examples/android-plugin/src/com/example/HelloWorldActivity.java`
- Bazel lines: 29
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Android `Activity` whose `onCreate` builds a `TextView`, sets its text to `Hello World`, centers it, and installs that view with `setContentView`.

### `third_party/java/proguard/proguard6.2.2/examples/android/src/com/example/HelloWorldActivity.java`
- Bazel lines: 29
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no sample Android `Activity` whose `onCreate` calls through to the superclass, builds a `TextView`, sets its text to `Hello World`, centers it with `Gravity.CENTER`, and installs it with `setContentView`.

### `third_party/java/proguard/proguard6.2.2/examples/annotations/examples/Applet.java`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard annotation example that declares a public `Applet` subclass of `java.applet.Applet`, marks it `@Keep` so the class and its essential methods are preserved, and provides an empty `init()`.

### `third_party/java/proguard/proguard6.2.2/examples/annotations/examples/Application.java`
- Bazel lines: 21
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No `@KeepApplication` class `Application` whose `main` prints `The answer is 42` and relies on that annotation to keep the class and `main`.

### `third_party/java/proguard/proguard6.2.2/examples/annotations/examples/Bean.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No bean class kept by `@Keep` and `@KeepPublicGettersSetters`, which preserve the class and its public getters and setters.
  - [P3] No public `boolean`, `int`, and `String` properties with accessors that return or assign those fields: `isBooleanProperty`/`setBooleanProperty`, `getIntProperty`/`setIntProperty`, and `getStringProperty`/`setStringProperty`.

### `third_party/java/proguard/proguard6.2.2/examples/annotations/examples/NativeCallBack.java`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard annotation sample that marks a class `@KeepApplication` so the class and `main` stay, leaves native `computeAnswer()` to the usual native-method keep rule, and marks `getAnswer()` with `@Keep` so a method that returns 42 and is referenced only from native code is not removed.
  - [P3] Zel has no entry point that constructs the class, calls `computeAnswer()`, and prints `The answer is ` concatenated with that result.

### `third_party/java/proguard/proguard6.2.2/src/proguard/ArgumentWordReader.java`
- Bazel lines: 111
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard word-reader that returns each CLI argument as one line (`arguments[index++]`) and then end-of-input.
  - [P3] No location text `argument number N`, where `N` is the 1-based index after that argument is consumed.
  - [P3] No constructor that keeps the argument array and passes a base directory to the parent reader for later relative includes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/AssumeNoSideEffectsChecker.java`
- Bazel lines: 98
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never walks ProGuard class specifications to note when `-assumenosideeffects` matches every method of a class (method name and descriptor both null).
  - [P3] There is no note-printer acceptance gate, no fallback from `className` to `extendsClassName`, and no any-class keyword when the class name is absent.
  - [P3] Null class-specification and method-specification lists are not treated as no-ops, because that check does not exist.

### `third_party/java/proguard/proguard6.2.2/src/proguard/ClassMemberChecker.java`
- Bazel lines: 157
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel code walks ProGuard class specifications and, for a non-null class name with no `!`, `*`, `?`, `,`, or `///` that the note printer accepts, looks the class up in the program class pool.
  - [P3] No zel code, for each field or method spec whose name and descriptor are both concrete, notes an unknown member when `findField` or `findMethod` misses, using the external full member description and external class name. A null spec list, a missing class, or a wildcard name or descriptor produces no note.
  - [P3] No zel `MemberVisitor` hooks print the stdout suggestions `Maybe you meant the field '...'?` and `Maybe you meant the method '...'?`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/ClassPath.java`
- Bazel lines: 94
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No ordered mutable list of ProGuard classpath entries (apk, jar, aar, war, ear, zip, jmod, or directory) with an input-versus-output flag.
  - [P3] No `hasOutput` scan that returns true when any entry is an output entry.
  - [P3] No list operations: clear, insert at index, append, append another classpath, indexed get, indexed remove, emptiness, and size.

### `third_party/java/proguard/proguard6.2.2/src/proguard/ClassPathEntry.java`
- Bazel lines: 414
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard classpath entry: a file plus an input/output flag, a cached canonical path that falls back to the raw path on I/O failure, and cache invalidation when the file changes.
  - [P3] No case-insensitive extension checks for `.dex`, `.apk`/`.ap_`, `.jar`, `.aar`, `.war`, `.ear`, `.jmod`, or `.zip`.
  - [P3] No name filters for nested resources and those archive types (empty lists stored as unset), and no `toString` that appends them as `(` aar `;` apk `;` zip `;` jmod `;` ear `;` war `;` jar `;` file-filter `)`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/ClassSpecification.java`
- Bazel lines: 260
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel type holds a ProGuard class specification: optional comments, required set and unset access flags, annotation type, wildcard class name, extends annotation type, extends class name, and field and method member lists.
  - [P3] No constructors for an all-classes spec, a member-less spec, or a copy that shares the existing field and method lists.
  - [P3] No `addField` or `addMethod` that lazily allocates a list and appends a member specification.
  - [P3] No equality or hash that ignores comments, compares the flags and null-safe strings and member lists, and rejects a different runtime class.
  - [P3] No shallow `clone` of that specification. The unused `attributeNames` field, which stays null and is excluded from equality and hashing, is also absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/ClassSpecificationVisitorFactory.java`
- Bazel lines: 699
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no builder that turns `ClassSpecification` values into a class-pool visitor. `createClassPoolVisitor` (lines 50–58) applies one member visitor to both fields and methods and passes no attribute visitor. The list overload (lines 78–103) returns an empty multi-visitor when the list is null or empty, and otherwise one child visitor per specification, in list order, with a null wildcard-variable list.
  - [P3] For one specification (lines 132–242), a null class name becomes `**`. Annotation type, class name, extended-class annotation, and extended-class name are parsed in that order with `ListParser`/`ClassNameParser`, so wildcard slots are filled in that order. A wildcard class name, an `extends` name, or an `extends` annotation selects every class and then applies a class-name filter. Otherwise a single named class is visited. A non-null annotation type keeps only classes carrying that annotation. Non-zero required-set or required-unset access flags add an access filter. An `extends` clause walks only subclasses (`ClassHierarchyTraveler(false, false, false, true)`). A wildcard extended name is filtered. A concrete extended name replaces the start class, so the walk starts at that named superclass.
  - [P3] Class attributes, fields, and methods are combined in lines 266–337. A null field-specification list disables the field visitor, and a null method-specification list disables the method visitor, even if a visitor was passed. If the only visitor is the class visitor, it is returned unwrapped. Class attribute names, when present, are filtered with the raw name list. If the class visitor is null while a field or method visitor is active, the class-attribute visitor added just above is dropped and only the member visitor is returned. Member attributes are still visited later.
  - [P3] Member matching (lines 357–371 and 380–400) adds one visitor per field specification, then one per method specification, and marks matches in the class and its superclasses (`ClassHierarchyTraveler(true, true, false, false)`), not interfaces or subclasses. `createNonTestingClassVisitor` (lines 416–426) only forwards to that member visitor.
  - [P3] Each member (lines 554–648) parses the annotation type, the name, the descriptor, and the attribute-name list, in that order, into the same wildcard-variable list. A non-null attribute visitor is composed with the member visitor, and attribute names are filtered with the parsed matcher (unlike the raw list used for class attributes). A member annotation and non-zero access flags add those filters. A name and a descriptor that are both present and contain none of `!`, `*`, `?`, `%`, `,`, `///`, or a `<digits>` backreference visit one named field or method. Any of those wildcard forms, or a missing name or descriptor, scans every field or method and applies only the filters that were specified.
  - [P3] The conditional testers (lines 435–539) are also absent. The pool form visits the pool, then classes that match the specification. The class form matches the class first and only then requires every field specification and every method specification, with field checks outside method checks. Each matching member runs the next test, so several matches repeat the inner visits. A null specification list adds no constraint. These tests pass no attribute visitor.
  - [P3] Searches of `/workspace/zel-cursor` for this factory, `ClassSpecification`, `MemberSpecification`, `ClassPoolVisitor`, and `ClassNameParser` found no port. `crates/zel-bazel-tools/resources/tools/jdk/proguard_allowlister.py` only accepts or rejects library ProGuard flags. It does not match classes or members.

### `third_party/java/proguard/proguard6.2.2/src/proguard/Configuration.java`
- Bazel lines: 375
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard configuration object. Bazel flags only select an external ProGuard (`proguard_top`, `*.pgcfg` enforcement, incremental dexing). They do not hold program/library class paths, keep specifications, or shrink/optimize/obfuscate/preverify settings.
  - [P3] Missing defaults that change the engine: `shrink`, `optimize`, `obfuscate`, and `preverify` start true; `optimizationPasses` is 1; `skipNonPublicLibraryClassMembers` and `useMixedCaseClassNames` start true; `skipNonPublicLibraryClasses`, access modification, aggressive interface merging, aggressive overloading, unique member names, `keepParameterNames`, `microEdition`, `android`, `verbose`, `ignoreWarnings`, `backport`, and `addConfigurationDebugging` start false; `lastModified` is 0; `targetClassVersion` is 0 (leave class version unchanged).
  - [P3] Missing the empty-file `STD_OUT` sentinel, where an empty `printSeeds`, `printUsage`, `printMapping`, `printConfiguration`, or `dump` path means standard output, and `Long.MAX_VALUE` `lastModified` forces a rebuild.
  - [P3] Missing null-vs-empty list meanings for `keepDirectories`, `optimizations`, `keepPackageNames`, `keepAttributes`, `note`, and `warn` (null means none or all, depending on the field; empty means the opposite).

### `third_party/java/proguard/proguard6.2.2/src/proguard/ConfigurationChecker.java`
- Bazel lines: 228
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard configuration sanity check. An empty program classpath (`-injars` unset) is not rejected.
  - [P3] The first program entry being an output, and an unfiltered output entry immediately followed by another output, are not rejected.
  - [P3] Duplicate classpath names are not checked across program and library entries: the same output twice, input and output sharing a name, or an unfiltered duplicate input are not rejected. Filtered duplicate inputs are the only pair this class allows.
  - [P3] There is no Windows/macOS note when obfuscation with mixed-case class names and no class-name dictionary writes class files to a plain output directory.
  - [P3] There is no note when `-adaptresourcefilecontents` is empty or matches every file.
  - [P3] There is no note pass over `-keepclassmembers` specs that name no members, or over `-assumenosideeffects` specs that wildcard every method, and no summary of those notes. Notes are skipped entirely when `note` is a non-null empty filter.

### `third_party/java/proguard/proguard6.2.2/src/proguard/ConfigurationConstants.java`
- Bazel lines: 140
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not define or match this ProGuard configuration vocabulary, so the directive and option strings (`@`, `-include`, `-basedirectory`, jar options, keep/optimize/obfuscate/preverify flags, and the diagnostic and processing options) are never recognized when parsing or writing a configuration.
  - [P3] Class-specification and value keywords (`class`, `extends`, `implements`, `*`, `***`, `<fields>`, `<methods>`, `...`, `..`, `return`, `true`, `false`, and the brace, argument, and separator tokens) are not implemented.
  - [P3] `JAR_SEPARATOR_KEYWORD` is the live JVM `path.separator`; zel has no equivalent runtime separator for ProGuard jar lists.
  - [P3] The `<` and `>` delimiters used to expand system properties inside configuration text are not implemented.

### `third_party/java/proguard/proguard6.2.2/src/proguard/ConfigurationParser.java`
- Bazel lines: 1837
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not parse ProGuard 6.2.2 configuration text into a configuration object. `proguard_allowlister.py` only strips `#` comments, splits on a leading `-`, and accepts library options whose first token starts with `keep`, `assumenosideeffects`, `assumevalues`, `adaptresourcefilecontents`, or `if`, plus `dontnote` and `dontwarn` only when they have an extra argument. It copies the original text with a merge comment and does not interpret the language.
  - [P3] Option names match by prefix. `-resourcejars` is rejected as unsupported. `@` and `-include` push a nested reader from an absolute URL, a URL relative to the current base, a classpath resource, or a file, and the included file's mtime raises `lastModified`. `-basedirectory` changes the reader's base directory. `-forceprocessing` sets `lastModified` to `Long.MAX_VALUE`.
  - [P3] `-injars`, `-outjars`, and `-libraryjars` take classpath entries separated by the jar separator. A parenthesized filter list is applied from the inside out as the general filter, then jar, war, ear, jmod, zip, apk, and aar filters. A missing separator is an error.
  - [P3] `-if` must be followed by one of `-keep`, `-keepclassmembers`, `-keepclasseswithmembers`, `-keepnames`, `-keepclassmembernames`, or `-keepclasseswithmembernames`. Keep modifiers are `includedescriptorclasses`, `includecode`, `allowshrinking`, `allowoptimization`, and `allowobfuscation`. The preceding comments are stored on the class specification.
  - [P3] Class specifications accept `public`, `final`, `interface`, `abstract`, `synthetic`, `@` annotation types, and `enum`, with `!` negation and an error when the same flag is both required and forbidden. A lone `*` class name means any class. `extends` and `implements` take an optional annotation and a class name. Members sit in braces. `<fields>`, `<methods>`, and the any-member wildcard become field and/or method specifications. A constructor is `<init>` or the class's simple name before `(`. Field and method descriptors are converted to internal form. Method-only or field-only access flags are rejected.
  - [P3] `-assumenosideeffects`, `-assumenoexternalsideeffects`, `-assumenoescapingparameters`, `-assumenoexternalreturnvalues`, and `-assumevalues` parse class specifications that may carry a value or `..` range. Booleans become 0 or 1. Byte, char, short, and int use `Integer.decode`. Long, float, and double are rejected. `-optimizationpasses` must be an integer greater than 0.
  - [P3] Comma-separated lists optionally allow an empty list, a closing parenthesis, file-name words, Java identifiers, generics, `<property>` substitution, and conversion of external class names or types to internal form. Angle brackets that are not a numeric wildcard are rejected when generics are forbidden. An undefined `<property>` is a parse error. An unclosed `<` is left unchanged. Relative files resolve against the base directory. Dictionary options become URLs, trying an absolute URL, the base URL, a classpath resource, then a file URL.
  - [P3] A word ends an option when it is absent, starts with `-`, or is exactly `@` (unless `@` is expected). Empty words are not Java identifiers. Identifiers may contain `.[]<>-!*%?` and Java identifier parts. `close()` closes the word reader.

### `third_party/java/proguard/proguard6.2.2/src/proguard/ConfigurationWriter.java`
- Bazel lines: 752
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not write an in-memory ProGuard configuration out as config text.
  - [P3] Missing output includes program and library jar lines with aar, apk, zip, jmod, ear, war, jar, and class filters. It also includes boolean, integer, list, string, URL, and file options, plus keep and `-if` rules with descriptor, code, shrinking, optimization, and obfuscation modifiers. Assumption options, member annotations, boolean and numeric value ranges, and `#` comments are missing too.
  - [P3] Missing path handling includes names relative to the configuration file's parent, quoting when the string is empty or contains spaces or `@{}():;,.`, and an I/O error when the writer reports a failure.

### `third_party/java/proguard/proguard6.2.2/src/proguard/DataEntryReaderFactory.java`
- Bazel lines: 236
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `createDataEntryReader` (lines 52–134) is absent. It prints `messagePrefix`, then `apk`/`jar`/`aar`/`war`/`ear`/`jmod`/`zip` or `directory`, then `" [" + name + "]"`. `getFilterExcludingVersionedClasses` always returns a non-null list, so the name filter is always installed and the line always ends with `" (filtered)"`.
  - [P3] That filter (lines 208–234) is `!META-INF/versions/**` when `ClassPathEntry.getFilter()` is null. If any element contains the substring `META-INF/versions`, the original list is kept. Otherwise that exclude is prepended and the original elements follow. Patterns are parsed with `ListParser(FileNameParser)` and applied as a `DataEntryNameFilter` inside later archive readers, so they match archive members (and plain directory entries).
  - [P3] Unwrapping is first-match and then stops (lines 102–127): apk, else jar, else aar, else war, else ear, else jmod, else zip. The chosen type is always opened. Earlier types are opened only when the entry name ends in `.apk`, `.jar`, `.aar`, `.war`, `.ear`, `.jmod`, or `.zip`. Later types are not wrapped, so an apk is not scanned for nested jars, and a war is not scanned for nested ears, jmods, or zips. A directory gets every extension check, outermost zip first.
  - [P3] The type’s own filter (`getApkFilter`, `getJarFilter`, `getAarFilter`, `getWarFilter`, `getEarFilter`, `getJmodFilter`, `getZipFilter`) is applied only in the extension-match branch (lines 182–188). When the classpath entry itself is that type (`isJar == true`, lines 174–177), that filter is ignored and every entry is unzipped.
  - [P3] War and jmod wraps set `stripClassesPrefix` (lines 114 and 122, 163–168). Members whose names end in `.class` pass through `PrefixStrippingDataEntryReader` with prefix `classes/` (`ClassConstants.JMOD_CLASS_FILE_PREFIX`); other members do not. The same stripped reader is the non-match fallback, so on a directory classpath the strip also applies to entries that are not inside a war or jmod, once per wrap. Jmod reads pass `stripJmodHeader` true (line 122); apk, jar, aar, war, ear, and zip do not.
  - [P3] Searches under `/workspace/zel-cursor/crates` for `META-INF/versions`, `DataEntryReader`, `JarReader`, `jmod`, `.aar`, and `classes/` prefix stripping found no reader factory. Zel only records ProGuard flags (`proguard_top`, `enforce_proguard_file_extension`) and ships `proguard_allowlister.py`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/DataEntryWriterFactory.java`
- Bazel lines: 239
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard output-writer factory that walks a classpath from `toIndex - 1` down to `fromIndex` and chains one writer per entry, returning null for an empty range.
  - [P3] Each entry is not logged as `Preparing output` apk, jar, aar, war, ear, jmod, zip, or directory, with the entry name and a `(filtered)` suffix when any filter list is non-null.
  - [P3] Archive nesting is absent: zip inputs flatten into jmod/ear/war/aar/jar/apk outputs, then jmod into ear and below, ear into war and below, war into aar/jar/apk, aar into jar/apk, and jar into apk; apk inputs are never flattened. Flattened parents are unpacked; otherwise entries are packed, with a jmod header and class-file prefix for jmod and a `WEB-INF/classes` prefix for war.
  - [P3] Parent-extension routing is absent: a parent matching `.zip`, `.jmod`, `.ear`, `.war`, `.aar`, `.jar`, or `.apk` is written into that archive, optionally filtered by the matching classpath filter parsed as file-name patterns; other parents are packed only when that type is the output archive.
  - [P3] Program classes from the class pool, the versioned-class name filter, and injected extra classes (same writer for both slots, `.class` suffix) are not applied, and there is no cascade to the next classpath entry when the current writer does not accept the entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/DescriptorKeepChecker.java`
- Bazel lines: 168
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no pass that clears visitor info, marks program and library seeds from keep specifications with `KeepMarker`, then walks kept program members.
  - [P3] Zel does not skip field descriptors and method return types, then visit only non-null method argument classes.
  - [P3] Zel does not note a kept entry point whose argument descriptor is a program class that is not itself kept, and it does not ignore unkept library classes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/DuplicateClassPrinter.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No class visitor prints a note for a duplicate program class, using the internal class name as the warning key and the message `Note: duplicate definition of program class [<external name>]`.
  - [P3] No class visitor prints the matching library-class note `Note: duplicate definition of library class [<external name>]`.
  - [P3] Nothing converts the visited class’s internal name to an external (dot-separated) name for those notes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/FileWordReader.java`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel reader opens a local file as UTF-8 (`FileInputStream` to `InputStreamReader` with charset `UTF-8` to `BufferedReader` to `LineNumberReader`), labels it `file '<File.getPath()>'`, and sets the include base to `file.getParentFile()` (null when the path has no parent).
  - [P3] No Zel reader opens `URL.openStream()` as UTF-8, labels it `file '<url.toString()>'`, and uses that URL itself as the include base.
  - [P3] Both constructors throw `IOException` when the stream cannot be opened. Zel has no equivalent entry point.

### `third_party/java/proguard/proguard6.2.2/src/proguard/FullyQualifiedClassNameChecker.java`
- Bazel lines: 201
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No check of ProGuard class specifications walks annotation types, class names, extends-annotation types, extends class names, field annotation types and descriptors, and method annotation types plus each return and argument type in the descriptor.
  - [P3] An internal name that is non-null, contains none of `!`, `*`, `?`, `,`, `///`, or `<`, and is absent from both the program and library class pools is not noted, subject to the note filter accepting that name, as `Note: the configuration refers to the unknown class '<external name>'`.
  - [P3] No follow-up search rewrites a name ending in `/class` to the prefix before the last `/`, or any other name to `**/<simple name>`, matches that pattern in both pools, and prints ` Maybe you meant the fully qualified name '<external name>'?` to stdout for each hit.
  - [P3] Primitive and non-class descriptors, null names and types, and null specification lists are not ignored as part of that pass, because the pass does not exist.

### `third_party/java/proguard/proguard6.2.2/src/proguard/GPL.java`
- Bazel lines: 197
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `GPL.check()` does not capture the current exception stack, collect caller packages outside the known allowlist, and print the four-line GNU GPL linking notice that interpolates those packages plus `...`.
  - [P3] Stack frames are not parsed: trimmed lines starting with `at `, then cut at the last `(`, then the last `.` twice (method and class removed). A missing separator becomes empty and is dropped. An `IOException` does not stop the scan while keeping names already found.
  - [P3] These `startsWith` allowlist prefixes are not skipped: `java`, `sun.reflect`, `proguard`, `org.apache.tools.ant`, `org.apache.tools.maven`, `org.gradle`, `org.codehaus.groovy`, `org.eclipse`, `org.netbeans`, `com.android`, `com.intel`, `com.sun.kvem`, `net.certiv.proguarddt`, `groovy`, `scala`, `sbt`, `xsbt`, `eclipseme`.
  - [P3] Unknown packages are not deduplicated, and a name is not omitted when a proper package prefix of it is also unknown. Kept names are not joined in set order with a trailing `, `.
  - [P3] `main` does not read a stack trace from stdin and print only that comma-separated unique-package string when the set is non-empty, printing nothing when it is empty.

### `third_party/java/proguard/proguard6.2.2/src/proguard/GetAnnotationChecker.java`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No constant visitor notes method references that read annotations.
  - [P3] A method reference is noted only when its class is `java/lang/Class`, `java/lang/reflect/Field`, or `java/lang/reflect/Method` and its name is `getAnnotation`, `getAnnotations`, `getDeclaredAnnotations`, or `getParameterAnnotations`. Every other constant is ignored.
  - [P3] The note is keyed by the referencing class’s internal name and reads `Note: <external caller> calls '<short referenced class>.<method>'` (for example `Note: com.example.Foo calls 'Class.getAnnotation'`).

### `third_party/java/proguard/proguard6.2.2/src/proguard/GetEnclosingClassChecker.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant-pool visitor that inspects `Methodref` entries and ignores every other constant kind.
  - [P3] Zel never checks whether a method reference’s class is `java.lang.Class` and its name is `getEnclosingClass` or `getDeclaringClass` (name only; descriptor is not checked).
  - [P3] Zel never emits the note `Note: <external class name> calls 'Class.<method>'` for the referencing class when either of those calls is present.

### `third_party/java/proguard/proguard6.2.2/src/proguard/GetEnclosingMethodChecker.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No constant visitor inspects method references and ignores every other constant.
  - [P3] No check that a method reference targets `java.lang.Class.getEnclosingConstructor` or `java.lang.Class.getEnclosingMethod`.
  - [P3] No note of the form `Note: <external class name> calls '<short external class name>.<method name>'` for those calls.

### `third_party/java/proguard/proguard6.2.2/src/proguard/GetSignatureChecker.java`
- Bazel lines: 78
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No constant visitor notes a method reference whose owner is exactly `java/lang/Class`, `java/lang/reflect/Field`, or `java/lang/reflect/Method` when the method name starts with `getType` or `getGeneric`.
  - [P3] No note `Note: <caller external name> calls '<Class|Field|Method>.<method>'`, keyed by the caller’s internal name so a note filter can suppress it.
  - [P3] Constants other than method references, including interface method references, produce no note.

### `third_party/java/proguard/proguard6.2.2/src/proguard/Initializer.java`
- Bazel lines: 623
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No pass initializes program and library class pools: superclass hierarchies (program classes with unresolved-class warnings, library classes with library-depends-on-program warnings and no class warnings), then program class and member references.
  - [P3] Unless globally unique obfuscated member names are required, the library pool is not reduced to library classes referenced by program classes (first directly, without reflection; then including superclasses, interfaces, and reflection). Unused library classes are not discarded, and referenced library members are not initialized without member warnings. With unique member names, every library class is not retained and initialized the same way.
  - [P3] Enum annotation field references, `Class.forName` and other dynamic class references, and `Class.get[Declared]{Field,Method}` references are not resolved. String constants are not treated as class names when `-adaptclassstrings` is set. The Java comment about `WebView.addJavascriptInterface` has no initializer; that note printer is constructed and never used.
  - [P3] Subclass hierarchies are not linked, and equal strings are not shared across program and library classes.
  - [P3] `-keep`, `-assumenosideeffects`, `-assumenoexternalsideeffects`, `-assumenoescapingparameters`, and `-assumenoeexternalreturnvalues` are not checked for unknown classes or unknown members. Unkept descriptor classes of kept members, and `-keep` specs that match only library classes, are not checked.
  - [P3] If `-keepattributes` does not match `RuntimeVisibleAnnotations`, `Signature`, `InnerClasses`, or `EnclosingMethod`, there is no note when program classes reflect on those attributes.
  - [P3] Note and warning summaries are absent (unknown classes and members, reflective annotation/signature/inner/enclosing access, unkept descriptors, kept library classes, unresolved dynamic classes, dynamic casts, reflective member access, unresolved class references, library classes depending on program classes, and unresolved program or library members). Conditional hints for `-skipnonpubliclibraryclasses` and `-dontskipnonpubliclibraryclassmembers` are absent. Warnings do not abort with "Please correct the above warnings first." unless `-ignorewarnings`. There is no "you're ignoring all warnings!" note, and verbose mode does not print the original and final library class counts.
  - [P3] Keep specs do not supply note exceptions: literal class or extends names (when the class is marked, or when only extensions are requested) and literal field or method names, skipping names that contain a numeric wildcard reference such as `<1>`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/InputReader.java`
- Bazel lines: 222
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no reader that fills a program class pool and a library class pool from the configured jars. Option names such as `proguard_top` do not load classes.
  - [P3] A set `favor.library.classes` property (any value) reads library jars first, so a library class keeps the name and a later program class with that name is only a duplicate. Unset, program jars are read first and a later library class with the same name is the duplicate. A null library-jar list skips the library pass. Output classpath entries are skipped. The public range is half-open, from index inclusive to index exclusive.
  - [P3] Only `.class` entries are decoded. Program entries become program classes. Library entries become library classes, with non-public library classes and members skipped when those options are set. A class already in the pool being checked is not added. The program pass checks the program pool and then the library pool. The library pass checks the program pool and then the library pool, except the library-first pass, which checks only the library pool.
  - [P3] After the program jars, an empty program pool throws `IOException` with `The input doesn't contain any classes. Did you specify the proper '-injars' options?` Library jars that have not been read yet do not satisfy that check, and the note and warning summaries are not printed.
  - [P3] Notes go to stdout and warnings to stderr, filtered by the note and warning class lists. A note count above zero then prints `Note: there were N duplicate class definitions.` and the `duplicateclass` troubleshooting URL on stderr. A warning count above zero prints `Warning: there were N classes in incorrectly named files.` plus the file-name and package-hierarchy lines and the `unexpectedclass` URL. Unless `-ignorewarnings` is set, it also prints the hint to use that option and throws `Please correct the above warnings first.`
  - [P3] Each input entry is pumped with the prefix `Reading program ` or `Reading library ` through a reader that unwraps jars, wars, ears, jmods, and zips. An `IOException` is rethrown as `Can't read [<entry>] (<message>)` with the original as the cause.

### `third_party/java/proguard/proguard6.2.2/src/proguard/KeepClassMemberChecker.java`
- Bazel lines: 86
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never notes a keep specification that does not mark the class (`markClasses` false: `-keepclassmembers`, `-keepclasseswithmembers`, `-keepclassmembernames`, `-keepclasseswithmembernames`) when both the field list and the method list are null or empty. Specs that mark the class (`-keep`, `-keepnames`) are not noted even with no members, and a null specification list is a no-op.
  - [P3] The note names `className`, otherwise `extendsClassName`, with `/` rewritten to `.`, or `*` when both are null. A null name is always noted; a non-null name is noted only when the note printer accepts it.

### `third_party/java/proguard/proguard6.2.2/src/proguard/KeepClassSpecification.java`
- Bazel lines: 179
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel type is a ProGuard class specification plus keep flags: mark classes, mark only when every specified member is present, mark descriptor classes, mark method code attributes, and allow shrinking, optimization, and obfuscation, with an optional extra class-specification condition.
  - [P3] No constructor copies an existing class specification (sharing its field and method lists) and stores those flags and the condition. The older constructor that forces code-attribute marking off and the condition to null is also absent.
  - [P3] No equality check requires the same runtime class, compares the seven flags, uses null-safe equality for the condition, and then the superclass comparison (which ignores comments).
  - [P3] No hash mixes those flags as XOR bits (each false flag contributes 1, 2, 4, 8, 16, 32, or 64 and each true flag contributes 0) with the condition hash (0 if null) and the superclass hash.
  - [P3] No shallow clone of that keep specification, including the shared condition and member lists.

### `third_party/java/proguard/proguard6.2.2/src/proguard/KeepClassSpecificationVisitorFactory.java`
- Bazel lines: 297
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No factory is parameterized by shrinking, optimizing, and obfuscating, then walks a keep-specification list and keeps a spec only when an enabled step is one the spec disallows (`allowShrinking`, `allowOptimization`, or `allowObfuscation`). A null list, all three flags false, or every spec allowing the enabled steps yields an empty multi class-pool visitor.
  - [P3] When `markDescriptorClasses` is set and a class visitor is present, matching fields and methods also visit the classes named in their descriptors. That descriptor visitor is attached before a later step may drop the class visitor, so descriptor classes are still marked when the keep class itself is not.
  - [P3] The class visitor is dropped unless `markClasses` or `markConditionally` is set. A supplied attribute visitor is kept only for attributes named `Code` when `markCodeAttributes` is set, and is dropped otherwise.
  - [P3] An `-if` condition is parsed before the keep body so wildcard matchers are registered in configuration order. Each condition match then reapplies the keep visitor to the whole class pool.
  - [P3] `markConditionally` parses the keep specification itself as the condition, adding wildcard matchers only to a copy of the matcher list, and applies the class and member visitors only to classes that satisfy that condition. With both an `-if` condition and `markConditionally`, the `-if` matchers stay on the shared list and the conditional-member matchers stay local.

### `third_party/java/proguard/proguard6.2.2/src/proguard/LibraryKeepChecker.java`
- Bazel lines: 125
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No check walks a keep-specification list and notes rules that match library classes only. A null list is a no-op, and a spec whose class name is exactly `*` (stored as null) is skipped. Any other class name, including other wildcards, is checked.
  - [P3] Each spec is matched against the program class pool first. If that count is not zero, the spec produces no note. If it is zero, every matching library class is noted as `Note: the configuration explicitly specifies '<external keep class name>' to keep library class '<external library class name>'`. The note key is the library class's internal name, so a note filter can suppress it. Program classes are never noted.
  - [P3] Matching follows the keep-spec visitor (name, access, annotations, extends/implements, `-if` conditions, and `includedescriptorclasses`). Specs that allow shrinking, optimization, or obfuscation are still checked. A `-keepclassmembers` spec does not visit the class itself unless `includedescriptorclasses` also visits descriptor classes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/LineWordReader.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no line-numbered word-reader that returns `readLine()` results (terminator stripped, null at EOF, line count advanced on `\n`, `\r`, or `\r\n`).
  - [P3] Zel does not format the stream location as `line <current line number> of <description>`.
  - [P3] Zel does not close that underlying reader after closing any included word reader.
  - [P3] Zel does not construct that reader with either a filesystem base directory or a base URL.

### `third_party/java/proguard/proguard6.2.2/src/proguard/MemberSpecification.java`
- Bazel lines: 116
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel type holds a ProGuard member specification: required set and unset access flags, annotation type, member name (null, or with `*` or `?`), and descriptor (null, or with `**`, `*`, or `?`).
  - [P3] No constructor for an all-members spec (both flags 0 and the three strings null) or for those five values.
  - [P3] No equality or hash that compares the flags and null-safe strings, rejects null and a different runtime class, and ignores `attributeNames`.
  - [P3] The unused `attributeNames` field, which stays null and is excluded from equality and hashing, is also absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/MemberValueSpecification.java`
- Bazel lines: 110
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel type is a member specification plus a constant value or value range (`Number[]`, one element or a two-element range) assigned to that member.
  - [P3] No zero-argument constructor that sets both access-flag masks to 0 and leaves the annotation, name, descriptor, and values null.
  - [P3] No constructor that forwards the access flags, annotation, name, and descriptor and stores the values array, including null.
  - [P3] No equality that returns false for null, throws unless the other object is a `MemberValueSpecification`, then requires the member fields to match and compares values element-wise (both null match; the receiver array’s length is the only length checked).
  - [P3] No hash that XORs the member-specification hash with 0 when values is null, otherwise a rotate-left XOR of each element’s hash code.

### `third_party/java/proguard/proguard6.2.2/src/proguard/OutputWriter.java`
- Bazel lines: 298
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not write a processed program class pool to classpath output entries. A run of input entries is not flushed when the last of a following run of output entries is reached, covering every output in that run, and the next input run does not start after it. Inputs with no following output are not written. If the classpath starts with an output, the read range still begins at index 0, so that output entry is read as an input.
  - [P3] There is no cascading writer for apks, jars, wars, ears, zips, and directories over that output range. An `IOException` is not rethrown as `Can't write [<that output entry's name>] (<message>)` with the original exception as the cause.
  - [P3] `-addconfigurationdebugging` does not add the configuration logger's class-map file as an extra output entry, and does not print these two warnings on stderr: `Warning: -addconfigurationdebugging is enabled; the resulting build will contain obfuscation information.` and `It should only be used for debugging purposes.`
  - [P3] With obfuscation and `-adaptresourcefilenames` set, matching resource names are not renamed with a package-prefix map. Other names are not left unchanged.
  - [P3] Resource bytes are not copied through by default. With shrinking, optimization, or obfuscation and `-adaptresourcefilecontents` set, only obfuscation actually rewrites matching contents: `META-INF/MANIFEST.MF` and `META-INF/*.SF` go through a manifest rewriter and every other matched name through a general resource rewriter, using UTF-8 when the configuration is Android and the JVM default charset otherwise. Names outside that filter stay plain copies. Shrink or optimize without obfuscation installs the filter but both sides still copy.
  - [P3] With `-keepdirectories` set, matching directories are renamed with that package-prefix map and then copied, and non-matching directories are copied under their original names. With no keep-directories filter, directories are dropped. Non-directories go through the resource rewriter. Class entries are separated out and written through an idle rewriter on the output writer, which emits the already processed classes.
  - [P3] That package-prefix map is not built by walking every program class name. The old internal package prefix is updated from the class's current name until a stored prefix is identical to the old prefix, after which later classes in that package leave the mapping unchanged. The injected class-name map is not passed into the writer factory.
  - [P3] The input slice is not read with the progress text ` Copying resources from program `, and the output writer is not closed afterward.

### `third_party/java/proguard/proguard6.2.2/src/proguard/ParseException.java`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No checked exception type that signals a ProGuard parse failure.
  - [P3] No no-argument constructor that leaves the detail message null.
  - [P3] No string constructor whose message is later returned by `getMessage`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/ProGuard.java`
- Bazel lines: 614
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No ProGuard 6.2.2 driver. `execute` prints the version banner, runs the GPL check, optionally writes the configuration, then runs the configuration check.
  - [P3] No up-to-date early return: when the program classpath has an output and `UpToDateChecker` reports it current, processing stops before any class is read.
  - [P3] A non-zero target class version forces backporting, then a later target-version rewrite. Neither step exists.
  - [P3] No pipeline over program and library class pools: read input, drop `StackMapTable` when shrinking, optimizing, obfuscating, or preverifying, initialize when seeds are printed or any of those steps or backporting runs, and introduce primitive-array constants only when optimizing or obfuscating.
  - [P3] No backport, configuration-debug logging, or seed printing, and no subroutine inlining when preverifying or when the Android flag is set.
  - [P3] No shrink pass. During a verbose shrink there is no "why kept" or usage-file message.
  - [P3] No optimization loop. The optimization filter defaults to matching everything, or is parsed from `-optimizations`. Gson optimization runs only when that filter matches the Gson library name and `com/google/gson/Gson` is in the program pool. Each pass stops when the optimizer reports no improvement. After a pass that may still shrink, usage and "why kept" output are cleared and shrink runs again. Line numbers are then linearized.
  - [P3] No obfuscation, and no expansion of primitive-array constants after optimize or obfuscate.
  - [P3] No preverification, and no line-number-table trim after optimize or preverify.
  - [P3] Class elements are not sorted after shrink, optimize, obfuscate, or preverify. Output is not written when the program classpath has an output, and the injected-class map (original name to injected names) is not passed through backport, config logging, Gson optimization, optimization, and output. No class dump when a dump file is set.
  - [P3] No CLI `main`. With no arguments it prints the version and `Usage: java proguard.ProGuard [options ...]`, then exits 1. Otherwise it parses arguments with system properties, closes the parser, and runs. Any exception prints a stack trace when verbose, otherwise `Error: ` plus the message on stderr, then exits 1. Success exits 0. Zel only packages the Java tool as `main_class = "proguard.ProGuard"` on a prebuilt jar.

### `third_party/java/proguard/proguard6.2.2/src/proguard/SeedPrinter.java`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `-printseeds` writer that lists program classes and members kept against shrinking, optimization, or obfuscation.
  - [P3] Missing the error when `keep` is unset: `You have to specify '-keep' options if you want to write out kept elements with '-printseeds'.`
  - [P3] Missing the mark-then-print sequence: clear visitor info on the program and library pools, mark seeds from the keep specifications with the same marker for shrinking, optimization, and obfuscation, then print only the program pool’s kept classes in alphabetical order (names without access modifiers) and their kept members.

### `third_party/java/proguard/proguard6.2.2/src/proguard/SubclassedClassFilter.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No class visitor forwards `visitProgramClass` only when `programClass.subClasses` is non-null.
  - [P3] No class visitor forwards `visitLibraryClass` only when `libraryClass.subClasses` is non-null.
  - [P3] A null subclass list does not skip the delegated visit, and a non-null empty list is not treated as subclassed, because this filter is absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/Targeter.java`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never sets a configured target class-file version on every program class.
  - [P3] Zel never records class versions newer than that target (and only when `warn` is unset), prints the stderr warning that lists those versions against the target, or throws unless `ignoreWarnings` is set.

### `third_party/java/proguard/proguard6.2.2/src/proguard/UpToDateChecker.java`
- Bazel lines: 251
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard freshness check. Bazel’s `check()` returns false when the newest input mtime is strictly newer than the oldest output mtime, and otherwise prints `The output seems up to date` and returns true so `ProGuard.execute` can skip processing when `programJars` has an output.
  - [P3] Inputs are `configuration.lastModified` (newest `-include` mtime, or `Long.MAX_VALUE` for `-forceprocessing`), non-output program jars, every library jar, `-applymapping`, and the three obfuscation dictionaries. Outputs are output program jars plus `-printseeds`, `-printusage`, `-printmapping`, `-printconfiguration`, and `-dump`. A null file is skipped, and an output whose name is empty is skipped.
  - [P3] Directories do not use their own mtime; children are walked recursively. An empty output directory is recorded as `Long.MIN_VALUE`. A missing file contributes mtime 0. The walk aborts at the first outdated pair via `IllegalStateException` (caught only there). `listFiles()` returning null is an uncaught `NullPointerException`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/WordReader.java`
- Bazel lines: 427
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard configuration word reader. `nextWord(isFileName, expectSingleFile)` skips Java `Character.isWhitespace` and `#` comments, then returns one token or `null` at end of input. Quoted words use matching `'` or `"` and omit the quotes. An unclosed quote sets the current word to the opening quote plus the rest of the line and throws `IOException` with `Missing closing quote for ` plus the location text.
  - [P3] A word that starts with `-` is never treated as a file name. Otherwise, when `isFileName` is true, the token runs to the end of the line, to `(` `)` `,` `;`, or to the platform `File.pathSeparatorChar` unless `expectSingleFile` is true, or to `-` or `#` only when the previous character is whitespace. Trailing whitespace is trimmed. On Linux that separator is `:`. On Windows it is `;`, which is already a hard delimiter.
  - [P3] Without file-name mode, a word that starts with `@` `{` `}` `(` `)` `,` `;` or the path separator is that one character. Any other word stops at whitespace, `#`, or those delimiters except `@`, so `@` only breaks a token when it is the first character. With file-name mode, a leading `@` is part of the file token.
  - [P3] Indexes and slices are Java `char` indexes (`charAt`, `length`, `substring`), so a supplementary character is two units and is never itself a delimiter, quote, or comment marker.
  - [P3] `#` comments before a word are kept without the `#`, joined by `\n`, and `lastComments()` returns that string and then clears it. While an included reader is active, only that reader's comments are returned.
  - [P3] `includeWordReader` pushes another reader onto the tail. `nextWord` reads the newest include first and, when it returns `null`, closes it and continues with the parent. `setBaseDir`, `getBaseDir`, and `getBaseURL` follow the include chain. `close()` closes and drops the include.
  - [P3] `locationDescription()` is `end of ` or `'<word>' in `, plus the subclass line location. Each include adds its own description, then `,\n included from `.
  - [P3] `crates/zel-bazel-tools/resources/tools/jdk/proguard_allowlister.py` only strips `#…` lines and splits on a leading `-`. It does not quote, delimit, include, or report locations.

### `third_party/java/proguard/proguard6.2.2/src/proguard/backport/AbstractAPIConverter.java`
- Bazel lines: 910
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that rewrites a program class's code first, then constant-pool class names, field and method descriptors, non-code member attributes, and class attributes, and that shrinks unused constant-pool entries and notifies a modified-class visitor only when something changed.
  - [P3] Zel has no descriptor and signature rewriter for local-variable tables, local-variable type tables, signatures, annotations, parameter annotations, annotation defaults, nested annotations, annotation arrays, enum-constant type names, and class element values.
  - [P3] Zel has no invoke or field-access rewriter. Bazel matches `invokevirtual`, `invokespecial`, `invokeinterface`, `invokestatic`, and get/put field instructions, then replaces a method call when the class pattern matches (including subclasses and implementors), the name matches (including `<default>` and `<static>`), and the descriptor matches (`**` matches any). Replacements support `*` and `<1>`, and `<default>` inserts the matched class as an extra receiver argument. The new opcode is `invokestatic`, `invokeinterface`, or `invokevirtual`, with an interface or ordinary method reference. A missing replacement method or type is left unchanged and reported through the warning printer. A successful replacement can be passed to an extra instruction visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/backport/Backporter.java`
- Bazel lines: 332
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file backport pass. ProGuard always clears visitor info on the program and library pools, then, when the configured target class version is below 1.9, rewrites invokedynamic string concatenation in 1.9+ classes that have `BootstrapMethods` into `StringBuilder` chains, drops unused bootstrap methods, and reinitializes the new references.
  - [P3] Below 1.8 it converts lambdas and method references (recording injected class names), then strips static and default interface methods from 1.8+ interfaces and reinitializes references only on classes those converters changed.
  - [P3] Below 1.7 it rewrites every program method: `Objects.requireNonNull(Object)` becomes `dup` / `Object.getClass` / `pop`, and `Throwable.addSuppressed` is deleted by popping both operands.
  - [P3] Below 1.8, stream backport runs only if some `java8/**` class is in the program or library pool, skips the `java8/**` library itself, and on any unconverted call prints the warning count plus the Gradle streamsupport guidance. The same gate and warning pattern applies to `org/threeten/bp/**` and the Java 8 time API.
  - [P3] A non-zero target version is written onto every program class. Verbose mode prints “Backporting class files...” and the seven conversion counts (string concatenations, lambdas, static interface methods, default interface methods, Java 7+ calls, stream calls, time calls), which stay zero when a pass is skipped.

### `third_party/java/proguard/proguard6.2.2/src/proguard/backport/DefaultInterfaceMethodConverter.java`
- Bazel lines: 362
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Library classes are ignored. For a program interface, only concrete program subclasses are collected (subclass walk, other interfaces skipped via `ACC_INTERFACE`).
  - [P3] Each non-static method that has a `Code` attribute is treated as a default method. It is copied onto an implementing class when that class and its superclasses do not already define the same name and descriptor, and the modified-class visitor is then notified.
  - [P3] An `INVOKESPECIAL` whose constant is an interface method reference to that exact interface and method is rewritten: the method is copied again as `default$` plus the original name, and the call becomes `INVOKEVIRTUAL` of that copy on the implementing class. The modified-class visitor is notified again.
  - [P3] The interface method then loses its `Code` attribute and gains `ACC_ABSTRACT`. An optional extra member visitor runs on that method. If any default method was seen, the interface constant pool is shrunk.

### `third_party/java/proguard/proguard6.2.2/src/proguard/backport/JSR310Converter.java`
- Bazel lines: 69
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No ClassVisitor that rewrites `java/time/**` types to `org/threeten/bp/<1>`, keeping the matched suffix (for example `java/time/LocalDate` and `java/time/chrono/Chronology`).
  - [P3] No method replacement that retargets every method on `java/time/**` (any name, any descriptor) to the same name and descriptor on `org/threeten/bp/<1>`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/backport/LambdaExpression.java`
- Bazel lines: 228
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No record of a lambda or method reference from a class file: referenced class, bootstrap-method index and info, factory descriptor, implemented interfaces, extra bridge descriptors, interface method name and descriptor, invoked reference kind, class, method name, and descriptor, plus the resolved invoked class and method. The converted lambda class is left unset.
  - [P3] No converted class name of the form `<referenced class name>$$Lambda$<bootstrap method index>`.
  - [P3] No constructor descriptor: `()V` when the factory descriptor has zero parameters; otherwise the factory descriptor through its first `)`, then `V`.
  - [P3] No serializable check that is true when any implemented interface is `java/io/Serializable`.
  - [P3] No method-reference check that is true when the invoked method name does not start with `lambda$`.
  - [P3] No stateless check that is true when the factory descriptor has zero parameters.
  - [P3] No static-interface invocation check: `invokeStatic`, a resolved invoked class, and `ACC_INTERFACE`. An unresolved class is treated as not an interface.
  - [P3] No private-synthetic interface-method check: the invoked class has `ACC_INTERFACE`, and the invoked method has `ACC_PRIVATE` or `ACC_SYNTHETIC`. It does not require both flags or a non-static method, and it assumes both the class and the method are resolved.
  - [P3] No accessor-needed check: the invoked class is resolved and that method is not visible from the lambda class to the invoked class. An unresolved class is treated as not needing an accessor.
  - [P3] No private-constructor reference check: `newInvokeSpecial`, method name `<init>`, and `ACC_PRIVATE` on the resolved invoked method.

### `third_party/java/proguard/proguard6.2.2/src/proguard/backport/LambdaExpressionCollector.java`
- Bazel lines: 243
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No class visitor fills a caller-supplied map from bootstrap-method index to a lambda expression. Library classes are ignored, and only `CONSTANT_InvokeDynamic` entries of a program class are scanned.
  - [P3] A bootstrap method is collected only when its method-handle class name is exactly `java/lang/invoke/LambdaMetafactory`. Every other bootstrap method is skipped.
  - [P3] Nothing builds that record from the invokedynamic name and factory descriptor, the factory return type as the sole interface, bootstrap argument 0 as the SAM method type, and bootstrap argument 1 as the invoked method handle (reference kind, owner, name, and descriptor). The referenced class and member are recorded only when that handle’s reference is a methodref; otherwise they stay unset.
  - [P3] Nothing handles an `altMetafactory` bootstrap: the integer flags at argument 3, then from argument 4 marker interfaces (`FLAG_MARKERS`), bridge method descriptors (`FLAG_BRIDGES`), and an appended `java/io/Serializable` (`FLAG_SERIALIZABLE`). A plain metafactory keeps bridges empty and interfaces to that single return type.
  - [P3] The map key is the bootstrap-method index, so a later invokedynamic constant that uses the same index replaces the earlier entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/backport/LambdaExpressionConverter.java`
- Bazel lines: 1033
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No visitor turns invokedynamic lambdas in program classes into synthetic classes that extend `java/lang/Object` and implement the lambda interfaces, then adds each class to the program class pool and the injected class-name map and optionally runs an extra class visitor. Library classes are ignored.
  - [P3] Matching invokedynamic sites are not rewritten. A stateless lambda becomes `getstatic INSTANCE`. A capturing lambda spills constructor arguments into temporaries starting at `maxLocals`, then emits `new`/`dup`/loads/`invokespecial <init>`. The single Category-1 argument `dup_x1`/`swap` path is compiled out (`parameterSize == 1 && false`).
  - [P3] After conversion, nothing initializes each lambda class’s super hierarchy, sub-hierarchy, and constant-pool references against the program and library pools.
  - [P3] Private static synthetic `$deserializeLambda$(Ljava/lang/invoke/SerializedLambda;)Ljava/lang/Object;` hooks are not removed once lambdas have been converted.
  - [P3] A private synthetic interface method referenced from a default method is not made package-private, and a non-static one is not made static by prepending the interface type and switching the reference kind to `invokeStatic`, in place of an accessor.
  - [P3] No synthetic static `accessor$<shortClass>$lambda<bootstrapIndex>` is added for a private constructor or another inaccessible target. Constructor accessors change the return type to the created type and emit `new`/`dup` before forwarding; instance methods prepend the receiver type. The lambda is not then retargeted to `invokeStatic` on that accessor.
  - [P3] No stateless lambda class is built with public static final `INSTANCE`, a public `<init>` that calls `Object.<init>`, a `<clinit>` that stores the singleton, a private `readResolve` when the lambda is serializable, and a public interface method that adapts arguments and the return value.
  - [P3] No capturing lambda class is built with private final `arg$N` fields, a constructor that stores the captures, and an interface method that loads those fields (skipping a cast on the receiver of `invokevirtual`/`invokeinterface`) and then the method parameters.
  - [P3] No public synthetic bridge methods cast each argument and the return value and `invokevirtual` the real interface method, skipping a descriptor that already exists on the class.
  - [P3] No stack conversion boxes a primitive with `valueOf`, unboxes `java/lang/Object` through `java/lang/Number` for int/long/float/double and through the specific wrapper for byte/char/short/boolean, or `checkcast`s unequal class or array types except when the target is `java/lang/Object`. The invoke kinds covered are `invokeStatic`, static interface, `invokeVirtual`, `invokeInterface`, `invokeSpecial`, and `newInvokeSpecial`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/backport/StaticInterfaceMethodConverter.java`
- Bazel lines: 293
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No visitor moves static methods off a visited program class into a public synthetic `<internal-name>$$Util` (version 1.2, major 46, superclass `java/lang/Object`, flags `ACC_PUBLIC|ACC_SYNTHETIC` only) whose private `()V` constructor is `aload_0`, `invokespecial java/lang/Object.<init>()V`, `return`, then adds that class to the program pool.
  - [P3] Library classes are left untouched. `<init>` and `<clinit>` stay. Every other `ACC_STATIC` method is copied onto `$$Util` with the same name, descriptor, flags, and body, then removed from the source class, and that class’s constant pool is shrunk. There is no interface or class-version test in this visitor. If an extra member visitor is set, it sees each original method after the copy and before the removal mark.
  - [P3] The source internal name is not recorded as mapping to the `$$Util` name, and the new class’s superclass hierarchy and references are not initialized from the program and library pools.
  - [P3] `invokestatic` of an interface methodref for each moved name and descriptor is not rewritten to `invokestatic` of a methodref on `$$Util`. Only program classes that already have a ref constant whose referenced class is the visited class are scanned, and the modified-class callback is not run once per rewritten instruction.

### `third_party/java/proguard/proguard6.2.2/src/proguard/backport/StreamSupportConverter.java`
- Bazel lines: 321
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No class visitor that rewrites Java 8 stream and function APIs onto the streamsupport `java8/` library, given a program pool, library pool, warning printer, modified-class visitor, and extra instruction visitor.
  - [P3] No type renames: `java/util/stream/**` and `java/util/function/**` to `java8/util/.../<1>`; `DoubleSummaryStatistics`, `IntSummaryStatistics`, `LongSummaryStatistics`, `PrimitiveIterator**`, `Optional**`, `Spliterator**`, `SplittableRandom`, and `StringJoiner` to the matching `java8/util` types; `CompletionException`, `CountedCompleter`, `ForkJoinPool`, `ForkJoinTask`, `ForkJoinWorkerTask`, `Phaser`, `RecursiveAction`, `RecursiveTask`, and `ThreadLocalRandom` to `java8/util/concurrent`; `java/lang/FunctionalInterface` to `java8/lang/FunctionalInterface`.
  - [P3] No method rewrites for `Collection.stream`, `parallelStream`, `spliterator`, and `removeIf` onto `StreamSupport` / `Spliterators` / `Iterables`, or for `Iterable.forEach` and `Iterable.spliterator` onto `java8/lang/Iterables` (the latter keeps the source descriptor `()Ljava/util/stream/Stream;`).
  - [P3] No wildcard rewrites of `List` and `Map` default methods, `Map$Entry` static methods, and `Comparator` static and default methods onto `java8/util/Lists`, `Maps`, and `Comparators`.
  - [P3] No wildcard rewrites of every method on the summary-statistics, `PrimitiveIterator`, `Optional`, `Spliterator`, `SplittableRandom`, and `StringJoiner` types, of default and static methods on the listed `java/util/function` interfaces (`BiConsumer` through `UnaryOperator`), or of static methods on `DoubleStream`, `IntStream`, `LongStream`, `Stream`, and `Collector`, followed by a catch-all for remaining `java/util/stream/**` and `java/util/function/**` methods.
  - [P3] No rewrite of `Iterator.forEachRemaining` onto `java8/util/Iterators`, of `ConcurrentMap` default methods onto `ConcurrentMaps`, or of every method on the listed `java/util/concurrent` classes (including a second identical `ForkJoinPool` rule).
  - [P3] No `ThreadLocalRandom` `ints` / `longs` / `doubles` rewrites (name kept, descriptor wildcarded) plus a catch-all for its remaining methods, and no `Arrays` `spliterator`, `stream`, `parallel**`, and `set**` rewrites onto `java8/util/J8Arrays`.
  - [P3] No rewrites of the listed `Integer`, `Long`, and `Double` methods (`min`, `max`, `sum`, compare/unsigned arithmetic, `hashCode`, and `Double.isFinite`) onto `java8/lang/Integers`, `Longs`, and `Doubles`.
  - [P3] No missing-replacement markers for `java/util/Random` `ints`, `longs`, and `doubles`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/backport/StringConcatenationConverter.java`
- Bazel lines: 318
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bytecode pass that rewrites `invokedynamic` bootstrapped on `java/lang/invoke/StringConcatFactory` into a `StringBuilder` append chain.
  - [P3] Missing recipe handling: `\u0001` spills the dynamic argument and calls a type-specific `append` (boolean, char, byte/short/int as int, long, float, double, String, otherwise Object) while sizing the builder from typical printed lengths; `\u0002` loads the bootstrap constant and appends it as String or Object (non-strings add 16 to the estimate); other characters are emitted as a literal `append(String)`.
  - [P3] Missing the replacement sequence `new StringBuilder` / `dup` / `pushInt(estimatedLength)` / `<init>(int)` / appends / `toString()`, and the optional extra visit of the original `invokedynamic` after the edit is recorded.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/ClassConstants.java`
- Bazel lines: 553
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No class-file container prefixes (`classes/` for WAR and JMOD), JMOD header `JM` plus bytes `1, 0`, `.class` extension, or magic `0xCAFEBABE`.
  - [P3] No packed class versions 1.0 through 13: majors 45–57, minor 3 for 1.0 and 0 otherwise, combined as `(major << 16) | minor`.
  - [P3] No JVM access-flag bits, including the overlapping values for super/synchronized/open/transitive, volatile/bridge, transient/varargs, and mandated/module, and no valid-flag masks for class, field, method, parameter, module, requires, exports, and opens. ProGuard’s extra flags for renamed members and removed methods or fields are also absent.
  - [P3] No constant-pool tags (standard tags plus primitive-array tag 21), method-handle reference kinds 1–9, or class-member reference flags (bridges 4, markers 2, serializable 1).
  - [P3] No attribute names, type-annotation target tags, module resolution warning flags, or element-value tags (`s`, `e`, `c`, `@`, `[`).
  - [P3] No descriptor punctuation, well-known class, method, and field names and descriptors (reflection, serialization, boxing, `StringBuilder`/`StringBuffer`, and enum helpers), primitive and generic type characters, typical buffer sizes, or maximum toString lengths used to size buffers.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/ClassPool.java`
- Bazel lines: 204
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No sorted name-to-class map (TreeMap) of `Clazz` representations, so insertion, removal, lookup, and iteration stay deterministic by class name.
  - [P3] No `clear`, `addClass` (put by `getName`, replacing an existing entry), `removeClass` by `Clazz` or by name, `getClass` (null when absent), `classNames` iterator, or `size`.
  - [P3] No `accept(ClassPoolVisitor)` that calls `visitClassPool` on this pool.
  - [P3] No `classesAccept` that visits every class via `clazz.accept`. The unsorted and alphabetical entry points both walk the same sorted map.
  - [P3] No filtered `classesAccept`: a string or list filter parsed by `ListParser`/`ClassNameParser`, or a `StringMatcher`, visiting only classes whose names match.
  - [P3] No `classAccept` that visits a single named class only when that class is present.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/Clazz.java`
- Bazel lines: 266
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No ProGuard class representation exposing access flags, the internal class name, the superclass name (null for `java.lang.Object`), the interface count, and interface names by index.
  - [P3] No constant-pool reads by index: tag, Utf8 string, String constant's string, Class constant's class name, NameAndType name and type, and Ref constant class name, name, and type.
  - [P3] No subclass registration, resolved superclass, or resolved interface lookup.
  - [P3] No `extends` check where a class always extends itself and an interface extends only the root Object class, by resolved class or by name.
  - [P3] No `extendsOrImplements` check where a class always implements itself and an interface implements all of its superinterfaces, by resolved class or by name.
  - [P3] No field or method lookup by name and descriptor.
  - [P3] No visitor walks of this class, its hierarchy (this class, superclasses, interfaces, subclasses), known subclasses, every constant-pool entry or one index, the this-class constant, the superclass constant when present, interface constants, all or one field, all or one method, and all or one named attribute.
  - [P3] No `mayHaveImplementations` test that is true only when the class is not final and the method is not private, static, final, or a constructor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/Field.java`
- Bazel lines: 32
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `Field` is an empty marker interface extending `Member`. It adds no methods. Its behavior is the type constraint that a class field is a member: access flags, name, descriptor, `accept(Clazz, MemberVisitor)`, and `referencedClassesAccept(ClassVisitor)`, distinct from a method.
  - [P3] Zel has no ProGuard class-file model and no field-versus-method member type.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/JavaConstants.java`
- Bazel lines: 99
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard Java source-form constant table: `.java`, class-version strings `1.0`–`1.9` with aliases `5`–`9` and versions `10`–`13`, access-flag keywords (`public` through `transitive`, annotation as `@`), separators (`.` for package and inner class, `-`, `$`, `(`, `)`, `,`), `java.lang.Object` and the `java.lang.` prefix, and type names `void`, `boolean`, `byte`, `char`, `short`, `int`, `float`, `long`, `double`, and `[]`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/LibraryClass.java`
- Bazel lines: 553
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no compact library class holding access flags, class name, superclass name, interface names, library fields and methods, plus later-filled superclass, interface, and subclass links and visitor info. Public visibility is the `ACC_PUBLIC` flag. Interface count is the length of the resolved interface array, while interface names come from the name array. A missing superclass name is still returned as stored.
  - [P3] Zel has no constant-pool reads for this kind of class. Tag, string, class, name, type, and reference lookups throw because the class stores no constant pool. Constant-pool, this-class, superclass, and interface constant visitors do nothing. Attribute visitors throw because attributes are not stored.
  - [P3] Zel has no subclass list that grows by one and appends, and no superclass or interface getters. `extends` is true for this class or a superclass chain, using object identity for a class argument and the class name for a string, and it does not walk interfaces. `extendsOrImplements` also walks each non-null interface. Field and method lookup is a linear scan of this class only, skipping null members, with a null name or descriptor matching any value.
  - [P3] Zel has no library-class visitor or hierarchy walk that visits this class, then the superclass (including its interfaces when requested, but not its subclasses), then direct interfaces and their superinterfaces, then subclasses only. Separate visitors cover the superclass, non-null interfaces, and subclasses. Field and method visitors skip null members, and named visitors visit only the first match.
  - [P3] Zel has no check that a library class may have implementations: the class is not final, and the method is absent or is not private, static, final, or `<init>`. Its string form is `LibraryClass(<name>)`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/LibraryField.java`
- Bazel lines: 77
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No library class-file field: access flags, name, and descriptor are not stored, and there is no referenced-class slot later filled from the descriptor (primitive types left unset).
  - [P3] No member visitor dispatch that hands the owning library class and this field to `visitLibraryField`.
  - [P3] No `referencedClassesAccept`: a non-null referenced class is not visited, and a null reference is not skipped.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/LibraryMember.java`
- Bazel lines: 104
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no library field or method record holding access flags, name, descriptor, and a visitor-info slot, and no constructors that leave visitor info unset while optionally initializing the other three.
  - [P3] Zel has no member accessors that return the stored flags and that return the stored name and descriptor while ignoring the class argument.
  - [P3] Zel has no accept path that requires the class to be a library class (a failed cast otherwise) and then dispatches that class plus this member to a member visitor.
  - [P3] Zel has no get and set of arbitrary per-member visitor info.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/LibraryMethod.java`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no library-method member that stores access flags, name, and descriptor together with a `referencedClasses` array (one slot per class in the descriptor, null for an unknown class, primitives and arrays of primitives omitted).
  - [P3] Zel has no member-visitor dispatch that visits this object as a library method of its library class (`visitLibraryMethod`).
  - [P3] Zel has no referenced-class walk that does nothing when that array is unset and otherwise accepts only the non-null class references.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/Member.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no field or method member that exposes access flags, a name and descriptor resolved through the owning class, acceptance of a member visitor for that class, and a walk of the classes named in the descriptor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/Method.java`
- Bazel lines: 32
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file method type. Bazel’s `Method` is an empty marker interface that extends `Member` and adds no methods of its own, so a method is only a member distinguished from other members while inheriting access flags, name, descriptor, member-visitor accept, and referenced-class accept.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/ProgramClass.java`
- Bazel lines: 656
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no in-memory Java class record (version, constant pool, access flags, this/super/interfaces, fields, methods, attributes, subclass list, visitor info) and no constructors that seed empty interface/field/method/attribute arrays.
  - [P3] Constant-pool reads are absent: index lookup, tag, and typed getters for Utf8, String, Class, NameAndType, and Ref constants that throw `IllegalStateException` (with the class name in the message except for `getClassName`) when the entry has the wrong type.
  - [P3] Hierarchy behavior is absent: `addSubClass` grows the subclass array by one; `getSuperClass` and `getInterface` follow referenced classes; `extends_` walks superclasses only (self matches); `extendsOrImplements` also walks interfaces and skips a null interface.
  - [P3] Member lookup is absent: `findField` and `findMethod` return the first field or method whose name and descriptor match, treating a null name or descriptor as a wildcard.
  - [P3] Visitor walks are absent: class, constant-pool (skipping index 0 and null entries), this/super/interface constants, fields, methods, and attributes, including name-filtered attribute visits and single member visits that no-op when the member is missing.
  - [P3] `hierarchyAccept` is absent. It visits this class, then the superclass (passing `visitInterfaces` through and not subclasses), then interfaces (and, when the superclass was not visited, the superclass's interfaces only), then subclasses without their supers or interfaces.
  - [P3] `mayHaveImplementations` is absent. It is false when the class is final, or when the method is private, static, final, or named `<init>`. A null method is allowed when the class is not final.
  - [P3] `toString` as `ProgramClass(<name>)` is absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/ProgramField.java`
- Bazel lines: 113
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No program-class field stores access flags, constant-pool name and descriptor indexes, an attribute count and array, visitor info, and the `Clazz` named by the descriptor. Primitive descriptors stay a null reference.
  - [P3] No uninitialized field (referenced class null, flags and indexes 0, attributes null). No attribute-less field (count 0, one shared zero-length attribute array, referenced class stored, including null). No full field that keeps the given count and attribute array by reference and stores the referenced class.
  - [P3] No member accept that dispatches only to `visitProgramField(programClass, this)`.
  - [P3] No attribute walk over `attributes[0, u2attributesCount)`. A shorter count skips the tail. A longer count or a null slot throws. A count of 0 does not touch the array, so a null array is safe.
  - [P3] No referenced-class visit that calls `referencedClass.accept` only when that reference is non-null. Classes named from attributes are not visited here.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/ProgramMember.java`
- Bazel lines: 140
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no program-class field or method model holding access flags, constant-pool name and descriptor indexes, an attribute array with a separate count, and per-member visitor info.
  - [P3] There is no first-attribute-by-name scan, no member or attribute visitor accept, and no generic accept that casts the class to a program class before dispatch.
  - [P3] ProGuard in Zel is only build options and an external tool (`proguard_top`, allowlister), which does not implement this classfile member behavior.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/ProgramMethod.java`
- Bazel lines: 121
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no program-method object. Bazel stores a public `referencedClasses` array (one `Clazz` per class reference in the method descriptor, null for an unknown class, primitives and primitive arrays omitted by the initializer) plus the `ProgramMember` access-flag, name-index, descriptor-index, and attribute fields. The no-arg constructor leaves `referencedClasses` null. The no-attribute constructor stores the given array by reference and uses a shared empty attribute array with count 0. The full constructor stores both the attribute array and `referencedClasses` by reference.
  - [P3] `accept` only calls `MemberVisitor.visitProgramMethod(programClass, this)`. It does not walk attributes or referenced classes.
  - [P3] `attributesAccept` visits `attributes[0..u2attributesCount)` through `Attribute.accept(programClass, this, attributeVisitor)`, so the visitor sees this method. The bound is the count, not `attributes.length`. A null array or element, or a count past the array, throws. A count of 0 visits nothing.
  - [P3] `referencedClassesAccept` does nothing when the array is null. Otherwise it calls `Clazz.accept` on each non-null entry in index order and skips null slots.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/VisitorAccepter.java`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor-accepter contract that stores exactly one untyped temporary object on a visited class-file entity. Bazel `getVisitorInfo` returns that object and `setVisitorInfo` replaces it, so a later visitor overwrites any earlier value that is still needed.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/Attribute.java`
- Bazel lines: 143
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file attribute base that stores a constant-pool name index and resolves it through the owning class's string lookup.
  - [P3] Zel has no visitor-info slot that visitors can read and replace on an attribute.
  - [P3] Zel has no accept methods that delegate to the narrower accept when the field, method, or code attribute is null, and otherwise fail because the concrete attribute type did not override the method.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/BootstrapMethodInfo.java`
- Bazel lines: 100
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bootstrap-method record holding a constant-pool method-handle index, an argument count, and the argument constant-pool indices.
  - [P3] Zel has no visitor that accepts that method-handle constant, or that walks `0..u2methodArgumentCount-1` and accepts each `u2methodArguments` entry (the count, not the array length, is the bound).
  - [P3] Zel has no visitor-info slot that callers can get and set on the bootstrap method.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/BootstrapMethodsAttribute.java`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file attribute that stores a bootstrap-methods count and `BootstrapMethodInfo` entries, or that is constructed with an attribute-name index plus those fields.
  - [P3] Zel has no `accept` dispatch to `visitBootstrapMethodsAttribute`.
  - [P3] Zel has no walk of every bootstrap method entry in `0..count`, and no visit of a single entry by index (unchecked, so an out-of-range index throws).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/CodeAttribute.java`
- Bazel lines: 242
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no JVM Code attribute that stores a name index, max stack, max locals, code length, code bytes, an exception table, and nested attributes.
  - [P3] No first-match lookup of a nested attribute by name.
  - [P3] No instruction walk over an offset range that advances by each instruction's length, and no single-offset instruction dispatch.
  - [P3] No exception-table walks for every entry, for one instruction offset, or for an offset range.
  - [P3] No visitor dispatch over the code attribute or its nested attributes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/ConstantValueAttribute.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no field attribute that stores a public constant-pool index (`u2constantValueIndex`) alongside the inherited attribute-name index. Bazel can construct it uninitialized, leaving both indexes at 0, or initialize it with an attribute-name index and a constant-value index.
  - [P3] Zel has no field-context `accept` that always forwards to `visitConstantValueAttribute(clazz, field, this)`, including when `field` is null. Class-level `accept` is not overridden and throws `UnsupportedOperationException` naming this class. Method-level and code-level `accept` are not overridden, so they keep `Attribute`'s null delegation or that same exception.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/DeprecatedAttribute.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No JVM class-file attribute type that is only a Deprecated marker with a constant-pool attribute-name index and no extra payload.
  - [P3] No class, field, or method visitor dispatch that selects a Deprecated-attribute visit and omits a code-attribute scope.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/EnclosingMethodAttribute.java`
- Bazel lines: 132
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no enclosing-method attribute that stores a constant-pool class index and a name-and-type index, plus optional resolved class and method references that both constructors leave unset.
  - [P3] Zel does not look up the enclosing class name from the class index, or the enclosing member name and descriptor from the name-and-type index, including when that index is zero.
  - [P3] Zel does not visit the resolved enclosing class only when it is non-null, or visit the resolved method with that class only when the method is non-null (the class reference is not checked).
  - [P3] Zel does not dispatch an attribute visitor to an enclosing-method visit.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/ExceptionInfo.java`
- Bazel lines: 99
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no exception-table entry with mutable `u2startPC`, `u2endPC`, `u2handlerPC`, and `u2catchType`, and no zero-default or fully initialized constructors for those fields.
  - [P3] Zel has no point-in-try test (`instructionOffset >= u2startPC && instructionOffset < u2endPC`) and no overlap test (`u2startPC < endOffset && u2endPC > startOffset`).
  - [P3] Zel has no `VisitorAccepter` slot that stores and returns an arbitrary visitor object on the entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/ExceptionsAttribute.java`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No model stores an Exceptions attribute name index, `u2exceptionIndexTableLength`, and the `u2exceptionIndexTable` constant-pool indices.
  - [P3] No method-attribute accept path dispatches to a visitor with the owning class, method, and this attribute.
  - [P3] No walk applies a constant-pool visitor to each exception-class entry from index 0 through `u2exceptionIndexTableLength - 1`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/ExtendedLineNumberInfo.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No line-number table entry that stores a source string alongside start PC and line number, for inlined or merged code that class files cannot record.
  - [P3] No source lookup that returns that string. The base entry reports no distinct source.
  - [P3] No uninitialized entry, and no constructor that sets start PC, line number, and source together.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/InnerClassesAttribute.java`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no InnerClasses attribute value holding `u2classesCount` and an `InnerClassesInfo` array, and no constructor that stores those with the attribute name index.
  - [P3] Zel has no `accept` dispatch to `visitInnerClassesAttribute(clazz, this)`.
  - [P3] Zel has no walk of entries from index 0 through `u2classesCount` that calls `visitInnerClassesInfo` on each entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/InnerClassesInfo.java`
- Bazel lines: 94
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no InnerClasses attribute entry holding an inner-class constant-pool index, an outer-class constant-pool index, an inner-name Utf8 index, and inner-class access flags.
  - [P3] Zel does not skip a constant-pool visit when the inner-class, outer-class, or inner-name index is 0, and otherwise dispatch a constant visitor to that pool entry.
  - [P3] Zel has no visitor-info slot that visitors can read and replace on the entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/LineNumberInfo.java`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no line-number table entry holding a start PC and a line number.
  - [P3] There is no default construction that leaves both fields at zero, and no two-argument construction that stores them.
  - [P3] There is no source accessor that returns null so a standard entry is treated as belonging to the class source file.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/LineNumberTableAttribute.java`
- Bazel lines: 174
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no JVM `LineNumberTable` attribute: no name-index plus length-prefixed `(start_pc, line_number)` entries.
  - [P3] No reverse scan that returns the last entry with `start_pc <= pc`, the first entry when `pc` precedes every start, or null when the table is empty.
  - [P3] No line lookup that returns that entry’s line number, or `0` when the lookup is null, and no source lookup that returns that entry’s source, or null.
  - [P3] No lowest/highest line scan limited to entries whose source is null (lowest is `0` when none match; highest stays `0` when none are greater than `0`).
  - [P3] No attribute visitor dispatch and no walk that visits every line-number entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/LocalVariableInfo.java`
- Bazel lines: 135
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No local-variable table entry holding start PC, length, name index, descriptor index, and slot index.
  - [P3] No name or descriptor lookup from those constant-pool indexes.
  - [P3] No referenced-class visitor dispatch when a descriptor class has been resolved.
  - [P3] No per-entry visitor-info slot.
  - [P3] No ordering by start PC, then slot index, then length, then descriptor index, then name index.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/LocalVariableTableAttribute.java`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file attribute that stores a constant-pool name index, a `u2localVariableTableLength`, and a table of local-variable entries.
  - [P3] There is no `accept` dispatch to a local-variable-table attribute visitor with the owning class, method, and code attribute.
  - [P3] There is no `localVariablesAccept` walk over `[0, u2localVariableTableLength)` that visits each `LocalVariableInfo` using that length field.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/LocalVariableTypeInfo.java`
- Bazel lines: 143
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no local-variable type table entry holding start PC, length, name index, signature index, and local slot, plus an optional list of classes referenced from the signature.
  - [P3] Zel does not resolve that entry's name and signature by asking the owning class for the constant-pool string at the stored index.
  - [P3] Zel does not apply a class visitor to each non-null referenced class, and does not skip the walk when the referenced-class array is null or an entry is null.
  - [P3] Zel has no per-entry visitor-info slot that visitors can read and replace.
  - [P3] Zel does not order two entries by start PC, then local index, then length, then signature index, then name index. Referenced classes and visitor info are not part of that order. Comparing with a non-entry throws ClassCastException.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/LocalVariableTypeTableAttribute.java`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no local-variable type table attribute that stores a u2 entry count and an array of local-variable type infos, with an uninitialized constructor and an initialized constructor that records the attribute name index.
  - [P3] Zel has no attribute visitor dispatch that, given a class, method, and code attribute, visits this local-variable type table.
  - [P3] Zel has no walk of local-variable type entries that calls a local-variable type visitor once per index in `0..u2localVariableTypeTableLength`, using that length field as the bound and performing no further subtype dispatch.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/MethodParametersAttribute.java`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no MethodParameters attribute: nothing stores `u1parametersCount` and a `ParameterInfo` table, nothing records them with the attribute name index, and nothing dispatches `accept` to `visitMethodParametersAttribute`.
  - [P3] Nothing walks indices `0..u1parametersCount` and calls `visitParameterInfo` with the class, method, index, and that parameter.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/NestHostAttribute.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no nest-host class attribute that stores the constant-pool class index of the nest host (`u2hostClassIndex`) together with the attribute name index.
  - [P3] Zel has no attribute visitor dispatch that sends this attribute to `visitNestHostAttribute(clazz, this)`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/NestMembersAttribute.java`
- Bazel lines: 78
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no NestMembers class-file attribute storing `u2classesCount` and the constant-pool indices of the nest member classes.
  - [P3] Zel has no `accept` dispatch to `visitNestMembersAttribute`.
  - [P3] Zel has no `memberClassConstantsAccept` walk that visits each member-class index in `u2classes[0..u2classesCount)` through the class constant pool.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/ParameterInfo.java`
- Bazel lines: 95
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No MethodParameters entry holding a constant-pool name index and parameter access flags.
  - [P3] No name lookup through the class constant pool, and no visit of that Utf8 constant that skips index 0.
  - [P3] No visitor-info slot on the parameter record.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/SignatureAttribute.java`
- Bazel lines: 111
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Signature attribute value holding a constant-pool signature index and an array of classes referenced by that signature, with null entries for unknown classes.
  - [P3] No signature-string lookup through the owning class constant pool.
  - [P3] No walk of referenced classes that skips a null array and null entries, then accepts a class visitor on each remaining class.
  - [P3] No `accept` dispatch to `visitSignatureAttribute` for class, field, and method contexts.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/SourceDirAttribute.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file attribute that stores a public constant-pool index (`u2sourceDirIndex`) beside the inherited attribute-name index. An empty constructor leaves both indexes at 0. The other constructor sets the name index and the source-directory index.
  - [P3] Zel has no class-level `accept` that always calls `visitSourceDirAttribute(clazz, this)`. Field, method, and code `accept` stay on `Attribute`: a null field, method, or code attribute falls through to that class visit, and a non-null one throws `UnsupportedOperationException` naming `proguard.classfile.attribute.SourceDirAttribute`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/SourceFileAttribute.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no SourceFile class-file attribute that stores a constant-pool source-file index together with the inherited attribute-name index.
  - [P3] Zel has no uninitialized or initialized construction of that attribute from an attribute-name index and a source-file index.
  - [P3] Zel has no accept path that dispatches the owning class and this attribute to a source-file attribute visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/SyntheticAttribute.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Synthetic attribute object that can be left uninitialized or created with only a constant-pool attribute-name index.
  - [P3] Zel has no class-, field-, or method-scoped accept path that hands this attribute to a visitor as a Synthetic attribute.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/UnknownAttribute.java`
- Bazel lines: 84
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no model of an unrecognized JVM class-file attribute that keeps the name index, the u4 length, and the raw info bytes (including a constructor that leaves the bytes unset).
  - [P3] There is no visitor dispatch for that attribute on a class, field, method, or code attribute that calls through with only the class and the attribute, dropping the field, method, and code context.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/Annotation.java`
- Bazel lines: 143
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No annotation value holding a constant-pool type index, an element-value count, and an element-value array.
  - [P3] No lookup of the annotation type string from the owning class constant pool.
  - [P3] No visit of only the first referenced class when the referenced-class array and that entry are non-null.
  - [P3] No visit of every non-null referenced class.
  - [P3] No visit of the first `u2elementValuesCount` element values, each passed the owning class and this annotation.
  - [P3] No visitor-info get/set side channel.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/AnnotationDefaultAttribute.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No attribute model stores an AnnotationDefault attribute name index and a single default `ElementValue`.
  - [P3] No method-attribute accept path dispatches to `visitAnnotationDefaultAttribute` with the owning class, method, and this attribute.
  - [P3] No default-value walk calls `ElementValue.accept` with the owning class, a null annotation, and the element-value visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/AnnotationElementValue.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no element-value type that stores a nested annotation together with a constant-pool element-name index, including the uninitialized state where that annotation reference is null.
  - [P3] Zel does not return the annotation element-value tag (`@`) or dispatch an annotation visitor and an element-value visitor over that nested annotation.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/AnnotationsAttribute.java`
- Bazel lines: 114
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file annotations attribute that stores an annotation count and an annotation array.
  - [P3] There is no visitor walk over `[0, u2annotationsCount)` that dispatches each annotation for a class, a field, a method, or a method code attribute.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/ArrayElementValue.java`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel type stores an annotation array element value (`u2elementValuesCount` plus nested `ElementValue`s), including the no-arg uninitialized form and the form that records `u2elementNameIndex` with that count and array.
  - [P3] No zel tag accessor returns `'['` (`ELEMENT_VALUE_ARRAY`).
  - [P3] No zel `accept` dispatches to `visitArrayElementValue(clazz, annotation, this)`.
  - [P3] No zel `elementValuesAccept` walks nested values from index `0` through `u2elementValuesCount` (the count field, not the array length) and calls `accept` on each.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/ClassElementValue.java`
- Bazel lines: 104
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No class-typed annotation element value holding a constant-pool class-info index, tag `c`, and referenced classes (primitive type references left unset).
  - [P3] No lookup of that class-info Utf8 string from the owning class.
  - [P3] No walk of the referenced-class array that skips null entries.
  - [P3] No visitor dispatch for a class element value.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/ConstantElementValue.java`
- Bazel lines: 71
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No type stores a constant annotation element value as a tag plus a constant-pool index. The uninitialized form records only the tag (index defaults to 0); the initialized form also records the element-name index and constant-value index.
  - [P3] No tag accessor returns that stored tag, and no visitor accept path dispatches specifically to a constant-element-value visit.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/ElementValue.java`
- Bazel lines: 126
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No annotation element-value model stores an optional element-name index (0 when the value is not in a name/value pair), a referenced class, a referenced method, and visitor info.
  - [P3] No lookup of the element name from the owning class's string table via that name index.
  - [P3] No abstract tag or visitor accept for a value attached to an annotation.
  - [P3] No conditional accept on the referenced method that runs only when that method is present and passes the referenced class to the visitor.
  - [P3] No get/set of per-value visitor info.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/EnumConstantElementValue.java`
- Bazel lines: 138
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No annotation element value tagged `'e'` that stores constant-pool indices for the enum type name and the constant name, plus an optional referenced-class array and referenced field.
  - [P3] No type-name or constant-name lookup through the owning class string pool.
  - [P3] No walk of referenced classes that skips null entries and does nothing when the array is null.
  - [P3] No referenced-field visit that, when the field is set, applies a member visitor to that field using `referencedClasses[0]` as the class.
  - [P3] No element-value accept path that dispatches to `visitEnumConstantElementValue` with the owning class, annotation, and this value.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/ParameterAnnotationsAttribute.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no parameter-annotations attribute: nothing stores `u1parametersCount`, a per-parameter `u2parameterAnnotationsCount` array, and a `parameterAnnotations` matrix, and nothing records them with the attribute name index.
  - [P3] Nothing walks parameters `0..u1parametersCount` and, for each parameter, annotations `0..u2parameterAnnotationsCount[parameterIndex]`, calling `visitAnnotation` with the class, method, parameter index, and that annotation.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/RuntimeInvisibleAnnotationsAttribute.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No runtime-invisible annotations attribute that stores a name index, an annotation count, and the annotation list.
  - [P3] No class, field, or method `accept` dispatch that identifies this attribute as runtime-invisible, separate from runtime-visible annotations.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/RuntimeInvisibleParameterAnnotationsAttribute.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No runtime-invisible parameter-annotations attribute type that stores an attribute name index, a parameter count, per-parameter annotation counts, and the annotation arrays.
  - [P3] No method-attribute visitor dispatch that identifies this attribute as runtime-invisible parameter annotations.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/RuntimeInvisibleTypeAnnotationsAttribute.java`
- Bazel lines: 77
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no runtime-invisible type annotations attribute holding a name index, an annotation count, and a type-annotation array.
  - [P3] Zel has no visitor dispatch for that attribute on a class, a field, a method, or a method code attribute.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/RuntimeVisibleAnnotationsAttribute.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no runtime-visible annotations attribute that stores an attribute-name index, an annotation count, and an annotation array, either uninitialized or initialized through those three values.
  - [P3] Zel has no visitor dispatch that, given a class, a field, or a method, routes this attribute to a runtime-visible-annotations visit separate from other annotation attributes.
  - [P3] Accepting this attribute with a non-null code attribute is left to the base attribute, which throws `UnsupportedOperationException`. Zel has no corresponding accept path.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/RuntimeVisibleParameterAnnotationsAttribute.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no runtime-visible parameter-annotations attribute. Nothing stores the attribute name index, parameter count, per-parameter annotation counts, and annotation arrays, or dispatches `accept` to `visitRuntimeVisibleParameterAnnotationsAttribute`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/RuntimeVisibleTypeAnnotationsAttribute.java`
- Bazel lines: 77
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no runtime-visible type-annotations attribute that stores a name index, annotation count, and type-annotation array.
  - [P3] Zel has no class, field, method, or code visitor dispatch that selects the runtime-visible type-annotations visit.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/TypeAnnotation.java`
- Bazel lines: 162
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no type-annotation value that extends a normal annotation with a target-info object and a type-path array.
  - [P3] There is no visitor entry that forwards the annotation plus its enclosing class, field, method, or code attribute to a target-info visitor.
  - [P3] There is no walk of every type-path element in order. The code-attribute overload passes the caller-supplied type annotation into the visitor rather than the receiver.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/TypeAnnotationsAttribute.java`
- Bazel lines: 98
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no abstract type-annotations attribute that holds an attribute-name index, an annotation count, and a `TypeAnnotation` array.
  - [P3] There is no class-level accept that casts the stored annotations to `TypeAnnotation[]`, walks indices `0..u2annotationsCount-1`, and calls `visitTypeAnnotation(clazz, annotation)` with no double dispatch.
  - [P3] There is no field or method accept that performs the same counted walk and calls `visitTypeAnnotation` with the field or method.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/TypePathInfo.java`
- Bazel lines: 50
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no type-annotation path element that stores a path kind and a type-argument index, and no constructors that leave those fields at 0 or copy both values in.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/CatchTargetInfo.java`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no catch type-annotation target that stores a mutable exception-table index (`u2exceptionTableIndex`, default 0) together with the inherited target type (`u1targetType`).
  - [P3] Zel has no uninitialized constructor, no constructor that sets only the target type, and no constructor that sets both the target type and the exception-table index.
  - [P3] Zel has no class-context accept that visits the target with a null method and null code attribute, and no code-context accept that visits it with the supplied method and code attribute. Field-only and method-only accepts stay the base unsupported-target errors.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/EmptyTargetInfo.java`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no type-annotation target that stores only the inherited target-type byte and no extra target payload.
  - [P3] The no-arg constructor leaves that byte at 0; the one-arg constructor stores the given target type. Neither exists in Zel.
  - [P3] Class-context `accept` dispatches `visitEmptyTargetInfo` with a null field; field and method `accept` forward that member. Zel has no such visitor dispatch.
  - [P3] Code-context `accept` is not overridden, so it still throws the base unsupported-type-annotation error. Zel has no equivalent path.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/FormalParameterTargetInfo.java`
- Bazel lines: 81
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No model stores a formal-parameter type-annotation target type and `u1formalParameterIndex`.
  - [P3] No class-level accept path dispatches `visitFormalParameterTargetInfo` with the method set to null.
  - [P3] No method-level accept path dispatches `visitFormalParameterTargetInfo` with the owning class, method, type annotation, and this target.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/LocalVariableTargetElement.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no local-variable type-annotation target table entry with mutable `u2startPC`, `u2length`, and `u2index`, and no zero-default or three-argument constructor that stores those fields.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/LocalVariableTargetInfo.java`
- Bazel lines: 99
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no local-variable type-annotation target: no target type plus a `u2tableLength` and a table of start-PC, length, and local-index elements.
  - [P3] No walk that visits each table element for indices `0..u2tableLength` (the length field, not the array length).
  - [P3] No visitor dispatch that reports this target with a null method and code attribute, and no overload that forwards the real method and code attribute.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/OffsetTargetInfo.java`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no type-annotation offset target that stores a `u2` bytecode offset and forwards both `accept` overloads to `visitOffsetTargetInfo`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/SuperTypeTargetInfo.java`
- Bazel lines: 75
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no type-annotation supertype target that stores a target-type byte plus a supertype index (`u2superTypeIndex`).
  - [P3] Zel has no `65535` (`EXTENDS_INDEX`) sentinel for a superclass (`extends`) target, as opposed to an implemented-interface index.
  - [P3] Zel has no class-scoped accept that forwards this target to `visitSuperTypeTargetInfo`. Field, method, and code accepts stay the base unsupported path, and that target kind is absent entirely.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/TargetInfo.java`
- Bazel lines: 97
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no JVM type-annotation target model: no target-type byte, no getter for it, and no class, field, method, or code visitor `accept` methods that reject an unsupported target by throwing with that type formatted as hex.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/ThrowsTargetInfo.java`
- Bazel lines: 81
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No throws type-annotation target stores a `u2throwsTypeIndex` (exception index in the method throws clause) together with the inherited `u1targetType`.
  - [P3] No constructors for an uninitialized target, a target-type-only target, or a target with both the target type and the throws index (the index stays 0 until set).
  - [P3] No visitor dispatch: the class-level `accept` always calls `visitThrowsTargetInfo` with a null method, and the method-level `accept` forwards the given method. The base `TargetInfo` path would otherwise reject this target as unsupported.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/TypeArgumentTargetInfo.java`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no type-argument annotation target that stores a `u2` offset and a `u1` type-argument index on a target-type byte.
  - [P3] Zel has no `accept` path that visits that target with `Method` and `CodeAttribute` forced to null, and none that forwards the supplied method and code attribute.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/TypeParameterBoundTargetInfo.java`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No model stores a type-parameter-bound annotation target: the inherited target type plus `u1typeParameterIndex` and `u1boundIndex` (both 0 when left unset).
  - [P3] No accept path on a class, field, or method dispatches a visitor with the owning member, the type annotation, and this target.
  - [P3] A type annotation on code still rejects this target with an unsupported-operation error that includes the target type, and that path is also absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/TypeParameterTargetInfo.java`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No type-parameter annotation target storing a target-type byte and a type-parameter index.
  - [P3] No class-context accept that dispatches the target to a type-parameter visitor.
  - [P3] No method-context accept that dispatches the target to a type-parameter visitor.
  - [P3] Field and code type-annotation contexts stay unsupported and raise an unsupported-target error.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/visitor/LocalVariableTargetElementVisitor.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No visitor callback for one local-variable type-annotation target element. The only method is abstract, returns void, and takes the enclosing class, method, code attribute, type annotation, the local-variable target table, and one table element (start PC, length, and local index). It has no default body, filter, or error path.
  - [P3] Zel has no class-file type-annotation target model that dispatches that callback.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/target/visitor/TargetInfoVisitor.java`
- Bazel lines: 50
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file visitor for JVM type-annotation `target_info`. ProGuard’s `TargetInfoVisitor` is the callback contract for class type-parameter and supertype targets, class/field/method type-parameter-bound targets, field and method empty targets, method formal-parameter and throws targets, and code-attribute local-variable, catch, offset, and type-argument targets.
  - [P3] Each callback receives the owning class, the field, method, or `CodeAttribute` when the target is member- or code-scoped, the parent type annotation, and the concrete target. Zel never performs that dispatch, so those targets are not visited.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/visitor/AllAnnotationVisitor.java`
- Bazel lines: 161
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that holds an `AnnotationVisitor` and forwards every annotation on the attributes it visits, keeping the owner context (class, field, method, method plus code, or parameter index).
  - [P3] Zel does not walk runtime-visible and runtime-invisible annotations on a class, field, or method and call `visitAnnotation` once per entry in declaration order, using the class-only, class-plus-field, or class-plus-method overload.
  - [P3] Zel does not walk both runtime-visible and runtime-invisible parameter annotations, looping parameters then annotations, and call `visitAnnotation` with the parameter index.
  - [P3] Zel does not walk runtime-visible and runtime-invisible type annotations in the class, field, method, and code-attribute contexts, passing the code attribute only for the code-attribute overload.
  - [P3] Zel has no empty fallback that ignores every other attribute, including `AnnotationDefault`, so those attributes are not left unvisited.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/visitor/AllElementValueVisitor.java`
- Bazel lines: 206
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that walks runtime-visible, runtime-invisible, parameter, and annotation-default attributes, then every element value of the annotations they contain, and delegates each value to an element-value visitor. Other attribute kinds are ignored.
  - [P3] Zel has no shallow/deep switch. When deep, a nested annotation is visited again by the same visitor, while an array’s children are passed straight to the delegate (so deep recursion does not continue through those children).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/visitor/AnnotationToAnnotatedClassVisitor.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No annotation visitor forwards each visited annotation to a class visitor on that annotation’s owning class.
  - [P3] No consecutive-class filter skips the class visitor when the owning class equals the immediately previous visited class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/visitor/AnnotationToAnnotatedMemberVisitor.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no annotation visitor that, for a field, method, method-parameter, or code annotation, calls `member.accept(clazz, memberVisitor)` and drops the annotation, parameter index, and code attribute.
  - [P3] Zel has no consecutive-member suppression: a member equal to the previous one (`Member.equals`, identity) is not visited again, while a later visit after a different member is visited again. That remembered member is not cleared between classes.
  - [P3] Zel has no class-level `visitAnnotation(Clazz, Annotation)` path that throws `UnsupportedOperationException` because only the member overload is implemented.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/visitor/AnnotationTypeFilter.java`
- Bazel lines: 145
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No annotation visitor that forwards a visit only when the annotation type string (`Annotation.getType`, the constant-pool Utf8 at `u2typeIndex`) matches a `StringMatcher`.
  - [P3] No constructors that store a prebuilt `StringMatcher`, or that parse a class-name expression with `ListParser`/`ClassNameParser`. The constructor that also takes a mutable wildcard-matcher list ignores that list and does not record wildcard matches.
  - [P3] No class, field, method, method-parameter, or code-attribute visit that keeps the original receiver and parameter index and drops the visit when the type does not match.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/visitor/AnnotationVisitor.java`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No callback visits a class-level annotation with the owning class and the annotation.
  - [P3] No callback visits a field annotation with the owning class, field, and annotation.
  - [P3] No callback visits a method annotation with the owning class, method, and annotation.
  - [P3] No callback visits a parameter annotation with the owning class, method, formal-parameter index, and annotation.
  - [P3] No callback visits a code-attribute annotation with the owning class, method, code attribute, and annotation.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/visitor/ElementValueVisitor.java`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never dispatches an annotation element value to a kind-specific callback.
  - [P3] There is no visit for a constant element value given the enclosing class and annotation.
  - [P3] There is no visit for an enum-constant element value given the enclosing class and annotation.
  - [P3] There is no visit for a class element value given the enclosing class and annotation.
  - [P3] There is no visit for a nested annotation element value given the enclosing class and annotation.
  - [P3] There is no visit for an array element value given the enclosing class and annotation.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/visitor/MultiAnnotationVisitor.java`
- Bazel lines: 108
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no annotation visitor that forwards each of the five visits (class, field, method, method parameter, and code attribute) to every registered delegate, in registration order, for indices `0..count` (the live count, not the array length). An exception from one delegate skips the rest of that visit.
  - [P3] The empty constructor allocates a 16-slot list with count 0, so visits do nothing until a visitor is added. The varargs constructor keeps the given array and sets the count to its length. Zel has neither.
  - [P3] Adding a visitor appends it at the current count and grows the backing array when that index does not fit. Zel has no such registry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/visitor/TypeAnnotationVisitor.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no type-annotation visitor with the four visit callbacks: class only, class plus field, class plus method, and class plus method plus code attribute.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/annotation/visitor/TypePathInfoVisitor.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor for `TypePathInfo` entries on type annotations.
  - [P3] The four context overloads are absent: class-level, field, method, and method plus `CodeAttribute`, each receiving the enclosing class, the optional member or code attribute, the `TypeAnnotation`, and the `TypePathInfo`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/ExportsInfo.java`
- Bazel lines: 106
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Module-attribute exports entry holding a package constant-pool index, export flags, an export-to count, and the target module indices.
  - [P3] There is no visit of the package constant when that index is nonzero, and no loop that visits each export-to constant-pool entry for the recorded count.
  - [P3] There is no visitor-info slot that callers can read and replace on the entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/ModuleAttribute.java`
- Bazel lines: 195
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Module attribute that stores a module-name constant-pool index, module flags, an optional version index, and the requires, exports, opens, uses, and provides tables (uses as constant-pool indices).
  - [P3] Zel has no attribute-visitor dispatch to that attribute, and no name or version constant-pool visit that skips a zero index.
  - [P3] Zel has no count-bounded walks of requires, exports, opens, and provides that hand each entry to the matching info visitor, and no uses walk that visits every listed constant-pool entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/ModuleMainClassAttribute.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No class-file attribute type stores a module main-class constant-pool index (`u2mainClass`) together with the inherited attribute-name index.
  - [P3] No `accept` path dispatches this attribute to `AttributeVisitor.visitModuleMainClassAttribute`.
  - [P3] No `mainClassAccept` walks the constant-pool entry at `u2mainClass` only when that index is non-zero, and skips the visitor when the index is 0.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/ModulePackagesAttribute.java`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ModulePackages attribute holding the attribute-name index, `u2packagesCount`, and the `u2packages` constant-pool index array.
  - [P3] Nothing dispatches an attribute visitor specifically to a ModulePackages visit.
  - [P3] Nothing walks `[0, u2packagesCount)` and applies a constant visitor to each package constant-pool entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/OpensInfo.java`
- Bazel lines: 106
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Module-attribute Opens entry that stores a package constant-pool index, opens flags, an opens-to count, and the target constant-pool indexes.
  - [P3] Zel does not visit that package constant only when the opens index is non-zero.
  - [P3] Zel does not visit each opens-to target constant for the recorded opens-to count.
  - [P3] Zel has no per-entry visitor-info slot that callers can read and replace.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/ProvidesInfo.java`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Module-attribute `provides` entry that stores a service constant-pool index, a with-count, and the implementation constant-pool indices.
  - [P3] Zel has no `providesAccept` that visits that service constant-pool entry only when its index is non-zero, and skips the visit when the index is 0.
  - [P3] Zel has no `withAccept` that visits every implementation constant-pool entry from index 0 through `u2providesWithCount - 1`, including a zero index.
  - [P3] Zel has no visitor-info slot (`getVisitorInfo` / `setVisitorInfo`) on that entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/RequiresInfo.java`
- Bazel lines: 102
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Module `requires` entry that stores a module constant-pool index, requires flags, an optional version constant-pool index (all zero when unset), and a visitor-info slot.
  - [P3] Zel has no `moduleAccept` that visits the required-module constant only when its index is nonzero.
  - [P3] Zel has no `versionAccept` that visits the version Utf8 constant only when its index is nonzero.
  - [P3] Zel has no get/set of that visitor-info slot.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/visitor/AllExportsInfoVisitor.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that ignores every attribute except `ModuleAttribute` and, for that attribute only, walks `exports[0..u2exportsCount)` in index order and calls `visitExportsInfo(clazz, exports[index])` on a stored `ExportsInfoVisitor`. A zero count visits nothing. There is no double dispatch.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/visitor/AllOpensInfoVisitor.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel visitor ignores every attribute except `ModuleAttribute` and then forwards each of that attribute's opens entries to a supplied opens visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/visitor/AllProvidesInfoVisitor.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No attribute visitor walks only a `Module` attribute and calls a nested visitor once per `ProvidesInfo`, in table order, with the owning class.
  - [P3] Every other attribute kind is ignored, including an empty provides table. That dispatch is absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/visitor/AllRequiresInfoVisitor.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No attribute visitor ignores every non-module attribute and, for a module attribute, walks `requires[0..u2requiresCount)` in order and calls `visitRequiresInfo` on each entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/visitor/ExportsInfoVisitor.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `ExportsInfo` visitor callback that receives the enclosing `Clazz` and one module-export entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/visitor/OpensInfoVisitor.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor callback that receives a class and one Module-attribute opens entry (package index, flags, and the modules that package is opened to). The Java type is only that single method; it does not walk entries or filter them.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/visitor/ProvidesInfoVisitor.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No visitor callback that receives a class and a Module attribute `provides` entry (`ProvidesInfo`: the provided service’s constant-pool index and the implementation indexes).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/module/visitor/RequiresInfoVisitor.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor callback that receives a class and one module `requires` entry (`RequiresInfo`).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/DoubleType.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No JVM stack-map verification type for double: nothing returns tag 3 (`DOUBLE_TYPE`).
  - [P3] No visitor dispatch for a double type, a double stack slot, or a double local variable (`visitDoubleType`, `visitStackDoubleType`, `visitVariablesDoubleType`).
  - [P3] No `"d"` display string for that type.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/FloatType.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no float verification type whose tag is `FLOAT_TYPE` (2).
  - [P3] Zel has no `accept` path that forwards the class, method, code attribute, instruction offset, and this type to `visitFloatType`.
  - [P3] Zel has no stack path that also passes the stack index to `visitStackFloatType`.
  - [P3] Zel has no local-variable path that also passes the variable index to `visitVariablesFloatType`.
  - [P3] Zel has no float verification type whose string form is `f`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/FullFrame.java`
- Bazel lines: 202
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no full stack-map frame that stores an offset delta plus counted local-variable and operand-stack verification types.
  - [P3] No walk of those counted locals through each type's variable visitor, or of the counted stack slots through each type's stack visitor.
  - [P3] No full-frame tag (255), `visitFullFrame` dispatch, equality of the offset and the counted type prefixes, XOR hash of those types, or the `Var:` / `, Stack:` text form.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/IntegerType.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no integer verification type whose tag is `INTEGER_TYPE` (1).
  - [P3] Zel has no `accept`, `stackAccept`, or `variablesAccept` dispatch to `visitIntegerType`, `visitStackIntegerType`, or `visitVariablesIntegerType` with the class, method, code attribute, instruction offset, and this type (plus stack or variable index).
  - [P3] Zel has no integer verification type whose string form is `i`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/LessZeroFrame.java`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no chop-frame stack map entry that stores a chopped-local count and an inherited `u2offsetDelta`.
  - [P3] Zel does not derive that count as `248 + 3 - tag` (tags 248–250 become 3, 2, and 1 chopped locals), store a signed byte count widened to `int`, or invert the count back to the frame tag with `248 + 3 - choppedVariablesCount`.
  - [P3] Zel has no `visitLessZeroFrame` accept path that forwards the class, method, code attribute, offset, and the frame.
  - [P3] Zel has no equality that, after the superclass same-class and offset-delta check, is true only when the chopped counts differ, and no hash that xors the superclass hash with the chopped count.
  - [P3] Zel has no string form that appends `Var: (chopped N), Stack: (empty)` to the superclass `[offset] ` text.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/LongType.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no long verification type whose tag is `LONG_TYPE` (4).
  - [P3] Zel has no `accept`, `stackAccept`, or `variablesAccept` path that dispatches a long type to `visitLongType`, `visitStackLongType`, or `visitVariablesLongType` with the class, method, code attribute, instruction offset, and stack or variable index.
  - [P3] Zel has no long verification type whose string form is `l`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/MoreZeroFrame.java`
- Bazel lines: 161
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no append stack-map frame. ProGuard `MoreZeroFrame` stores `additionalVariablesCount` and that many `VerificationType` locals.
  - [P3] Tag construction and `getTag` use `additionalVariablesCount = tag + 1 - 252` and `252 + additionalVariablesCount - 1`.
  - [P3] `additionalVariablesAccept` visits only the first `additionalVariablesCount` types, and `accept` dispatches to `visitMoreZeroFrame`.
  - [P3] Equality requires the superclass match, the same `u2offsetDelta` and count, then pairwise type equality. `hashCode` xors the superclass hash with each appended type. `toString` prints `Var: ...[type]...` and `, Stack: (empty)`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/NullType.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no null verification type whose tag is `NULL_TYPE` (5).
  - [P3] Zel has no `accept` path that visits a null type with the class, method, code attribute, and instruction offset.
  - [P3] Zel has no stack or variable accept paths that visit a null type with the stack or variable index.
  - [P3] Zel has no null-type display string `"n"`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/ObjectType.java`
- Bazel lines: 107
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No stack-map verification object type (tag 7) that stores a constant-pool class index (`u2classIndex`), defaulting to 0.
  - [P3] No visitor dispatch that routes a plain, stack, or local-variable context to distinct object-type callbacks.
  - [P3] No equality that requires the same runtime class and the same class index, no hash that XORs the class hash with that index, and no `a:<index>` string form.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/SameOneFrame.java`
- Bazel lines: 115
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no stack-map frame for a same-locals frame with one stack verification type.
  - [P3] No tag constructor that sets the offset delta to `tag - 64`, and no `getTag` that returns `64 + offsetDelta` when the delta is below 64 and 247 otherwise.
  - [P3] No stack-item visitor, and no equality or hash that includes both the offset delta and that stack item.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/SameZeroFrame.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no stack-map frame for unchanged locals and an empty operand stack. Nothing stores the inherited `u2offsetDelta` for that frame, either left at 0 or set from a tag.
  - [P3] The tag constructor sets `u2offsetDelta` to `tag - SAME_ZERO_FRAME` (`tag - 0`). `getTag` returns that delta when it is less than 64 (same-frame tags 0–63, including a negative delta) and otherwise always returns `SAME_ZERO_FRAME_EXTENDED` (251), independent of the stored delta.
  - [P3] `accept` always forwards the class, method, code attribute, bytecode offset, and this frame to `visitSameZeroFrame`. Zel has no such dispatch.
  - [P3] `toString` is the base `"[delta] "` plus `Var: ..., Stack: (empty)`. Zel has no equivalent text.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/StackMapAttribute.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no StackMap attribute that stores an explicit frame count and a `FullFrame` array.
  - [P3] No constructor sets the count from the array length, or stores a count that can differ from the array length.
  - [P3] No `accept` path dispatches to `visitStackMapAttribute` with the class, method, code attribute, and this attribute.
  - [P3] No frame walk visits only the first `u2stackMapFramesCount` entries and calls `visitFullFrame` with each frame’s offset delta.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/StackMapFrame.java`
- Bazel lines: 117
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no abstract stack map frame that stores a `u2` offset delta (returned as the bytecode offset relative to the previous frame) and the frame-tag boundaries 0, 64, 247, 248, 251, 252, and 255.
  - [P3] Zel has no visitor-info slot on that frame, and no `accept` dispatch that passes the class, method, code attribute, bytecode offset, and a stack-map-frame visitor.
  - [P3] Equality is the same runtime class and an equal offset delta, ignoring visitor info. The hash is the class hash XOR the delta, and the string form is `[` + delta + `] ` with a trailing space. Zel has none of that.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/StackMapTableAttribute.java`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no stack map table attribute holding a frame count and frame array, and no constructors that set the count from an explicit length or from the array length.
  - [P3] Zel does not dispatch the attribute to an attribute visitor (`visitStackMapTableAttribute`).
  - [P3] Zel does not walk frames, turning each offset delta into an absolute bytecode offset (running total starts at 0; add 1 for every frame after the first) before visiting the frame.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/TopType.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no top verification type that reports tag `0` (`TOP_TYPE`) and stringifies as `"T"`.
  - [P3] Zel has no `accept`, `stackAccept`, or `variablesAccept` path that dispatches to `visitTopType`, `visitStackTopType`, or `visitVariablesTopType` with the class, method, code attribute, instruction offset, and (for the stack and variable paths) the index.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/UninitializedThisType.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no verification type whose tag is `UNINITIALIZED_THIS_TYPE` (6) and whose string form is `u:this`.
  - [P3] Zel has no visitor dispatch that sends that type through `visitUninitializedThisType`, `visitStackUninitializedThisType` (with a stack index), or `visitVariablesUninitializedThisType` (with a variable index).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/UninitializedType.java`
- Bazel lines: 106
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no uninitialized stack-map verification type that stores the `u2` offset of the creating `new` instruction (default `0`).
  - [P3] Zel has no tag accessor that returns `UNINITIALIZED_TYPE` (`8`), and no `accept`, `stackAccept`, or `variablesAccept` dispatch onto the matching uninitialized-type visitor methods.
  - [P3] Zel has no equality that requires the same class and the same new-instruction offset, no hash code of the class hash XOR that offset, and no `u:<offset>` string form.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/VerificationType.java`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no verification-type model for ProGuard preverification: no tags 0–8 (Top, Integer, Float, Double, Long, Null, UninitializedThis, Object, Uninitialized), no `getTag` or visitor `accept` / `stackAccept` / `variablesAccept` dispatch, no visitor-info slot, and no class-identity `equals` / `hashCode`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/VerificationTypeFactory.java`
- Bazel lines: 112
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No factory returns shared singleton Integer, Long, Float, Double, Top, Null, and UninitializedThis verification types.
  - [P3] No constructor allocates a fresh UninitializedType from a new-instruction offset, or an ObjectType from a constant-pool class index.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/visitor/StackMapFrameVisitor.java`
- Bazel lines: 40
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no stack-map frame visitor and no equivalent dispatch over the five ProGuard frame kinds: same locals with an empty stack, same locals with one stack item, fewer locals with an empty stack, more locals with an empty stack, and a full frame.
  - [P3] Nothing in zel accepts a class, method, code attribute, bytecode offset, and frame together to walk those frame types.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/preverification/visitor/VerificationTypeVisitor.java`
- Bazel lines: 65
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no verification-type visitor that splits callbacks into a bare type visit, a stack-slot visit, and a local-variable visit.
  - [P3] No callbacks for Integer, Float, Long, Double, Top, Object, Null, Uninitialized, and UninitializedThis, each given the class, method, code attribute, bytecode offset, and the type object.
  - [P3] Stack and variable callbacks also receive the slot index. Zel never dispatches on that index.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/AllAttributeVisitor.java`
- Bazel lines: 117
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No walker sends every attribute of a program class to a delegate attribute visitor.
  - [P3] No deep mode then walks that class's fields and methods (delegating their attributes) and walks the class attributes again so each code attribute's nested attributes are delegated.
  - [P3] No member entry point delegates a program member's attributes and, when deep, visits those attributes again so code-attribute children are delegated.
  - [P3] Library classes and library members are ignored, non-code attributes are ignored, and nested code attributes are passed only to the delegate. Zel has no equivalent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/AllBootstrapMethodInfoVisitor.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that ignores every attribute except a `BootstrapMethods` attribute and then, in stored index order, applies a nested visitor to each bootstrap-method entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/AllExceptionInfoVisitor.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No attribute visitor that, for a code attribute, forwards the class, method, and each exception-info entry to a nested exception visitor.
  - [P3] No no-op path for every other attribute type.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/AllInnerClassesInfoVisitor.java`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that ignores every attribute except `InnerClassesAttribute` and then walks `classes[0..u2classesCount)` calling `visitInnerClassesInfo` on a held `InnerClassesInfoVisitor`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/AllLineNumberInfoVisitor.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel type accepts an attribute and, only for a line-number table, forwards every line-number entry to a line-number visitor while leaving every other attribute unvisited.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/AttributeCounter.java`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file attribute visitor that counts attributes as they are visited.
  - [P3] There is no accumulator that starts at 0, returns that total from `getCount()`, and increments by one in `visitAnyAttribute` while ignoring the class and the attribute contents.
  - [P3] Name filtering and the "count greater than zero" checks stay with the caller. This type only tallies visits, and zel has no equivalent counter.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/AttributeNameFilter.java`
- Bazel lines: 513
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that parses a name pattern (a string or a list, through name/list parsing) or takes a string matcher, then forwards each attribute visit to a delegate only when that matcher accepts `attribute.getAttributeName(clazz)`, for every class, field, method, and code-attribute overload.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/AttributeToClassVisitor.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On every attribute visit, the wrapped `ClassVisitor` is applied to the owning class via `clazz.accept`, and the `Attribute` is ignored.
  - [P3] `SimplifiedVisitor` routes every concrete attribute callback (class, field, method, code, and annotation attributes) into that one method, dropping member and code context, with one class visit per attribute visit and no deduplication.
  - [P3] Zel has no attribute-visitor adapter that promotes an attribute walk to a class visit.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/AttributeVisitor.java`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file attribute visitor. The Java interface is only a dispatch contract: one visit method per attribute kind, with overloads that also pass the field, the method, or the method plus its `CodeAttribute` when the attribute is attached there. That covers unknown, bootstrap methods, source file, source dir, inner classes, enclosing method, nest host, nest members, module, module main class, module packages, deprecated, synthetic, signature, constant value, method parameters, exceptions, code, stack map, stack map table, line number table, local variable table, local variable type table, runtime-visible and runtime-invisible annotations, parameter annotations, type annotations, and annotation default.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/BootstrapMethodInfoVisitor.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor callback that is invoked with an owning class and a `BootstrapMethodInfo` (the invokedynamic bootstrap-method entry).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/DebugAttributeVisitor.java`
- Bazel lines: 611
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute-visitor wrapper that forwards every `AttributeVisitor` visit to a delegate.
  - [P3] Construction reads the `debug.time` system property and parses it with `Long.valueOf` (invalid values throw); otherwise the threshold is 10000 ms and the message defaults to the delegate's class name. That setup has no zel equivalent.
  - [P3] `DEBUG` is compiled as `false` (`dav` is commented out), so visits only forward and never time or print. The field and method slow-path printers (`SimpleClassPrinter`) and the class-level printer are unreachable. `visitLibraryClass` is an empty method and does not forward.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/ExceptionInfoVisitor.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor callback invoked once per JVM exception-table entry with the enclosing class, method, code attribute, and that entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/InnerClassesInfoVisitor.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel type exposes a visitor callback that receives a class and one `InnerClassesInfo` entry the way `visitInnerClassesInfo(Clazz, InnerClassesInfo)` does.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/InstructionToAttributeVisitor.java`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No instruction visitor stores an attribute visitor and, on every instruction, ignores the instruction and offset and re-visits the enclosing code attribute with that attribute visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/LineNumberInfoVisitor.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `LineNumberInfo` visitor callback invoked with the enclosing class, method, code attribute, and line-number entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/LineNumberRangeFinder.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `LineNumberInfo` visitor that tracks the minimum and maximum `u2lineNumber` seen (initial lowest `Integer.MAX_VALUE`, initial highest `0`) and sets a flag when any visited entry has a non-null source.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/LocalVariableInfoVisitor.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No visitor callback is invoked with the owning class, method, code attribute, and one local-variable info record.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/LocalVariableTypeInfoVisitor.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor callback for a `LocalVariableTypeInfo` entry that is invoked with the enclosing class, method, code attribute, and that local-variable type record.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/MultiAttributeVisitor.java`
- Bazel lines: 479
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No composite attribute visitor forwards each class, field, method, and code attribute visit, in registration order, to every registered delegate.
  - [P3] Empty construction of a 16-slot visitor array, a varargs constructor that records the initial count, and `addAttributeVisitor` growth via `ArrayUtil.add` are absent.
  - [P3] The split between loops bounded by `attributeVisitorCount` and loops bounded by `attributeVisitors.length` (module attributes, method parameters, and runtime type annotations) has no counterpart.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/NonEmptyAttributeFilter.java`
- Bazel lines: 417
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that forwards a visit only when the attribute is non-empty.
  - [P3] Empty collection attributes are not skipped: bootstrap methods, inner classes, nest members, module packages, method parameters, exceptions, stack map and stack map table frames, line numbers, local variables, local variable types, and runtime-visible or invisible annotations, parameter annotations, and type annotations (class, field, method, and code).
  - [P3] A module attribute is not kept only when at least one of requires, exports, opens, uses, or provides is non-zero.
  - [P3] Attributes that are always forwarded are also absent: unknown, source file, source dir, enclosing method, nest host, module main class, deprecated, synthetic, signature, constant value, code, and annotation default.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/ParameterInfoVisitor.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor callback that receives a class, a method, a parameter index, and a `ParameterInfo` (a MethodParameters attribute entry). This Java file defines only that callback and no other behavior.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/RequiredAttributeFilter.java`
- Bazel lines: 489
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that sends strictly required attributes (`BootstrapMethods`, `NestHost`, `NestMembers`, `ConstantValue`, `Code`) to a required visitor and every other attribute, including `StackMap` and `StackMapTable`, to an optional visitor.
  - [P3] A null required or optional visitor is a no-op for that side. That split dispatch is absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/SingleTimeAttributeVisitor.java`
- Bazel lines: 559
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that forwards a visit to a delegate only when the attribute is not equal to the immediately previous one, then stores that attribute as the last visit.
  - [P3] Every `AttributeVisitor` overload in this class is absent: unknown, bootstrap methods, source file/dir, inner classes, enclosing method, nest host/members, module/main-class/packages, deprecated, synthetic, signature, constant value, method parameters, exceptions, code, stack map and stack map table, line numbers, local variable and local variable type tables, runtime visible/invisible annotations, parameter annotations, type annotations, and annotation default, including the class, field, method, and code-attribute overloads.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/attribute/visitor/StackSizeComputer.java`
- Bazel lines: 389
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not compute per-offset operand-stack sizes for a ProGuard code attribute. Non-code attributes are ignored. A code attribute resets reachability and the before/after sizes for `u4codeLength`, starts at stack 0, walks from offset 0, then visits each exception handler with the stack forced to 1.
  - [P3] An offset is reachable only after it is evaluated. The size before or after an unreachable offset throws `IllegalArgumentException` (`Unknown stack size before/after unreachable instruction offset [...]`). The maximum is the high-water mark of the incoming stack at each block and the stack after each instruction, including a handler entry of 1.
  - [P3] Each instruction subtracts `stackPopCount` and then adds `stackPushCount`. A negative size throws `IllegalArgumentException` naming the instruction, class, and method name plus descriptor. A block that starts at an already evaluated offset does not run again and does not check that the incoming size matches the stored size. The caller's stack is restored when the block returns.
  - [P3] The block ends on `ireturn`, `lreturn`, `freturn`, `dreturn`, `areturn`, `return`, `athrow`, `ret`, `goto`, `goto_w`, `jsr`, and `jsr_w`, and on every switch after each jump offset and the default have been walked. Constant-pool instructions never end the block. Other simple and variable instructions, and conditional branches after their target is walked, fall through.
  - [P3] `jsr` and `jsr_w` walk the target at the post-instruction size, then subtract 1 (the return address is popped) and walk the following instruction at that reduced size.
  - [P3] A `RuntimeException` from that walk is printed to stderr with the class name, method name plus descriptor, and the exception class and message, then rethrown. Tracing is off, so the method is not dumped.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/ClassConstant.java`
- Bazel lines: 105
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant-pool class entry with a name index, a referenced class, and a separate `java.lang.Class` class link.
  - [P3] No constructor that stores the name index and referenced class while leaving the `java.lang.Class` link unset.
  - [P3] No name lookup through the owning class's string at that index.
  - [P3] No `CONSTANT_Class` tag and no visitor dispatch to `visitClassConstant`.
  - [P3] No accept of a class visitor on the referenced class only when that reference is non-null.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/Constant.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant-pool entry type and no tag that identifies the entry kind.
  - [P3] Zel has no `accept(Clazz, ConstantVisitor)` visitor dispatch for a pool entry.
  - [P3] Zel has no per-entry visitor-info slot with get and set.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/DoubleConstant.java`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant-pool entry that stores a mutable double (default `0.0`, with get/set).
  - [P3] Nothing returns constant-pool tag `6` (`CONSTANT_Double`).
  - [P3] Nothing dispatches a visitor to a double-constant visit method.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/DynamicConstant.java`
- Bazel lines: 148
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `CONSTANT_Dynamic` constant-pool entry holding a bootstrap-method attribute index, a name-and-type index, and the classes referenced by the descriptor.
  - [P3] No name or type lookup through the owning class, no visitor walk of those referenced classes that skips null entries, no bootstrap method-handle visit, and no `CONSTANT_Dynamic` tag or `visitDynamicConstant` dispatch.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/FieldrefConstant.java`
- Bazel lines: 71
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No constant-pool entry tagged `CONSTANT_Fieldref`, distinct from method and interface-method references.
  - [P3] No field reference holding a class index, a name-and-type index, and optional resolved referenced class and member.
  - [P3] No visitor dispatch of a field reference through `visitFieldrefConstant`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/FloatConstant.java`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant-pool float entry that stores a `float` (default `0.0f` when uninitialized), reads or writes it, returns the `CONSTANT_Float` tag, or dispatches `visitFloatConstant`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/IntegerConstant.java`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no integer constant-pool entry with a mutable 32-bit `u4value`, a default or value constructor, or get/set accessors.
  - [P3] Zel has no `CONSTANT_Integer` tag and no `accept` path that dispatches to `visitIntegerConstant`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/InterfaceMethodrefConstant.java`
- Bazel lines: 71
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant-pool value whose tag is `CONSTANT_InterfaceMethodref` (11).
  - [P3] Zel has no visitor dispatch equivalent to `accept` calling `visitInterfaceMethodrefConstant`.
  - [P3] Zel does not store an interface method reference’s class index, name-and-type index, referenced class, and referenced member.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/InvokeDynamicConstant.java`
- Bazel lines: 148
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no invokedynamic constant-pool entry: no bootstrap-method attribute index, name-and-type index, or descriptor class references, and no name/type lookup through that name-and-type entry.
  - [P3] Zel does not visit non-null referenced classes, does not hand the entry to a bootstrap method-handle traveler, and does not dispatch a `CONSTANT_InvokeDynamic` tag through a constant visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/LongConstant.java`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No constant-pool entry stores a mutable 64-bit long (`u8value`), defaulting to 0, with get and set of that value.
  - [P3] No `getTag` returning `CONSTANT_Long` (5), and no `accept` dispatch to `visitLongConstant`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/MethodHandleConstant.java`
- Bazel lines: 142
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No constant-pool method-handle entry stores a reference kind and a constant-pool index, reports tag `CONSTANT_MethodHandle`, or keeps a `java.lang.invoke.MethodHandle` class link.
  - [P3] Nothing returns the referenced field or method's class name, name, and type, or walks that referenced constant and this entry through a constant visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/MethodTypeConstant.java`
- Bazel lines: 125
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no CONSTANT_MethodType pool entry: no descriptor index, no `getType` lookup of that index as a string on the owning class, and no `CONSTANT_MethodType` tag.
  - [P3] No extra reference to `java.lang.invoke.MethodType`, and no array of classes named by the method descriptor (primitive types omitted) for a class-reference initializer to fill. The two-argument constructor sets only the descriptor index and that array.
  - [P3] No `referencedClassesAccept` walk that skips a null array and null slots and otherwise calls `accept` on each referenced class.
  - [P3] No constant-visitor dispatch to `visitMethodTypeConstant` with the owning class and this constant.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/MethodrefConstant.java`
- Bazel lines: 71
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant-pool method-reference entry whose tag is `CONSTANT_Methodref` (10).
  - [P3] Zel has no constructor that stores a class index, a name-and-type index, a referenced class, and a referenced member for that entry.
  - [P3] Zel has no visitor dispatch that accepts a class and this constant and calls `visitMethodrefConstant`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/ModuleConstant.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant-pool module entry (JVM tag 19) that stores a UTF-8 name index.
  - [P3] There is no name lookup of that index through the owning class string table.
  - [P3] There is no visitor dispatch to `visitModuleConstant`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/NameAndTypeConstant.java`
- Bazel lines: 119
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No constant-pool entry that stores a name index and a descriptor index and reports tag `CONSTANT_NameAndType` (12).
  - [P3] No resolution of those indices to the name and type strings through the owning class.
  - [P3] No visitor dispatch to `visitNameAndTypeConstant`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/PackageConstant.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no JVM constant-pool package entry (tag 20) that stores a name index, resolves it with the owning class string table, or dispatches a package-constant visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/PrimitiveArrayConstant.java`
- Bazel lines: 230
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no unofficial constant-pool entry (tag `CONSTANT_PrimitiveArray`, 21) that stores a primitive array and exposes it as the constant value.
  - [P3] Nothing reports the element type as the JVM type character (`Z`, `B`, `C`, `S`, `I`, `F`, `J`, or `D`) or `0` when the value is null or not one of those eight array types.
  - [P3] Nothing reports the array length, or `0` for null or an unrecognized value.
  - [P3] Nothing dispatches a typed visitor over the whole array, or over each element in index order, and nothing silently skips null or a non-primitive array.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/RefConstant.java`
- Bazel lines: 130
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no field, method, or interface-method reference constant that stores a class-pool index and a name-and-type index, including a setter for the name-and-type index.
  - [P3] No resolved class or member side fields, and no null-checked visitor dispatch that walks the referenced class or walks the referenced member while passing that class to the member visitor.
  - [P3] No lookups of the referenced class name, member name, or descriptor through those two indices.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/StringConstant.java`
- Bazel lines: 135
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no JVM `CONSTANT_String` pool entry with a UTF-8 index, an optional referenced class, an optional referenced member, and a separate `java.lang.String` class link. The three-argument constructor stores the index, class, and member and leaves the `java.lang.String` link unset.
  - [P3] There is no `getString` lookup through the owning class constant pool, no `CONSTANT_String` tag, and no `visitStringConstant` dispatch.
  - [P3] There is no referenced-class walk that runs only when a class is set and the member is unset, and no referenced-member walk that visits the member with that class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/Utf8Constant.java`
- Bazel lines: 285
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant-pool UTF-8 entry: no exclusive lazy storage of modified-UTF-8 bytes versus a Java string, no `CONSTANT_Utf8` tag, and no `visitUtf8Constant` dispatch.
  - [P3] No modified-UTF-8 encoder: U+0000 becomes the two bytes `C0 80`, code units below U+0080 are one byte, below U+0800 are two bytes, and every other UTF-16 code unit (including unpaired surrogates) is three bytes.
  - [P3] No decoder that treats a lead byte below `0xC0` as one character, below `0xE0` as two, and otherwise as three, without checking continuation flag bits, or that turns a truncated sequence into a `RuntimeException` whose message is `Missing UTF-8 bytes after initial byte [0x…] in string […]`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/visitor/AllBootstrapMethodArgumentVisitor.java`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Nothing in zel walks a bootstrap method’s argument constant-pool entries and forwards each one to a constant visitor. The Java visitor’s only behavior is `visitBootstrapMethodInfo` calling `methodArgumentsAccept`, which visits `u2methodArguments[0..u2methodArgumentCount)` and does not visit the bootstrap method handle.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/visitor/AllConstantVisitor.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that, on a program class, sends every constant-pool entry to a wrapped constant visitor.
  - [P3] Zel has no matching library-class visit that does nothing.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/visitor/BootstrapMethodHandleTraveler.java`
- Bazel lines: 107
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel code, given a dynamic or invokedynamic constant, stores `u2bootstrapMethodAttributeIndex`, walks the class attributes to that bootstrap-method entry, and visits the entry's method-handle constant (`u2methodHandleIndex`). A direct bootstrap-method-entry visit does the same method-handle visit. Every other constant and attribute is ignored.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/visitor/ConstantCounter.java`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel counts every visited class-file constant: the counter starts at 0, `visitAnyConstant` increments it by one for any constant, and `getCount` returns that total. Zel has no constant visitor or counter with this behavior.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/visitor/ConstantTagFilter.java`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No constant visitor filters visits by a bitmask of constant tags. Bazel builds the mask as `1 << tag` from one tag or the bitwise OR of an array of tags, and `visitAnyConstant` delegates with `constant.accept` only when `(1 << constant.getTag())` intersects that mask.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/visitor/ConstantVisitor.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No visitor contract that receives the owning class plus each constant-pool kind: integer, long, float, double, primitive array, string, UTF-8, dynamic, invoke-dynamic, method handle, fieldref, interface methodref, methodref, class, method type, name-and-type, module, and package.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/visitor/ExceptClassConstantFilter.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Nothing in zel filters constant visits so that only `ClassConstant`s are forwarded, and only when the UTF-8 string at `u2nameIndex` is not exactly equal to a configured class name. Other constant kinds are ignored, and a matching class name is not delegated.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/visitor/MethodrefTraveler.java`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Nothing in zel walks from a method-handle constant to the constant-pool entry at its reference index and forwards that entry to a constant visitor. The Java visitor’s only behavior is `visitMethodHandleConstant` calling `constantPoolEntryAccept` on `u2referenceIndex` (a fieldref, methodref, or interface methodref). `visitAnyConstant` is a no-op, so every other constant kind is ignored, and the reference kind is not consulted.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/visitor/PrimitiveArrayConstantElementVisitor.java`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor for primitive-array constant elements that dispatches by element type (boolean, byte, char, short, int, float, long, double) with the owning class, the constant, the element index, and the typed value.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/visitor/PrimitiveArrayConstantVisitor.java`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that receives the owning class, a primitive-array constant, and its values as a `boolean[]`, `byte[]`, `char[]`, `short[]`, `int[]`, `float[]`, `long[]`, or `double[]`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/constant/visitor/SuperClassConstantVisitor.java`
- Bazel lines: 69
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that, for a program class, optionally forwards that class's superclass constant and its interface constants to a constant visitor.
  - [P3] The two independent flags that choose superclass constants, interface constants, or both are absent.
  - [P3] The no-op for library classes is absent with the visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/AccessFixer.java`
- Bazel lines: 330
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never widens access of program classes referenced from a visited program class. Bazel raises a non-public referenced class to package-visible when the referencing class shares its internal package name, and to public otherwise, and applies the same rule to the matching `InnerClasses` entry. Library classes are left unchanged, and access is never narrowed.
  - [P3] Zel never widens access of program members reached from constant instructions or from annotation element values. Bazel uses private when the member’s class is the referencing class, package-visible when the packages match, protected only for a non-static referencing method when the invocation class is null or a subclass of the referencing class and the referencing class extends the member’s class, and public otherwise. Annotation references are treated as static, so they cannot take the protected case. Array references use the resolved class (`java.lang.Object`). Library members are left unchanged.
  - [P3] Zel never widens implicitly referenced overrides. Bazel walks non-private, non-static, non-initializer methods and fixes the same-name, same-descriptor members in superclasses and subclasses, with the referencing method treated as non-static and the referenced class unset so a superclass member can become protected.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/AnnotationAdder.java`
- Bazel lines: 152
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel code copies each visited annotation into a target annotation element value, an annotations attribute, or a parameter-annotations attribute.
  - [P3] Missing the copy that remaps the annotation type constant into the target class, sizes the element-value array (sharing one empty array when the count is zero), aliases `referencedClasses` without cloning, fills element values, then either overwrites the target element value or appends the annotation (the method overload also records the parameter index).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/AnnotationsAttributeEditor.java`
- Bazel lines: 105
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor bound to one annotations attribute that appends a pre-filled annotation, growing the backing array by exactly one slot when the count already fills it, then incrementing the annotation count.
  - [P3] No deletion by object identity (the same annotation instance, not equal contents) that shifts later entries down, nulls the vacated tail slot, and decrements the count.
  - [P3] No index deletion of that same form. A missing annotation is resolved to index -1 and then removed at that index.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/AttributeAdder.java`
- Bazel lines: 569
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that copies each visited class-file attribute onto a target class, member, or code attribute, remapping constant-pool indices and optionally replacing an existing attribute of the same kind.
  - [P3] No copies of unknown, source file, source directory, inner classes, enclosing method (keeping a zero name-and-type index and the referenced class and method), nest host, nest members, deprecated, synthetic, signature (including referenced classes), constant value, method parameters, exceptions, line numbers, local variables, local variable types, runtime-visible and invisible annotations, parameter annotations, type annotations, or annotation defaults.
  - [P3] No code-attribute copy that rewrites instructions and exception handlers into a new code attribute, recursively copies nested attributes, and inserts a synthetic line-number entry (`class.methoddescriptor:0:0`) when the source code has no line-number table. Stack-map and stack-map-table visits are empty stubs in Bazel.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/AttributeSorter.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not sort class-file attributes by constant-pool attribute-name index. Bazel, for each program class, stably sorts the class attribute prefix `[0, u2attributesCount)` by `u2attributeNameIndex` (less → -1, greater → 1, equal → 0), then does the same for each field and method, and sorts a `CodeAttribute`'s own attribute prefix when that attribute is visited.
  - [P3] Equal name indices compare equal, so their relative order is kept. Library classes and members are not visited, and attributes other than `CodeAttribute` are not reordered or recursed into.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/AttributesEditor.java`
- Bazel lines: 314
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No editor that targets a class, member, or code attribute and finds an attribute by name resolved through the class constant pool.
  - [P3] No add that, when replacement is enabled, overwrites the first same-named attribute and otherwise appends, growing the array by one slot when it is full.
  - [P3] No delete that removes only the first same-named attribute, shifts the tail down, and clears the vacated last slot.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/BootstrapMethodInfoAdder.java`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Nothing copies a visited bootstrap method into a target bootstrap-methods attribute by remapping the method-handle constant and every argument constant (`u2methodArguments[0..u2methodArgumentCount)`) from the source class into the target constant pool, then appending a new bootstrap-method entry.
  - [P3] Nothing records or returns the index of that most recently appended bootstrap method.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/BootstrapMethodRemapper.java`
- Bazel lines: 107
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Nothing in zel remaps bootstrap-method attribute indices on dynamic and invokedynamic constants. The Java visitor rewrites `u2bootstrapMethodAttributeIndex` through an int map; a negative mapped index is left unchanged when dangling constants are ignored (default false) and otherwise throws `IllegalArgumentException` ("Can't remap bootstrap method index [N]"). Other constant kinds are left untouched.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/BootstrapMethodsAttributeAdder.java`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Nothing in zel ensures a class has a `BootstrapMethods` attribute and copies visited bootstrap methods into it. The Java visitor lazily creates a class-level attribute (UTF-8 name constant, zero methods) when the target has none, then copies each visited method’s handle and argument constants into that attribute and records the index of the method just added. `getBootstrapMethodIndex` returns that index.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/BootstrapMethodsAttributeEditor.java`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor that appends a pre-filled bootstrap method onto a bootstrap-methods attribute, growing the array to exactly the new count when it is short, and returning the index that the count had before it is incremented.
  - [P3] Zel has no removal that matches a bootstrap method by reference identity. The Java removal decrements the count before searching, so a hit in the remaining prefix is shifted out and the old last slot cleared, while a last entry or a miss removes the last entry. An empty attribute decrements the count below zero and then throws on the array copy.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/BootstrapMethodsAttributeShrinker.java`
- Bazel lines: 339
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no program-class pass that marks `BootstrapMethods` entries referenced by `invokedynamic`, then compacts the array to the used entries and records an old-to-new index map (`-1` for unused entries).
  - [P3] Nothing deletes the `BootstrapMethods` attribute when that count falls to zero, then removes `InnerClasses` entries whose inner or outer class name starts with `java/lang/invoke/MethodHandles` and drops the `InnerClasses` attribute if it becomes empty.
  - [P3] When some bootstrap methods remain, nothing remaps constant-pool bootstrap-method indices through that map, and nothing runs a constant-pool shrink only after the attribute changed. Library classes are left unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/BridgeMethodFixer.java`
- Bazel lines: 116
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not visit program methods that have `ACC_BRIDGE`, walk their `Code` attributes, and inspect `invokevirtual`, `invokespecial`, `invokestatic`, and `invokeinterface` instructions.
  - [P3] Zel does not read the invoked method's name from the constant pool and clear `ACC_BRIDGE` when that name differs from the enclosing method's name.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ClassEditor.java`
- Bazel lines: 255
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor that appends a constant-pool interface index to a program class, growing the interface array by one when it is full.
  - [P3] Zel has no removal of the first matching interface index that shifts later entries down, writes 0 into the vacated slot, and decrements the count. A missing index makes Java `System.arraycopy` fail because the copy length is negative.
  - [P3] Zel has no append of a pre-filled field or method onto grow-by-one `ProgramField` / `ProgramMethod` arrays, and no removal by `equals` that shifts later entries down, nulls the last slot, and decrements the count. A missing member fails the same way as a missing interface.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ClassElementSorter.java`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no program-class visitor that sorts the constant pool first (by entry type, then contents), collapses equal entries, keeps a null second slot for `long` and `double`, and remaps every constant-pool reference to the new indices.
  - [P3] It does not then sort interface constant-pool indices, rewrite the signature attribute to match, and delete adjacent duplicate interfaces.
  - [P3] It does not then sort class, field, method, and code attributes by attribute-name index, while leaving field and method declaration order unchanged.
  - [P3] Visiting a library class is unsupported and throws.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ClassMemberSorter.java`
- Bazel lines: 69
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-member sorter. On a program class, Bazel sorts only the live field prefix (`0..u2fieldsCount`) and the live method prefix (`0..u2methodsCount`), leaving any unused trailing array slots in place.
  - [P3] Order is ascending access flags, then constant-pool name index, then constant-pool descriptor index. Ties compare equal, and `Arrays.sort` keeps their original relative order. Comparison uses the pool indices, not the resolved name or descriptor strings.
  - [P3] Library classes are left unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ClassReferenceFixer.java`
- Bazel lines: 609
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never rewrites constant-pool, member, or attribute class references after a class is renamed. Program classes are not walked through the constant pool, fields, methods, and attributes. Library classes are not walked through fields and methods only.
  - [P3] String constants that refer to a class and not a member are not rewritten: the string is internalized, replaced with the referenced class’s new name, and stored back in dotted external form only when the original string contained `.`.
  - [P3] Class constants, method-type descriptors, and dynamic or invokedynamic name-and-type descriptors are not updated when the referenced class name changes. Field, method, and interface-method references are left unchanged.
  - [P3] Descriptors and signatures are not rebuilt for program or library fields and methods, signature attributes, local-variable descriptors and signatures, annotation types, enum element types, or class element values. A single referenced class replaces only the first class name and drops any later ones. Several referenced classes are walked in order, and an inner-class name keeps only the segment after `$`. An array type keeps the original prefix through `L` and appends `;`. A null or empty referenced-class list leaves the text unchanged.
  - [P3] With `ensureUniqueMemberNames`, a program field or method whose descriptor changes is renamed to the old name plus the special member separator plus the hex of the absolute hash of the old descriptor, while `<init>` stays `<init>`. Library members are never renamed. An inner-class simple name is replaced from the inner class’s name after the last `$` when both the inner-class and inner-name indices are nonzero.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/CodeAttributeComposer.java`
- Bazel lines: 1123
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not compose JVM method bodies: no nested code fragments (32 levels), instruction and label append, optional shrink-then-widen, or old-to-new offset remap.
  - [P3] Branch and switch targets, exception ranges (empty ranges dropped; unmapped handlers negated when external handlers are allowed), line numbers (sorted insert, including the negative-line tie rule), stack-map and StackMapTable deltas, uninitialized `new` offsets, local-variable and local-variable-type tables (empty or past `max_locals` dropped), and type-annotation offset and local-variable targets are not rewritten into a `Code` attribute with updated max stack and locals.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/CodeAttributeEditor.java`
- Bazel lines: 1671
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor that accumulates per-offset pre-offset inserts, pre-instruction inserts, replacements, post-inserts, and deletes, with out-of-range offsets rejected and optional shrinking of added instructions.
  - [P3] No same-length in-place overwrite path, and no full rewrite that remaps branches, `tableswitch` and `lookupswitch` targets, and instruction lengths that depend on the new offset.
  - [P3] No remapping of exception ranges (dropping ranges whose start is no longer before the end), stack-map and stack-map-table deltas, uninitialized `new` offsets, line numbers, local-variable and local-variable-type ranges, or type-annotation offset and local-variable targets.
  - [P3] No zero-length label or catch pseudo-instructions whose branch targets are resolved after insertion, with catch entries prepended and then remapped like other handlers.
  - [P3] No post-edit instruction widening, and no optional update of maximum stack size and local-variable frame size even when the bytecode itself is unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/CodeAttributeEditorResetter.java`
- Bazel lines: 60
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No visitor holds a code-attribute editor and, on a code attribute, resets that editor to the attribute’s code length.
  - [P3] Visits of every other attribute are no-ops in Bazel; zel has no equivalent visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/CompactCodeAttributeComposer.java`
- Bazel lines: 2075
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no fluent code-attribute composer bound to a target class. The Java type stacks code fragments (maximum old offset plus one), appends instructions, labels, exceptions, and line numbers, and `visitCodeAttribute` copies that fragment into the visited attribute. `visitAnyAttribute` does nothing. The one-argument constructor turns off external branch targets and external exception handlers and turns on instruction shrinking. Those three flags are otherwise passed through to the wrapped composer.
  - [P3] No opcode appenders that intern constants before emitting the instruction. `ldc`, `ldc_w`, and `ldc2_w` add integer, float, long, double, string, class, or primitive-array constants. Field, method, and interface-method ops add the matching ref, and `invokedynamic` adds an invoke-dynamic constant. `invokeinterface` stores the descriptor’s argument-word count shifted left by 8. `invokestaticinterface` emits `invokestatic` against an interface methodref. Generic `iconst`, `lconst`, `fconst`, and `dconst` emit the `*_0` opcode with an embedded constant and rely on later shrinking.
  - [P3] No compact pushes. `pushInt` uses `iconst` for −1 through 5, `bipush` for other bytes, `sipush` for other shorts, and `ldc` otherwise. `pushFloat` uses `fconst` only for 0f and 1f. `pushLong` and `pushDouble` use the const form only for 0 and 1. `pushPrimitive` maps boolean to `iconst_0` or `iconst_1`, byte to `bipush`, short to `sipush`, char, int, and float to `ldc`, long and double to `ldc2_w`, and throws `IllegalArgumentException` for any other type tag. `pushNewArray` pushes the length and then emits `newarray` or `anewarray`.
  - [P3] No type-dispatched `load`, `store`, `storeToArray`, or `return_`. Boolean, byte, char, short, and int share the int opcode. Long, float, and double have their own. `V` is a void return. Every other type tag is a reference load, store, array store, or `areturn`.
  - [P3] No debug sequences that duplicate the top int, long, String, Object, or Throwable and print it, or that print a message first, via `System.err`, or that call `Throwable.printStackTrace`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ComparableConstant.java`
- Bazel lines: 328
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no comparable constant-pool wrapper that stores the owning class, the original index, and the constant, then orders entries by a fixed tag priority rather than the raw JVM tag. That order is Integer, Float, String, Class, Long, Double, Fieldref, Methodref, InterfaceMethodref, Dynamic, InvokeDynamic, MethodHandle, NameAndType, MethodType, Module, Package, Utf8, PrimitiveArray. Unassigned tags in `0..21` share Integer’s priority `0`, and a tag of `22` or higher throws.
  - [P3] When the two wrappers hold the same `Constant` instance, the order is the stored indices. Otherwise the same tag is compared by contents: `int`/`long`/`float`/`double` with signed `compare` (`-0` before `+0`, `NaN` after every value); primitive arrays by type character, then element-wise (non-boolean/byte/char/short/int/float/long values treated as `double[]`), then length; strings and Utf8 by UTF-16 `String` order, resolving both strings through the left-hand class; Dynamic and InvokeDynamic by bootstrap-method index, then name, then descriptor; method handles by reference kind, then class, name, and descriptor; field, method, and interface-method refs by class, name, and descriptor; class, module, and package by name; method type by descriptor; name-and-type by name then descriptor.
  - [P3] Equality requires a non-null wrapper of the same class, a constant of the same runtime class, and `compareTo == 0`. Every instance hashes to the wrapper class only. Comparison writes the other constant and the result onto the receiver.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ConstantAdder.java`
- Bazel lines: 312
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that copies a source-class constant into a target class constant pool and exposes the index of the most recently added constant, either by pool index or by the constant object.
  - [P3] No copy of integer, long, float, double, primitive-array, UTF-8, string (with its referenced class and member), module, package, class (with its referenced class), name-and-type, or method-type (passing the referenced-classes array through) constants.
  - [P3] No recursive field, method, or interface-method reference copy that first adds the class constant, then adds the member reference using that new class index plus the original name, descriptor, referenced class, and referenced member.
  - [P3] No method-handle copy that first copies the referenced field or method constant, then records the handle with the original reference kind and that new index.
  - [P3] No dynamic or invokedynamic copy that looks up `BootstrapMethods` on the source `ProgramClass`, recursively copies the name-and-type, shallow-copies a non-null referenced-classes array, copies that bootstrap method onto the target, and records the new bootstrap-method index with the new name-and-type index.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ConstantPoolEditor.java`
- Bazel lines: 944
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor bound to one program class that finds or appends constant-pool entries. The optional form installs class and member reference initialization only when a program class pool is supplied.
  - [P3] Integer, long, float, and double entries are reused on the first in-range match of tag and value (Java `==`, so NaNs never match and `+0` matches `-0`). Primitive-array constants are always appended with no search.
  - [P3] String entries match only when the UTF-8 index is in range, the text is equal, and the referenced class and member are the same objects. A new string first adds that UTF-8.
  - [P3] `Dynamic` and `InvokeDynamic` match only the bootstrap-method index and the name-and-type index. Method handles match only the reference kind and reference index. The name-and-descriptor `InvokeDynamic` overload adds a name-and-type constant first.
  - [P3] Module and package entries match by resolved name, then add a UTF-8 and the constant. Class entries match when the name index is in range and the name is equal. Method types match the descriptor string the same way. Name-and-type entries require both indices in range and both strings equal. The referenced class or classes are stored on new entries and are not part of those matches.
  - [P3] Field, method, and interface-method refs have overloads from a class plus member, from names, and from pool indices. The index form matches only the class index and the name-and-type index. Name forms first ensure the class and name-and-type constants exist.
  - [P3] UTF-8 entries match by string equality. Appending grows the pool when fewer than two spare slots remain, copies only the live prefix, then publishes the new array. Long and double also occupy a following null slot. The returned index is the new entry’s slot. A configured initializer runs on the constant after it is inserted.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ConstantPoolRemapper.java`
- Bazel lines: 823
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that rewrites every constant-pool index in a program class from a caller-supplied old-to-new map, throwing when the mapped index is negative, while leaving library classes, fields, and methods unchanged.
  - [P3] No remapping of class this/super/interfaces, member name and descriptor indices, or the internal indices of string, class, module, package, name-and-type, method-type, method-handle, dynamic, invoke-dynamic, field, method, and interface-method constants. Integer, long, float, double, primitive-array, and UTF-8 constants stay untouched.
  - [P3] No attribute walk that remaps attribute-name indices plus source file/dir, signature, constant value, exceptions, nest host/members, module (version only when non-zero), module main class, module packages, bootstrap-method handles and arguments, enclosing method, and method-parameter names.
  - [P3] No inner-class, exception catch-type, or module-version remaps that skip a zero index, and no requires, exports, opens, or provides entry remaps.
  - [P3] No code-attribute pass that replaces only constant instructions whose index changed, applies those edits with stack-map frame sizes left unchanged and replacement instructions shrunk, then remaps exception and nested attribute indices.
  - [P3] No stack-map or stack-map-table remapping of object verification-type class indices on same-one, more-zero, and full frames, and no local-variable or local-variable-type name, descriptor, and signature remaps.
  - [P3] No annotation, parameter-annotation, or annotation-default remapping of type indices and constant, enum, class, nested-annotation, and array element values, including their element-name indices.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ConstantPoolShrinker.java`
- Bazel lines: 730
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no pass that marks a program class’s reachable constant-pool entries, compacts the pool in place, and remaps references only when the pool count decreases.
  - [P3] Nothing marks `this`, the superclass, interfaces, each member’s name and descriptor, or the attribute, instruction, annotation, module, nest, bootstrap, stack-map, and local-variable references this visitor walks. Optional indices of 0 are left unmarked, and `Dynamic` and `InvokeDynamic` entries also mark every class attribute so `BootstrapMethods` stays live.
  - [P3] Nothing keeps slot 0, lets a null second half of a `long` or `double` inherit the previous entry’s used flag, maps unused indices to -1, shifts used entries together, and nulls the tail.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ConstantPoolSorter.java`
- Bazel lines: 123
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Nothing in zel sorts a program class constant pool. Non-null entries from index 1 are ordered by tag priority and then by contents, while two wrappers of the same `Constant` instance are ordered by their old index and are not merged.
  - [P3] Content-equal entries collapse onto the first sorted copy. That copy’s new index is recorded for every duplicate, and `Long` and `Double` entries take a following null slot.
  - [P3] The sorted prefix replaces the pool, the tail is cleared, `u2constantPoolCount` is updated, and constant-pool references in the class, pool, fields, methods, and attributes are remapped. Library classes are left unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ElementValueAdder.java`
- Bazel lines: 216
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never copies a visited annotation element value into an annotation-default attribute, an annotation, or an array element value.
  - [P3] Constant, enum, and class element values are not duplicated with constant-pool indices remapped into the target class (a source name index of 0 stays 0) while referenced class and method links are kept.
  - [P3] Nested annotation values are not rebuilt through a nested annotation copy, and array values are not rebuilt by a nested copy that appends into a new array (or a shared empty array when the source count is 0).
  - [P3] An annotation-default target is not overwritten with the finished value, and annotation or array targets are not added or replaced according to the constructor flag (that flag is ignored for the default-attribute target).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ElementValuesEditor.java`
- Bazel lines: 237
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No editor adds or deletes element values on either an annotation or an array element value.
  - [P3] No replace-by-method-name overwrite that keeps the element count, and no append that grows the array by one and increments the count.
  - [P3] No delete-by-method-name that shifts the tail down, nulls the last slot, and decrements the count, or leaves the count unchanged when the name is absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ExceptionAdder.java`
- Bazel lines: 65
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant visitor that, for each visited class constant, copies that constant into a target class constant pool and appends the resulting pool index to a target exceptions attribute.
  - [P3] Other constant kinds are ignored. Nothing in Zel binds those two steps to a specific target class and exceptions attribute.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ExceptionInfoAdder.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Nothing in zel copies a visited exception handler into a target code attribute, preserving start, end, and handler program counters.
  - [P3] A zero catch-type index stays zero (catch-all). Any other catch type is copied into the target class constant pool and the new handler stores that remapped index.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ExceptionInfoEditor.java`
- Bazel lines: 91
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor bound to a code attribute that prepends an already-filled exception-table entry at index 0, allocating a new table of exactly the new length and shifting existing entries when capacity is short, otherwise shifting them one slot right in place, then storing the new length.
  - [P3] No append that writes the entry at the current logical length, reusing spare array capacity or replacing the table with one sized to length plus one, and increments the stored exception-table length.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ExceptionsAttributeEditor.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor that appends a constant-pool exception index to an exceptions attribute.
  - [P3] The Java editor, when the index array’s capacity is less than or equal to the used length, allocates a new array one slot larger, copies the existing indices, then writes the new index and increments the length. There is no duplicate check, sorting, or constant-pool validation.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/InnerClassesAccessFixer.java`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] For each `InnerClassesInfo`, the visitor starts from `u2innerClassAccessFlags`, resolves the inner-class `ClassConstant`, and when the referenced class is a `ProgramClass` writes back `AccessUtil.replaceAccessFlags` of those flags and `programClass.u2accessFlags`.
  - [P3] A library class, an unresolved reference, or a non-class constant leaves the inner-class access flags unchanged.
  - [P3] `replaceAccessFlags` clears `ACC_FINAL` only when the program-class flags word equals `ACC_PRIVATE` exactly, then replaces only the public/private/protected bits and keeps every other inner-class flag. Zel has no class-file pass that updates `InnerClasses` access flags this way.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/InnerClassesAttributeEditor.java`
- Bazel lines: 91
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor bound to an InnerClasses attribute that appends an inner-classes entry at the current logical end, growing the backing array to exactly that new length when it is shorter, returning that pre-increment index.
  - [P3] Zel has no removal that decrements the logical count before an identity scan of only that shortened prefix, then shifts the tail down and nulls the slot at the old length. A miss returns the decremented count, so it drops the last entry; an empty table passes index -1 into the shift.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/InstructionAdder.java`
- Bazel lines: 75
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instruction visitor that appends each visited instruction into a target code attribute at the original bytecode offset.
  - [P3] Simple, variable, branch, and switch instructions are appended as the same instruction object. Constant instructions are copied with the same opcode and resolved constant, while the constant-pool index is replaced by the index from adding that constant into the target class.
  - [P3] Zel's ProGuard flags and allowlister do not copy bytecode or remap constant-pool indices.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/InstructionSequenceBuilder.java`
- Bazel lines: 1986
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no builder that accumulates JVM instructions and constant-pool entries, then returns the instruction list and the copied pool while clearing both.
  - [P3] Opcode appenders are absent, including loads and stores, arithmetic, conversions, branches, `tableswitch`/`lookupswitch`, field and method invokes, `invokedynamic`, array and object creation, monitors, and `wide`.
  - [P3] Convenience sequences are absent: efficient `pushInt`/`pushFloat`/`pushLong`/`pushDouble`, typed `pushPrimitive`, `pushNewArray`, and typed `load`/`store`/`storeToArray`.
  - [P3] `invokeinterface` does not encode the argument-word count into the instruction constant, and there is no constant-pool editor that interns integer, float, long, double, string, class, field, method, interface-method, and invokedynamic constants into a target class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/InstructionWriter.java`
- Bazel lines: 320
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Writes each visited instruction into the code attribute. Simple and switch instructions are written as-is. Constant, variable, and branch instructions that throw `IllegalArgumentException` because an operand does not fit are zeroed and written as a dummy, and a later edit replaces them.
  - [P3] A constant that does not fit is replaced by the same opcode, constant-pool index, and constant. A variable that does not fit is replaced by the same opcode, variable index, and constant with the wide flag set when the index or constant needs more than one byte. The editor is created with frame-size updates off and instruction shrinking on, so `ldc` becomes `ldc_w`, and `goto`/`jsr` become `goto_w`/`jsr_w`, only when the operand requires it.
  - [P3] Conditional branches with no wide form (`ifeq`/`ifne`, `iflt`/`ifge`, `ifgt`/`ifle`, the `ificmp*` and `ifacmp*` pairs) insert the complementary opcode `((opcode + 1) ^ 1) - 1` with a fixed offset of 8, then replace the original with `goto_w` carrying the original branch offset. `ifnull`/`ifnonnull` use `opcode ^ 1` the same way. Other branches are replaced by the same opcode and offset.
  - [P3] `reset` and `extend` store the anticipated code length and forward to the editor only if one already exists. The editor is created lazily on the first insert or replace and reset to that length. Visiting the code attribute applies the collected edits once and then drops the editor. Zel has no bytecode pass that widens instructions this way.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/InterfaceAdder.java`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant visitor bound to a target program class that, on a class constant, copies that class (name and referenced class) into the target constant pool and then adds the resulting pool index to the target interface list.
  - [P3] The Java add is skipped when that exact pool index is already in the interface array. Any other constant kind is not handled and throws `UnsupportedOperationException`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/InterfaceDeleter.java`
- Bazel lines: 209
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On a program class, interfaces whose parallel `delete` flag is false are packed in place to the front of `u2interfaces`, in order, and `u2interfacesCount` is updated only when that count shrinks. The array is not reallocated, and a class with nothing removed is left unchanged, including its attributes. A library class is not edited and hits `UnsupportedOperationException`.
  - [P3] After at least one interface is removed, each class `Signature` attribute with a non-null `referencedClasses` array is rebuilt: formal type parameters and the superclass type are always kept, and each following interface type is dropped when the same `delete` flag is true. Referenced-class slots move forward by `DescriptorClassEnumeration.classCount()` for kept fragments and are skipped for dropped interface types, then the vacated range up to the old scan index is set to null. The new text is stored in the existing `Utf8` constant (`setString` clears the byte cache and leaves `u2signatureIndex` unchanged). A signature with null `referencedClasses`, and every other attribute, is left as it is.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/InterfaceSorter.java`
- Bazel lines: 246
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On a program class with more than one interface, the used prefix of `u2interfaces` is sorted by constant-pool index. A count of 0 or 1, and every library class, is left unchanged. Zel has no class-file pass that sorts interface indices.
  - [P3] Signature attributes are then rebuilt: formal type parameters and the superclass type stay in place, and interface types are emitted in that sorted interface-name order by matching each interface’s binary name to the first class name in a signature interface type. Signature interface types that do not match any interface are dropped. The Utf8 constant is overwritten in place only when the string changes, and a non-null `referencedClasses` array is replaced by a same-length copy whose unused tail is null.
  - [P3] After that rewrite, consecutive equal interface indices are removed and the count is compacted. When `referencedClasses` is non-null, the signature is rewritten again by dropping the interface types at those deleted positions. Equal interface names stored under different constant-pool indices are kept.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/InterfacesEditor.java`
- Bazel lines: 108
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor bound to a class that appends a constant-pool interface index only when that index is absent from the logical interface prefix, reusing spare array capacity or replacing the array with one sized to the new length, then incrementing the stored interface count.
  - [P3] No delete that, on the first exact index match, decrements the count, shifts the later logical entries one slot left, and writes 0 into the slot just past the new count, leaving the array length unchanged and doing nothing when the index is absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/LineNumberInfoAdder.java`
- Bazel lines: 91
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that copies every line-number entry from a visited line-number table into a target table as an extended entry.
  - [P3] On a line-number table, the source fallback is `className + '.' + methodName + methodDescriptor + ':' + lowestNullSourceLine + ':' + highestNullSourceLine`. Lowest and highest consider only entries whose source is null; with none, both are 0. Each entry keeps its own non-null source, otherwise that fallback, and copies `startPC` and line number unchanged.
  - [P3] Other attributes are ignored. Entries are appended in table order. When the target array is full, it is replaced by a copy one slot longer before the new entry is stored and the length is incremented.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/LineNumberTableAttributeEditor.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor that appends an already-filled line-number entry to a line number table attribute.
  - [P3] The Java editor, when the entry array’s capacity is less than or equal to the used length, allocates a new array one slot larger, copies the existing entries, then writes the new entry at the current length and increments that length. Spare capacity is reused in place. There is no sort by start PC, duplicate collapse, or validation of the line number.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/LineNumberTableAttributeTrimmer.java`
- Bazel lines: 78
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that compacts a code attribute's line-number table in place: keep an entry only when its start PC is inside the code, it is the last entry or its start PC is strictly less than the next entry's, and it is the first entry or its line number differs from the previous entry's; then null the dropped slots and set the table length to the number kept.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/LocalVariableInfoAdder.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that copies each visited local-variable entry into a target local-variable table.
  - [P3] Each visit copies `u2startPC`, `u2length`, and `u2index` unchanged, replaces the name and descriptor indices with the indices from adding those source-class constants to the target class, and copies `referencedClass`. `visitorInfo` stays unset.
  - [P3] The new entry is appended and the table length is incremented, with no duplicate, overlap, or ordering check. A library source class does not supply pool entries, so those two indices keep the constant adder’s previous result (0 until a program-class constant has been copied).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/LocalVariableTableAttributeEditor.java`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor that appends a `LocalVariableInfo` to a local variable table attribute.
  - [P3] The Java editor writes the given info at the current `u2localVariableTableLength` and increments that length by one. When the backing array’s capacity is less than or equal to that length, it allocates a new array one slot larger and copies the existing entries; otherwise it reuses the array. The info is stored as-is. There is no duplicate check, sorting, range check, constant-pool remapping, or update of the attribute’s byte length.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/LocalVariableTypeInfoAdder.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Nothing copies each visited local-variable type entry into a target local-variable type table: the new entry keeps start PC, length, and slot, remaps the name and signature constants into the target class, aliases `referencedClasses` without cloning, and leaves visitor info unset.
  - [P3] Nothing appends that entry at the current table length, growing the array when it is full, and increments the stored local-variable type count.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/LocalVariableTypeTableAttributeEditor.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor bound to a local variable type table attribute that appends an already-filled local-variable type entry.
  - [P3] The Java editor writes the entry at the current logical length, reusing the table when its capacity is at least the new length and otherwise replacing it with an array of exactly that length after copying the old entries, then increments the stored length. It does not check duplicates, sort, or validate the entry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/MemberAdder.java`
- Bazel lines: 401
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that copies each visited program field into a target class: optional name transform, unchanged access flags, new UTF-8 name and descriptor, an attribute array sized to the source count or a shared empty array, the source `referencedClass`, visitor info pointing at the source field, attributes copied with replacement off, then the field added and an optional extra visitor run. A field with the same name and descriptor is still added; the conflict path is commented out.
  - [P3] For methods, a same name and descriptor on the target is skipped when the source is abstract. When the target method is abstract, its access flags are replaced by the source flags with `ACC_FINAL` cleared and its attributes are copied with replacement on, and no new method is added and the extra visitor is not called. Two concrete methods still produce a second method; the rename path is commented out.
  - [P3] A copied method always drops `ACC_FINAL`, clones `referencedClasses` or keeps null, sets visitor info to the source method, copies attributes with replacement off, adds the method, and then runs the extra visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/MemberReferenceFixer.java`
- Bazel lines: 485
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On each program class, the constant pool is walked from index 1, skipping null slots. A `String` whose referenced member is known is rewritten to that member’s current name. A field, method, or interface-method reference with a known member is retargeted to a found-or-created `NameAndType` when the name or descriptor differs. Zel has no class-file pass that rewrites these references after a member rename.
  - [P3] A known interface-method reference whose class entry is a non-interface program class, or an array type (`length > 1` and leading `[`), is replaced at the same pool index by a `Methodref` that keeps the class index, the possibly updated name-and-type index, and the referenced class and member. A known method reference whose class has `ACC_INTERFACE` is replaced by an `InterfaceMethodref` the same way. An unresolved non-array class leaves the tag unchanged. Field references and unknown members are not retagged.
  - [P3] `u2maxStack` is recomputed for every `Code` attribute in the class only when some method or interface-method name or descriptor actually changed. A tag-only change, a field or string update, or an enclosing-method update does not trigger that recomputation.
  - [P3] An `EnclosingMethod` attribute with a known method gets a new name-and-type when either the name or the descriptor differs. Annotation, parameter-annotation, annotation-default, nested-annotation, and array element values update only the element name to the referenced method’s current name, not its descriptor. On Android, an enum element whose referenced field is known also updates the constant-name Utf8 to that field’s name via `referencedClasses[0]`. Other targets leave the original enum constant name in place.
  - [P3] Other constants and attributes are left unchanged, and library classes are not visited.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/MemberRemover.java`
- Bazel lines: 95
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no two-step visitor that records `ProgramField` and `ProgramMethod` instances in separate sets, then, only when a `ProgramClass` is visited, deletes those members from that class and clears both sets. Library classes and library members are ignored.
  - [P3] Collection is by member object identity. Visiting the same program field or method again does not enqueue a second removal. Methods are removed before fields, and `HashSet` iteration does not preserve visit order.
  - [P3] The sets are not tied to the class that was visited during collection. The next `ProgramClass` visit removes every still-pending member from that class, then empties both sets, so a later class visit removes nothing unless more members are collected.
  - [P3] Each removal looks up the member with identity equality, shifts the later field or method array entries down, nulls the last slot, and decrements the count. A collected member that is not in the visited class makes that shift use a negative length and fails before the corresponding set is cleared.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/MethodInvocationFixer.java`
- Bazel lines: 245
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On each code attribute, a resolved method reference is rewritten in place, keeping its constant-pool index. Unresolved references and non-method instructions are left unchanged. An array owner uses the resolved class (`java.lang.Object`); any other owner is the class named by the method reference. The member, its flags, and its name and descriptor come from the class where the member was found.
  - [P3] A static method whose opcode is not `invokestatic` becomes `invokestatic`. This is checked before the private, `<init>`, interface, and virtual cases.
  - [P3] A private method or `<init>` whose opcode is not `invokespecial` becomes `invokespecial` only when the caller and the referenced class are not nestmates (same class, or the same nest-host name, defaulting to the class itself when there is no `NestHost` attribute). A nestmate call, and any call that is already `invokespecial`, is left unchanged and is not later rewritten as interface or virtual.
  - [P3] For a non-static, non-private, non-`<init>` method whose referenced class is an interface, `invokespecial` of a non-abstract method is preserved. Every other opcode, and `invokeinterface` whose count operand is not the stack-slot size of the descriptor plus the receiver (`long` and `double` count as two) shifted left eight bits, becomes `invokeinterface` with that count.
  - [P3] Any other instance method becomes `invokevirtual`, except an existing `invokevirtual` and an `invokespecial` from a different class that extends the referenced class. Same-class `invokespecial` and `invokespecial` to a non-superclass become `invokevirtual`. Zel has no class-file pass that rewrites these invocation opcodes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/NameAndTypeShrinker.java`
- Bazel lines: 195
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never marks `NameAndType` constant-pool entries referenced by ref and `InvokeDynamic` constants, or by `EnclosingMethod` attributes whose name-and-type index is nonzero.
  - [P3] Zel never drops unmarked `NameAndType` entries, compacts the pool (keeping a null long/double second slot with the preceding entry), fills a trailing range with null, or remaps constant-pool indexes when the pool shrinks.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/NamedAttributeDeleter.java`
- Bazel lines: 81
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that deletes the first class-file attribute of a given name from program classes, program members, or code attributes, while leaving library classes and library members unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/NestMemberAdder.java`
- Bazel lines: 81
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor that appends classes onto a target class's NestMembers attribute.
  - [P3] A visited class constant is always appended: the target constant pool is searched for an existing Class entry with the same name (a hit is reused and its referenced class is left unchanged; otherwise a Utf8 name and a new Class entry are created), and that index is written at `u2classes[u2classesCount]` with the count incremented. Other constant tags are ignored.
  - [P3] Visiting a class does the same append using that class's name, with no deduplication of nest-member indices.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ParameterAnnotationsAttributeEditor.java`
- Bazel lines: 91
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor bound to one parameter-annotations attribute that appends a pre-filled annotation onto one existing parameter's list, growing that parameter's backing array to exactly the new count when it is already full, and incrementing that parameter's annotation count.
  - [P3] No deletion by object identity (the same annotation instance, not equal contents) within one parameter's list: later entries shift down through the physical array, the vacated logical tail slot is nulled, and that parameter's count decrements.
  - [P3] No index deletion of that same form. A missing annotation is resolved to index -1 and then removed at that index.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/ParameterInfoAdder.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that copies a method parameter into a target method-parameters attribute at the visited parameter index.
  - [P3] When the source name index is 0, the Java adder stores name index 0 and does not copy a constant. Otherwise it copies that constant into the target class constant pool and stores the new index, keeping the original access flags. It overwrites that slot and does not grow the parameters array.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/SimplifiedClassEditor.java`
- Bazel lines: 517
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no editor that builds a version-1.2 program class (initial constant-pool capacity 10, this-class constant, optional superclass constant) or wraps an existing class with a class editor, constant-pool editor, and injected-member name factory.
  - [P3] `finishEditing` only finalizes deferred method composers. The class-pool overload also runs superclass and subclass hierarchy initialization when a superclass name was supplied at construction, then always initializes references against the program and library pools.
  - [P3] Adding a field inserts a field with the given access flags and UTF-8 name and descriptor and no attributes. The composer `addMethod` creates the method and opens a code fragment of the given maximum length, but the method and its `Code` attribute are attached only when that composer is finished.
  - [P3] The instruction-array `addMethod` attaches the method immediately. It writes the given instructions first, then any prefix instructions, then the trailing instruction, so the documented "front" instructions actually follow the main body.
  - [P3] Static-initializer editing creates `<clinit>()V` ending in `return` when none exists. Otherwise it either inserts the instructions at offset 0 of the existing `<clinit>`, or adds a uniquely named static `()V` (also ending in `return`) and inserts an `invokestatic` of that method at offset 0.
  - [P3] Instance-initializer editing, when no `<init>` exists, adds public `<init>()V` whose body is the given instructions, then `aload_0` plus `invokespecial` of the superclass `<init>()V`, then `return`. One super-calling constructor gets the instructions inserted at offset 0. Any other count creates or reuses private `init$()V` and, only on creation, inserts `aload_0` plus `invokespecial init$()V` at offset 0 of every super-calling constructor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/StackSizeUpdater.java`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that, for a code attribute, recomputes operand-stack depths and writes that maximum into the attribute's max-stack field, leaving every other attribute unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/SubclassAdder.java`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that appends one configured subclass onto every program class and every library class it visits.
  - [P3] Each visit grows that class’s subclass array by one and writes the subclass in the new last slot. There is no duplicate check, null check, or different treatment of program classes and library classes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/SubclassToAdder.java`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A `SubclassToAdder` stores one target `Clazz`. `visitProgramClass` and `visitLibraryClass` both call `targetClass.addSubClass` on the visited class, with no superclass check, no deduplication, and no program-versus-library distinction.
  - [P3] `addSubClass` appends the visited class: a null subclass array becomes a one-element array, and an existing array is copied into a new array one slot longer with the visitor stored at the end.
  - [P3] Zel has no class-file visitor that records visited classes as subclasses of a target class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/TargetInfoCopier.java`
- Bazel lines: 161
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that replaces one fixed type annotation’s target info with a new object copied from the visited target. The stored target class is never read, so nothing is remapped into that class, including the supertype index.
  - [P3] Each visit overwrites the previous target info and ignores the visited class, member, method, code attribute, and source annotation. Copied fields are: type parameter (target type, type-parameter index); supertype (target type, supertype index unchanged); type-parameter bound (target type, type-parameter index, bound index); empty (target type only); formal parameter (target type, formal-parameter index); throws (target type, throws-type index); catch (target type, exception-table index); offset (target type, offset); type argument (target type, offset, type-argument index).
  - [P3] A local-variable target is deep-copied: a new table whose length is `u2tableLength` (the length field, not the array length), each slot a new element with the same start PC, length, and local index. A length past the source array throws before the annotation is updated; a shorter length drops the extra entries.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/TypeAnnotationAdder.java`
- Bazel lines: 131
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that copies each visited type annotation into either a target annotation element value or an annotations attribute.
  - [P3] The annotation type constant is not remapped into the target class, element values are not copied, target info is not copied, and the type path (kind and argument index) is not deep-copied.
  - [P3] A completed annotation is not written as the element value’s annotation when that target is set, and is not appended to the annotations attribute otherwise.
  - [P3] The source annotation’s referenced classes are not shared onto the copy.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/Utf8Shrinker.java`
- Bazel lines: 514
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On a program class, marks every UTF-8 constant still referenced from the constant pool, fields, methods, and attributes, then compacts the pool. Remapping runs only when the new count is smaller than `u2constantPoolCount`.
  - [P3] String, class, and name-and-type constants mark their UTF-8 indices. Every other constant is ignored, because its strings are reached through those entries. Field and method name and descriptor indices are always marked.
  - [P3] Attribute visitors mark the attribute-name UTF-8, plus source file, source dir, signature, local-variable name and descriptor, local-variable-type name and signature, and annotation type indices. Enclosing-method class and name-and-type indices are not marked again. Nest, module, deprecated, synthetic, constant-value, exceptions, code, stack-map, and line-number attributes mark only the attribute name, aside from nested attributes.
  - [P3] Inner-class names, method-parameter names, and annotation element names are marked only when the index is non-zero. A constant element value marks its constant index only for the string tag. Enum, class, nested-annotation, and array element values mark their UTF-8 indices and recurse.
  - [P3] Compaction starts at index 1. A non-UTF-8 entry is always kept. An unmarked UTF-8 entry is dropped and mapped to -1. A null slot, the second half of a long or double, keeps the previous used flag. Kept entries are shifted down, the index map records the new index, and the tail of the pool is set to null. Zel has no class-file pass that drops unused UTF-8 constants this way.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/VariableCleaner.java`
- Bazel lines: 328
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no code-attribute pass that drops local-variable and local-variable-type entries whose index is outside `[0, maxLocals)`, or whose `startPC` is nonzero and `length` is 0, while keeping `startPC == 0` entries even when `length` is 0, then nulls the vacated tail.
  - [P3] It does not sort the surviving entries by `startPC`, index, length, descriptor (signature for types), then name, and walk that order backward, shortening each `length` so the range ends at the next same-index entry’s `startPC` (`Integer.MAX_VALUE` when none). Equal keys keep the later sorted entry’s length and set the earlier one to 0. Entries trimmed to length 0 stay in the table.
  - [P3] When both tables are present, it does not then walk the two sorted lists in order and copy the variable `length` onto a type only when both index and `startPC` match, leaving every later type unchanged once a type has no later variable match.
  - [P3] An empty table after the unused-entry removal is removed by deleting the first `LocalVariableTable` or `LocalVariableTypeTable` on that code attribute. Other attributes are left unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/VariableEditor.java`
- Bazel lines: 130
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no local-variable editor that accumulates deletions and applies them to code attributes.
  - [P3] Nothing implements `reset(maxLocals)` (grow or reuse a deletion mask, clear only the used prefix, clear the modified flag), `deleteVariable` (mark an index and set modified), or `isDeleted` (read that mask).
  - [P3] Nothing implements the code-attribute visit: skip when unmodified; otherwise compact surviving locals (deleted slots map to -1), remap the attribute through a variable remapper, and set `u2maxLocals` to the surviving count. Other attributes are ignored.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/VariableRemapper.java`
- Bazel lines: 218
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that remaps local-variable indexes from a caller-supplied `int[]` map, where a lookup is a direct array read and a deleted variable is represented by storing `-1`.
  - [P3] On a code attribute it first remaps nested attributes, then resets a code editor to the current code length, then replaces only variable instructions whose mapped index differs, keeping the original opcode and constant, and then applies that editor. It does not change `u2maxLocals` or touch non-variable instructions.
  - [P3] Local-variable table entries, local-variable type-table entries, and local-variable type-annotation target elements have their `u2index` overwritten with the mapped value. Other type-annotation targets and other attributes are left unchanged.
  - [P3] A method-parameters attribute is rebuilt as a new array of the same length, placing each old `ParameterInfo` at `map[oldIndex]`. Colliding destinations overwrite, unmapped slots stay null, and the parameter count is not updated.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/editor/VariableSizeUpdater.java`
- Bazel lines: 105
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On each code attribute, `u2maxLocals` is reset to the parameter slot count from the method descriptor and access flags: 0 when `ACC_STATIC` is set, otherwise 1 for the receiver; `J` and `D` take two slots, and arrays, object types, and every other argument type take one. Non-variable instructions do not change the count.
  - [P3] Each variable instruction then raises `u2maxLocals` to `variableIndex + 1`, or `variableIndex + 2` when the opcode is category 2 (`lload`, `dload`, `lstore`, `dstore`, and their `_0`–`_3` forms). A previously larger `u2maxLocals` is discarded when no variable instruction reaches that slot.
  - [P3] The same visit then runs `VariableCleaner` against the new `u2maxLocals`: it drops `LocalVariableTable` and `LocalVariableTypeTable` entries whose index is outside `[0, maxLocals)` or whose length is 0 with a nonzero start PC, sorts the rest and shortens overlapping ranges for the same index, copies those trimmed lengths onto type entries with the same index and start PC, and deletes a table that becomes empty. Zel has no class-file pass that recomputes `max_locals` or cleans those tables.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/BranchInstruction.java`
- Bazel lines: 184
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no branch-instruction model that stores an opcode and a branch offset relative to the instruction, or that copies those two fields.
  - [P3] Nothing canonicalizes `goto_w` to `goto` and `jsr_w` to `jsr`, leaving every other opcode unchanged.
  - [P3] Nothing shrinks a wide `goto`/`jsr` to the short form when the offset fits in a signed 16-bit value, widens `goto`/`goto_w` or `jsr` to `goto_w`/`jsr_w` when it does not, or rejects widening any other opcode.
  - [P3] Nothing reads or writes a signed offset of 4 bytes for `goto_w`/`jsr_w` and 2 bytes otherwise, refuses to write when the value needs more bytes than that encoding, or reports length as 1 plus that size.
  - [P3] Nothing dispatches a branch-instruction visitor or formats the mnemonic with a signed offset and an absolute target of `offset + branchOffset`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/ConstantInstruction.java`
- Bazel lines: 318
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant-pool instruction that stores an opcode, a constant-pool index, and an extra constant, with copy of those three fields, a canonical opcode that maps only `ldc_w` to `ldc`, and shrink that rewrites `ldc_w` to `ldc` when the index fits in an unsigned byte and `ldc` to `ldc_w` otherwise, leaving every other opcode unchanged.
  - [P3] No read or write of a 1-byte index for `ldc` and a 2-byte index for every other opcode, then a trailing operand of 1 byte for `multianewarray`, 2 bytes for `invokedynamic` and `invokeinterface`, and none otherwise. Length is one plus those sizes. Writing throws when the index needs more bytes than that encoding (unsigned byte, else unsigned short, else 4).
  - [P3] No stack adjustment on top of the fixed opcode table: `multianewarray` pops its dimension byte; `putstatic` and `putfield` pop the field type's category size; the five invoke opcodes pop the descriptor's parameter size counted as static (long and double as 2, no receiver); `getstatic`, `getfield`, and those invokes push the return or field type size (void 0, long and double 2, otherwise 1). Field, dynamic, invoke-dynamic, and method references are resolved through the name-and-type descriptor. Other constant kinds leave the cached deltas unchanged. `ldc` family push counts stay the fixed table values.
  - [P3] No visitor dispatch to a constant-instruction callback, and no text form of the opcode name, `#` plus the pool index, and the extra constant only when that operand is present.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/Instruction.java`
- Bazel lines: 1147
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no JVM instruction object with a stored opcode, canonical and actual opcode accessors, shrink-to-shortest form, byte length at an offset, or visitor dispatch over class, method, code attribute, and offset.
  - [P3] No writer that optionally prefixes `OP_WIDE` when the instruction is wide, then writes the actual opcode and trailing info into a code array or code attribute.
  - [P3] No opcode-indexed tables for whether an instruction may throw, whether it is category 2 (long or double), or the fixed stack pop and push counts, including category-2 slot accounting for loads, stores, arithmetic, dup and pop, comparisons, and returns. Field, invoke, and array-creation push counts stay zero in the fixed table so subclasses can supply them. The class argument on the stack-count methods is unused.
  - [P3] No big-endian unsigned and signed byte, short, and int readers and writers, including a value-size switch of 0, 1, 2, or 4. Unsigned writes reject values above `0xff` or `0xffff`. Signed byte and short writes reject values that do not fit. `readInt` sign-extends the high byte. Unsupported sizes throw.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/InstructionConstants.java`
- Bazel lines: 449
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not define the signed JVM opcode bytes from `nop` = 0 through `jsr_w` = -55 (unsigned 0x00–0xc9), including the wrap at `ior` = -128.
  - [P3] Zel does not provide the mnemonic table indexed by the unsigned opcode (0–201), from `"nop"` through `"jsr_w"`.
  - [P3] Zel does not define the `newarray` type tags boolean = 4, char = 5, float = 6, double = 7, byte = 8, short = 9, int = 10, and long = 11.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/InstructionFactory.java`
- Bazel lines: 314
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never reads a bytecode array at an offset, consumes a leading `wide` prefix, and builds an instruction whose kind is simple, constant-pool, variable (with that wide flag), branch, tableswitch, or lookupswitch.
  - [P3] The opcode-only constructor (wide forced off, opcode stored, operands not read) is absent.
  - [P3] Unknown opcodes are not rejected with an illegal-argument error that includes the opcode.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/InstructionUtil.java`
- Bazel lines: 95
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not map `newarray` type codes to internal type characters (4→`Z`, 5→`C`, 6→`F`, 7→`D`, 8→`B`, 9→`S`, 10→`I`, 11→`J`), and an unknown code does not fail with `IllegalArgumentException` `"Unknown array type [" + code + "]"`.
  - [P3] Zel does not map those internal primitive characters back to the same `newarray` codes, and an unknown character does not fail with `IllegalArgumentException` `"Unknown primitive: " + character`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/LookUpSwitchInstruction.java`
- Bazel lines: 135
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no JVM `lookupswitch` instruction: no case keys, default offset, or per-case jump offsets, and no field copy of those values.
  - [P3] No 4-byte-aligned read of the default offset, pair count, and match/offset pairs, and no padded write of the default offset, `cases.length`, and those pairs.
  - [P3] No length of `1 + (-(offset+1) & 3) + 8 + cases.length * 8`, and `shrink()` is not a no-op that returns the same instruction.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/SimpleInstruction.java`
- Bazel lines: 255
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no JVM simple-instruction model: opcode plus an immediate or opcode-embedded constant for `iconst_m1` through `iconst_5`, `lconst_*`, `fconst_*`, `dconst_*`, `bipush`, `sipush`, and `newarray`.
  - [P3] Missing canonical-opcode folding of those const and push forms onto `iconst_0`, `lconst_0`, `fconst_0`, or `dconst_0`, and shrink that rewrites the opcode from the constant (`iconst_0 + constant` when it fits in no extra bytes, otherwise `bipush` or `sipush`; typed const opcodes are base plus the constant).
  - [P3] Missing read and write of the signed immediate: embedded constants when the opcode has no extra bytes (`iconst_m1` is -1, `*_1` is 1, `iconst_2` and `fconst_2` are 2, `iconst_3` through `iconst_5` are 3 through 5, otherwise 0), length `1 + constant size` (`bipush` and `newarray` are 1 byte, `sipush` is 2, otherwise 0), and an invalid-constant-size error when the value needs more bytes than the opcode provides (0 bytes for -1 through 5, 1 if it fits in a byte, 2 if it fits in a short, otherwise 4).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/SwitchInstruction.java`
- Bazel lines: 83
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No switch-instruction value that stores an opcode, a default branch offset, and a jump-offset array.
  - [P3] No copy that overwrites those three fields and reuses the same jump-offset array.
  - [P3] No text form `[offset] <name> (target=<offset + defaultOffset>)`, or `<name> (<count> offsets, default=<defaultOffset>)`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/TableSwitchInstruction.java`
- Bazel lines: 139
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No tableswitch value with opcode, default offset, inclusive low/high case keys, and a dense jump-offset table of length `highCase - lowCase + 1`.
  - [P3] No 4-byte-aligned read or write of the default offset, low case, high case, and one big-endian jump offset per case, including zero padding on write.
  - [P3] No encoded length `1 + (-(offset+1) & 3) + 12 + n*4`, no-op shrink, field copy that shares the jump-offset array, or `visitTableSwitchInstruction` dispatch.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/VariableInstruction.java`
- Bazel lines: 372
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no JVM local-variable instruction: no wide flag, variable index, or `iinc` constant, and no constructors that embed `_1`–`_3` indexes or set wide from the required index or constant size.
  - [P3] No load/store classification (`opcode < istore` is a load, including `ret`; `opcode >= istore` or `iinc` is a store).
  - [P3] No canonical opcode that strips `_0`–`_3`, and no shrink that rewrites indexes 0–3 back to those short opcodes and recomputes wide.
  - [P3] No read/write of the index and signed constant, including embedded index decode, length `(wide ? 2 : 1) + index size + constant size`, or the illegal-argument checks when the stored index or constant does not fit the encoded size.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/visitor/AllInstructionVisitor.java`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that ignores every non-code attribute and, for a code attribute, forwards to a stored instruction visitor.
  - [P3] It does not walk the code array from offset 0 through `u4codeLength`, decoding each instruction, dispatching it with the class, method, code attribute, and offset, then advancing by that instruction’s length.
  - [P3] Nested code-attribute contents (exception table, stack map, line numbers, local variables) are not visited; only the instructions are.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/visitor/InstructionConstantVisitor.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No instruction visitor no-ops on every instruction except `ConstantInstruction`, which forwards that instruction’s `constantIndex` to a `ConstantVisitor` through `Clazz.constantPoolEntryAccept`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/visitor/InstructionCounter.java`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instruction visitor that starts at zero and increments a count by one on every visited instruction, ignoring class, method, code attribute, offset, and instruction, then returns that count.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/visitor/InstructionVisitor.java`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor over JVM instructions that dispatches on simple, variable, constant, branch, tableswitch, and lookupswitch, each with the enclosing class, method, code attribute, bytecode offset, and the instruction.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/instruction/visitor/MultiInstructionVisitor.java`
- Bazel lines: 112
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no composite instruction visitor that keeps an ordered list of instruction visitors and forwards every visit to each of them.
  - [P3] An empty visitor is not created with a fixed unused slot array and a zero count, and a varargs list is not stored by reference with the count set to that list’s length.
  - [P3] Visitors cannot be appended after construction, with the backing array grown and the visited prefix extended by one.
  - [P3] Simple, variable, constant, branch, tableswitch, and lookupswitch visits are not dispatched in insertion order over only the counted prefix, passing the same class, method, code attribute, offset, and instruction object to each visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/io/LibraryClassReader.java`
- Bazel lines: 433
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not read a class-file stream into a library class: magic and version checks, a constant pool that retains only `Class` and `Utf8` entries (with `Long`/`Double` taking two slots), and an exception on an unknown constant tag.
  - [P3] Zel does not stop after the access flags when non-public classes are skipped, nor resolve this, super (null when the index is 0), and interface names from that pool.
  - [P3] Zel does not keep only fields and methods at protected-or-higher (or package-or-higher) visibility, share empty member arrays, or skip class and member attributes by length.
  - [P3] Program-class and program-member visits, which are no-ops in Bazel, have no counterpart.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/io/ProgramClassReader.java`
- Bazel lines: 1377
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not read a class-file stream into a program-class model: magic and version checks, the constant pool (including the unused index 0, two-slot long/double entries, and ProGuard primitive-array constants), fields, methods, and attributes (code, stack maps, annotations, modules, and element values), or the unknown-tag failures for constants, verification types, annotation targets, and element values.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/io/ProgramClassWriter.java`
- Bazel lines: 1073
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not serialize a ProGuard `ProgramClass` to a JVM class file (magic, version, constant pool and primitive-array bodies, access flags masked to 16 bits, interfaces, fields, methods, and attributes with lengths computed from a temporary buffer).
  - [P3] Missing attribute and annotation bodies: unknown info, bootstrap methods, module/nest/inner-class records, code (max stack/locals, bytecode, exception table, nested attributes), StackMap versus StackMapTable frames, line-number overflow clamped to 0, local variables, type annotations and type paths, and element values (constant, enum, class, nested annotation, array).
  - [P3] Library classes and library members are silently skipped in Bazel; that no-op path is also absent. ProGuard mentions in zel are optimizer option defaults and `proguard_allowlister.py` config filtering, which do not write class files.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/io/RuntimeDataInput.java`
- Bazel lines: 223
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no wrapper that stores a `DataInput` and forwards `readBoolean`, `readByte`, `readChar`, `readDouble`, `readFloat`, both `readFully` overloads, `readInt`, `readLine`, `readLong`, `readShort`, `readUnsignedByte`, `readUnsignedShort`, `readUTF`, and `skipBytes`.
  - [P3] An `IOException` from any of those reads, including `EOFException`, is not turned into a `RuntimeException` whose message is only `ex.getMessage()` and whose cause is unset.
  - [P3] Successful calls do not return the delegate’s value unchanged (`boolean`, `byte`, `char`, `double`, `float`, `int`, `String`, `long`, `short`, or the `skipBytes` count).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/io/RuntimeDataOutput.java`
- Bazel lines: 261
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `DataOutput` adapter that rethrows `IOException` as a `RuntimeException` whose message is only `ex.getMessage()` (the original exception is not the cause).
  - [P3] Zel has no `writeUnsignedShort` check that rejects values outside `0..=65535` (`(v & 0xffff) != v`) with `IllegalArgumentException("Overflow of unsigned short value ["+v+"]")` before `writeShort`.
  - [P3] Zel has no `writeSignedShort` check that rejects values outside `-32768..=32767` (`(short)v != v`) with `IllegalArgumentException("Overflow of signed short value ["+v+"]")` before `writeShort`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/AccessUtil.java`
- Bazel lines: 105
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no mapping of JVM access flags (mask `ACC_PUBLIC|ACC_PRIVATE|ACC_PROTECTED`) to ordered levels private, package-visible, protected, and public, treating any value other than exactly private, protected, or public as package-visible.
  - [P3] Zel has no reverse mapping of those levels to `ACC_PRIVATE`, `0`, `ACC_PROTECTED`, and `ACC_PUBLIC`, with every other level returning `0`.
  - [P3] Zel has no replacement of only the public/private/protected bits that also clears `ACC_FINAL` when the new flags equal exactly `ACC_PRIVATE`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/AllParameterVisitor.java`
- Bazel lines: 256
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No member visitor walks every parameter of visited methods, or the type of visited fields, and dispatches a parameter visitor.
  - [P3] Non-static methods are not given an optional leading `this` parameter (class name as an internal type, the class itself as the referenced type), and the return type is never visited.
  - [P3] JVM descriptors are not parsed into parameter index, total count, slot offset, slot size (`long`/`double` take two slots), type substring, and referenced class (`null` when referenced classes are absent; class types and object arrays consume a referenced class, primitive arrays do not).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/ArrayInitializationMatcher.java`
- Bazel lines: 346
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no matcher that accepts a `newarray` whose length is a particular integer, optionally skips a dex2jar `astore`/`aload` of the same local and then treats that `aload` as the array producer, and requires each index `0..length-1` to be a push of that array, a particular integer equal to the index, a particular element, and the type-specific store (`bastore`, `castore`, `sastore`, `iastore`, `lastore`, `fastore`, `dastore`).
  - [P3] Zel does not record the initialization start (first instruction after `newarray` or after the pre-store pair) and end (offset of the last store), reject a match that does not advance past that anchor (including length 0), or materialize the typed primitive array (boolean from a nonzero int; narrowing casts for byte, char, and short).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/ArrayInitializationReplacer.java`
- Bazel lines: 200
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that rewrites methods whose code contains `NEWARRAY` into internal primitive array constants.
  - [P3] A code attribute is not partial-evaluated, and a `NEWARRAY` whose preceding instruction pushes exactly one stack value is not matched as an array-initialization sequence.
  - [P3] On a match, the length-pushing instruction is not replaced by `LDC` of a newly added primitive array constant, and both the `NEWARRAY` and every instruction from the matched start through end (inclusive) are not deleted.
  - [P3] Edits are not applied only when the code editor was modified, and code without any `NEWARRAY` is not skipped.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/ClassReferenceInitializer.java`
- Bazel lines: 610
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not walk program classes (constant pool, fields, methods, attributes) or library fields and methods to attach direct class and member references.
  - [P3] It does not resolve string, dynamic, invokedynamic, method-handle, class, method-type, or field/method references (array owners remapped to `java.lang.Object`, hierarchy member lookup, program-versus-library missing-member warnings).
  - [P3] It does not fill enclosing-method, local-variable, signature, or annotation and element-value references, including element methods found by name with a null descriptor.
  - [P3] It does not look up referenced types in the program pool and then the library pool, skip primitive arrays, or emit missing-class and library-depends-on-program-class warnings.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/ClassSubHierarchyInitializer.java`
- Bazel lines: 77
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Visiting a program class appends it to the subclass list of its resolved superclass and of each resolved interface (`u2interfacesCount` / `getInterface`). A null superclass or interface is skipped.
  - [P3] Visiting a library class does the same for `superClass` and for every entry of `interfaceClasses`, and skips the interface loop when that array is null.
  - [P3] Zel has no class-pool visitor or in-memory subclass lists. ProGuard shows up only as Android/Java flags that call an external tool.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/ClassSuperHierarchyInitializer.java`
- Bazel lines: 173
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that links a visited class to its superclass and interfaces. A program class is linked by setting `referencedClass` on its superclass class-constant (when the superclass index is non-zero) and on each interface class-constant. A library class is linked by storing `superClass` from a non-null `superClassName` and a new `interfaceClasses` array from a non-null `interfaceNames` array, one slot per name in index order. A null name or name array leaves the existing reference unchanged.
  - [P3] Lookup checks the program class pool first and the library class pool only on a miss. The program-pool class is returned when both pools contain the name. A missing class returns null and, when a missing-class printer is set, warns `Warning: <external referencing class>: can't find superclass or interface <external name>`. The same text is used for a superclass and an interface.
  - [P3] A program-pool hit with a dependency printer set warns `Warning: library class <external referencing class> extends or implements program class <external name>`, including when the referencing class is itself a program class. A library-pool hit prints nothing. Both printers are null in the two-argument constructor, so that path is silent and still returns the class or null.
  - [P3] External names in those warnings replace `/` with `.` and leave `$` unchanged. Each warning is passed to `WarningPrinter` with the referencing class name and the target name, so a filter that matches either name suppresses it.
  - [P3] Zel’s ProGuard surface is build options and an external tool (`proguard_top`, the allowlister). It does not fill these superclass or interface references.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/ClassUtil.java`
- Bazel lines: 1897
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no JVM class-file name and descriptor utility. Magic `0xCAFEBABE` is not checked, and combined versions are not packed as `(major << 16) | minor` or split with `>>> 16` and `& 0xffff`.
  - [P3] External versions `1.0`/`1.1` through `13`, including aliases `1.5`–`1.9`, are not mapped to internal versions. An unknown string becomes `0`, an unknown internal version becomes null, and versions outside `1.0`–`13` are not rejected with the major.minor and Java 13 maximum.
  - [P3] External class names are not turned into internal names by replacing `.` with `/`, or the reverse. Full class text is not access flags plus the external name, the short name is not the suffix after the last `.`, and an array base type is not the prefix before the first `[]`.
  - [P3] Internal array types are not detected as length greater than 1 and a leading `[`. Dimension counts do not walk leading `[`. `java/lang/Object`, `java/lang/Cloneable`, and `java/io/Serializable` are not treated as the interfaces implemented by every array.
  - [P3] Primitive types `ZBCSIJFD` are not recognized (void excluded). Category 2 is not only a one-character `J` or `D`. A class type is not “length greater than 1 and a trailing `;`”. Class names are not wrapped as `Lname;` with optional leading `[` dimensions, and extra dimensions are not prepended to an existing type.
  - [P3] Element types are not the suffix after the last `[`. Array descriptors stay arrays when converting between class-constant form and field type form; non-arrays drop or add `L`…`;`. Primitive descriptors, including `[`, are left unchanged when they are not class types, and a non-class type’s class name is null.
  - [P3] `V`, `Z`, `B`, `C`, `S`, `I`, `J`, `F`, `D`, and `[` are not mapped to `java/lang/Void`, `Boolean`, `Byte`, `Character`, `Short`, `Integer`, `Long`, `Float`, `Double`, and `java/lang/reflect/Array`, or back. Any other character or name is not rejected.
  - [P3] `<clinit>` and `<init>` are not recognized as initializers. A method return type is not the substring after `)`. Parameter counts do not start at 1 for instance methods, skip `[`, skip class names through `;`, and count every other argument character.
  - [P3] Argument stack size does not give `J` and `D` two slots, other arguments one, and a non-static receiver one more. Array arguments count as one slot after their brackets and optional class name are skipped. Static is `ACC_STATIC`.
  - [P3] Parameter number from a local-variable index does not return `0` for `this`, skip category-2 widths, and return `-1` when the index is not a parameter. Variable index from a parameter number does not start at 1 for instance methods and add each preceding type’s stack size.
  - [P3] External types such as `java.lang.Object[][]` and `int[]` are not converted to `[[Ljava/lang/Object;` and `[I`. Trailing `[]` sets the dimension count. `void`, `boolean`, `byte`, `char`, `short`, `int`, `float`, `long`, `double`, and `%` map to single characters; anything else is wrapped as a class type. The reverse appends `[]` and rejects an unknown type.
  - [P3] `Class.forName` form is not the external name of an array descriptor, or the external name of the class inside a non-array type. A descriptor is not a method descriptor solely because it starts with `(`. An external member is not a method solely because `(` occurs after index 0.
  - [P3] External return type plus `name(args)` or a list of external argument types is not assembled into an internal descriptor. Internal argument types are not concatenated between `(` and `)` with the return type after.
  - [P3] Field text is not access words, external type, and name. Method text is not access words, return type, name, and comma-separated arguments, with `<init>` shown as the short class name and no return type.
  - [P3] Class, field, method, parameter, module, requires, exports, and opens access flags are not rendered as prefixed modifier words. Zero flags stay empty. Class flags treat interface, enum, abstract, synthetic, and module as mutually exclusive, and annotation has no trailing space. Requires maps static-phase to `static`.
  - [P3] Internal and external package prefixes are not the substring through the separator found by `lastIndexOf` starting at `length - 2`. The package name is that prefix without the final separator, or empty when there is none.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/DescriptorClassEnumeration.java`
- Bazel lines: 252
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no enumerator that walks a JVM descriptor or signature and yields class names separated from surrounding fluff.
  - [P3] Missing `classCount`, which counts class names by scanning fluff and names and then resets the cursor.
  - [P3] Missing `nextFluff` handling of generic nesting (`<`/`>`), bounds (`:`), arrays (`[`), class start/end (`L`/`;`), signature inner-class dots (`.`), and type variables (`T…;`), including the type-parameter skip when nesting is 1.
  - [P3] Missing `nextClassName`, which stops at `<`, `;`, or `.` and recomposes inner names with `$` when the last fluff token was an inner-class separator.
  - [P3] Missing `isInnerClassName` for whether the most recently returned name was that recomposed inner class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/DynamicClassReferenceInitializer.java`
- Bazel lines: 486
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No bytecode walk resolves `Class.forName("…")` and javac/jikes `.class` string constants onto program or library classes.
  - [P3] No note for `(SomeClass)Class.forName(variable).newInstance()`, no missing-class note, and no warning when a library class dynamically depends on a program class.
  - [P3] Synthetic `class$` / `class` methods are not recognized by name or by matching their implementation instruction sequences.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/DynamicMemberReferenceInitializer.java`
- Bazel lines: 970
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not scan method code for constant `Class.get[Declared]{Field,Constructor,Method}` or `Atomic{Integer,Long,Reference}FieldUpdater.newUpdater` sequences, link those name strings to program or library members (hierarchy search versus declared-only, constructor notes instead of a filled reference), rewrite the loading `ldc` to a fresh string constant, or print dynamic-access notes with exception-matcher suppression and candidate members.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/EnumFieldReferenceInitializer.java`
- Bazel lines: 148
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not resolve an annotation enum-constant element value’s referenced field. ProGuard does that only when `referencedClasses` is non-empty and the first class is non-null: it scans that class’s `<clinit>` for `ldc`/`ldc_w` of the constant name followed by `putstatic` whose field type equals the enum type, keeps that field, and if none matches falls back to `findField(constantName, typeName)`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/ExternalTypeEnumeration.java`
- Bazel lines: 106
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no scan of the argument types in an external method descriptor such as `name(type, type)`. Bazel keeps the descriptor string and a cursor. The public constructor and package-private `setDescriptor` both store that string and reset the cursor. The package-private no-arg constructor leaves the descriptor null, so a later call throws `NullPointerException`.
  - [P3] `reset` places the cursor one past the first `(`. With no `(`, it throws `IllegalArgumentException` (`Missing opening parenthesis in descriptor [<descriptor>]`). A `(` at index 0 is accepted and yields an empty method name.
  - [P3] `hasMoreTypes` is true only while the cursor is strictly less than `descriptor.length() - 1`, so the last character is never a type character and a missing `)` is not reported here. `m()` yields nothing. `foo(a` yields nothing and does not throw. `m( )` yields one empty type because a non-final character remains.
  - [P3] `nextType` returns the trimmed slice from the cursor up to the next `,`, or else the next `)`, then advances one past that separator. Java `trim` removes characters `<= U+0020`. A `,` is taken even when it would sit inside a generic type. If neither separator exists, it throws `IllegalArgumentException` (`Missing closing parenthesis in descriptor [<descriptor>]`). `m(int,)` yields only `int`. `m(int, )` yields `int` and `""`.
  - [P3] `methodName` returns the trimmed substring before the first `(`, independent of the cursor. If `(` is absent it throws `StringIndexOutOfBoundsException` instead of the opening-parenthesis `IllegalArgumentException`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/InstructionSequenceMatcher.java`
- Bazel lines: 839
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bytecode visitor that matches a pattern instruction sequence with wildcard arguments (`X`–`R`), constant-pool equality (including the primitive-array `UnsupportedOperationException`), branch and jump offsets, or the failed-second-instruction retry.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/InternalTypeEnumeration.java`
- Bazel lines: 313
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not enumerate the types in a JVM internal descriptor or signature, starting after a leading formal type-parameter list when the string begins with `<`.
  - [P3] It does not report whether that `<...>` span exists, return it, or treat the descriptor as a method signature only when `(` is present.
  - [P3] It does not walk each type—arrays, `L...;` and `T...;` (including nested `<...>`), and other single-character types—stopping at `)` for methods and at the end of the string otherwise.
  - [P3] It does not count those types, or sum slot sizes with `long` and `double` as 2, `void` as 0, and every other type, including arrays, as 1.
  - [P3] It does not return the method return type as the substring after `)`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/MemberFinder.java`
- Bazel lines: 286
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no lookup that returns the first field, method, or member whose name and descriptor match in a class, optionally continuing through superclasses and interfaces.
  - [P3] Name and descriptor patterns containing `*` or `?` are not matched, and a referencing class is not used to reject members that are inaccessible, including private members outside the same nest.
  - [P3] The class that owns the most recently found member is not retained.
  - [P3] There is no check for a non-private method with the same name and descriptor in subclasses, and no equivalent check for a shadowed field.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/MethodLinker.java`
- Bazel lines: 159
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that links corresponding non-private, non-static, non-initializer methods across each visited class, its superclasses, and its interfaces.
  - [P3] Methods are not grouped by name plus descriptor, and previously linked chains are not merged so that a library member is placed at the end of the chain.
  - [P3] There is no walk along visitor info to the last linked member, or to the last linked visitor accepter.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/NestHostFinder.java`
- Bazel lines: 108
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no check for whether two classes share a nest: the same class object is treated as a match, otherwise the nest-host class names must be equal.
  - [P3] Zel does not resolve a nest-host name, which defaults to the class’s own name and, for a program class at class-file version 54.0 (Java 10) or newer, is replaced by the `NestHost` attribute’s constant-pool host class.
  - [P3] Library classes stay named as themselves because their versions and attributes are not consulted, and every attribute other than `NestHost` is ignored.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/PrimitiveArrayConstantReplacer.java`
- Bazel lines: 218
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no pass that rewrites `CONSTANT_PrimitiveArray` references in method code into standard bytecode.
  - [P3] A class is visited only when its constant pool contains at least one primitive-array constant. Each matching constant instruction is replaced by push length, `newarray` of that primitive type, then per element `dup`, push index, push value, and the matching store.
  - [P3] Boolean elements are stored as `1` or `0` with `bastore`. Byte values are pushed as ints then `bastore`. Char and short are pushed as ints then `castore` or `sastore`. Int, float, long, and double use `iastore`, `fastore`, `lastore`, and `dastore`. An empty array is only the length push plus `newarray`.
  - [P3] The code attribute is rewritten only when that sequence actually replaces an instruction. Non-code attributes and instructions that do not reference a primitive-array constant are left unchanged.
  - [P3] After any such rewrite, unused primitive-array constants are removed by shrinking the constant pool. If nothing was rewritten, the pool is left as-is.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/SimplifiedVisitor.java`
- Bazel lines: 1279
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file visitor base whose default methods forward specific visits to broader ones: program/library classes to any class; fields and methods to any member; scalar, ref, and name/type constants through `visitAnyConstant` / `visitAnyRefConstant` / `visitAnyMethodrefConstant`; primitive-array constants and elements (dropping the element value) to the any-array or any-element method; attributes, including deprecated, synthetic, signature, annotation, and type-annotation overloads, dropping member, field, method, parameter, and code context; instructions, with both switch kinds via any-switch; stack-map frames; verification types, with stack and variable visits dropping the index; annotations and type annotations, dropping member, parameter, and code context; target-info and type-path visits; and element values.
  - [P3] The required catch-alls (`visitAnyClass`, `visitAnyMember`, `visitAnyConstant`, `visitAnyPrimitiveArrayConstant`, `visitAnyPrimitiveArrayConstantElement`, `visitAnyAttribute`, `visitAnyInstruction`, `visitAnyStackMapFrame`, `visitAnyVerificationType`, `visitAnnotation`, `visitTypeAnnotation`, `visitAnyTargetInfo`, `visitTypePathInfo`, `visitAnyElementValue`) are also absent, so nothing throws `UnsupportedOperationException` naming the concrete class when one of those is reached without an override.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/StringReferenceInitializer.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not fill a string constant’s class reference when it is unset.
  - [P3] Bazel resolves the constant’s string by cutting at the first `[]`, turning `.` into `/`, then searching the program class pool and, only if that misses, the library class pool.
  - [P3] Other constant kinds are left untouched, and a reference that is already set is not replaced. Zel has no equivalent visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/StringSharer.java`
- Bazel lines: 172
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-file pass that shares referenced member, class, and attribute strings into a program class constant pool.
  - [P3] UTF-8 constants are not replaced with an equal referenced name or descriptor, and the pending string is not cleared after each UTF-8 visit.
  - [P3] Attribute names are not interned and written back through the attribute-name constant.
  - [P3] Library-class superclass and interface name strings are not replaced with the names of the resolved classes when those classes are non-null.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/util/WarningPrinter.java`
- Bazel lines: 127
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no warning printer that writes a warning line and increments a count only when the warning is actually emitted.
  - [P3] There is no optional class-name filter, parsed from a pattern list, that suppresses a warning when the single involved class name matches and that accepts every class when the filter is absent.
  - [P3] There is no two-class check that suppresses the warning when either class name matches that filter.
  - [P3] The number of warnings printed so far is not exposed.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/AllClassVisitor.java`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Visiting a class pool does not apply a wrapped class visitor to every class in that pool. Bazel `AllClassVisitor.visitClassPool` calls `ClassPool.classesAccept`, which walks the pool's `TreeMap` values (class-name order) and calls `Clazz.accept` on each entry. Zel has no class pool or class visitor that performs this walk.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/AllFieldVisitor.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No `ClassVisitor` holds a `MemberVisitor` and, on both `visitProgramClass` and `visitLibraryClass`, forwards that class's fields through `fieldsAccept`.
  - [P3] Program classes walk `fields[0..u2fieldsCount)` with no null skip. Library classes walk `fields[]` and skip null entries. Methods are not visited. Zel has no class-file field walk.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/AllMemberVisitor.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that, for both a program class and a library class, applies one member visitor to every field and then every method.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/AllMethodVisitor.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that, for a program class, accepts every method in table order with a stored member visitor.
  - [P3] The same member visitor is not applied to a library class, where null method slots are skipped and the remaining methods are accepted in table order.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/BottomClassFilter.java`
- Bazel lines: 69
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that forwards `visitProgramClass` and `visitLibraryClass` only when `subClasses == null` (a class with no known subclasses) and otherwise skips the class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassAccessFilter.java`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that forwards a program or library class only when its access flags satisfy a required-set mask and a required-unset mask.
  - [P3] A class is not accepted only when every required flag bit is set and none of the forbidden flag bits are set.
  - [P3] Classes that fail that test are not dropped, and matching classes are not delegated as the same program or library class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassCleaner.java`
- Bazel lines: 364
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that sets visitor info to null on a program class and then on every constant-pool entry, field, method, and attribute, or on a library class and only its fields and methods. Library constant pools and library member attributes are left unchanged.
  - [P3] Program members are cleared and then their attributes are cleared. Library members are cleared with no attribute walk.
  - [P3] Each attribute is cleared. Nested clearing continues only for bootstrap-method entries, inner-class entries, method parameters, code exception handlers and nested code attributes, stack-map and stack-map-table frames, local-variable and local-variable-type entries, annotations, parameter annotations, type annotations, and annotation default values. The exceptions attribute re-visits its referenced constant-pool entries after casting the class to a program class. Module requires, exports, opens, and provides records are not cleared.
  - [P3] Same-zero and less-zero frames are cleared alone. Same-one frames also clear their stack verification type, more-zero frames clear their extra variable types, and full frames clear both variable and stack types. Every verification type is cleared.
  - [P3] Annotations and type annotations are cleared along with their element values. An annotation-typed element value also clears the nested annotation. Constant, enum, and class element values are cleared only themselves. An array element value is cleared without visiting its elements, so those nested values keep their visitor info. Type-annotation targets and type paths are not visited.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassCollector.java`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel class visitor collects each visited class object into a caller-supplied set. `visitAnyClass` only calls `set.add(clazz)` and does not filter, rename, or visit members.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassCounter.java`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that counts how many classes have been visited.
  - [P3] Bazel starts the count at 0. `visitProgramClass` and `visitLibraryClass` each add one, ignoring the class argument, and `getCount` returns that total.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassHierarchyTraveler.java`
- Bazel lines: 91
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `ClassVisitor` that stores four independent flags (this class, superclasses, interfaces, subclasses) plus a delegate and, from both `visitProgramClass` and `visitLibraryClass`, forwards those flags unchanged into that class’s `hierarchyAccept`.
  - [P3] That walk, which zel also lacks, visits the current class first when requested (`ProgramClass.accept` versus `LibraryClass.accept`). A non-null superclass is then walked with `(true, true, visitInterfaces, false)`. If interfaces are requested and superclasses were not, the superclass is walked with `(false, false, true, false)`; each non-null direct interface is walked with `(true, false, true, false)` (program classes use `u2interfacesCount`/`getInterface`; a null library `interfaceClasses` array skips the loop). If subclasses are requested and `subClasses` is non-null, each entry is walked with `(true, false, false, true)` with no null check on the entry. There is no visited set, so a class reached by more than one path is visited again. All four flags false visits nothing.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassNameFilter.java`
- Bazel lines: 148
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that parses a class-name pattern (one string or a list, optionally filling a mutable wildcard `VariableStringMatcher` list through `ListParser`/`ClassNameParser`, or an already-built `StringMatcher`) and forwards `visitProgramClass` or `visitLibraryClass` only when that matcher accepts the visited class name.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassPoolClassVisitor.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-pool or class visitor that remembers the most recently visited `ClassPool` and, on every later class visit, applies a wrapped class-pool visitor to that pool.
  - [P3] `visitProgramClass` and `visitLibraryClass` both ignore the class and call `classPoolVisitor.visitClassPool(classPool)`. That delegation is absent.
  - [P3] A class visit before any pool visit passes a null pool into the wrapped visitor. That path is absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassPoolFiller.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that, for every program or library class, inserts that class into a pool under `clazz.getName()`, replacing any class already stored with the same name.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassPoolRemover.java`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor bound to a class pool that, on both program-class and library-class visits, removes the visited class from that pool.
  - [P3] Bazel removes by the class name (`clazz.getName()`) from a name-keyed map: a different instance with the same name is removed, a missing name is a no-op, and the class object itself is left unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassPoolVisitor.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-pool visitor callback. Bazel’s `ClassPoolVisitor` is a single void `visitClassPool(ClassPool)` entry, and `ClassPool.accept` only forwards that pool to it. Nothing in Zel is invoked with a whole class pool that way.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassPresenceFilter.java`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that selects a present-class or missing-class visitor by an exact name lookup in a class pool, then forwards the original program or library class to that visitor and skips the visit when the selected visitor is null.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassPrinter.java`
- Bazel lines: 1389
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no writer that dumps a class’s full internal structure. ProGuard prints to a `PrintWriter` (default auto-flushing `System.out` in the platform encoding), prefixes each visited object with `+` when `getVisitorInfo()` is set and `-` otherwise, and indents nested lines by two spaces.
  - [P3] Program classes print a separator, internal name, superclass, major/minor version in hex plus the external target version, access flags in hex, and an external declaration that inserts the word `class` only when none of `ACC_ENUM`, `ACC_INTERFACE`, or `ACC_MODULE` is set and appends `extends` only when `u2superClass != 0`. They then print interfaces, the constant pool, fields, methods, and class attributes, each with a count.
  - [P3] Library classes omit versions, the constant pool, and class attributes. `extends` is omitted when `getSuperName()` is null. Interfaces use `interfaceClasses.length` and print ` + name` only for non-null entries. Fields and methods are not wrapped in the extra indent used for program members.
  - [P3] Constants print bracketed values for int, long, float, double, string, UTF-8, module, package, class, method type, and name-and-type. Primitive arrays print type and length only. Dynamic and invoke-dynamic print the bootstrap index and then the name-and-type. Method handles print the reference kind and then the reference. Field, interface-method, and method refs print `class.name type`.
  - [P3] Program fields and methods print hex access flags and the external full description. Member attributes are printed only when the count is greater than 0, under `Class member attributes`. Program methods concatenate the name and descriptor with no space. Library methods insert a space, and library members do not walk attributes.
  - [P3] Attributes printed include unknown (by name), bootstrap methods, source file and dir, inner classes, enclosing method (name-and-type only if the index is non-zero), nest host and members, the full module attribute (flags, optional version, requires, exports, opens, uses, provides), module main class and packages, deprecated, synthetic, signature, constant value, method parameters, exceptions, and code (length, max locals, max stack, then instructions, exception table, and nested attributes). Stack-map and stack-map-table headers use the code attribute’s visitor marker, not the map attribute’s. Also line numbers, local variables, local-variable types, runtime-visible and invisible annotations, parameter annotations, and type annotations (those headers have no trailing colon), plus annotation defaults.
  - [P3] Ordinary instructions print `toString(offset)`. Constant instructions also print the referenced constant. `tableswitch` and `lookupswitch` add each case as `value: offset = jump, target = offset+jump` and a default line. Exception entries print `start -> end: handler` and the catch type only when `u2catchType != 0`.
  - [P3] Stack-map frames print same-zero as `Var: ..., Stack: (empty)`, same-one with the stack item on the same line, less-zero as `Var: -N`, more-zero with extra variables then `, Stack: (empty)`, and full frames as variables then `, Stack:` then stack items. Verification types are inline tokens: `[i]`, `[f]`, `[l]`, `[d]`, `[T]`, `[a:classname]`, `[n]`, `[u:offset]`, `[u:this]`.
  - [P3] Line numbers append `[source]` only when a source is set. Parameters print flags and a name, or empty brackets when the name index is 0. Local variables and local-variable types print `start -> start+length` with descriptor or signature and name. Requires always prints the version constant, including index 0. Exports, opens, and provides print their target or implementation constants.
  - [P3] Annotations print the type and element values. Parameter annotations include the parameter index. Type annotations print target info, the type-path count and steps (`kind`, argument index), then element values. Targets distinguish type parameter, super versus interface index, bound, empty, formal parameter, throws, local-variable ranges, catch, offset, and type argument. Element values cover constants (name `(default)` when the name index is 0, plus tag and constant), enum type and constant names, class names, nested annotations, and arrays. Inner-class entries always print inner, outer, and inner-name constants with no zero-index skip.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassVersionFilter.java`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that delegates a program-class visit only when the class version is inside an inclusive minimum–maximum range, with the two-argument constructor treating the maximum as `Integer.MAX_VALUE`.
  - [P3] Zel has no matching no-op for library classes, which this filter skips because they carry no version number.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassVersionSetter.java`
- Bazel lines: 83
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel code forces every visited program class's `u4version` to a configured target, including when the existing version is lower or equal.
  - [P3] No Zel code, when given a non-null set, records the previous version only if it is strictly greater than that target; equal and older versions are not recorded, and duplicate newer versions collapse to one entry.
  - [P3] Library-class visits are a no-op because those classes have no version; Zel has no equivalent visit.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ClassVisitor.java`
- Bazel lines: 36
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor of ProGuard `Clazz` objects that splits program classes (`visitProgramClass(ProgramClass)`) from library classes (`visitLibraryClass(LibraryClass)`).
  - [P3] ProGuard in zel is option flags and `proguard_allowlister.py`, which do not walk class files.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ConcreteClassDownTraveler.java`
- Bazel lines: 100
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No class visitor walks `subClasses` through interfaces and abstract classes (`ACC_INTERFACE` or `ACC_ABSTRACT`) and delegates only the first concrete class to a wrapped visitor, without visiting that abstract or interface type or any further subclasses.
  - [P3] Program and library classes share that rule, and a null subclass array is a no-op. Zel has no equivalent hierarchy walk.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ConstructorMethodFilter.java`
- Bazel lines: 141
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `MemberVisitor` that, for a program method, forwards `visitProgramMethod` to one of three delegates: a constructor that calls `super.<init>`, any other `<init>`, or a non-constructor. A null chosen delegate skips the visit.
  - [P3] Zel has no one-argument constructor that sends both constructor kinds to the same visitor and passes a null delegate for every other method. The three-argument constructor stores the three delegates separately.
  - [P3] Zel has no per-method scan that resets a flag, walks every attribute, and inspects only `Code` attributes. An `invokespecial` whose constant-pool index is a wildcard and whose extra constant is 0 matches when the referenced method name is `<init>` and that reference’s class name equals the visited class’s superclass name (`null` when the superclass index is 0). Any such call sets the flag. A `this()` `<init>`, a non-`<init>` `invokespecial`, or a constructor with no code stays on the plain constructor delegate. The descriptor is not checked.
  - [P3] Program fields and library fields and methods are not classified. They fall through to `visitAnyMember`, which throws `UnsupportedOperationException`. Searches of `/workspace/zel-cursor` found no `invokespecial` matcher or this filter.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/DotClassClassVisitor.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel visitor walks method instructions, accepts the constant-pool entry of `ldc` and `ldc_w` only, and delegates a class visitor to the class referenced by a class constant (a `.class` construct). Other opcodes and non-class constants stay no-ops.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/DynamicReturnedClassVisitor.java`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant visitor that, for dynamic and invoke-dynamic constants only, visits the last referenced class when that array is non-empty and the name-and-type descriptor’s return type is an internal class type (including a class array), and that skips a null class, primitive or void returns, and every other constant.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ExceptClassFilter.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No class visitor stores one excluded class and a delegate, then forwards `visitProgramClass` and `visitLibraryClass` only when the visited class is not equal to that excluded class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ExceptClassesFilter.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that skips `visitProgramClass` and `visitLibraryClass` when the visited class is in an exclusion list.
  - [P3] Bazel treats a null exclusion array as excluding nothing, tests membership with `Clazz.equals` (identity; `Clazz` does not override `equals`), and forwards only non-matching visits to the delegate.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ExceptionCounter.java`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `ExceptionInfo` visitor that starts at zero and increments a count by one on every visit, ignoring the class, method, code attribute, and exception info, then returns that total.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ExceptionExcludedOffsetFilter.java`
- Bazel lines: 64
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No filter forwards an exception-table visit only when the given instruction offset is outside that entry’s try range (`offset >= startPC && offset < endPC` is false).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ExceptionHandlerConstantVisitor.java`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `ExceptionInfo` visitor that, when `u2catchType` is non-zero, forwards that constant-pool index to a `ConstantVisitor`, and skips catch-all handlers whose catch type is 0.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ExceptionHandlerFilter.java`
- Bazel lines: 69
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] ProGuard's `ExceptionHandlerFilter` forwards an `ExceptionInfo` visit only when `u2handlerPC` is in the half-open range `[startOffset, endOffset)`; otherwise the visit is dropped.
  - [P3] Zel has no exception-handler visitor that filters or delegates by handler program-counter range.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ExceptionOffsetFilter.java`
- Bazel lines: 64
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no exception-info visitor that keeps an instruction offset and a delegate visitor, then forwards `visitExceptionInfo` only when that offset lies in the handler’s try range (`instructionOffset >= u2startPC && instructionOffset < u2endPC`). Offsets outside that half-open range are skipped in ProGuard, and the same class, method, code attribute, and exception entry are passed through when the range matches.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ExceptionRangeFilter.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no exception-info visitor that forwards a visit only when the exception try range overlaps a given instruction range (`startPC < endOffset` and `endPC > startOffset`), and otherwise drops it.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/FunctionalInterfaceFilter.java`
- Bazel lines: 110
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that forwards program and library class visits only for interfaces whose interface hierarchy contains at least one abstract method and, after removing name-and-descriptor matches that also have a non-abstract implementation, at most one abstract method remains (including zero, so an invokedynamic call can still name the single abstract method).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ImplementedClassConstantFilter.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant visitor that forwards a class constant to a delegate unless the resolved referenced class is the given class or extends or implements it. An unresolved referenced class is still forwarded, and every other constant kind is ignored.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ImplementedClassFilter.java`
- Bazel lines: 100
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that, for both program and library classes, delegates to an accepted visitor when the class extends or implements a given class and either is not that same class or `includeImplementedClass` is true, and otherwise delegates to a rejected visitor. A null chosen visitor skips the visit, and the program versus library visit kind is preserved.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ImplementingClassConstantFilter.java`
- Bazel lines: 69
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-constant visitor that forwards a class constant only when its referenced class is unresolved or is not the given class and not a type that class extends or implements (itself, superclasses, and interfaces), and that ignores every other constant kind.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/InitializerMethodFilter.java`
- Bazel lines: 108
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that ignores fields and forwards program or library methods to one visitor when the method name is `<init>` or `<clinit>`, and to another visitor otherwise.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/LibraryClassFilter.java`
- Bazel lines: 60
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that ignores `visitProgramClass` and forwards only `visitLibraryClass` to a wrapped visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/LibraryMemberFilter.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that skips program field and method visits and forwards only library field and method visits to a delegate visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MemberAccessFilter.java`
- Bazel lines: 122
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that forwards program and library field and method visits only when access flags match. ProGuard requires every non-visibility required-set flag to be present and every required-unset flag to be absent. Public, private, and protected in the required-set mask are special: if any of those three are required, at least one of them being set is enough.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MemberAccessFlagCleaner.java`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class or member visitor that clears a caller-supplied access-flag mask with `flags &= ~mask` on program classes, library classes, program fields, and program methods.
  - [P3] Library field and library method visits stay no-ops in Bazel while those class and program-member visits are updated. That selective skip is absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MemberAccessFlagSetter.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that stores a caller-supplied access-flag mask and bitwise-ORs it into `u2accessFlags` of visited program fields and program methods.
  - [P3] Library fields and library methods are left unchanged by empty visit methods in Bazel; that no-op visitor is absent in Zel.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MemberAccessSetter.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that rewrites program field and program method `u2accessFlags` by replacing only the visibility bits (`ACC_PUBLIC`, `ACC_PRIVATE`, `ACC_PROTECTED`) with the constructor value and leaving every other flag unchanged.
  - [P3] When that value is exactly `ACC_PRIVATE`, the existing `ACC_FINAL` bit is cleared first. Any other value, including private combined with other bits, does not clear `final`, and bits outside the visibility mask are ignored.
  - [P3] Library field and library method visits do nothing, so library member access flags are never rewritten.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MemberClassAccessFilter.java`
- Bazel lines: 112
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that forwards program and library field and method visits only when the member is accessible from a referencing class.
  - [P3] The missing check accepts public members always, private members in the same nest, package-visible members in the same internal package (and also in the same nest, because the level is at least private), and protected members in the same package or nest or when the referencing class extends or extends-or-implements the member’s class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MemberCollector.java`
- Bazel lines: 91
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Every program or library field or method visit builds one string and inserts it into the caller-supplied `Set`.
  - [P3] The string is `className.` when the class-name flag is set, then `memberName.` when the member-name flag is set, then the descriptor when the descriptor flag is set. The dots stay even if a later part is omitted.
  - [P3] The class name is `clazz.getName()`, and the member name and descriptor are `member.getName(clazz)` and `member.getDescriptor(clazz)`.
  - [P3] Duplicate strings are dropped by the set. All three flags false still inserts `""`. There is no member filtering or ordering.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MemberCounter.java`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that starts at 0 and increments once for every library field, library method, program field, and program method visit, then returns that running total.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MemberDescriptorFilter.java`
- Bazel lines: 134
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that forwards program and library field and method visits only when the member descriptor matches.
  - [P3] Nothing builds that check from a ProGuard wildcard expression (`ListParser` over `ClassNameParser`, with an optional list of captured wildcard matchers) or from a caller-supplied string matcher, and then skips the visit when the descriptor does not match.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MemberDescriptorReferencedClassVisitor.java`
- Bazel lines: 65
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel member visitor takes a class visitor and, for both program and library members, visits every class referenced by that member's descriptor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MemberNameFilter.java`
- Bazel lines: 135
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that delegates program and library field and method visits only when the member name matches a name pattern (parsed list of name expressions, optional wildcard variable matchers, or a prebuilt matcher).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MemberToClassVisitor.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A `MemberVisitor` that ignores the member and visits its enclosing class through a wrapped `ClassVisitor` is absent.
  - [P3] `visitProgramField` and `visitProgramMethod` call `visitProgramClass` on the owning `ProgramClass`.
  - [P3] `visitLibraryField` and `visitLibraryMethod` call `visitLibraryClass` on the owning `LibraryClass`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MemberVisitor.java`
- Bazel lines: 40
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that dispatches separately to program fields, program methods, library fields, and library methods, each with the enclosing class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MethodCollector.java`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Visiting a program method or a library method adds that method to the caller-supplied set. Zel has no member visitor that records visited methods this way.
  - [P3] The only call site gathers super-calling constructors (under `ConstructorMethodFilter`) while editing initializer instructions. Zel has no class-file editor that performs that collection.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MethodImplementationFilter.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that forwards a program or library method visit only when that class reports the method may have implementations.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MethodImplementationTraveler.java`
- Bazel lines: 128
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that, for a program or library method, optionally visits that method and then the same name and descriptor in superclasses, interfaces, and subclasses.
  - [P3] `visitThisMethod` accepts the original method immediately, including private, static, final, abstract, and `<init>`. That visit is not passed through the access filter.
  - [P3] The hierarchy walk is skipped when `(accessFlags & (ACC_PRIVATE | ACC_STATIC)) != 0` or the name is exactly `<init>`. Final, abstract, protected, public, and package-private instance methods still walk. `<clinit>` is skipped only because it is static.
  - [P3] The walk always passes `visitThisClass` false, so the declaring class is not searched again. Superclasses, interfaces, and subclasses follow the three stored flags. There is no visited set, so a class reached by more than one path is delegated again.
  - [P3] Each reached class forwards only the first method whose name and descriptor equal the original (`String.equals`, program table order with no null-slot skip, library slots skipped when null). A missing method is a no-op. That method is forwarded only when it has neither `ACC_PRIVATE` nor `ACC_STATIC`. No required-set bits are applied, so package-private, protected, public, abstract, and final matches all pass.
  - [P3] Superclass recursion visits the superclass and continues upward, and includes interfaces only when that flag is set. Interface-only mode does not visit the superclass itself, but still walks its interfaces and each non-null direct interface. Subclass mode visits each subclass and its subclasses, and does not visit those subclasses' supers or interfaces. A null subclass entry throws. All four flags false visits nothing.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MultiClassPoolVisitor.java`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-pool visitor that stores an ordered list of delegates and, on `visitClassPool`, calls each one in order with the same pool.
  - [P3] There is no empty constructor, no constructor that adopts a caller-supplied visitor array and its length, and no `addClassPoolVisitor` that appends a delegate, allocating or extending the list when it is full.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MultiClassVisitor.java`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no composite class visitor that keeps an ordered list of delegates and forwards both program-class and library-class visits to each delegate in insertion order.
  - [P3] There is no empty constructor that starts with a 16-slot visitor array and a zero count, and no varargs constructor that adopts the given visitor array and sets the count to its length.
  - [P3] There is no append operation that inserts a visitor at the current count and grows the backing array when that count exceeds the array length.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MultiConstantVisitor.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no composite constant visitor that forwards every constant visit to each registered delegate in order.
  - [P3] There is no empty visitor (count 0, capacity 16), no varargs constructor that keeps the given array and sets the count to its length, and no `addClassVisitor` append that grows that array.
  - [P3] Typed constant visits that `SimplifiedVisitor` routes through `visitAnyConstant` are not re-dispatched with `constant.accept(clazz, delegate)` for each delegate in `[0, count)`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/MultiMemberVisitor.java`
- Bazel lines: 95
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that keeps an ordered list of member visitors and, on `visitProgramField`, `visitProgramMethod`, `visitLibraryField`, and `visitLibraryMethod`, calls that same method on each entry from index 0 through the count, passing the same class and member. An empty list visits nothing. A null entry is not skipped.
  - [P3] The no-arg constructor starts the count at 0 with 16 unused slots. The varargs constructor stores the given array and sets the count to its length. `addMemberVisitor` appends one visitor, reusing spare capacity or copying into a new array whose length is exactly the new count. There is no removal.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/NamedClassVisitor.java`
- Bazel lines: 49
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel class-pool visitor stores a class name and a class visitor, then on visit looks up that exact name and applies the visitor only when the class is present.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/NamedFieldVisitor.java`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that, on both a program class and a library class, looks up the field stored by name and descriptor and, when that field exists, dispatches it to a member visitor.
  - [P3] The Java visitor leaves the class unvisited when `fieldAccept` finds no match, and it never visits methods. That accept path is absent from zel.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/NamedMethodVisitor.java`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No class visitor stores a method name, descriptor, and member visitor and, for both a program class and a library class, accepts only the method with that name and descriptor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ParallelAllClassVisitor.java`
- Bazel lines: 202
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-pool visitor that walks every library and program class in parallel, creating one `ClassVisitor` per worker via a factory.
  - [P3] No thread count of `availableProcessors() - 1`, capped by the `parallel.threads` system property (invalid values ignored), with a single-threaded `classesAccept` fallback when the count is 1 or less.
  - [P3] No fixed pool of named workers that each keep one visitor and `accept` the class, then surface task failures as `RuntimeException` (the execution cause, or "Parallel execution is taking too long" if interrupted).

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ParameterVisitor.java`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No visitor callback that receives the owning class, the field or method, the parameter index, the parameter count, the slot offset, the total slot size, the parameter type, and the referenced class.
  - [P3] No contract that omits `this` and the method return value, and that treats a field type as a parameter when storing a value.
  - [P3] No accounting for `long` and `double` occupying two slots in the offset and the total size.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ProgramClassFilter.java`
- Bazel lines: 60
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No visitor stores a delegate and forwards only `visitProgramClass` to that delegate with the same program class.
  - [P3] Library-class visits are not dropped. Zel never distinguishes program classes from library classes in a class-visitor filter.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ProgramMemberFilter.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No member visitor forwards program-class field and method visits to a delegate and drops library-class field and method visits.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ReferencedClassVisitor.java`
- Bazel lines: 269
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that walks only downstream referenced classes, so program classes never dispatch a class visitor over the constant pool, fields, methods, and attributes, and library classes never dispatch one over the superclass, interfaces, fields, and methods.
  - [P3] Member descriptor classes are never visited, and program members never continue into their attributes while library members stop after the descriptor.
  - [P3] String, ref, class, invoke-dynamic, and method-type constants are never forwarded, nor are enclosing-method classes, code-attribute nested attributes, local-variable and local-variable-type classes, signature classes, annotations, parameter annotations, annotation defaults, enum and class element values, nested annotations, or array element values. Other constants, attributes, and element values stay unvisited.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/ReferencedMemberVisitor.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that, for a wrapped member visitor, forwards referenced members of string constants and any ref constant (`referencedMemberAccept`) and the referenced method of every annotation element value (`referencedMethodAccept`).
  - [P3] Bazel ignores every other constant (`visitAnyConstant` is a no-op). That dispatch is absent in Zel.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/SignatureAttributeReferencedClassVisitor.java`
- Bazel lines: 77
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On a class, field, or method `Signature` attribute, Bazel forwards to `SignatureAttribute.referencedClassesAccept`, which visits each non-null entry of the pre-resolved `referencedClasses` array and does nothing when that array is null. Zel has no visitor that walks classes referenced by signature attributes.
  - [P3] Every other attribute is ignored (`visitAnyAttribute` is empty). Zel has no equivalent attribute visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/SimilarMemberVisitor.java`
- Bazel lines: 173
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that, for a program or library field or method, forwards members with the same name and descriptor from a class hierarchy to a wrapped member visitor. A null stored target class starts at the visited member’s own class; otherwise the walk starts at that target. The four flags (this class, superclasses, interfaces, overriding subclasses) are passed unchanged into `hierarchyAccept`.
  - [P3] The name and descriptor are read from the visited member against its owning class (`getName`/`getDescriptor`) and matched with Java `String.equals` (exact and case-sensitive; both are required). Fields match only fields and methods only methods; access flags are ignored. Each class yields at most the first match in declaration order (`findField`/`findMethod`); a class with no match is skipped. A null program field or method slot throws; library classes skip null slots.
  - [P3] The hierarchy walk this visitor invokes is also absent. The current class is visited first when requested. A non-null superclass is then walked with `(true, true, visitInterfaces, false)`. If interfaces are requested and superclasses were not, the superclass is walked with `(false, false, true, false)`; each non-null direct interface is walked with `(true, false, true, false)` (program classes iterate `u2interfacesCount`; a null library `interfaceClasses` array skips the loop). If subclasses are requested and `subClasses` is non-null, each entry is walked with `(true, false, false, true)` with no null check on the entry. There is no visited set, so a class reached twice is matched again. All four flags false visits nothing.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/SimpleClassPrinter.java`
- Bazel lines: 161
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No visitor prints Java-like class names, or `class: field` / `class: method` lines, for program and library classes and members, optionally including access modifiers, to a writer or auto-flushing stdout.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/SingleTimeClassVisitor.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that stores one delegate and the class from the previous forwarded visit, then skips `visitProgramClass` and `visitLibraryClass` when the new class is that same instance.
  - [P3] Bazel starts with no remembered class, so the first visit is always forwarded. Program and library visits share that one slot. Equality is identity (`ProgramClass` and `LibraryClass` do not override `equals`), so a different instance with the same name is visited. Only a consecutive repeat is dropped. A later visit of a different class is forwarded and replaces the remembered class, so an earlier class is visited again when it is no longer the previous one. A skipped visit does not change the remembered class.
  - [P3] Searches of `/workspace/zel-cursor` found no `SingleTimeClassVisitor`, class-visitor trait, or consecutive-class filter. ProGuard there is option flags and `proguard_allowlister.py`, which do not walk classes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/SubclassFilter.java`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that skips a program or library class when a given class is present in its direct-subclass list and otherwise forwards the visit. ProGuard treats a null subclass array as absent and matches entries with `Clazz.equals`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/SubclassTraveler.java`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that, on a program class or a library class, delegates to each direct subclass and no further. `SubclassTraveler` only calls `subclassesAccept`, which visits each entry of `subClasses` once when that array is non-null and does not visit the class itself, its superclass, its interfaces, or deeper descendants.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/VariableClassVisitor.java`
- Bazel lines: 78
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that stores a replaceable delegate, starts unset or seeded from a constructor, and exposes that delegate for later replacement and retrieval.
  - [P3] Program-class and library-class visits are not skipped when the delegate is unset and otherwise forwarded to it.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/VariableMemberVisitor.java`
- Bazel lines: 96
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no replaceable member-visitor delegate that starts unset and can be swapped later.
  - [P3] Zel does not no-op `visitProgramField`, `visitProgramMethod`, `visitLibraryField`, and `visitLibraryMethod` when that delegate is unset, or forward the same class and member to it when it is set.

### `third_party/java/proguard/proguard6.2.2/src/proguard/classfile/visitor/VisitorInfoSetter.java`
- Bazel lines: 171
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that stores one caller-supplied object, including null, and replaces the visited accepter's visitor info with that same object. Each visit stamps only that accepter. It does not walk nested constants, members, attributes, frames, annotations, or element values.
  - [P3] Program and library classes, and program and library fields and methods, are stamped through `visitAnyClass` and `visitAnyMember`. Every `ConstantVisitor` kind, including a primitive-array constant, is stamped through `visitAnyConstant`. The values array is ignored. Every attribute overload that `SimplifiedVisitor` forwards to `visitAnyAttribute` is stamped, including unknown, bootstrap-methods, source, inner-classes, enclosing-method, nest, module, deprecated, synthetic, signature, constant-value, method-parameters, exceptions, code, stack-map, line-number, local-variable, annotation, parameter-annotation, type-annotation, and annotation-default attributes. Member, field, method, and code context is dropped.
  - [P3] An inner-class record, an exception handler, every stack-map frame kind, and every verification type are stamped on that record alone. Stack and variable verification visits drop the index and do not visit other types in the frame. A local-variable record and a local-variable-type record are stamped alone. Annotation and type-annotation visits, including member, parameter, and code overloads, stamp that annotation and do not visit its element values, targets, or type path. Constant, enum, class, annotation, and array element values are stamped alone. A nested annotation or the elements of an array value are left unchanged.
  - [P3] Visits that still reach the inherited catch-alls throw `UnsupportedOperationException` naming `VisitorInfoSetter`: primitive-array value visits, primitive-array element visits, every instruction kind, every type-annotation target, and every type-path visit. This class does not implement those visitor interfaces. It also has no visits for bootstrap-method entries, parameter records, line-number records, or module requires, exports, opens, and provides records.
  - [P3] Searches of `/workspace/zel-cursor` at `2084b3f5a9bd343f38ffbc7633d65f21bab0f654` found no `setVisitorInfo`, `VisitorAccepter`, or class-file visitor that stores a mark on a visited class-file object. ProGuard-related zel code only covers Bazel flags and `proguard_allowlister.py`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/configuration/ConfigurationLogger.java`
- Bazel lines: 789
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No runtime logger records failed reflection lookups (`Class.forName`, `ClassLoader.loadClass`, `getConstructor`/`getDeclaredConstructor`, `getMethod`/`getDeclaredMethod`, `getField`/`getDeclaredField`) and emits suggested `-keep` / `-keepclassmembers` rules.
  - [P3] No one-time dedupe of missing classes, constructors, methods, and fields, and no skip of library classes whose loader matches `String`'s loader.
  - [P3] No `classmap.txt` load that maps obfuscated class names back to originals and gates constructor/method/field listing warnings on the obfuscated-method and obfuscated-field flags.
  - [P3] No Android `Log.w("ProGuard", …)` path with stderr fallback, and no `Runnable` dump of the accumulated keep rules.

### `third_party/java/proguard/proguard6.2.2/src/proguard/configuration/ConfigurationLoggingAdder.java`
- Bazel lines: 129
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instrumentation that loads `ConfigurationLogger` and `ConfigurationLogger.MethodSignature` into the program class pool, initializes their references and subclass hierarchy, and wraps a read failure as a runtime exception.
  - [P3] Zel does not rewrite reflection-related instruction sequences in non-`proguard/**` program classes through a peephole optimizer and configuration-logging sequence replacer.
  - [P3] Zel does not record, for each matched instruction, injected-class-map dependencies from the modified class onto both logger classes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/configuration/ConfigurationLoggingInstructionSequenceConstants.java`
- Bazel lines: 348
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no peephole match/replace sequences that wrap reflection calls (`Class.forName` with one argument and with name/initialize/loader, `ClassLoader.loadClass`, `getDeclaredConstructor`/`getConstructor`, `getDeclaredMethod`/`getMethod`, `getDeclaredField`/`getField`) so a `ClassNotFoundException`, `NoSuchMethodException`, or `NoSuchFieldException` is logged through `ConfigurationLogger` and rethrown.
  - [P3] Zel also does not log before the original invoke for the no-exception bulk lookups (`getDeclaredConstructors`/`getConstructors`, `getDeclaredMethods`/`getMethods`, `getDeclaredFields`/`getFields`), and it does not define the shared matcher wildcards or try/catch ranges those sequences use.

### `third_party/java/proguard/proguard6.2.2/src/proguard/configuration/ConfigurationLoggingInstructionSequenceReplacer.java`
- Bazel lines: 130
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instruction-sequence replacer that, on a match, maps the `LOCAL_VARIABLE_INDEX_1/2/3` wildcards (`0x30000001`–`0x30000003`) to fresh locals at `codeAttribute.u2maxLocals`, `+1`, and `+2`, and forwards every other argument to the base `matchedArgument(clazz, argument)`.
  - [P3] Zel does not resolve the `CLASS_NAME` wildcard (`0x30000000`) by adding a string constant of the external class name (`ClassUtil.externalClassName(programClass.getName())`) to that class’s constant pool with a null referenced class, and forwarding every other constant index to the base replacer.

### `third_party/java/proguard/proguard6.2.2/src/proguard/configuration/ConfigurationLoggingInstructionSequencesReplacer.java`
- Bazel lines: 145
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instruction visitor that turns each pattern/replacement pair into a configuration-logging sequence replacer and forwards every instruction visit to all of them.
  - [P3] Each pair uses the same constant array for the pattern (index 0) and the replacement (index 1), and all replacers share one branch-target finder, one code-attribute editor, and one optional extra visitor for deleted loads.
  - [P3] The four-argument constructor is the five-argument constructor with that extra visitor set to null.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/AssumeClassSpecificationVisitorFactory.java`
- Bazel lines: 150
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] When a member spec is a `MemberValueSpecification` whose `values` array is non-null, the factory wraps the member visitor so it records an assumed integer: one `Number` becomes `createIntegerValue(int)`, and any other length uses `values[0]` and `values[1]` as an integer range. An empty array throws on `values[1]`.
  - [P3] That wrapper runs only through `OptimizationInfoMemberFilter`. Program and library fields get `FieldOptimizationInfo.setValue`; program and library methods get `MethodOptimizationInfo.setReturnValue`.
  - [P3] Specs that are not `MemberValueSpecification`, or whose `values` are null, leave the visitor chain unchanged and delegate to `ClassSpecificationVisitorFactory.createNonTestingClassVisitor`.
  - [P3] Zel has no class-pool visitor that installs `-assumevalues` onto field or method optimization info.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/BasicBranchUnit.java`
- Bazel lines: 114
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no branch unit that records invoked branch commands and ignores the condition value.
  - [P3] `reset` clears the accumulated targets to empty and clears the called flag.
  - [P3] `branch` replaces the accumulated targets with a single offset and sets the called flag.
  - [P3] `branchConditionally` adds the target to the accumulated set regardless of `conditional` and sets the called flag.
  - [P3] `returnFromMethod` and `throwException` clear the accumulated targets to empty and set the called flag.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/BasicInvocationUnit.java`
- Bazel lines: 230
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard invocation-unit that builds partial-evaluation values for method entry, field access, calls, or exception handlers.
  - [P3] Caught exceptions are a non-null reference that may be a subtype: the catch class when the constant is present, otherwise `java/lang/Throwable` with no referenced class.
  - [P3] Stores of the receiver, field value, method parameter, and method return value are ignored.
  - [P3] Field loads and reference-constant method returns are values that may be a subtype and may be null. The referenced class is the field’s referenced class, or, for a method, the last `referencedClasses` entry only when that array is present and the descriptor ends with `;` (class or object-array return); otherwise the class is unset.
  - [P3] `invokedynamic` returns use that same last referenced class only when `referencedClasses` is present and the return type ends with `;`.
  - [P3] A non-static parameter 0 (`this`) is never null. Every other parameter may be null. All parameters may be a subtype, and the referenced class is the one supplied by the caller.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/BranchUnit.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bytecode-evaluation callback that sets the next instruction offset for an unconditional branch from a class, code attribute, current offset, and branch target.
  - [P3] Zel has no conditional-branch callback that updates the instruction offset according to a branch-certainty value.
  - [P3] Zel has no evaluator notification that the method returns with a value.
  - [P3] Zel has no evaluator notification that an exception is thrown.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/ClassConstantValueFactory.java`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard partial evaluator and no constant-value factory for bytecode `ldc` / `ldc_w` / `ldc2_w`.
  - [P3] For a class constant, this factory builds a non-null, exact `java.lang.Class` reference (`ClassConstants.NAME_JAVA_LANG_CLASS` and the constant’s linked `javaLangClassClass`, extension and null both false). The inherited visitor would instead reference the class named by the constant.
  - [P3] Every other constant kind is unchanged from `ConstantValueFactory`; only `visitClassConstant` is overridden.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/ConstantValueFactory.java`
- Bazel lines: 144
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant-pool visitor that turns a class and pool index into an evaluation value, and it does not keep the previous value when the entry kind is unhandled.
  - [P3] Integer, long, float, and double entries are not turned into particular primitive values of those widths.
  - [P3] A primitive-array entry is not an array reference whose element type is the single primitive descriptor character, whose class is null, and whose length is that entry’s length.
  - [P3] A string entry is not a non-null, non-extension `java/lang/String` reference bound to that entry’s `java.lang.String` class.
  - [P3] A dynamic entry is not a value of the bootstrap method’s return type that may be null and an extension, with the referenced class set to the last referenced class only when that type is an internal class type and the referenced-class list is non-empty.
  - [P3] A method-handle entry is not a non-null, non-extension `java/lang/invoke/MethodHandle` reference, and a method-type entry is not the same for `java/lang/invoke/MethodType`.
  - [P3] A class entry is not a non-null, non-extension reference of the named class (the pool class name and its resolved class), rather than `java.lang.Class`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/InvocationUnit.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bytecode-evaluation callback that initializes local variables when entering a method.
  - [P3] Zel has no callback that records leaving a method with a return value.
  - [P3] Zel has no callback that sets up the operand stack when entering an exception handler from a class, method, code attribute, offset, and catch type.
  - [P3] Zel has no callback that updates the operand stack for a field or method reference instruction (`ConstantInstruction`).

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/Processor.java`
- Bazel lines: 933
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instruction visitor that executes JVM bytecode on a shared local-variable frame and operand stack, with an injected value factory, branch unit, invocation unit, and an `alwaysCast` flag for null and downcasts.
  - [P3] No simple-instruction execution for constants, typed array load/store, stack dup/pop/swap, arithmetic, shifts, bitwise ops, and numeric conversions. `idiv`, `ldiv`, `irem`, and `lrem` catch `ArithmeticException` and push an unknown int or long instead of propagating the exception. `lcmp`, `fcmpl`, and `dcmpl` compare in original operand order; `fcmpg` and `dcmpg` use the reverse NaN-biased compare. Returns notify the invocation unit (except `return`) and then the branch unit. `newarray` builds an array reference from the primitive type byte and the popped length. `athrow` clears the stack, pushes the exception, and notifies the branch unit. `monitorenter` and `monitorexit` only pop the reference.
  - [P3] No constant-pool execution: `ldc`/`ldc_w`/`ldc2_w` push a class-constant value; field and method ops, including `invokedynamic`, are delegated to the invocation unit; `new` pushes the constant reference; `anewarray` wraps that type with the popped length; `checkcast` casts using `alwaysCast`; `instanceof` yields 0, 1, or an unknown int, and is forced unknown when the type may be an extension; `multianewarray` pops each dimension length and discards it, then pushes the constant type reference.
  - [P3] No variable execution: typed loads and stores, `iinc` of a local by the instruction constant, and `astore` via an untyped pop so a return address can be stored. `ret` branches to the last instruction offset stored in that variable.
  - [P3] No branch execution that first maybe-falls through to the next instruction, then applies the opcode condition. `if_icmplt`/`ge`/`gt`/`le` invert the comparison because the stack is popped in reverse. `goto`/`goto_w` branch unconditionally. `jsr`/`jsr_w` push the following instruction's offset and then branch. `ifnull`/`ifnonnull` test reference nullity.
  - [P3] No `tableswitch` or `lookupswitch` execution that maybe-branches to the default and then equality-tests each case against the popped index.
  - [P3] Unknown simple, constant-pool, variable, and branch opcodes throw `IllegalArgumentException`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/SimplifiedInvocationUnit.java`
- Bazel lines: 306
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no invocation unit that sizes a method’s locals from its descriptor and access flags, stores each parameter value (including `this`), and rewrites the operand stack for field and method instructions.
  - [P3] Method entry resets the variables to the slot count (`long` and `double` take two; a non-static method adds one for `this`), then stores `getMethodParameterValue` at each category-aware offset.
  - [P3] Method exit only forwards the return value to `setMethodReturnValue`.
  - [P3] Entering a handler casts the class to `ProgramClass`, loads constant-pool index `catchType` as a `ClassConstant`, and pushes `getExceptionValue`. A zero catch type is still looked up and cast.
  - [P3] `getstatic` and `putstatic` are static; `getfield` and `putfield` are instance; gets load and puts store. `invokestatic` and `invokedynamic` are static; `invokevirtual`, `invokespecial`, and `invokeinterface` are instance and do not change the previous load/store flag. Any other opcode leaves both flags unchanged, and the constant is still visited.
  - [P3] A field store pops the value (wide-aware) and records it before an instance access pops the receiver. A field load then pushes `getFieldValue` for the field’s type.
  - [P3] A method reference counts the receiver as parameter 0 when it is not static, pops from the last parameter down to 0, and records each popped value. A non-void return (descriptor not starting with `V`) is pushed.
  - [P3] `invokedynamic` uses the same count with no receiver, discards the popped arguments, and pushes a non-void return from the invokedynamic return-value hook.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/Stack.java`
- Bazel lines: 560
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No ProGuard operand stack of `Value` slots, sized so a category-2 value occupies two slots. `Stack(int)` allocates that many slots; `Stack(Stack)` allocates the other array's length and then copies. Every category-2 `push` writes one shared `TopValue` (`"T"`, category 1) and then the value.
  - [P3] No high-water `actualMaxSize`. `push`, `dup`, `dup_x1`, `dup_x2`, `dup2`, `dup2_x1`, and `dup2_x2` raise it to `currentSize`. `reset` clears, then sets it to 0. `copy` replaces it. `generalize` raises it to the other stack's mark and that update does not affect the boolean result. `clear`, `pop`, `pop1`, `pop2`, `removeTop`, and `swap` leave it unchanged.
  - [P3] No `generalize`. Unequal `currentSize` throws `IllegalArgumentException` with `Stacks have different current sizes [A] and [B]`. A null slot on this stack stays null and is not a change, even when the other slot is non-null. A non-null slot becomes null when the other is null, or `thisValue.generalize(otherValue)` when both are non-null. The return value is whether any slot's `equals` result changed.
  - [P3] No bottom/top slot access. `getBottom`/`setBottom` index from the bottom. `getTop`/`setTop` use `currentSize - index - 1`. `removeTop(index)` copies the top `index` slots down one and decrements size, leaving the old top slot intact and not removing a category-2 pair. `copy` copies only the live prefix and leaves the tail. `clear` nulls `0..currentSize` and sets size 0. `size` is the slot count. No capacity check: a short array throws `ArrayIndexOutOfBoundsException`.
  - [P3] No `pop` that nulls the top slot and, only when that value `isCategory2()`, also nulls the slot under it. `pop1` and `pop2` drop one or two slots with no type check. `ipop`, `lpop`, `fpop`, `dpop`, `apop`, and `opop` pop and then cast, throwing `Value`'s `IllegalArgumentException` (`Value "…" is not a … value […]`) on a type mismatch.
  - [P3] No JVM dup/swap shapes. `dup` and `dup_x1` pass each moved value through `category1Value()` (`…, v2, v1` becomes `…, v1, v2, v1`). `dup_x2` does that only for the duplicated top and moves the two slots below unchanged. `dup2`, `dup2_x1`, and `dup2_x2` copy two slots with no category check (`…, v2, v1` becomes `…, v2, v1, v2, v1`, with one or two further slots inserted under the copy). `swap` exchanges the top two slots after `category1Value()`. A non-category-1 value throws `IllegalArgumentException` (`Value "…" is not a Category 1 value […]`).
  - [P3] No live-slot `equals`, `hashCode`, or `toString`. Equality requires the same runtime class and the same `currentSize`, then null-safe `Value.equals`. `actualMaxSize` and capacity are ignored, so a subclass is unequal. Hash is `currentSize` XOR each non-null `hashCode`. `toString` is bottom-to-top `[value]` or `[empty]` with no separator.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/TracedStack.java`
- Bazel lines: 374
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No operand stack that records the current producer value on every push and pop, including a second slot for category-2 values.
  - [P3] No parallel producer stack that replaces the slots touched by `dup`, `dup_x1`, `dup_x2`, `dup2`, `dup2_x1`, `dup2_x2`, and `swap` with the current producer, while a second stack replays those instructions unchanged.
  - [P3] No `reset`, `copy`, `generalize`, `clear`, or `removeTop` that keep both producer stacks aligned with the value stack, and no bottom/top producer accessors, equality, hash, or debug string that prints empty slots and concatenates producer with actual producer when they differ.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/TracedVariables.java`
- Bazel lines: 199
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no local-variable frame that keeps a parallel producer value for each variable.
  - [P3] The producer used by later `store` calls starts unset and is not copied by the copy constructor. `store` then throws, because storing that null producer calls `isCategory2` on it.
  - [P3] `store` writes the value, records the current producer at that index, and when the value is category 2 also records that same producer at `index + 1`, replacing the top value the producer frame would have written there.
  - [P3] `setProducerValue(index, value)` writes only the producer frame, so a category-2 producer puts top in the next slot. `getProducerValue` reads a producer slot without changing the variable value.
  - [P3] `reset` and `initialize` apply to both frames. `generalize` generalizes both, and only when the value frame changed it clears a producer wherever the value is now null, including on the other frame when `clearConflictingOtherVariables` is set. It returns true if either frame changed.
  - [P3] Equality requires the same class and equal value and producer frames. The hash is the XOR of both frame hashes. `toString` emits `[` + producer (`empty:` if null) + value (`empty` if null) + `]` for each slot. `NONE` is `-1` and is unused in this class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/Variables.java`
- Bazel lines: 347
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard local-variable frame: a sized `Value` array that can be reset (grow or clear the previous size), copied via `initialize` (source may be shorter; destination must be at least as large), and merged via `generalize` (equal sizes required; same computational type is generalized, otherwise the slot is cleared and the other frame’s slot is optionally cleared too; returns whether this frame changed).
  - [P3] No `store` that writes a category-2 value and plants a shared `TopValue` in the next slot, and no `getValue`/`load` or typed loads (`iload`, `lload`, `fload`, `dload`, `aload`, `oload`) with index bounds checks.
  - [P3] No equality that ignores nulls and slots whose computational types differ, no hash over non-null values, and no `[value|empty]` string form.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ArrayReferenceValue.java`
- Bazel lines: 176
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No partially evaluated array reference stores an `IntegerValue` length and is always constructed with `mayBeNull` false.
  - [P3] `arrayLength` returns that stored length and ignores the `ValueFactory`.
  - [P3] `generalize` and `equal` against a plain `ReferenceValue` reverse the call onto the other value.
  - [P3] Generalizing two array references returns this value when `equals` holds. If both types are non-null and equal and `referencedClass` is the same object, it returns a new array reference with `mayBeExtension` OR-ed and the lengths generalized. Otherwise it drops the length and generalizes as a typed reference.
  - [P3] `equal` of two array references is `NEVER` when the lengths are `NEVER` equal, and otherwise uses typed-reference equality. A length of `MAYBE` or `ALWAYS` does not force `ALWAYS`.
  - [P3] `equals` requires the typed-reference match plus `arrayLength.equals`. The referenced class is not compared. `hashCode` XORs the typed hash with the length hash, and `toString` appends `[` plus the length plus `]`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ArrayReferenceValueFactory.java`
- Bazel lines: 46
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no partial-evaluation value factory that, for a null element type, returns the shared null reference and otherwise builds an array reference of type `[` plus the element type, with the given class, `mayBeExtension` false, `mayBeNull` false, and the given array length retained for later length queries.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/BasicValueFactory.java`
- Bazel lines: 160
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no basic value factory that reuses one shared unknown value for each of integer, long, float, double, and reference.
  - [P3] `createValue` returns null for `V`; the shared unknown integer for `Z`, `B`, `C`, `S`, and `I`; the shared unknown long, float, or double for `J`, `F`, or `D`; and the shared unknown reference for every other non-empty descriptor. An empty descriptor throws `StringIndexOutOfBoundsException`.
  - [P3] The particular integer, long, float, and double factories ignore the constant or the integer range and return the same shared unknown value as the no-argument factory.
  - [P3] `createReferenceValue`, `createReferenceValueNull`, and the typed `createReferenceValue` all return that same unknown reference. Type, referenced class, extension, and nullness are ignored, and null is not a distinct value.
  - [P3] Both `createArrayReferenceValue` overloads ignore array length and element value and return that same unknown reference.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/Category1Value.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no partial-evaluation value type for a JVM category-1 value (one stack slot).
  - [P3] `category1Value()` is missing: a category-1 value returns itself instead of the base `Value` behavior that throws `IllegalArgumentException`.
  - [P3] `isCategory2()` is missing: it is fixed to `false` for every category-1 value.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/Category2Value.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no partial-evaluation value that is always computational category 2: `category2Value()` returns the value itself, and `isCategory2()` is always true (the category-1/category-2 slot split used for long and double). ProGuard appears only as Android build flags, not as this value lattice.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ComparisonValue.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific integer abstract value that records the two scalar operands of a comparison (the `lcmp`-style result in {-1, 0, 1}).
  - [P3] There is no value identity keyed by that operand pair (same class plus both operands equal; hash is the class hash XOR both operand hashes), so comparison results cannot be distinguished or merged in the integer value lattice.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/CompositeDoubleValue.java`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No symbolic double value stores a left `DoubleValue`, an operator byte (`+`, `-`, `*`, `/`, or `%`), and a right `DoubleValue`, and stays a specific double so later specific-double arithmetic can nest another composite.
  - [P3] Equality is reference identity, or the same class with equal left operand, equal operator, and equal right operand. The same operands with different operators are not equal.
  - [P3] Hash code is the class hash XOR both operand hashes and does not include the operator, so composites that differ only by operator share a hash.
  - [P3] The text form is `(left<op>right)`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/CompositeFloatValue.java`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A specific float value that stores two `FloatValue` operands and a byte operation (`+`, `-`, `*`, `/`, `%`) without folding or computing a numeric result.
  - [P3] `equals` is true for the same instance, or when the other object is the same class, both operands are equal in order, and the operation bytes are equal.
  - [P3] `hashCode` is the class hash XOR both operand hashes. The operation byte is not mixed in, so different operations on the same operands share a hash.
  - [P3] `toString` is `(`, the left operand, the operation character, the right operand, and `)`.
  - [P3] Zel has no symbolic float value that records an unevaluated binary float operation.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/CompositeIntegerValue.java`
- Bazel lines: 86
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific integer value that stores an unevaluated binary operation on two integer operands without folding it to a concrete int.
  - [P3] Zel does not represent the eleven operations add, subtract, multiply, divide, remainder, shift left, arithmetic shift right, unsigned shift right, and, or, and xor, encoded as the bytes `+`, `-`, `*`, `/`, `%`, `<`, `>`, `}`, `&`, `|`, and `^`.
  - [P3] Zel has no equality that is true for the same instance, or for the same class with equal left operand, equal operation byte, and equal right operand. Operand order and the operation both matter.
  - [P3] Zel has no hash that mixes the specific-integer class hash with both operand hashes and leaves out the operation, so different operations on the same operands share a hash.
  - [P3] Zel has no string form `(left<operation character>right)`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/CompositeLongValue.java`
- Bazel lines: 86
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not keep an unevaluated specific long that is a binary expression of a left `LongValue`, an operator, and a right `Value`.
  - [P3] The recorded operators are add, subtract, multiply, divide, remainder, shift left, signed shift right, unsigned shift right, and, or, and xor. The right operand is a general `Value`, so a shift count can be an integer rather than a long.
  - [P3] Two composites are equal when they are the same instance, or when they are the same class with equal operands and the same operator. The hash mixes the class hash with both operand hashes and omits the operator, so different operators on the same operands share a hash. The debug text is `(left<op>right)`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ConvertedByteValue.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific integer for `i2b` of a non-particular specific integer. That value stays a category-1 integer of internal type `I`, is specific, and is not particular.
  - [P3] The wrapped integer is not sign-extended into [-128, 127], and `value()` stays 0, so the conversion is not constant-folded. Generalizing it with a particular or range integer yields the unknown integer rather than a byte range.
  - [P3] Two converted bytes are equal when they are the same object, or both are this class and their wrapped integers are equal. Equal sources then generalize, and, and or to one of them; subtract and xor to particular 0; and compare as always equal, never less-than, and always less-than-or-equal. Unequal sources generalize to the unknown integer and compare as maybe. A converted byte is not equal to its pre-conversion value.
  - [P3] A further `i2b` wraps another converted byte around this one, so the second conversion is not equal to the first.
  - [P3] The hash is the class hash XOR the wrapped integer's hash. Stack and variable-frame hashes mix that in, so frames match only when those converted bytes match.
  - [P3] A later push of an equal specific value can be rewritten to load a lower-numbered variable that already holds the same converted byte.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ConvertedCharacterValue.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no symbolic integer value for an `i2c` / `(char)` conversion of a specific integer. This type only stores the source `IntegerValue` and does not truncate to the unsigned 16-bit range.
  - [P3] Equality is missing: reference equality, or the same runtime class and equal source values. A conversion is not equal to its source, to a particular integer of the truncated bits, or to a byte or short conversion.
  - [P3] The matching hash (`getClass().hashCode() ^ source.hashCode()`) and the `"(char)("+source+")"` rendering are absent.
  - [P3] Inherited specific-integer operations are absent. Arithmetic and comparisons stay symbolic (equal sources yield `ALWAYS` / `NEVER` / zero; otherwise composite values or `MAYBE`), and another `convertToCharacter` wraps a new conversion instead of masking to `0..65535`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ConvertedDoubleValue.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no partial-evaluation double that remembers the specific int, long, or float it was converted from.
  - [P3] Two such values are equal when they are the same instance or the same class with equal sources; the hash mixes the class hash with the source hash; the text form is `(double)(<source>)`. Zel has none of that.
  - [P3] The value stays specific: equal sources generalize to the same value, other specific doubles become an unknown double, and arithmetic, negation, and further conversions wrap this value. The concrete `value()` stays the default `0.0`. Zel does not track that.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ConvertedFloatValue.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific float value that retains the scalar it was converted from.
  - [P3] Equality is missing: the same instance, or the same class (via `SpecificFloatValue.equals`) and equal source values.
  - [P3] Hashing is missing: the class hash XOR the source value's hash.
  - [P3] The `(float)(<source>)` display form is missing.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ConvertedIntegerValue.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No integer value that stands for a scalar converted to `int` (from a specific long, float, or double).
  - [P3] No equality that treats two such values as the same only when they are the same object or the same class with equal source values.
  - [P3] No hash that mixes the class hash with the source value's hash, and no `(int)(<source>)` text form.
  - [P3] No specific-integer behavior for that converted value: `isSpecific` is true, and arithmetic, shifts, bitwise ops, and comparisons follow `SpecificIntegerValue` (composites, identity simplifications when equal, and `ALWAYS`/`NEVER`/`MAYBE` relations).

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ConvertedLongValue.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific long that stands for a conversion of another scalar value.
  - [P3] Equality is the same instance, or the same class with equal source values. The hash is the class hash XOR the source hash. Zel does not implement that.
  - [P3] The text form `(long)(<source>)` is absent.
  - [P3] Converting a specific int, float, or double to long does not yield a symbolic long that keeps `SpecificLongValue` arithmetic instead of a concrete long.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ConvertedShortValue.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific integer value that records an `i2s` / `(short)` conversion of another integer without narrowing that integer into the signed 16-bit range.
  - [P3] Equality is identity, or the same class plus an equal wrapped integer. The hash is the class hash XOR the wrapped integer's hash, and the debug form is `(short)(<inner>)`. A converted short therefore matches another converted short only when the wrapped integers match, and it does not match the pre-conversion value, so `(short)x == x` stays maybe. A second conversion wraps again instead of collapsing. Zel does not track any of that.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/DetailedArrayReferenceValue.java`
- Bazel lines: 356
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard partial-evaluation value lattice. This class keeps a per-element table only when the length is particular and in `0..=32`. Any other length leaves the table unset, and loads and stores then ignore elements.
  - [P3] The element type is `type` with the leading `[` removed when `type` has length greater than 1 and starts with `[`; otherwise it is `type` unchanged. Each slot starts as the initial value for that type: integer `0`, long `0`, float `0.0`, double `0.0`, or a null reference.
  - [P3] A particular in-range load returns that slot as an integer, long, float, double, or reference. A missing table, a non-particular index, or an out-of-range index yields a fresh unknown value of the requested kind.
  - [P3] A particular in-range store overwrites that slot in place, including with a non-particular value. A particular out-of-range store changes nothing. A non-particular index calls `generalize` on every slot and drops the returned value, so the table stays as it was. With no table, store is a no-op.
  - [P3] `isParticular` is true when the table exists and every element is particular, so a fresh tracked array of zeros and nulls is particular. `equals` and `hashCode` include the element array together with the identified-array identity. `generalize` and `equal` only reverse-dispatch, and the other side then treats this value as an identified array, dropping the element table.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/DetailedArrayValueFactory.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no identified value factory whose array references also store element values.
  - [P3] A null component type returns the shared null reference (null type, not an extension, may be null) and does not take a reference id.
  - [P3] A non-null component type returns an identified array reference whose type is `[` plus that component type, with the given referenced class and length, `mayBeExtension` false, this factory, and the next shared `referenceID`.
  - [P3] The created value keeps one initial element per index only when the length is a particular integer from 0 through 32. A longer, negative, or unknown length stores no elements. Each initial element is produced by an `InitialValueFactory` backed by this same factory, and an array element type drops one leading `[`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/DoubleValue.java`
- Bazel lines: 359
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard partial-evaluation double. The concrete `value()` is `0.0`, `doubleValue()` returns this value, the computational type is `4` (`TYPE_DOUBLE`), the internal type is `"D"`, and the value is category 2.
  - [P3] `generalize` on a `Value` first asks that value for a double. A non-double throws `IllegalArgumentException` with `Value "<text>" is not a double value [<class>]`.
  - [P3] `compareReverse` of a double, specific double, or particular double is `compare(other).negate()`, so a compare result of `-1`, `0`, or `1` becomes `1`, `0`, or `-1`.
  - [P3] Negate, conversion to int, long, and float, and the double generalize, add, subtract, subtractFrom, multiply, divide, divideOf, remainder, remainderOf, and compare operations are abstract. The specific and particular overloads widen the argument and call those double operations.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/FloatValue.java`
- Bazel lines: 359
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no category-1 partial-evaluation float. This type’s computational type is `TYPE_FLOAT` (`3`) and its internal type is `"F"`.
  - [P3] The default concrete `value()` is `0f`. `floatValue()` returns this instance. `generalize(Value)` joins with `other.floatValue()`, which throws `IllegalArgumentException` when the other value is not a float.
  - [P3] `compareReverse` returns `compare(other).negate()`, so less/equal/greater becomes `1`/`0`/`-1`, for general, specific, and particular operands.
  - [P3] Specific and particular operands are widened before generalize, arithmetic, remainder, and compare. A particular operand is dispatched through the specific overload first, so a base float does not keep the operand’s known value.
  - [P3] Negation, conversion to int, long, and double, and the two-float operations generalize, add, subtract, subtractFrom, multiply, divide, divideOf, remainder, remainderOf, and compare are abstract here. Zel implements none of them.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/IdentifiedArrayReferenceValue.java`
- Bazel lines: 213
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no identified array reference: type, referenced class, may-be-extension, length, creating value factory, and integer id. `isSpecific` is true. `isParticular` stays the typed-reference rule, so it is true only when the type is null.
  - [P3] `equal` against another identified array, including a detailed array, is ALWAYS only when `equals` holds: same class, type, may-be-extension, may-be-null, equal length, same factory instance, and same id. Otherwise it uses typed-reference equality and ignores length, so two different ids of the same array type are MAYBE even when the lengths are NEVER equal. A plain array participates only through reverse dispatch, so the length NEVER check runs only when that plain array receives the second call.
  - [P3] `generalize` only reverse-dispatches. The base identified-array case widens to array generalization: an equal pair returns the receiver, still identified; the same type string and the same referenced-class object yield a new plain array reference with the OR of the extension flags and the generalized length, and the id is dropped; every other pair uses typed generalization.
  - [P3] `equals` and `hashCode` add the factory and the id to the array identity. The text form appends `#` and the id to the array form.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/IdentifiedDoubleValue.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard partial-evaluation value for a specific double tagged with a `ValueFactory` and a unique integer id.
  - [P3] Equality is missing: same instance, or both `IdentifiedDoubleValue` with an equal factory and the same id. Same class alone is not enough.
  - [P3] `hashCode` combining the class hash, the factory hash, and the id is missing.
  - [P3] The display form `d` plus the id is missing.
  - [P3] Inherited specific-double behavior is missing: `isSpecific` is true, and negate, numeric conversions, generalize (equal values stay specific, otherwise the unknown double), and binary arithmetic build negated, converted, or composite values instead of computing a concrete double.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/IdentifiedFloatValue.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no float value identified by a unique id and the value factory that created it.
  - [P3] Equality is missing: the same instance, or the same class (via `SpecificFloatValue.equals`) with an equal value factory and the same id.
  - [P3] Hashing is missing: the class hash XOR the value factory's hash XOR the id.
  - [P3] The `f<id>` display form is missing.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/IdentifiedIntegerValue.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No integer value that stands for one unknown int distinguished only by a `ValueFactory` and a unique id. It is specific (`isSpecific` is true) but `value()` stays the default `0`.
  - [P3] Equality is missing: the same object, or another `IdentifiedIntegerValue` with an equal factory and the same id. A different factory, a different id, or any other integer subclass is not equal.
  - [P3] The hash (`getClass().hashCode() ^ valueFactory.hashCode() ^ id`) and the `"i"+id` text form are absent.
  - [P3] Inherited specific-integer operations are absent. Matching factory and id collapse subtract and xor to `0`, and/or and generalize to self, and comparisons to `ALWAYS` or `NEVER`. Otherwise the results are composite values, the unknown integer, or `MAYBE`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/IdentifiedLongValue.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific long identified by a value factory and an integer id.
  - [P3] Equality is the same instance, or the same class with an equal value factory and the same id. The hash is the class hash XOR the factory hash XOR the id. Zel does not implement that.
  - [P3] The text form `l<id>` is absent.
  - [P3] The value is specific, so equal ids collapse under inherited long arithmetic (generalize, and, and or stay this value; subtract and xor become 0) and unequal ids stay symbolic. Zel has no such lattice.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/IdentifiedReferenceValue.java`
- Bazel lines: 212
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard partial-evaluation reference lattice, so there is no identified reference: a typed reference plus the creating `ValueFactory` and a unique integer id.
  - [P3] `isSpecific()` is always true. `equal(IdentifiedReferenceValue)` returns `ALWAYS` only when `equals` holds (same class, same type/null/extension flags, same factory, and same id). Otherwise it uses typed equality: `ALWAYS` only for two nulls, else `MAYBE` if either side may be an extension or the types match, else `NEVER`.
  - [P3] `generalize(ReferenceValue)` and `equal(ReferenceValue)` double-dispatch to the other operand. There is no live `generalize(IdentifiedReferenceValue)`, so generalizing two identified references drops the id and follows the typed-reference path.
  - [P3] `equals` and `hashCode` include the factory and id, and `toString` appends `#` plus the id.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/IdentifiedValueFactory.java`
- Bazel lines: 96
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no value factory that stamps unknown values with per-instance integer ids. Five counters start at 0: integer, long, float, double, and one shared reference counter.
  - [P3] `createIntegerValue()`, `createLongValue()`, `createFloatValue()`, and `createDoubleValue()` each return a new identified value holding this factory and the next id of that category. A known particular number does not take an id (cached integers `-1` through `5`, longs `0` and `1`, floats `0`/`1`/`2`, doubles `0`/`1`, with `+0` distinct from `-0`). An integer range drops the bounds and still takes a new integer id.
  - [P3] `createReferenceValue` with a null type returns the shared null reference (null type, not an extension, may be null) and does not take an id. Any other type, including `""`, returns a new identified reference with that type, class, `mayBeExtension`, and `mayBeNull`, and the next reference id.
  - [P3] `createArrayReferenceValue` with a null component type returns that same shared null reference and does not take an id. Otherwise the type is `[` plus the component type, `mayBeExtension` and `mayBeNull` are both false, the given length is kept, and the next id comes from the same reference counter.
  - [P3] The no-arg constructor still delegates the methods this class does not override to an `ArrayReferenceValueFactory`. No-arg `createReferenceValue()` returns the shared unknown reference and takes no id. `createReferenceValueNull()` returns the shared null reference. The array overload that also receives an element value ignores that element, does not take a reference id, and returns a non-identified typed reference whose type is `[` concatenated with the component type (`"[null"` when the component type is null).
  - [P3] Inherited `createValue` returns null for `void` and routes `boolean`, `byte`, `char`, `short`, `int`, `long`, `float`, and `double` through those identified no-arg creators. An internal type of length greater than 1 that starts with `[` becomes an identified reference with that type unchanged, not an identified array, so it has no length. Any other reference type is the internal class name (`L...;` with the leading `L` and trailing `;` removed when it is a class type).

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/InitialValueFactory.java`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard helper that turns a field or array-element type descriptor into the JVM default by forwarding to a stored value factory. This type is not itself a value factory.
  - [P3] Only the first character is read. `Z`, `B`, `C`, `S`, and `I` become integer 0; `J` becomes long 0; `F` becomes float 0.0; `D` becomes double 0.0; `L` and `[` become a null reference. The rest of the descriptor is ignored, so `I…`, `Ljava/…;`, and `[[I` follow that first character. Boolean, byte, char, and short are not narrower values.
  - [P3] Any other first character, including `V` and `T`, throws `IllegalArgumentException` with message `Invalid type [<type>]`. An empty type throws `StringIndexOutOfBoundsException` from `charAt(0)` instead.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/InstructionOffsetValue.java`
- Bazel lines: 477
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard partial-evaluation value that stores zero or more instruction offsets, each tagged in the high bits: method parameter `0x01000000`, method return `0x02000000`, field `0x04000000`, new instance `0x08000000`, cast `0x10000000`, exception handler `0x20000000`. The offset mask is `0x01ffffff`, so `instructionOffset` still keeps the parameter bit; `methodParameter` clears only that bit. The other origin tests are single-bit checks.
  - [P3] `contains`, `add`, and `remove` match the stored int exactly, flags included. `add` appends when absent. `remove` drops the first exact hit. Both return the same instance when the list does not change. An empty list is the shared `EMPTY_VALUE`. The array constructor keeps the caller’s array.
  - [P3] `minimumValue` and `maximumValue` use masked offsets. An empty list yields `Integer.MAX_VALUE` and `Integer.MIN_VALUE`.
  - [P3] `generalize` keeps the other list at the end, in order. An empty side returns the other instance. It returns the other instance when `this.length` equals the number of other entries found in this. It returns this only when the union length equals this length, the other list has one entry, and that entry is already last. Otherwise it drops this entries that the other list contains and appends the whole other list. The allocated length is `this.length` plus other entries absent from this, so extra copies on this side leave trailing `0`s, and extra copies on the other side make `System.arraycopy` throw.
  - [P3] The value is always specific and particular, category 1, computational type instruction-offset (`6`), and internal type `"I"`. `generalize(Value)` requires the other value to already be an instruction-offset value. `equals` is same class and equal length, then every other entry contained in this, so duplicate counts can make different lists equal. `hashCode` is the class hash XOR each stored int. `toString` prints a leading comma-separated list and a trailing `:`; a negative entry is printed raw, and any other entry is its origin letters (`P`, `M`, `F`, `N`, `C`, `E`) plus the low 16 bits.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/IntegerValue.java`
- Bazel lines: 1221
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard integer partial-evaluation value. The default `value()` is `0`. `integerValue()` returns this value. `computationalType()` is `TYPE_INTEGER` (`1`). `internalType()` is `"I"`.
  - [P3] `generalize(Value)` generalizes with `other.integerValue()`. A non-integer other value throws `IllegalArgumentException` from that cast.
  - [P3] `notEqual` is `-equal`, `greaterThan` is `-lessThanOrEqual`, and `greaterThanOrEqual` is `-lessThan`, for plain, unknown, specific, particular, and range arguments. The results are `NEVER` (`-1`), `MAYBE` (`0`), or `ALWAYS` (`1`).
  - [P3] Unknown, specific, and range arithmetic, shifts (including shifting a long by this integer), bitwise ops, and the three primitive comparisons cast the argument to `IntegerValue` or `LongValue` and call the abstract operation. Particular arguments are cast to the specific type and go through the specific overload first, so a specific override applies to particular inputs.
  - [P3] Negation, narrowing to byte, char, or short, widening to long, float, or double, and the abstract binary operations are only declared here. Concrete results are supplied by subclasses, and zel has none of that lattice.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/LongValue.java`
- Bazel lines: 554
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No abstract category-2 long. `value()` is 0 unless overridden. `longValue()` returns this. `generalize(Value)` generalizes with `other.longValue()`, so a non-long throws `IllegalArgumentException`. The computational type is `TYPE_LONG` (2) and the internal type is `J`.
  - [P3] `negate` and conversion to integer, float, and double are abstract and have no implementation.
  - [P3] Ops on another plain `LongValue` swap to that operand: commutative ops call the same method, `subtract`/`divide`/`remainder` use `subtractFrom`/`divideOf`/`remainderOf`, shifts call `IntegerValue.shiftLeftOf`/`shiftRightOf`/`unsignedShiftRightOf`, and `compare` calls `compareReverse`. Two values that only use these base methods recurse. `multiply`, `divide`, and `remainder` may throw `ArithmeticException` from the other side.
  - [P3] Against a `SpecificLongValue`, generalize, arithmetic, bitwise ops, and shifts return this long unchanged. The specific `divide` and `remainder` overloads do not throw. `compare` returns `new ComparisonValue(this, other)`, not -1, 0, or 1.
  - [P3] `ParticularLongValue` overloads delegate to those specific-long methods. A `ParticularIntegerValue` shift is cast to `SpecificIntegerValue` and uses the `IntegerValue` shift path, not the specific-long shift that returns this.
  - [P3] `compareReverse` is final and returns `compare(other).negate()`, so -1, 0, and 1 become 1, 0, and -1 for plain, specific, and particular arguments.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/NegatedDoubleValue.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no partial-evaluation double that stores another double as its negation. `negate()` returns that stored value, so a second negation is the original object. The concrete `value()` stays the default `0.0`.
  - [P3] Two such values are equal when they are the same instance or the same class with equal stored values. The hash mixes the class hash with the stored hash. The text form is `-` concatenated with the stored value. Zel has none of that.
  - [P3] The value stays specific: equal values generalize to the same value, other specific doubles become an unknown double, and arithmetic, comparisons, and conversions use the inherited specific-double rules. Zel does not track that.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/NegatedFloatValue.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific float that stores the float it negates. `SpecificFloatValue.negate()` builds one for an identified, converted, or composite float. A particular float becomes `ParticularFloatValue(-value)`, and an unknown float stays itself.
  - [P3] `negate()` returns the stored float, so a second negation is that same object. `value()` stays the inherited `0f` and does not apply a numeric sign change.
  - [P3] Equality is the same instance, or the same class and equal stored floats. The hash is the class hash XOR the stored hash. The text form is `-` plus the stored value, with no parentheses. A negated float is not equal to its source, so generalizing them yields the unknown float.
  - [P3] It stays specific. Arithmetic with another specific float becomes a composite float, comparison yields the unknown integer, and conversion to int, long, or double wraps a converted value.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/NegatedIntegerValue.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no symbolic integer value for the negation of another `IntegerValue`. This type only stores that operand. It does not compute a two's-complement negation, and `value()` stays the inherited default `0`.
  - [P3] `negate()` returns the wrapped operand, so a second negation cancels. It does not wrap another negated value.
  - [P3] Equality is missing: reference equality, or the same runtime class and equal wrapped values. A negated value is not equal to its operand, or to a particular integer of the arithmetic negation, unless those wrapped values are equal.
  - [P3] The matching hash (`getClass().hashCode() ^ operand.hashCode()`) and the `"-"+operand` rendering are absent.
  - [P3] Inherited specific-integer operations are absent. `isSpecific` is true, and arithmetic, shifts, bitwise ops, and comparisons stay symbolic (equal values yield `ALWAYS` / `NEVER` / zero / the value itself; otherwise composite values or `MAYBE`).

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/NegatedLongValue.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific long that stands for the negation of another long.
  - [P3] Negating that value returns the wrapped long, so a second negation is the original value. Zel does not unwrap a negation that way.
  - [P3] Equality is the same instance, or the same class with equal wrapped longs. The hash is the class hash XOR the wrapped hash, and the text form is `-<inner>`. A negated long matches another only when the operands match, and it does not match the unnegated operand. Zel does not implement that.
  - [P3] Because the value stays specific, equal negated longs subtract and xor to the particular long 0 and and/or to themselves, while a negated long against its operand stays a composite. Zel does not keep that distinction.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ParticularDoubleValue.java`
- Bazel lines: 220
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no concrete double in the partial evaluator. This value stores one `double`, `value()` returns it, and `isParticular()` is true.
  - [P3] Negation is arithmetic negation of that double. Conversion uses the Java casts `(int)`, `(long)`, and `(float)`, each wrapped as a particular value.
  - [P3] With any other double category, generalize, arithmetic, and compare reverse-dispatch to the other operand. A stored `0.0` is not short-circuited.
  - [P3] Two particular doubles generalize to this value only when `doubleToRawLongBits` matches, so distinct NaN payloads and `-0.0` stay distinct; otherwise the result is an unknown double. Add, subtract, multiply, divide, and remainder are Java `double` operations and stay particular.
  - [P3] Compare uses `<` then `==`: `-0.0` equals `+0.0`, and any NaN yields `1`.
  - [P3] Equality requires the same class and equal `doubleToLongBits` (all NaNs match, `-0.0` does not match `+0.0`). The hash mixes the class hash with the low 32 bits of those bits. The text form is the Java double text plus `d`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ParticularFloatValue.java`
- Bazel lines: 220
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No known float value: `value()` is that float, and `isParticular()` is true.
  - [P3] No concrete negation (`-value`) and no Java conversions to int, long, or double (`(int)value`, `(long)value`, and the float widened to double).
  - [P3] Generalize and arithmetic on a non-particular float always double-dispatch to the other operand. A stored `0.0` is not treated as an identity, so signed zero is left to that operand.
  - [P3] Two particular floats generalize to this value only when `floatToRawIntBits` matches (so `+0`/`-0` and distinct NaN payloads differ); otherwise the result is the unknown float. Add, subtract, multiply, divide, and remainder are IEEE float operations, including the reversed subtract, divide, and remainder forms. Compare is `<`, then `==`, otherwise `+1` (a NaN yields `+1`; `+0` compares equal to `-0`).
  - [P3] Equality requires the same class and `floatToIntBits` (every NaN matches; `+0` and `-0` do not). The hash is the class hash XOR those bits. The text form is the float followed by `f`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ParticularIntegerValue.java`
- Bazel lines: 506
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no concrete `int` value in a ProGuard-style lattice. This type stores one 32-bit value, `value()` returns it, and `isParticular()` is true.
  - [P3] Unary ops are missing: negation, `(byte)` / `(char)` / `(short)` narrowing (reuse the same value when the bits do not change), and widening to a particular `long`, `float`, or `double`. Negation and the casts follow Java overflow (`-Integer.MIN_VALUE` stays `MIN_VALUE`).
  - [P3] Arithmetic on two particular ints is missing: 32-bit wraparound `+`, `-`, `*`, `&`, `|`, `^`, and `<<` / `>>` / `>>>` (shift count masked to 5 bits). `/` and `%` use Java division and throw `ArithmeticException` for divisor `0` and for `MIN_VALUE / -1`. The same shifts of a particular `long` by this int are missing.
  - [P3] Two particular ints compare as `ALWAYS` or `NEVER` (`==`, `<`, `<=`). Generalizing them keeps this value when they are equal and otherwise becomes the unknown integer. Ops and comparisons with a general or range integer are reversed onto the other operand (`lessThan` becomes the other’s `greaterThan`, and likewise for `<=`).
  - [P3] Equality is the same class and the same int. The hash is `getClass().hashCode() ^ value`, and `toString` is the decimal form of the int.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ParticularLongValue.java`
- Bazel lines: 270
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no concrete long in the ProGuard value lattice. This type stores one long, `value()` returns that long, and `isParticular()` is true.
  - [P3] Negation and conversions are Java primitive operations: `-value` leaves `Long.MIN_VALUE` unchanged, `(int)` keeps the low 32 bits, and `(float)` / `(double)` round with Java’s precision rules.
  - [P3] Two particular longs use wrapping Java `+`, `-`, `*`, `/`, `%`, `&`, `|`, and `^`. A zero divisor makes `/` and `%` throw `ArithmeticException`. `Long.MIN_VALUE / -1` is `Long.MIN_VALUE`, `Long.MIN_VALUE % -1` is 0, and the remainder takes the dividend’s sign. Generalizing two of these returns this value when they are equal and the unknown long otherwise.
  - [P3] Shifts by a particular int use Java `<<`, `>>`, and `>>>`, masking the distance to six bits. A non-particular operand is reverse-dispatched and does not fold to a concrete long.
  - [P3] `lcmp` is not computed numerically. Comparing two particular longs yields a negated `ComparisonValue` of the swapped operands, not a particular -1, 0, or 1.
  - [P3] Equality requires the same class and the same 64-bit value. The hash is the class hash XOR only the low 32 bits, so longs that differ only above bit 31 collide. The text form is the decimal value plus `L`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ParticularValueFactory.java`
- Bazel lines: 169
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no value factory that returns particular primitive constants and forwards every reference creator to another factory. The no-arg constructor uses an `ArrayReferenceValueFactory`; the other constructor stores the given reference factory.
  - [P3] `createIntegerValue(int)` returns a shared `ParticularIntegerValue` for -1, 0, 1, 2, 3, 4, and 5, and a new one for every other int. Unknown integers and `(min, max)` ranges are not overridden, so both stay the shared unknown integer.
  - [P3] `createLongValue(long)` returns a shared `ParticularLongValue` for 0 and 1, and a new one otherwise. An unknown long stays the shared unknown long.
  - [P3] `createFloatValue(float)` returns the shared +0.0f only when `floatToIntBits` matches positive zero, so -0.0f is a new `ParticularFloatValue`. 1.0f and 2.0f are shared; every other float, including NaN, is new. An unknown float stays unknown.
  - [P3] `createDoubleValue(double)` returns the shared +0.0 only when `doubleToLongBits` matches positive zero, so -0.0 is new. 1.0 is shared and there is no shared 2.0. An unknown double stays unknown.
  - [P3] `createReferenceValue`, the null reference, a typed reference, and both array-reference creators only delegate. Inherited `createValue` still yields unknown primitives, while a reference type goes through that delegate.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/PrimitiveTypedReferenceValueFactory.java`
- Bazel lines: 69
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard reference-value factory that keeps a type only for primitive arrays.
  - [P3] A null reference, including a null type passed to reference creation, is one shared typed value: type null, referenced class null, not an extension, and may be null.
  - [P3] Any other reference whose type is not an internal array (length greater than 1 and starting with `[`), or whose type is an internal class type (length greater than 1 and ending with `;`), is the shared untyped unknown reference. Type, referenced class, extension, and nullness are dropped. A primitive array such as `[I` or `[[Z` is a new typed reference that keeps all four.
  - [P3] An array reference with a null component type is that same null value. Any other component type is a new array reference whose type is `[` plus the component type, with the given referenced class and length, not an extension, and not null. The element value passed to the four-argument creator is ignored.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/RangeIntegerValue.java`
- Bazel lines: 736
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no inclusive integer interval. `value()` is the lower bound, `isParticular()` is true only when the bounds are equal, and the value stays non-specific (no identifier) even then.
  - [P3] Negation uses `-max` as the low bound, except a low bound of `Integer.MIN_VALUE` stays `MIN_VALUE`, and the high bound is `-min`. A range whose low bound is `MIN_VALUE` therefore collapses, because `-MIN_VALUE` overflows to `MIN_VALUE`.
  - [P3] A range that already fits in a byte, char, or short is returned unchanged. Any other range becomes the shared full range `-128..127`, `0..65535`, or `-32768..32767`. Conversion to long, float, or double is the unknown basic value.
  - [P3] Ops on an unspecialized integer are reversed onto the other operand: an unknown integer stays unknown, and a specific integer becomes a composite (generalization becomes the unknown integer; comparisons are `MAYBE`). `greaterThan` and `greaterThanOrEqual` are the negated `lessThanOrEqual` and `lessThan` results.
  - [P3] With a particular integer, add and subtract keep the range only when the translated bounds do not wrap (`+0` and `-0` return this); otherwise the result is the unknown integer. `subtractFrom` is `[value-max, value-min]` or unknown on a 64-bit overflow check. Multiply returns the particular `0`, this for `1`, swaps the endpoints when the factor is negative, and is unknown on overflow. Divide by `0` is unknown and does not throw; divide by `1` returns this; a negative divisor swaps the endpoints. `divideOf` is unknown if the range contains `0`, and the endpoint order follows whether the dividend and the low bound have different signs. Remainder is unknown when the divisor is non-positive or the range contains a negative; if every value is smaller than the divisor it returns this, otherwise `0..divisor-1`. `remainderOf` is unknown for a negative dividend or a non-positive low bound; a dividend below the range returns that particular value; otherwise the result is `0..value` when `value < max`, and `0..max-1` otherwise.
  - [P3] A particular shift whose low 5 bits are 0 returns this. Signed `>>` keeps `[min>>s, max>>s]`. Signed `<<` is unknown when the 64-bit shift differs from the 32-bit shift (a negative count usually does, because the long check masks 6 bits). Unsigned `>>>` of a range that crosses 0 is `[0, MIN_VALUE>>>s]`; an all-negative range is ordered `[max>>>s, min>>>s]`; an all-non-negative range keeps the original order. Shifting a particular integer by this range is unknown when the shift range is negative or reaches 32 or more. Inside `0..31`, `<<` and `>>` swap endpoints for a negative value, and `<<` is also unknown when `value<<max` overflows; `>>>` is always `[value>>>max, value>>>min]`. Shifting a particular long by this range returns that long only when it is `0`, and the unknown long otherwise.
  - [P3] `and` with `0` or `-1` returns that particular value or this range; any other positive mask becomes `0..mask`, and a negative mask is unknown. `or` with `0` or `-1` returns this or that value; any other negative mask becomes `mask..-1`, and a positive mask is unknown. `xor` returns this only for `0`, and the unknown integer otherwise.
  - [P3] Two ranges generalize to the covering range, or to whichever range already contains the other. Add is `[min+min, max+max]` and subtract is `[min-other.max, max-other.min]`, each unknown if either bound wraps. Range multiply, divide, remainder, shifts, and, or, and xor are all the unknown integer, with no throw on a divisor range that contains `0`.
  - [P3] Equality is `ALWAYS` only when both ranges are the same singleton, `NEVER` when the ranges are strictly disjoint, and `MAYBE` when they touch or overlap. `lessThan` is `ALWAYS` when `max < other.min` and `NEVER` when `other.max <= min`. `lessThanOrEqual` is `ALWAYS` when `max <= other.min` and `NEVER` when `other.max < min`. Against a particular integer, `equal` is `ALWAYS` only for a matching singleton and `NEVER` outside the range; `lessThan` is `ALWAYS` when `max < value` and `NEVER` when `value <= min`; `lessThanOrEqual` is `ALWAYS` when `max <= value` and `NEVER` when `value < min`.
  - [P3] Object equality is reference equality (`super.equals` is identity, so equal bounds do not compare equal). The hash is the class hash XOR both bounds, and the text form is `min..max`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/RangeValueFactory.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A known integer interval becomes a single particular integer when the bounds are equal, and a range integer when they differ.
  - [P3] Shared full-domain ranges are byte −128..127, char 0..65535, and short −32768..32767, reused when a conversion result does not already fit that type.
  - [P3] Reference values are created by a nested factory, defaulting to the particular factory’s array-reference factory.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ReferenceValue.java`
- Bazel lines: 382
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no partial-evaluation reference-value lattice: no type, referenced class, may-be-extension, nullness (`NEVER`/`MAYBE`/`ALWAYS`), `instanceof`, or cast queries.
  - [P3] No default array behavior: unknown integer length, unknown integer/long/float/double/reference loads from the value factory, or a no-op store.
  - [P3] No derived nullness or equality (`isNotNull`/`notEqual` as negation), no `generalize`/`equal` overload chain (unknown reference returns the other value and `MAYBE`; typed, identified, array, detailed-array, and traced variants delegate), and no `generalize(Value)` via `referenceValue()`.
  - [P3] No reference computational type, and no class-identity `equals`/`hashCode` or `"a"` string form.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/SpecificDoubleValue.java`
- Bazel lines: 186
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific double that stays specific and only builds symbolic results: negation wraps a negated double, and `d2i` / `d2l` / `d2f` wrap converted int, long, and float values.
  - [P3] A general other double is handled by reversing the call (`add` to `other.add`, `subtract` to `subtractFrom`, `divide` to `divideOf`, `remainder` to `remainderOf`, `compare` to `compareReverse`). Two specific doubles generalize to this value when `equals` is true and otherwise to an unknown double. Add, subtract, multiply, divide, and remainder build a composite, swapping the operands for `subtractFrom`, `divideOf`, and `remainderOf`. Comparing two specific doubles always yields an unknown int. The path that would return 0 when they are equal, and a comparison value otherwise, is commented out because it mishandles NaN.
  - [P3] Equality and hashing use only the concrete class: any two non-null instances of the same subclass match and share that class's hash, with no field comparison. Zel tracks none of this.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/SpecificFloatValue.java`
- Bazel lines: 186
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific-float abstract value. Unary `negate` and `convertToInteger` / `convertToLong` / `convertToDouble` never produce a negated float or a converted integer, long, or double.
  - [P3] Binary ops on a generic float never double-dispatch to the other operand (`generalize`, `add`, `subtract` via `subtractFrom`, `multiply`, `divide` via `divideOf`, `remainder` via `remainderOf`, `compare` via `compareReverse`).
  - [P3] Two specific floats never generalize to the receiver when `equals` and otherwise to an unknown float. Add, subtract, multiply, divide, and remainder never build a composite float, including the operand swap in `subtractFrom`, `divideOf`, and `remainderOf`.
  - [P3] Comparing two specific floats always yields an unknown integer. The commented NaN-aware alternative (zero when equal, otherwise a comparison value) is not executed.
  - [P3] `isSpecific` is always true. Equality is non-null and the same concrete class, with no numeric payload, and `hashCode` is that class’s hash.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/SpecificIntegerValue.java`
- Bazel lines: 477
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific-integer abstract value. ProGuard marks it specific, equates values only by concrete class, wraps unary negation and narrowing/widening conversions, and reverses general binary integer and long-shift operations onto the other operand.
  - [P3] Same-class pairs collapse equal subtract and xor to 0, equal and/or to this value, and equal comparisons to ALWAYS, NEVER, or ALWAYS. Unequal pairs become composite operations, and division and remainder still build those composites without a zero check here.
  - [P3] Range interactions generalize to the unknown integer, otherwise stay composite, and compare as MAYBE. None of that lattice is implemented in zel.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/SpecificLongValue.java`
- Bazel lines: 259
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no specific long that stays specific and only builds symbolic results. Negation wraps a negated long, and conversion wraps a converted integer, float, or double.
  - [P3] A general other long is handled by reversing the call: commutative ops call the same method, `subtract`/`divide`/`remainder` use `subtractFrom`/`divideOf`/`remainderOf`, and `compare` uses `compareReverse`. Shifts by a general integer call `shiftLeftOf`/`shiftRightOf`/`unsignedShiftRightOf` on that integer. `divide` and `remainder` may throw `ArithmeticException` from the other side.
  - [P3] Two specific longs generalize to this value when `equals` is true and otherwise to an unknown long. Equal values subtract or xor to the particular long 0, and and/or to this value. Otherwise add, subtract, multiply, divide, remainder, shifts, and bitwise ops build a composite. `subtractFrom`, `divideOf`, and `remainderOf` put the other value on the left. `and`, `or`, and `xor` put the other value on the left as well. The specific `divide` and `remainder` overloads do not throw.
  - [P3] Comparing two specific longs returns a comparison value, not -1, 0, or 1.
  - [P3] `isSpecific` is always true. Equality is non-null and the same concrete class, with no numeric payload, and `hashCode` is that class’s hash.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/TopValue.java`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no partial-evaluation top value: the category-1 dummy word that fills the extra stack or local slot of a long or double.
  - [P3] `isSpecific()` and `isParticular()` are both true.
  - [P3] `generalize` returns this value only when the other value has the same class, and null otherwise.
  - [P3] The computational type is `TYPE_TOP` (7) and the internal type is null.
  - [P3] Any two top values are equal, the hash code is the class hash, and `toString` is `"T"`.
  - [P3] As a category-1 value, `category1Value()` returns this and `isCategory2()` is false. Typed downcasts from `Value` throw.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/TracedReferenceValue.java`
- Bazel lines: 334
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no traced reference: a `ReferenceValue` paired with a separate trace `Value`. Type, referenced class, may-be-extension, nullness, `instanceof`, array length, primitive array loads, array store, specificity, particularity, and internal type all come from the inner reference. The trace is ignored for those queries.
  - [P3] `cast` does not cast the inner value itself. It requires a `ReferenceTracingValueFactory`, casts the inner reference with that factory’s underlying factory, and rewraps the result. The new trace is the old trace when `preserveTraceValueOnCasts` is set, otherwise the factory’s current trace.
  - [P3] `referenceArrayLoad` keeps an already traced load. Otherwise it asks the tracing factory to attach its current trace, which is a no-op when that trace is null. A non-tracing factory throws `ClassCastException`.
  - [P3] `generalize(ReferenceValue)` and `equal(ReferenceValue)` double-dispatch to the other operand. Generalizing with an unknown, typed, identified, array, identified-array, or detailed-array value keeps this trace and generalizes only the inner reference. Equality with those types, and with another traced reference, compares only the inner references.
  - [P3] Generalizing two traced references returns `this` when `equals` holds. Otherwise both the inner references and the traces are generalized and wrapped again.
  - [P3] `equals` requires the same class plus equal inner reference and trace. `hashCode` is the XOR of those two hash codes. `toString` is the trace text immediately followed by the reference text.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/TracingValue.java`
- Bazel lines: 164
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard partial-evaluation wrapper that stores a sticky trace `Value` beside an inner `Value`.
  - [P3] `category1Value`, `category2Value`, `integerValue`, `longValue`, `floatValue`, `doubleValue`, `referenceValue`, and `instructionOffsetValue` return the inner value's view and drop the trace. A type error is the inner value's exception, so the message uses the inner `toString` and class rather than the wrapper.
  - [P3] `isSpecific`, `isParticular`, `isCategory2`, `computationalType`, and `internalType` follow only the inner value. The trace does not change them.
  - [P3] `generalize(TracingValue)` returns this when `equals` is true. Otherwise it allocates a `TracingValue` whose trace is `traceValue.generalize(other.traceValue)` and whose value is `value.generalize(other.value)`, even if one side did not change.
  - [P3] `generalize(Value)` uses that two-trace path when the other value is a `TracingValue`, including a subclass. Otherwise, if the inner value `equals` the other value, it returns this and keeps the original trace. Otherwise it returns a new `TracingValue` with the same trace object and `value.generalize(other)`. A non-tracing other never generalizes or drops the trace.
  - [P3] `equals` requires the exact class, then equal traces and equal inner values. A subclass is not equal, and null is false. `hashCode` is the class hash XOR the trace hash XOR the inner hash. `toString` is `P` immediately followed by the trace's `toString` and then the inner value's `toString`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/TypedReferenceValue.java`
- Bazel lines: 730
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no typed reference (type string, referenced class, may-be-extension, may-be-null). A null type is the null value: `isNull` is `ALWAYS`, `isParticular` is true, `internalType` is `Ljava/lang/Object;`, and `toString` is `n`. Otherwise nullness is `MAYBE` or `NEVER`, array descriptors are kept, and other types are wrapped as `L` + type + `;`.
  - [P3] `instanceOf` is `NEVER` for a null type. After the shared array rank is stripped, a primitive element is decided by exact type equality (`NEVER`, or `MAYBE`/`ALWAYS` from nullness). A higher-rank array matches only `java/lang/Object`, `java/lang/Cloneable`, or `java/io/Serializable`. Equal types, `java/lang/Object`, or a higher-rank array are `ALWAYS` unless null is possible. A lower-rank value against a higher-rank array is `MAYBE`. A missing class yields `MAYBE`. Otherwise the result follows `extends`/`implements`; a final class is `NEVER`, and two non-interface classes outside each other's hierarchy are `NEVER`.
  - [P3] `cast` returns this when the type strings match, or, unless `alwaysCast`, when this type is null or `instanceOf` is `ALWAYS`. Otherwise the factory creates the target type with may-be-extension true and this nullness.
  - [P3] `generalizeMayBeNull` returns this when the flag already matches, and otherwise a new value that may be null. `referenceArrayLoad` returns the shared null for a null type, the shared maybe-null `java/lang/Object` for a non-array, and for an array a maybe-extension, maybe-null value of `type` without its first character, still carrying the array's referenced class.
  - [P3] `generalize(ReferenceValue)` and `equal(ReferenceValue)` double-dispatch. `equal` on two typed values is `ALWAYS` only for two null types; one null is `MAYBE` only if the other may be null; two non-nulls are `MAYBE` if either may be an extension or the type strings match, otherwise `NEVER`.
  - [P3] `generalize` of two typed values returns this when `equals` holds (exact class, and for a non-null type the type string, extension flag, and nullness; the referenced class is ignored, and a null type ignores flags). Both null types otherwise become the shared null. One null widens the other to maybe-null. The same type string keeps that type and ORs both flags, reusing this only when it is exactly `TypedReferenceValue` and the extension flag already matches. Equal rank with both classes known returns the superclass with may-be-extension forced true, otherwise the most specific common superclass (or a common interface when that superclass is `java/lang/Object`), with the lexicographically smaller name winning ties and may-be-extension ORed. An incomplete hierarchy throws `IllegalArgumentException`, or returns the unknown reference when `allow.incomplete.class.hierarchy` is set. Different ranks keep the shorter type when it is an array interface, with may-be-extension forced true. Otherwise the result is an array of `java/lang/Object` at the common rank (that rank drops by one when either element there is primitive), or the shared maybe-null or not-null `java/lang/Object`.
  - [P3] `equals` and `hashCode` use the exact class plus, when the type is non-null, the type string, extension flag, and nullness. A null type ignores those flags. The referenced class is ignored. `toString` is the type plus `?` when the class is missing, `=` when it may not be an extension, and `!` when it may not be null.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/TypedReferenceValueFactory.java`
- Bazel lines: 83
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard value factory whose references keep a type, referenced class, extension flag, and nullness, with shared copies of null and `java/lang/Object`.
  - [P3] `createReferenceValueNull()` is one shared typed value: type null, referenced class null, not an extension, and may be null. A null type passed to `createReferenceValue` is that same value; the class, extension, and nullness arguments are ignored.
  - [P3] Any other type is a new typed reference that keeps all four arguments, except `java/lang/Object` when `mayBeExtension` is true. That case drops the referenced class and returns one of two shared values, may-be-null or not null. `java/lang/Object` with `mayBeExtension` false is a new value and keeps the given class. The no-argument `createReferenceValue()` is still the shared untyped unknown reference.
  - [P3] Both array creators ignore the length. The three-argument creator first builds an element value with `createValue(type, referencedClass, true, true)` and then discards it, so a null or empty component type throws before a reference is built. The four-argument creator ignores the element value and returns a typed reference whose type is `[` concatenated with the component type (`"[null"` when the component type is null), not an extension, and not null, keeping the referenced class. That type is never a shared `java/lang/Object` value.
  - [P3] Inherited `createValue` returns null for `void`. `boolean`, `byte`, `char`, `short`, and `int` are one shared unknown integer, and a particular value or range is dropped. `long`, `float`, and `double` are each one shared unknown value. An internal type of length greater than 1 that starts with `[` is passed through unchanged. Any other reference type is the internal class name: when the string ends with `;` and its length is greater than 1, the span from the first `L` through the final `;` is removed. `Ljava/lang/Object;` therefore uses the shared `java/lang/Object` values when it may be an extension, while `[Ljava/lang/Object;` stays a new typed array type.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/UnknownDoubleValue.java`
- Bazel lines: 124
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no unknown double that stays unknown under every double operation: negation, generalization, add, subtract, subtractFrom, multiply, divide, divideOf, remainder, and remainderOf all return this value. Converting to int, long, or float, and comparing with another double, return the unknown integer, long, or float.
  - [P3] Specific and particular overloads inherit the parent forwarding and still return that unknown result. Reverse comparison negates the unknown integer, which remains the unknown integer. The concrete `value()` stays the default `0.0`. `doubleValue()` is this, generalizing with any value uses the other's double view, the computational type is double, and the internal type is `D`.
  - [P3] Equality and hashing use only the concrete class: any two non-null instances of this class match and share that class's hash. The text form is `d`. Zel tracks none of this.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/UnknownFloatValue.java`
- Bazel lines: 124
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no unknown float in a partial-evaluation lattice. `isSpecific()` and `isParticular()` stay false, and the inherited `value()` is `0f`. `computationalType()` is `TYPE_FLOAT` (`3`), `internalType()` is `"F"`, and it is a category-1 value whose `floatValue()` is itself.
  - [P3] `negate()` returns this same unknown float. Conversion yields the shared unknown integer, long, or double, not a converted wrapper around this float.
  - [P3] `generalize` with any float returns this, including specific and particular arguments, whose overloads cast to `FloatValue` and hit that method. It does not double-dispatch. Add, subtract, `subtractFrom`, multiply, divide, `divideOf`, remainder, and `remainderOf` also return this and ignore the operand, including NaNs, signed zeros, and the reversed forms.
  - [P3] `compare` returns the unknown integer for a plain, specific, or particular float. `compareReverse` negates that integer, which is still the unknown integer.
  - [P3] Equality is any non-null instance of this class, not the same object. The hash is the class hash. The text form is `f`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/UnknownIntegerValue.java`
- Bazel lines: 218
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no unknown integer in a ProGuard partial-evaluation lattice. `isSpecific()` and `isParticular()` stay false, and the inherited `value()` is `0`. `generalize(Value)` generalizes with `other.integerValue()` and returns this unknown integer. A non-integer other value throws `IllegalArgumentException` from that cast.
  - [P3] Negation and narrowing to byte, char, or short return this same value, with no range. Widening returns the shared unknown long, float, or double (`BasicValueFactory.LONG_VALUE`, `FLOAT_VALUE`, `DOUBLE_VALUE`).
  - [P3] Generalization and every integer binary operation (add, subtract, `subtractFrom`, multiply, divide, `divideOf`, remainder, `remainderOf`, both directions of signed and unsigned shifts, and, or, xor) return this unknown integer for any integer operand, including unknown, specific, particular, and range values. Multiply, divide, and remainder declare `ArithmeticException` but never throw, including division or remainder by zero.
  - [P3] Shifting a long by this integer (`shiftLeftOf`, `shiftRightOf`, `unsignedShiftRightOf`) returns the shared unknown long.
  - [P3] `equal`, `lessThan`, and `lessThanOrEqual` always return `MAYBE` (`0`), including against the same unknown integer. `notEqual` is `-equal`, `greaterThan` is `-lessThanOrEqual`, and `greaterThanOrEqual` is `-lessThan`, so those are `MAYBE` as well.
  - [P3] Object equality is the same runtime class (any two instances match). The hash is `getClass().hashCode()`, and `toString()` is `"i"`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/UnknownLongValue.java`
- Bazel lines: 159
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No unknown-long value in a bytecode value lattice. Negate, generalize, arithmetic, shifts, and bitwise ops stay an unknown long; int/float/double conversions and long compare yield the unknown value of that type.
  - [P3] `multiply`, `divide`, `divideOf`, `remainder`, and `remainderOf` declare `ArithmeticException` and never throw.
  - [P3] Equality is same concrete class (any two unknown longs match); hash code is that class’s hash; `toString` is `"l"`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/UnknownReferenceValue.java`
- Bazel lines: 205
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no unknown reference in a partial-evaluation lattice: type is always `java/lang/Object`, referenced class is null, it may be an extension, nullness is `MAYBE`, `instanceOf` is always `MAYBE`, and `isParticular()` is false (`isSpecific()` stays the base false).
  - [P3] `cast` ignores `alwaysCast` and always calls `valueFactory.createReferenceValue(type, referencedClass, true, true)` (may be an extension and may be null). `internalType()` is `Ljava/lang/Object;` and `toString()` is `"a"`.
  - [P3] `generalize(ReferenceValue)` and `equal(ReferenceValue)` double-dispatch to the other operand. The unknown/typed/identified/array overloads in this class are commented out, so the base implementations apply unless overridden: generalize returns this unknown value, and equal returns `MAYBE`. `TracedReferenceValue` wraps `referenceValue.generalize(unknown)` in a new traced value with the same trace and forwards equality to the wrapped reference.
  - [P3] Inherited reference operations still apply to this value: unknown array length and element loads, a no-op array store, `isNotNull` and `notEqual` as the negation of `MAYBE` (still `MAYBE`), category-1 reference computational type, and class-identity `equals`/`hashCode`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/Value.java`
- Bazel lines: 169
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no abstract partial-evaluation value with comparison results `NEVER`/`MAYBE`/`ALWAYS` (`-1`/`0`/`1`) or computational-type codes integer `1`, long `2`, float `3`, double `4`, reference `5`, instruction offset `6`, and top `7`.
  - [P3] `isSpecific()` and `isParticular()` both return false.
  - [P3] `category1Value`, `category2Value`, `integerValue`, `longValue`, `floatValue`, `doubleValue`, `referenceValue`, and `instructionOffsetValue` throw `IllegalArgumentException`. The message is `Value "` plus `toString()`, plus `" is not a … value ["`, plus the runtime class name, plus `]`.
  - [P3] `generalize(Value)`, `isCategory2()`, `computationalType()`, and `internalType()` have no implementation. Category 2 means two category-1 slots. `internalType()` is a JVM internal type string (primitive, class, or array).

### `third_party/java/proguard/proguard6.2.2/src/proguard/evaluation/value/ValueFactory.java`
- Bazel lines: 138
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard value factory for partial evaluation of integers, longs, floats, doubles, and references.
  - [P3] `createValue` takes a fully specified internal type (primitive, class, or array), a referenced class, whether the value may be an extension, and whether it may be null, and returns a `Value`.
  - [P3] Integers have an undefined creator, a particular `int` creator, and a creator for a possible range given by min and max. Long, float, and double each have only an undefined creator and a particular-value creator.
  - [P3] References are an undefined type, the null reference, or an internal class name or array type with a referenced class, `mayBeExtension`, and `mayBeNull`. A null type on that last creator means the reference is null.
  - [P3] Array references are non-null. Both creators take the element type, the referenced class, and an `IntegerValue` length. The second also takes the initial element `Value`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/CascadingDataEntryWriter.java`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry writer that tries a primary writer and falls back to a secondary writer when `createDirectory` or `sameOutputStream` returns false, or when `createOutputStream` returns null.
  - [P3] Zel does not close both delegates and then drop them, and it has no debug dump that prints both child writers.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/ClassDataEntryWriter.java`
- Bazel lines: 134
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry writer that, for a name ending in `.class`, strips that suffix, looks the class up in a class pool, and writes the pooled class instead of the original bytes. A pooled name that differs replaces the entry with a renamed entry whose name is the new class name plus `.class`. A non-null delegate stream is wrapped in a `DataOutputStream` and the class is written with `ProgramClassWriter`. A `RuntimeException` is rethrown as `Unexpected error while writing class [<original class name>] (<cause message>)` with the original as the cause. The stream is closed in a `finally`. A missing class or a null delegate stream writes nothing.
  - [P3] Every `.class` entry still returns a new `FilterOutputStream` over a null stream, including when the class is absent or the delegate returns null, so cascading writers treat the entry as consumed and must not write the original class bytes. Names that do not end in `.class` return the delegate stream unchanged.
  - [P3] `createDirectory` and `sameOutputStream` are forwarded to the delegate. `close` closes the delegate. `println` prints `ClassDataEntryWriter` and then the delegate indented by two spaces.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/ClassFilter.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No data-entry reader routes names that end with `.class`, case-insensitively, to a class reader and every other name to a second reader.
  - [P3] A null reader drops that branch. The one-argument form drops every non-`.class` entry.
  - [P3] A null data entry is not accepted and is not passed to the class reader.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/ClassMapDataEntryWriter.java`
- Bazel lines: 164
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry writer that, on `createOutputStream`, writes the whole program class pool as a class map and ignores the entry body. `close`, `createDirectory`, and `sameOutputStream` only forward to the wrapped writer. The debug `println` prints `ClassMapDataEntryWriter`, then the wrapped writer with two extra spaces of indent.
  - [P3] The stream comes from the wrapped writer and is encoded with the platform default charset. Each record is `poolKey,currentName,methodsFlag,fieldsFlag` plus the `line.separator` captured by `PrintWriter`. `/` becomes `.`; `$` is left as-is. There is no header. An empty pool writes no lines. `PrintWriter` does not auto-flush and swallows write errors. `close` flushes and closes the underlying stream, then that same stream is returned. A null stream from the delegate throws before any map is written.
  - [P3] Rows follow the pool’s `TreeMap` key order (`String` order of the names stored at insertion). The first field is that key. The second is the class’s current name, so a rename that updates the class but not the map key shows the old name, then the new one. The value is cast to `ProgramClass`; another class type throws, and a missing value throws.
  - [P3] The methods digit is `1` when the class has `ACC_REMOVED_METHODS` (`0x00020000`) or any program method has `ACC_RENAMED` (`0x00010000`); otherwise `0`. The fields digit uses `ACC_REMOVED_FIELDS` (`0x00040000`) or a renamed program field. The class bit is tested first, and a set bit skips the member walk. Members are visited in array order. Only program methods and program fields set the bit. The two flags live on the writer and are cleared at the start of each walk that runs.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/ClassPathDataEntry.java`
- Bazel lines: 118
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data entry that reads a named resource from the class loader that loaded this type. The string constructor stores that name unchanged. The `Class` constructor turns `Class.getName()` into an internal name by replacing `.` with `/` and appends `.class`, so nested classes keep `$` and array classes keep the JVM descriptor.
  - [P3] The name and the original name are that stored string. The size is always `-1`, it is never a directory, and the parent is always null. `toString` is the name.
  - [P3] The input stream is opened lazily with `getClass().getClassLoader().getResourceAsStream(name)` and then reused. A missing resource returns null and is looked up again on the next call. `closeInputStream` closes the cached stream and clears it so the next open fetches it again. Closing when nothing is cached throws `NullPointerException`. A bootstrap class loader, which returns null from `getClassLoader()`, also throws on open.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/ClassReader.java`
- Bazel lines: 117
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never reads a data-entry stream as a program class, or as a library class when `isLibrary` is set, passing `skipNonPublicLibraryClasses` and `skipNonPublicLibraryClassMembers` only to the library reader.
  - [P3] Zel has no post-parse check: if the class name is non-null, warn (only when a warning printer is set) unless the entry name is exactly `module-info.class` or, after replacing `File.pathSeparatorChar` with `/`, equals `<internal name>.class`. The warning is `Warning: class [<entry>] unexpectedly contains class [<external name>]`, and the class is still visited.
  - [P3] A null class name skips the visitor. The entry stream is closed only after a successful read. Any exception is rethrown as an `IOException` whose message is `Can't process class [<entry>] (<cause message>)` and whose cause is the original exception.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DataEntry.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel type models a ProGuard input entry (file, ZIP entry, or directory) with a current name distinct from the pre-rename original name.
  - [P3] No size query that returns the length in bytes or -1 when the length is unknown.
  - [P3] No directory check paired with a content stream that is only valid for a non-directory, with I/O failure on open.
  - [P3] No separate close of the stream previously opened for that entry, with I/O failure on close.
  - [P3] No parent-entry link that is absent when the entry has no parent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DataEntryClassWriter.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard class visitor that writes a `ProgramClass` through a `DataEntryWriter`. A program-class visit renames the template entry to the internal class name plus `.class` (`RenamedDataEntry`: only `getName` and `toString` change; original name, size, directory flag, parent, and input stream stay on the template). `toString` is `name + " == " + wrappedEntry`.
  - [P3] `createOutputStream` returning null skips the class. A non-null stream is wrapped in a `DataOutputStream` and passed to a new `ProgramClassWriter`. The stream is always closed in a `finally` (`DataOutputStream.close` flushes, then closes the underlying stream). This visitor does not close the `DataEntryWriter`.
  - [P3] An `IOException` from `createOutputStream` or from `close` becomes `RuntimeException` with message `Can't write program class [<internal name>] to [<renamed entry toString>] (<io message>)` and that exception as the cause. Bytes written by `ProgramClassWriter` go through `RuntimeDataOutput`, which turns a write `IOException` into `RuntimeException` of the I/O message only, with no cause and without the "Can't write program class" text, when `close` then succeeds. If `close` also throws, that `IOException` is the one wrapped and the write exception is suppressed. An `IllegalArgumentException` from an unsigned or signed short overflow during the write is likewise unwrapped when `close` succeeds.
  - [P3] A library-class visit is not implemented. It reaches `SimplifiedVisitor.visitAnyClass` and throws `UnsupportedOperationException` (`Method must be overridden in [proguard.io.DataEntryClassWriter] if ever called`).

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DataEntryCopier.java`
- Bazel lines: 393
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard `DataEntryReader` that copies one entry to a `DataEntryWriter`. A directory calls `createDirectory` and does not copy bytes. Any other entry calls `createOutputStream`; a null stream skips the entry with no warning. Otherwise it reads the entry, copies, flushes, closes the input with `closeInputStream`, then closes the output. A later close still runs if copy or the input close fails.
  - [P3] The copy uses one shared 1024-byte buffer and repeats `read` until the result is negative. A zero-length read is written as zero bytes. This method does not flush or close either stream.
  - [P3] An `IOException` is swallowed after `System.err` prints `Warning: can't write resource [<name>] (<message>)`. Any other exception is rethrown as an `IOException` whose message is `Can't write resource [<name>] (<message>)` and whose cause is the original exception.
  - [P3] The `main` test copies `args[0]` to `args[1]` and prints any exception without failing the process. Path checks are case-sensitive `endsWith` for `.apk`, `.ap_`, `.jar`, `.aar`, `.war`, `.ear`, `.jmod`, and `.zip`. `.apk` and `.ap_` are the same input and output kind. `DirectoryWriter` is opened as an archive when any of those suffixes match. The output stack, outer to inner, is apk, jar, aar, war, jmod, zip, then that directory writer. A matching `.zip`, `.jmod`, `.war`, `.jar`, or `.apk`/`.ap_` layer always uses `JarWriter`; the jmod writer prefixes `JM` plus bytes `1, 0`. Otherwise each layer zips only when the parent name matches `.zip`, `.jmod`, `.war`, `.aar`, `.jar`, or `.apk`, case-insensitively. There is no `.ear` writer and no `.ap_` parent match.
  - [P3] The aar layer tests `outputIsWar`, so a `.war` output is wrapped by both the war writer and the aar writer, and a `.aar` output is not forced through the aar writer.
  - [P3] On input, an `.apk` or `.ap_` path is only a `JarReader` over the copier, so nested archives stay intact. Any other path adds filters only for kinds that did not match, outermost zip, then jmod, ear, war, aar, jar, and apk. A matched kind is an unconditional `JarReader` and later kinds are omitted. The jmod reader is `JarReader(reader, true)`; the others are `JarReader(reader)`. A directory therefore unpacks each of those suffixes and feeds unpacked entries to the next inner filter. The writer is closed after `DirectoryPump` finishes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DataEntryDirectoryFilter.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No filter accepts a data entry only when the entry is non-null and a directory (`isDirectory()`).

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DataEntryFilter.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry filter whose only behavior is `accepts(DataEntry) -> boolean`, reporting whether the given entry is accepted.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DataEntryNameFilter.java`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry filter that accepts an entry only when the entry is non-null and a stored string matcher matches `getName()`.
  - [P3] A null entry is rejected without calling the matcher. The current name is used, not the original name. A null name or a null matcher throws before a boolean result.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DataEntryParentFilter.java`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry filter that rejects a null entry and otherwise accepts the entry only when a wrapped filter accepts that entry's parent, including a null parent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DataEntryPump.java`
- Bazel lines: 43
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry pump that applies a reader to every entry a source can provide, such as zip entries or directory files, and propagates I/O errors from that read.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DataEntryReader.java`
- Bazel lines: 38
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard-style reader callback whose implementation consumes one data entry and may fail with an I/O error.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DataEntryRewriter.java`
- Bazel lines: 151
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry copier that decodes a resource with a given charset, splits the text into words of Java identifier parts plus `.` and `-`, looks each word up in a class pool (`.` becomes `/` only when the word contains a dot), and writes the renamed class name (restoring `.` only if the original word contained dots), leaving unknown or unchanged words and all other characters as-is and flushing the encoded output.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DataEntryWriter.java`
- Bazel lines: 75
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry writer that creates a directory for an entry and returns whether that directory was created.
  - [P3] Zel has no check for whether two data entries would share one output stream.
  - [P3] Zel has no output-stream factory that may yield no stream so the entry is skipped, leaving the caller to close a stream that is created.
  - [P3] Zel has no finish step that closes every pending entry, and no debug dump of the writer structure.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DirectoryFilter.java`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry reader that sends a non-null directory entry to one reader and every other entry, including a null entry, to a second reader.
  - [P3] Zel does not skip the entry when the chosen reader is null, which the one-argument constructor does for non-directories by leaving the other reader unset.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DirectoryPump.java`
- Bazel lines: 86
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard data-entry pump that rejects a missing root with `No such file or directory: <path>`, then passes the reader a `FileDataEntry` rooted at that path for the root and every nested file and directory.
  - [P3] The root entry is delivered before its children. Each directory is listed with `File.listFiles()` (unsorted, unfiltered, symlink directories followed) and every child is visited the same way. A plain-file root is one entry and is not an error.
  - [P3] A failure while reading a child is rethrown as `Can't read [<child file name>] (<cause message>)` with the original exception as the cause. A failure on the root itself is not wrapped that way. If `listFiles()` returns null, the child loop throws a null-pointer exception instead of an `IOException`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/DirectoryWriter.java`
- Bazel lines: 118
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard `DataEntryWriter` that writes entries into a base directory, or into a single file when `isFile` is set.
  - [P3] `createDirectory` is absent: `mkdirs` on the resolved path, throw `Can't create directory [<path>]` if creation fails, and return true when the directory already exists.
  - [P3] `sameOutputStream` is absent: it is abstract `File` equality of the two resolved paths.
  - [P3] `createOutputStream` is absent: create missing parents (`Can't create directory [<parent>]` on failure), then truncate the file with `FileOutputStream` wrapped in a `BufferedOutputStream`. The caller closes the stream; `close()` on the writer is a no-op.
  - [P3] Path mapping is absent: `isFile` uses `baseFile` for every entry; otherwise the entry name is resolved under `baseFile` after replacing ProGuard `PACKAGE_SEPARATOR` (`/`) with `File.separatorChar`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/ExtraDataEntryWriter.java`
- Bazel lines: 182
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry writer that, on `createOutputStream`, injects mapped extra entries before delegating the original entry. `createDirectory` and `sameOutputStream` only forward to the main writer, and `close` closes that writer alone.
  - [P3] A single extra name is stored under a null key and is emitted once, on the first output stream, through either the same writer or a separate extra writer. A name-to-names map looks up the entry name after stripping an optional suffix from the end, appends that suffix to each extra name, and writes each extra name at most once.
  - [P3] Each extra entry keeps the triggering entry and replaces only its name. The extra output stream is opened and discarded. After a successful open, extras attached to that extra entry are written the same way. A failed open leaves the name unrecorded, so a later attempt retries it.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/FileDataEntry.java`
- Bazel lines: 131
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No file-backed data entry whose name is the path relative to a base directory with the platform separator replaced by `/`, or just `file.getName()` when the file and base directory are the same path. The rebuilt file is the original file in that equal case, and `base + relative name` otherwise.
  - [P3] Original name is always that name, size is `file.length()`, directory-ness is `file.isDirectory()`, and the parent entry is always null.
  - [P3] The input stream is a cached `BufferedInputStream` over the file, returned again without rewinding. Closing it nulls the cache so the next read reopens the file, and closing before any open dereferences a null stream.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/FilteredDataEntryReader.java`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no reader that tests a data-entry filter and delegates `read` to an accepted reader when the filter accepts the entry, or to a rejected reader when it does not, skipping the call when the selected reader is null.
  - [P3] The two-argument constructor that defaults the rejected reader to null is also absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/FilteredDataEntryWriter.java`
- Bazel lines: 144
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry writer that sends `createDirectory` and `createOutputStream` to an accepted writer or a rejected writer based on a filter. A null chosen writer makes `createDirectory` return false and `createOutputStream` return null. The two-argument constructor leaves the rejected writer null.
  - [P3] `sameOutputStream` returns true when the two entries take different branches. When both are accepted or both are rejected, a null writer also counts as the same stream; otherwise it delegates to that writer.
  - [P3] `close` closes the accepted writer, then the rejected writer, and drops both so a later close does nothing. If closing the accepted writer throws, the rejected writer is left open. The debug dump prints the filter, then each non-null child with the same extra two-space prefix and no accepted or rejected label.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/Finisher.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no listener invoked to finish an output stream immediately before that stream is closed. Bazel’s `Finisher.finish()` is that callback and is declared to throw `IOException`. This ProGuard tree has no implementor or caller.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/IdleRewriter.java`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No data-entry reader that, for each entry, only calls the writer's `createOutputStream` and discards the returned stream.
  - [P3] That stream is never closed, and the reader never reads the entry bytes or writes any content itself.
  - [P3] `IOException` from `createOutputStream` propagates unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/JarReader.java`
- Bazel lines: 94
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry reader that opens one parent entry as a JAR, ZIP, APK, AAR, WAR, or EAR and forwards every entry, including directories, to a nested reader.
  - [P3] The default reader leaves the stream unskipped. The jmod reader first calls `read` for 4 bytes on the cached input stream and ignores the count and the bytes, so a short read, EOF (`-1`), or a non-jmod prefix still continues. The same stream is then wrapped in `ZipInputStream` with charset UTF-8, which decodes entry names as UTF-8 and walks local headers in order until `getNextEntry()` returns null.
  - [P3] Each entry is passed on while sharing that one zip stream. `closeInputStream()` on the parent always runs afterward, including when a later entry throws. The `ZipInputStream` itself is not closed.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/JarWriter.java`
- Bazel lines: 220
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No writer opens one zip/jar on the parent entry's output stream and switches archives when that parent stream changes.
  - [P3] No optional leading archive header, fixed alignment of 1, or null zip comment on that stream.
  - [P3] Directory entries are not written as `name/` with compression off, and file entries are not written compressed under the entry name.
  - [P3] No DOS-packed modification time (current time by default) stamped on those zip entries.
  - [P3] `sameOutputStream` is not implemented as equal entry names plus the delegate agreeing the parents share a stream, and close does not finish the open zip before closing the delegate.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/ManifestRewriter.java`
- Bazel lines: 212
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never copies a manifest entry through a charset, joins continuation lines, and rewrites class-name words from a rename pool.
  - [P3] CR, LF, and CR+LF are not normalized to LF. A newline followed by one space is not treated as a continuation and dropped. A repeated CR or LF is not left as a second line break.
  - [P3] Output lines are not wrapped at 70 Java characters by inserting a newline and a space, and a newline does not reset that count. The Java code counts characters, not UTF-8 bytes.
  - [P3] Tokens made of Java identifier characters plus `.` and `-` are not looked up with `.` mapped to `/`. A renamed class is not written back, with `/` restored to `.` only when the original token contained a dot.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/NameFilter.java`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry reader that sends an entry to an accepted reader or a rejected reader based on its name.
  - [P3] A filter string, or a list of filters, is parsed as a comma-separated ProGuard file-name pattern (`?` one non-separator character, `*` any run of non-separators, `**` any characters including `/`, `!` negation). An empty list matches every name. A null entry does not match.
  - [P3] A null accepted or rejected reader drops the entry instead of reading it. The two-argument constructors always pass a null rejected reader, so non-matching names are skipped.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/NameFilteredDataEntryWriter.java`
- Bazel lines: 107
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry writer that turns a ProGuard file-name expression, a list of expressions, or a string matcher into a name filter and routes `createDirectory`, `createOutputStream`, `sameOutputStream`, and `close` to an accepted writer or a rejected writer (a null rejected writer drops non-matching entries).

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/ParentDataEntryWriter.java`
- Bazel lines: 82
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel wraps another `DataEntryWriter` and forwards `createDirectory`, `sameOutputStream`, and `createOutputStream` with each entry’s parent (`getParent()`, including null) instead of the entry itself. Zel has no such parent-redirecting writer.
  - [P3] `close()` closes the delegate and then clears it, so a later call throws. Zel has no equivalent close-and-drop.
  - [P3] `println` prints `ParentDataEntryWriter` and then the delegate indented by two spaces. Zel has no equivalent structure dump.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/PrefixAddingDataEntryWriter.java`
- Bazel lines: 95
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard `DataEntryWriter` that stores a name prefix and, for `createDirectory`, `sameOutputStream`, and `createOutputStream`, delegates a `RenamedDataEntry` whose name is that prefix concatenated with `dataEntry.getName()`.
  - [P3] `close` forwards to the wrapped writer with no rename. `println` is shadowed by its indent argument, so the dumped prefix is that indent, not the stored name prefix.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/PrefixStrippingDataEntryReader.java`
- Bazel lines: 65
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry reader that, on each `read`, checks `name.startsWith(prefix)` and, only when that matches, forwards a renamed entry whose name is `name.substring(prefix.length())`.
  - [P3] Non-matching names are passed through unchanged, the delegate is always invoked, and only the name changes (parent entry content is unchanged). An empty prefix matches every name; a name equal to the prefix becomes empty; the match is a raw string prefix, so `classes` also strips `classesFoo`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/RenamedDataEntry.java`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel type wraps a data entry and returns a caller-supplied name from `getName` while leaving the original name, size, directory flag, parent, input stream, and close behavior on the wrapped entry.
  - [P3] No `toString` of the form `name == <wrapped entry>`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/RenamedDataEntryReader.java`
- Bazel lines: 104
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry reader that looks up the entry name in an old-to-new name map and forwards a renamed wrapper to another reader.
  - [P3] A non-empty directory name gets `/` appended before the lookup, and a successful directory rename drops the last character of the mapped name before the wrapper is read.
  - [P3] An unmapped name is passed through unchanged to an optional missing reader, or dropped when that reader is absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/RenamedDataEntryWriter.java`
- Bazel lines: 175
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no writer that renames a data entry from a class pool and a package-prefix map before delegating `createDirectory` and `createOutputStream`.
  - [P3] The class-name scan is absent: walk backward to each non-letter/digit, stop at `/`, look up the prefix, and on a rename keep the original separator plus suffix (`newName + name[suffixIndex..]`). An unrenamed class hit returns the original entry and ends the scan.
  - [P3] The package-prefix scan is absent: repeatedly take `ClassUtil.internalPackagePrefix` (including the empty prefix), and on a different mapped prefix replace only that prefix. An equal mapped prefix returns the original entry and stops.
  - [P3] `sameOutputStream` forwards the two original entries with no renaming. `close` and `println` only delegate. None of that wrapper behavior exists in Zel.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/RenamedParentDataEntryWriter.java`
- Bazel lines: 116
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no data-entry writer that, before `createDirectory`, `sameOutputStream`, and `createOutputStream`, replaces the parent when the parent name matches a string matcher.
  - [P3] A matching parent is not rewritten to `newParentName` while the child entry itself stays wrapped and otherwise unchanged; a null entry, a null parent, or a non-matching parent is not passed through unchanged.
  - [P3] `close` and `println` are not delegated without applying that parent rename.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/WrappedDataEntry.java`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard `DataEntry` decorator that stores a wrapped entry and forwards `closeInputStream`, `getName`, `getOriginalName`, `getSize`, `isDirectory`, `getInputStream`, and `getParent` unchanged, with `toString` returning `getName()` so a subclass override of the name is what string conversion shows.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/ZipDataEntry.java`
- Bazel lines: 117
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard `DataEntry` over a `ZipEntry`, its parent entry, and the shared `ZipInputStream`.
  - [P3] `getName` rewrites `File.separatorChar` to `/` and drops one trailing `/`. On Unix the separator rewrite is a no-op. `getOriginalName` returns that same string, so the raw zip name is not kept.
  - [P3] `isDirectory` follows the raw `ZipEntry` name (trailing `/`), which can disagree with the stripped `getName`. `getSize` is the zip size, including `-1` when unknown.
  - [P3] `getInputStream` caches one `BufferedInputStream` around the shared zip stream. `closeInputStream` calls `closeEntry()` and then drops both stream fields. It does not close the zip stream itself.
  - [P3] `toString` is `parent.toString() + ':' + getName()`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/ZipFileDataEntry.java`
- Bazel lines: 123
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No `DataEntry` over a random-access `ZipFile` entry whose name replaces the platform separator with `.` and drops one trailing separator, with the original name equal to that rewritten name.
  - [P3] Size and directory status are not taken from the zip entry (`getSize()` may be -1; a directory is a name ending in `/`), and there is no parent entry or `parent:name` string form.
  - [P3] No cached buffered stream from `ZipFile.getInputStream`, and no close that closes that stream, drops the zip file and buffer, and leaves the raw stream set so a later open wraps the already-closed stream.

### `third_party/java/proguard/proguard6.2.2/src/proguard/io/ZipOutput.java`
- Bazel lines: 577
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No ZIP writer buffers each entry until that entry stream is closed, then writes a local header (version 10, general-purpose flag 0, method 0 or 8, DOS time, CRC-32, 32-bit sizes, UTF-8 name, optional extra field) and the payload. Compressed entries are not raw deflate at best compression.
  - [P3] No central directory in entry-open order, and no end-of-central-directory record (disk numbers 0, entry counts as 16-bit, 32-bit directory size and offset, optional UTF-8 archive comment) followed by closing the underlying stream.
  - [P3] Duplicate names are not rejected. Optional preamble bytes, per-entry comments, and several entries open at once are absent. The Java `uncompressedAlignment` argument is stored and never applied; zel has no such parameter.
  - [P3] The only ZIP byte emitter is the test helper `build_zip` in `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs`. It writes version-20 headers immediately, uses a stored-block stand-in for method 8, sets external attributes, and omits extra fields, comments, a preamble, and duplicate-name checks.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/AttributeShrinker.java`
- Bazel lines: 120
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never drops unused attributes on program classes, their fields and methods, or `Code` attributes. Bazel keeps only attributes marked used by `AttributeUsageMarker`, compacts each array, nulls the vacated tail, and rewrites the attribute count.
  - [P3] Library classes are left unchanged by this visitor. Zel has no matching pass that skips them.
  - [P3] After a class or member shrink, remaining attributes are visited, and only nested `Code` attributes are shrunk the same way. Other attribute kinds are not compacted. That selective nested pass is absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/AttributeUsageMarker.java`
- Bazel lines: 71
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that marks every visited attribute as used by storing one shared sentinel on its visitor info.
  - [P3] Zel has no identity check that reports an attribute used only when its visitor info is that same sentinel.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/ClassObfuscator.java`
- Bazel lines: 569
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never assigns a stored obfuscated name to a program class that does not already have one, including the outer-class prefix (`Outer$`) taken from InnerClasses or EnclosingMethod, or a computed package prefix when there is no outer class.
  - [P3] Zel does not treat an all-digit suffix after the outer class name as a numeric inner class, and it does not otherwise draw unique class names from a per-package dictionary wrapped around a simple name factory.
  - [P3] Zel does not reserve names already taken by program classes that have a new name and by library classes (new name, or the original name if none), compared case-sensitively or lowercased when mixed-case names are disabled. It also does not record a newly generated name as taken only when a class-name dictionary is set and mixed-case names are off.
  - [P3] Zel does not map an already-named class’s package onto its new package, recursively through parent packages unless flattening or repackaging is set, and it does not skip that mapping when repackaging with access modification allowed.
  - [P3] Zel does not choose a package prefix by keeping a filtered package (match against the prefix with the trailing separator removed), forcing the repackage base, flattening under a fixed base, or inventing a unique child package under the parent prefix, and it does not append a package separator only to a non-empty flatten or repackage base.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/ClassRenamer.java`
- Bazel lines: 109
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that writes previously chosen obfuscated names onto a program class and its members. A program class updates only its this-class `Class` constant: when `ClassObfuscator.newClassName` is non-null (`visitorInfo` is a `String`; any other value, including null, skips the update), `u2nameIndex` becomes `ConstantPoolEditor.addUtf8Constant(newName)` even if that string already equals the current name. The editor is built with no class pools, so new constants are not reference-initialized. It scans pool indexes `1..count-1`, skips nulls, and returns the first `Utf8` whose string equals the new name; otherwise it appends a `Utf8` and, when `length < count+2`, grows the pool to `count+2`. A this-class entry that is not a `Class` constant throws `UnsupportedOperationException` (`Method must be overridden in [proguard.obfuscate.ClassRenamer] if ever called`). Super, interfaces, descriptors, attributes, and other references are left unchanged.
  - [P3] Program fields, then program methods, are visited in array order. The current name is read first (`Utf8` at `u2nameIndex`; a non-`Utf8` throws `IllegalStateException` `Expected Utf8Constant at index […] in class […]` with the `ClassCastException` as the cause). The new name is `MemberObfuscator.newMemberName`: follow `visitorInfo` while it is a `VisitorAccepter` (method-link chain, including a fixed-name wrapper) and cast the last info to `String`. A null result leaves the member unchanged. Any other non-`VisitorAccepter` throws `ClassCastException`. The name index is replaced only when the new name is non-null and `!newName.equals(current)`; an equal string keeps the existing index even if an earlier duplicate `Utf8` exists.
  - [P3] A library class always assigns `thisClassName = ClassObfuscator.newClassName(libraryClass)`, so a missing `String` visitor info clears the name to null. It then renames library fields and methods. A non-null new member name is stored on `libraryMember.name` even when it equals the current name; a null new name leaves the old name. Library classes have no constant pool, and null member slots are skipped by the library class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/DictionaryNameFactory.java`
- Bazel lines: 286
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no obfuscation name factory that reads a UTF-8 dictionary from a URL, file, or reader, closes the reader, and keeps the first occurrence of each name.
  - [P3] Zel does not skip `#` through the end of the line, or split names on Java identifier boundaries when that mode is on and on CR, LF, and `#` when it is off.
  - [P3] Zel does not walk that dictionary in order, then fall back to another name factory while rejecting names already in the dictionary, share one dictionary across factories with separate indexes, or reset both the index and the fallback.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/MapCleaner.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On every class visit, Bazel clears the map passed to the visitor and ignores the class itself. Zel has no class visitor that does this.
  - [P3] Bazel’s member obfuscator relies on that clear so collected names do not carry from one class to the next. Zel has no equivalent per-class map reset.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/MappingKeeper.java`
- Bazel lines: 180
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no processor that applies received obfuscation mappings onto a class pool so a later obfuscation step keeps those names.
  - [P3] No class-mapping step converts the external class name to an internal name, looks the class up, warns when an existing new class name differs from the mapped name, sets the new class name, and returns true so members are processed. An unknown class returns false.
  - [P3] No field-mapping step runs only when the class was found and the class name is unchanged, resolves the field by name and internal type descriptor, warns on a conflicting fixed name, and pins the mapped field name.
  - [P3] No method-mapping step does the same using a descriptor built from the return type and comma-separated arguments, and formats the conflict warning with the original return type, name, and argument list. Incoming line-number ranges are unused.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/MappingPrinter.java`
- Bazel lines: 365
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not print ProGuard program-class mappings as `externalOld -> externalNew:` and then walk fields and methods.
  - [P3] Zel does not print field lines ` externalType name -> obfuscatedName`, using the original name when no obfuscated member name is set.
  - [P3] Zel does not print method lines, either as a bare signature or as `low:high:return name(args) -> obfuscatedName` from a line-number table whose source is null (range only when the lowest line is greater than 0).
  - [P3] Zel does not track inlined and enclosing blocks from line-number source identity, parse `class.method(descriptor):start:end`, print the shifted inlined range (plus a class prefix when the inlined class differs), or correct enclosing line numbers with `LineNumberLinearizer.SHIFT_ROUNDING`, including the empty-stack guards when exiting a block.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/MappingProcessor.java`
- Bazel lines: 86
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No callback that takes an original class name and its obfuscated name and returns whether mappings of that class's members should be delivered.
  - [P3] No field-mapping callback carrying the original class name, external field type, original field name, new class name, and new field name.
  - [P3] No method-mapping callback carrying the original class name, original and new line ranges (0 when unknown), external return type, original method name, external arguments, new class name, and new method name.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/MappingReader.java`
- Bazel lines: 240
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard obfuscation-mapping parser. Bazel reads the file as UTF-8, skips `#` comments, and reports class lines (`old -> new:`) plus field and method lines (optional line numbers, argument lists, and an original class embedded in the member name) to a processor. A class line that does not parse, or a processor that declines the class, drops following member lines until the next class. Unparsable member lines and empty names are skipped. I/O errors are rethrown as `Can't process mapping file (…)`; close failures are ignored; bad line-number integers propagate unwrapped.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/MemberNameCleaner.java`
- Bazel lines: 60
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No member visitor clears a stored obfuscated name. On each program field, program method, library field, and library method, ProGuard writes `null` over that member's visitor info, replacing either a plain new name or a fixed-name wrapper.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/MemberNameCollector.java`
- Bazel lines: 105
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never collects already-assigned member obfuscation names into a descriptor-keyed map of new name to original name.
  - [P3] It does not skip `<init>` and `<clinit>`, and it does not ignore members whose new name is still unset.
  - [P3] With aggressive overloading off, it does not cut each descriptor at the first `)` (methods drop the return type; a descriptor with no `)` becomes the empty key, so every field shares one map).
  - [P3] It does not resolve two originals for the same new name by keeping a fixed new name, otherwise the strictly smaller original (`String.compareTo`), leaving an equal original in place.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/MemberNameConflictFixer.java`
- Bazel lines: 158
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel code resolves obfuscated member-name collisions. Program fields and program methods other than `<init>` and `<clinit>` never look up a descriptor-keyed new-name to old-name map, warn when a fixed new name already belongs to a different old name, clear that name, and ask a member obfuscator for another one.
  - [P3] When aggressive overloading is off, method descriptors are not reduced to the argument list through `)`, and field descriptors are not collapsed to one shared key, so same-name members of different types are not forced into a single namespace. Library fields and methods are not explicitly left unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/MemberNameFilter.java`
- Bazel lines: 120
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No visitor forwards program or library field and method visits only when that member already has an obfuscated name.
  - [P3] No special case treats a constructor (`<init>`) as renamed when the member itself has no new name but its enclosing class does.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/MemberObfuscator.java`
- Bazel lines: 229
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never assigns obfuscated names to linked class fields and methods: it does not skip `<init>` and `<clinit>`, trim the descriptor through `)` unless aggressive overloading is allowed, or take the next unused name from a per-descriptor new-name to old-name map.
  - [P3] Zel has no fixed-name marker or visitor-info accessors that record a locked name (library member or explicit mapping) separately from a generated name.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/MemberSpecialNameFilter.java`
- Bazel lines: 101
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that forwards a program field, program method, library field, or library method visit only when that member’s stored new name is special: the visitor-info string on the linked member, which must be non-null and end in `_`. An empty string throws on the last-character read, and a null name is skipped.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/MultiMappingProcessor.java`
- Bazel lines: 105
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no composite mapping processor that forwards class, field, and method mappings to every processor in a list, OR-ing each `processClassMapping` boolean (false when the list is empty) and ignoring return values for field and method mappings.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/NameFactory.java`
- Bazel lines: 34
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no name-factory contract that yields a unique sequence of valid Java identifiers, with `reset` restarting the sequence and `nextName` returning the next name.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/NameFactoryResetter.java`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Visiting a program class resets the obfuscation name factory so the next generated name starts over.
  - [P3] Visiting a library class resets that same name factory.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/NameMarker.java`
- Bazel lines: 164
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no obfuscation visitor that pins a visited class’s new name to `getName()`. A program class is pinned and then its attributes are walked; a library class is pinned and its attributes are not.
  - [P3] Program and library fields are not given a fixed new member name equal to `field.getName(clazz)`. Program and library methods are not given that fixed name either, except `<init>` and `<clinit>`, which this visitor skips.
  - [P3] On an `InnerClasses` attribute, an entry is not followed only when both the inner and outer class indexes are non-zero and the inner class name equals the class being visited. That path does not then visit the outer class constant’s referenced class, which pins that outer name and, for a program outer class, repeats the attribute walk.
  - [P3] Other attributes are ignored. No zel code marks class or member names so a later obfuscation step leaves them unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/NumericNameFactory.java`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel name factory starts a counter at 0, resets it to 0, and returns the next decimal name as `"1"`, `"2"`, and so on via a pre-incremented `int` (`Integer.toString`).

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/Obfuscator.java`
- Bazel lines: 533
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-pool obfuscator. It does not require `-keep`, `-applymapping`, or `-printmapping` before renaming, and it does not clean visitor info or link non-private non-static methods (or all members when unique class-member names are requested).
  - [P3] Zel does not mark keep seeds, retain library class and member names, or keep abstract method names of functional interfaces reached from bootstrap arguments or dynamic returns (Java 7+).
  - [P3] Zel does not mark required or kept attributes, keep parameter names, or shrink attributes only after seeds are marked.
  - [P3] Zel does not apply a mapping file over program and library classes, warn when kept names are remapped, or fail unless warnings are ignored.
  - [P3] Zel does not assign obfuscated class or package names (dictionaries, mixed case, keep/flatten/repackage packages, access modification) or member names (global uniqueness versus hierarchical private/non-private collection, including abstract-class and default-interface clashes), and it does not replace conflicting names with special names or report those conflicts.
  - [P3] Zel does not print an alphabetical mapping, set renamed flags for configuration debugging, rename program and library classes, fix class and member references, widen access and inner-class flags when repackaging with access modification, fix bridge methods, rename SourceFile attributes, or shrink constant pools.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/ParameterNameMarker.java`
- Bazel lines: 127
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that, for an unused `LocalVariableTable` or `LocalVariableTypeTable` on a method whose descriptor has parameters (`charAt(1) != ')'`), compacts the table in place to entries with start PC 0, updates the length, and marks that attribute used only when at least one entry remains.
  - [P3] Already-used tables, methods with an empty parameter list, and every other attribute kind are left unchanged. That filtering is also absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/PrefixingNameFactory.java`
- Bazel lines: 60
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no name factory that prepends a fixed prefix to every name from a wrapped factory (`prefix + delegate.nextName()`).
  - [P3] Zel has no matching `reset()` that only resets that wrapped factory and leaves the prefix unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/RenamedFlagSetter.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On each program class, if `ClassObfuscator.newClassName` is non-null and differs from the original class name, `ACC_RENAMED` (`0x00010000`) is OR-ed into the class access flags. Zel has no class-file visitor that records a renamed access flag.
  - [P3] Every field and method is then visited, and the same flag is OR-ed into the member access flags when `MemberObfuscator.newMemberName` is non-null and differs from the original member name. Zel has no equivalent member walk.
  - [P3] A null new name or a name equal to the original leaves the flag unset, and library classes are not visited. Zel does not implement that obfuscation-flag pass at all.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/SimpleNameFactory.java`
- Bazel lines: 156
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No short unique-name generator: mixed-case uses 52 characters (`a`–`z`, then `A`–`Z`) and lower-case uses 26 (`a`–`z`); `nextName` returns the name for the current index and increments it; `reset` sets the index back to 0; names longer than one character are the name at `index / radix - 1` plus the character at `index % radix`, stored in a static cache shared by every factory of the same case mode.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/SourceFileRenamer.java`
- Bazel lines: 84
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Visiting a program class rewrites only that class's attributes.
  - [P3] A SourceFile attribute's source-file constant-pool index is set to a found or newly added UTF-8 constant for the configured replacement string.
  - [P3] A SourceDir attribute's source-dir constant-pool index is set to a found or newly added UTF-8 constant for that same replacement string.
  - [P3] Other attribute kinds are left unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/SpecialNameFactory.java`
- Bazel lines: 83
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No wrapper name factory whose `nextName` returns the delegate's next name plus a trailing `_`.
  - [P3] No `reset` that forwards to that delegate.
  - [P3] No special-name check that is true only when the name is non-null and its last character is `_` (an empty name indexes `length - 1` and throws).

### `third_party/java/proguard/proguard6.2.2/src/proguard/obfuscate/UniqueMemberNameFactory.java`
- Bazel lines: 92
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no name factory that repeatedly takes the next candidate from a delegate until that name is absent from both the class’s own fields and its own methods (descriptor ignored; either hit rejects the name).
  - [P3] Zel has no injected-member factory that builds those candidates as `$$` plus a simple name generator.
  - [P3] Zel has no reset that only resets that delegate generator.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/BootstrapMethodArgumentShrinker.java`
- Bazel lines: 104
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Visiting a bootstrap method resolves its method-handle constant to the referenced member and then drops unused constant arguments from that entry.
  - [P3] A library method, or any constant that is not a program method, leaves the usage mask at all bits set, so every argument is kept. A program method replaces the mask with the parameters marked used.
  - [P3] Arguments stay in order. An argument is kept when its index is 64 or higher, or when its bit in the usage mask is set. Kept arguments are packed to the front of the same array and the argument count is set to the new length. Slots past that count are left unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/ChangedCodePrinter.java`
- Bazel lines: 381
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute-visitor wrapper that forwards every non-code attribute visit and, for a code attribute, snapshots the code bytes, runs the wrapped visitor, then checks whether the code changed.
  - [P3] Change detection treats `oldCode.length` (the pre-visit `code.length`, not `u4codeLength`) differing from the post-visit `u4codeLength` as a change. Otherwise it compares each byte in `0..u4codeLength`.
  - [P3] On a change it writes to stdout a `Class ` line with the external class name, a `Method ` line from the external full method description with access flags forced to 0, then one line per index of the new code: ` -- ` when the bytes match and ` => ` when they differ, the decimal index, and two lowercase hex bytes (old, then new) from `(0x100 | byte & 0xff)`.
  - [P3] That dump loops to the new `u4codeLength`, so a longer replacement array indexes past the old snapshot, and a shorter one omits the dropped tail. Identical code bytes produce no output even if exception tables, line numbers, or stack maps changed. Non-code attributes are never compared or printed.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/ConstantMemberFilter.java`
- Bazel lines: 77
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A program field is forwarded only when its stored field value is non-null and a single known constant; any other field visit is dropped.
  - [P3] A program method is forwarded only when its stored return value is non-null and a single known constant; any other method visit is dropped.
  - [P3] Library fields and library methods are never forwarded.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/ConstantParameterFilter.java`
- Bazel lines: 78
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no program-method filter that delegates a visit for each parameter whose stored optimization value is non-null and particular.
  - [P3] The scan starts at parameter index 0 for static methods and 1 for instance methods (`this`), and stops at `internalMethodParameterCount` (descriptor parameters, plus one for `this` when not static; `long` and `double` still count as one index).
  - [P3] Each matching parameter causes another visit of the same method, so a method with several constant parameters is visited once per parameter. Visits of anything other than a program method are ignored.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/DuplicateInitializerFixer.java`
- Bazel lines: 241
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no pass that, for an `<init>` that is not the first method with its descriptor, rewrites that method — or the other initializer when the visited one is kept — to a unique descriptor by inserting a dummy last argument cycling through `B`, `C`, `S`, `I`, `Z`, then the same types with extra array dimensions.
  - [P3] Zel does not raise the code attribute’s `maxLocals` to the new parameter size, splice the new type character into the Signature attribute just before `)`, or grow parameter-annotation arrays by one empty slot.
  - [P3] Zel does not update method optimization info by inserting that last parameter, increasing the parameter size by one, and marking the new slot used, and it has no visitor callback for initializers that were fixed.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/DuplicateInitializerInvocationFixer.java`
- Bazel lines: 165
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no pass that, on `invokespecial` of `<init>`, compares the resolved program constructor’s descriptor length with the constant-pool descriptor and inserts `iconst_0` when the length delta is 1 or `aconst_null` when it is greater than 1 immediately before that call. Library constructors, non-`<init>` refs, and other opcodes are left unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/KeepMarker.java`
- Bazel lines: 134
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Visiting a program or library class replaces that class's visitor info with a fresh `ClassOptimizationInfo`, whose `isKept()` is always true, discarding any earlier class optimization facts.
  - [P3] Visiting a program or library field replaces that field's visitor info with a fresh `FieldOptimizationInfo`, whose `isKept()` is always true.
  - [P3] Visiting a program or library method attaches a fresh `MethodOptimizationInfo` (`isKept()` always true) to `MethodLinker.lastMember` of that method, the linked representative, rather than necessarily the method that was visited.
  - [P3] Visiting a code attribute replaces its visitor info with a `CodeAttributeOptimizationInfo` (`isKept()` always true). Any other attribute is ignored.
  - [P3] `isKept` for a class, field, method, or code attribute is true only when that optimization info is present. A missing info object is not kept, and method lookup uses the linked representative.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/KeptClassFilter.java`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A program-class or library-class visit is delegated to the accepted visitor when `KeepMarker.isKept` is true (class optimization info exists and is marked kept), and to the rejected visitor otherwise.
  - [P3] A null selected visitor skips the visit. The single-visitor constructor leaves the rejected visitor null, so unkept classes are ignored.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/KeptMemberFilter.java`
- Bazel lines: 86
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that forwards program and library field and method visits only when the member is marked kept, and otherwise skips the visit.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/MemberDescriptorSpecializer.java`
- Bazel lines: 139
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Visiting a program field, when the stored field value is a reference, replaces `referencedClass` with that value's referenced class if they differ, then notifies an optional extra member visitor.
  - [P3] Visiting a program method walks parameters after `this` on instance methods (the count includes `this` and counts each descriptor parameter once). For each reference parameter it replaces `referencedClasses[classIndex]` when it differs from the stored referenced class and notifies that extra visitor once per change. `classIndex` advances only for reference parameters, so primitives and the return type stay unchanged.
  - [P3] Only the referenced-class link is updated. The descriptor string is left as-is.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/MethodDescriptorShrinker.java`
- Bazel lines: 393
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not remove unused parameters from program-method descriptors, using `ParameterUsageMarker` slot indices (non-static methods shifted by one).
  - [P3] Zel does not shrink `Signature` attributes, skipping leading synthetic parameters of enum and inner-class constructors, or compact the attribute’s referenced classes.
  - [P3] Zel does not drop unused parameter-annotation entries or clear the trailing annotation slots after shrinking.
  - [P3] Zel does not rename non-`<init>` methods by appending a separator and the absolute hex hash of the original descriptor, then rewrite the descriptor and its referenced-class array (empty array becomes null).
  - [P3] Zel does not run an optional extra member visitor only after a descriptor actually changes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/MethodStaticizer.java`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A program method whose parameter 0 is unused is updated in place: `ACC_FINAL` is cleared and `ACC_STATIC` is set, and every other access flag is left as it was.
  - [P3] That flag update still runs when the method is already static, so an unused first parameter clears `ACC_FINAL`.
  - [P3] Library methods and every other member visit are left unchanged.
  - [P3] After the flag change, an optional extra member visitor is called on that program method. Descriptors, invocations, and the unused parameter slot are not rewritten here.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/OptimizationInfoClassFilter.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A class visit is forwarded only when `ClassOptimizationInfo.getClassOptimizationInfo` is a `ProgramClassOptimizationInfo` (editable optimization info). `visitProgramClass` and `visitLibraryClass` both use that check and, on a match, call the same method on the wrapped visitor with the same class. Null visitor info, a base `ClassOptimizationInfo`, or any other type drops the visit. Zel has no class-visitor filter with this gate.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/OptimizationInfoMemberFilter.java`
- Bazel lines: 102
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A program field is forwarded to the primary member visitor only when its optimization info is a `ProgramFieldOptimizationInfo`; otherwise it is forwarded to the alternate visitor.
  - [P3] A program method is forwarded to the primary member visitor only when its optimization info is a `ProgramMethodOptimizationInfo`; otherwise it is forwarded to the alternate visitor.
  - [P3] A null selected visitor skips the visit. The one-argument constructor leaves the alternate visitor null, so members without editable optimization info are dropped.
  - [P3] Library field and library method visits are ignored.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/Optimizer.java`
- Bazel lines: 1738
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The optimization filter is `configuration.optimizations` (all enabled when unset). Field, parameter, or return-value propagation forces advanced simplification. Write-only field removal, static marking, or parameter removal forces advanced removal. Branch simplification forces simple removal. Advanced or simple removal forces exception removal. Any variable, arithmetic, cast, field, branch, object, string, or math simplification enables peephole optimization.
  - [P3] The public optimization name list omits `class/unboxing/enum`, `class/merging/wrapper`, and `code/simplification/object`, although those filters still run. `library/gson` is named and parsed but does not change the pass.
  - [P3] `execute` throws `IOException` ("You have to specify '-keep' options for the optimization step.") when `keep`, `applyMapping`, and `printMapping` are all unset.
  - [P3] Visitor info is cleared and methods that share optimization info are linked on both pools. Keep seeds are marked with `KeepClassSpecificationVisitorFactory(false, true, false)`.
  - [P3] Every library class and member is kept. Non-enum classes used by `.class` instructions are kept. Classes and members referenced by string constants are kept. For class version 1.7 and above, bootstrap method handles, bootstrap arguments (functional interfaces, their abstract methods, and lambda methods), and functional interfaces returned by invokedynamic are kept.
  - [P3] Program class and member optimization info is attached. `assumeNoSideEffects`, `assumeNoExternalSideEffects`, `assumeNoEscapingParameters`, and `assumeNoExternalReturnValues` are marked on both pools when set.
  - [P3] Classes, and methods of non-interface classes, are finalized when those optimizations are enabled.
  - [P3] With write-only field removal, fields are marked read or written from instructions. Otherwise every field is marked read and written. Side effects and parameter escape are seeded from method headers, then updated by repeated parallel partial evaluation until the marks converge. Read and write marking inside that loop is disabled.
  - [P3] Synchronized-method marking marks superclasses of escaping classes and classes that escape to the heap, then clears `ACC_SYNCHRONIZED` on non-static methods of non-escaping classes. That count is printed in verbose mode and is not part of the return value.
  - [P3] Simple-enum unboxing runs only when final enum candidates exist. Uses as objects and kept-member descriptors unmark them. Remaining enums have their uses, static initializers, and descriptors simplified, then member references are fixed with the Android flag.
  - [P3] Used parameters are marked in parallel. `this` is treated as used when static marking is off, and every parameter is treated as used when parameter removal is off. Methods referenced from kept code have every parameter, including `this`, marked so their descriptors are not shrunk.
  - [P3] Value propagation or wrapper merging evaluates synthetic classes with detailed array values and other classes in parallel, storing field, parameter, and return values. `assumeValues` is applied to both pools. Constant fields, parameters, and returns are counted. Unboxed enums propagate array constants, and advanced simplification reloads stored values into synthetic classes.
  - [P3] Advanced simplification rewrites instructions from those constants. Advanced removal deletes unused code, drops unused invocation arguments, and can make methods static. Usage is recomputed only when advanced simplification did not already run.
  - [P3] Parameter removal shrinks descriptors. Static marking makes methods that do not use `this` static. Descriptor changes fix member references, using the Android flag, and shrink bootstrap arguments. Descriptor changes, private-method marking, or any staticized method also shrink locals and optimization info. Otherwise advanced removal only updates stack sizes. Removed parameters can retarget duplicate initializers and their calls.
  - [P3] Package-visible members and invocations, wrapper classes, catch types, super and dynamic calls, backward branches, access and synchronized-block methods, final field assignments, non-empty-stack returns, instantiations, `instanceof`, `.class`, and method invocations are always marked.
  - [P3] Wrapper merging uses the access-modification flag and then simplifies wrapper uses. Vertical merging uses access modification and aggressive interface merging. Horizontal merging uses the same flags and skips names in `injectedClassNameMap` values. Any merge removes retargeted inner-class attributes, retargets class and member references, fixes access when modification is allowed, fixes inner-class access only on classes that are not kept, and may retarget duplicate initializers.
  - [P3] Methods invoked once are inlined before short methods, then tail recursion is simplified. Each inliner sees `microEdition`, `android`, and `allowAccessModification`. Non-private fields and methods of non-interface classes are then made private. Inlining plus access modification fixes access flags. Parameter removal, any merge, privatized methods, or that inlining case rewrites invocations.
  - [P3] Code merging shares common branch targets. Enabled peephole groups run in parallel. Branch simplification also folds goto-goto and goto-return. Math simplification adds Android math sequences only when `configuration.android` is set.
  - [P3] Unnecessary exception handlers, unreachable code, unused locals, and local-variable frames are optimized only when their flags, including forced ones, are on. The constant pool is always shrunk.
  - [P3] If advanced simplification changes nothing, constant field, parameter, and return counts are cleared so they do not schedule another pass. Verbose mode prints every counter with `" (disabled)"` when its flag is off, and prints the Android math line only for Android. The method returns true when any reported count is positive except desynchronized methods. Write-only field removal can count a field that is not removed. Parameter-removal counts subtract initializer fixes, and the bytecode parameter count also subtracts staticized methods. The removed-instruction count is deletions minus insertions.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/ParameterShrinker.java`
- Bazel lines: 150
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On a method code attribute, local slots are remapped only when the saved parameter size is larger than the size of the current descriptor, counting `long` and `double` as two slots and a leading `this` for instance methods. Non-code attributes are left unchanged.
  - [P3] Used parameter slots are packed to the front in their original order. Each unused slot is moved to just after that new parameter block and kept as a local, so later uses still resolve. Slots at or above the old parameter size keep their indices.
  - [P3] An optional extra member visitor, if set, is invoked once per unused parameter slot on that same method.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/TailRecursionSimplifier.java`
- Bazel lines: 360
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Only private, static, or final methods are rewritten, and synchronized, native, and abstract methods are left unchanged.
  - [P3] A same-method `invokevirtual`, `invokespecial`, or `invokeinterface` is not the target. Only `invokevirtual`, `invokespecial`, and `invokestatic` whose resolved member is the method being visited are candidates. `invokeinterface` and `invokedynamic` are copied through.
  - [P3] The call is rewritten only when the next instruction is `ireturn`, `lreturn`, `freturn`, `dreturn`, `areturn`, or `return`, the call offset is outside every exception range, and the stack is empty after that return.
  - [P3] The matching invoke is replaced by stores of the stacked arguments into the method’s parameter locals (`this` in slot 0 for instance methods; category-2 `long`/`double` slots skipped), then a `goto` to offset 0. The following return instruction is kept.
  - [P3] The method body and exception table are written back only after at least one such rewrite. Otherwise the code is left as it was. Copied exception ranges keep their catch types.
  - [P3] When an extra visitor is supplied, it is notified with the original invoke for each simplified tail call.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/WriteOnlyFieldFilter.java`
- Bazel lines: 65
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that forwards a program field only when it is marked written and not read.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/EvaluationShrinker.java`
- Bazel lines: 1678
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no partial-evaluation bytecode shrinker. ProGuard config allowlisting and Android ProGuard flags do not rewrite method bodies.
  - [P3] After an optional instruction-usage pass, necessary `invokevirtual`, `invokespecial`, and `invokeinterface` calls become `invokestatic` with the same constant index when the target program method is non-static and parameter 0 is unused.
  - [P3] Traced but unnecessary branches whose targets are all at or before the branch, and that are not straddled by another traced unnecessary branch, are replaced by `goto 0`. The search for the previous necessary instruction always rechecks the branch offset, so when the branch itself is unnecessary the straddling scan starts at offset 0.
  - [P3] A necessary `jsr` whose following instruction is unnecessary is followed by replacing that next instruction with `goto 0`.
  - [P3] Necessary instructions are kept stack-consistent: unwanted present inputs are popped (an unused exception that is not the top slot is popped at its handler instead), missing inputs are pushed as `iconst_0`, `lconst_0`, `fconst_0`, `dconst_0`, or `aconst_null` from the top value's computational type, and unused outputs are popped. A push wider than that top value's category throws.
  - [P3] Instructions that are only needed as extra push or pop sites are replaced by the pops of still-present inputs and the pushes of still-needed outputs.
  - [P3] Necessary `dup` through `swap` opcodes are rewritten from which slots are present before and necessary after, including multi-opcode sequences. A pure `nop` deletes the instruction. An unchanged opcode undeletes it. Some patterns throw `UnsupportedOperationException`.
  - [P3] A necessary branch or switch whose top input is absent becomes `goto` of the branch offset or the switch default.
  - [P3] Every unnecessary instruction is deleted. Exception handlers whose handler PC was not traced are emptied by setting end PC equal to start PC. Edits are then applied. A runtime failure is logged and that method is left unoptimized.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/EvaluationSimplifier.java`
- Bazel lines: 1648
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no partial-evaluation code simplifier. Non-code attributes are ignored. A code attribute is evaluated, edits are reset to `u4codeLength`, and only traced offsets are visited. Edits are applied by a `CodeAttributeEditor` constructed with frame-size updates and instruction shrinking both on, so each recorded insertion or replacement is shrunk immediately and a later edit at the same offset overwrites the earlier one. The side-effect checker is constructed so returns and array stores count as side effects. Any `RuntimeException` from the pass, including a `ClassCastException` when the class is not a `ProgramClass`, is printed to stderr as `Unexpected error while simplifying instructions after partial evaluation:` plus class name, method name and descriptor, exception class and message, and `Not optimizing this method`. `DEBUG` is the constant `false`, so the method is not dumped and the exception is not rethrown. An exception before the final editor visit leaves the method unchanged.
  - [P3] `idiv`, `irem`, `ldiv`, `lrem`, `fdiv`, `frem`, `ddiv`, and `drem` fold to a constant push when the checker reports no side effects. Otherwise a particular divisor of integer `0`, long `0`, float `== 0f` (both signed zeros), or double `== 0d` is replaced by `new`/`dup`/`invokespecial <init>()V`/`athrow` of `java/lang/ArithmeticException`. That exception path does not pop the original operands. The same exception rewrite is used for a null array on `iaload`, `baload`, `caload`, `saload`, `laload`, `faload`, `daload`, `aaload`, and `arraylength` when those instructions have side effects. The null test is stack slot `stackPopCount - 1`: reference, particular, and `isNull() == ALWAYS`, and the class is `java/lang/NullPointerException`.
  - [P3] Arithmetic and conversions with no exception fallback fold only when they have no side effects: `iadd`, `isub`, `imul`, `ineg`, `ishl`, `ishr`, `iushr`, `iand`, `ior`, `ixor`, `l2i`, `f2i`, `d2i`, `i2b`, `i2c`, `i2s` to an int push; `ladd`, `lsub`, `lmul`, `lneg`, `lshl`, `lshr`, `lushr`, `land`, `lor`, `lxor`, `i2l`, `f2l`, `d2l` to a long push; `fadd`, `fsub`, `fmul`, `fneg`, `i2f`, `l2f`, `d2f` to a float push; `dadd`, `dsub`, `dmul`, `dneg`, `i2d`, `l2d`, `f2d` to a double push. Array loads take the matching push when they have no side effects.
  - [P3] A particular int that fits in a signed short is recorded as `sipush`; any other int is an `ldc` integer. Longs use `lconst_0` only for `0L` and `1L` (operand `0` or `1`) and `ldc2_w` otherwise. Floats use `fconst_0` with the int cast only for `+0.0f` (`floatToIntBits` equal to `+0.0f`), `1.0f`, and `2.0f`; `-0.0f` is an `ldc`. Doubles use `dconst_0` only for `+0.0` and `1.0`; `-0.0` is `ldc2_w`. A non-particular but specific value is replaced by a load of an equal local. Int search stops at the lowest match. Long, float, and double searches do not stop, so the highest match overwrites the edit. Long and double also require the next local to be non-null and computational type top. For `iload`/`lload`/`fload`/`dload` (including the `_0`..`_3` forms) the search stops before that variable’s own index. Other producers search every local before the instruction.
  - [P3] `aload` and `checkcast` become `aconst_null` only when the pushed reference is always null. `aaload` does that only when it has no side effects; a null array with side effects becomes the `NullPointerException` sequence instead. `iastore`, `bastore`, `castore`, `sastore`, `lastore`, `fastore`, `dastore`, and `aastore` are never folded; under `optimize.conservatively` a null array becomes that same `NullPointerException` sequence. `invokevirtual`, `invokespecial`, and `invokeinterface` do the same for a null receiver and then do not fold. Otherwise those invokes, plus `getstatic`, `getfield`, and `invokestatic`, fold only when they push at least one value, have no side effects, and the top result is particular, dispatching on int, long, float, double, or reference. A zero-push instruction is left in place. `invokedynamic` is not handled.
  - [P3] `goto` and `goto_w` are left unchanged. Any other branch, including `ret` and a switch, becomes `goto` when partial evaluation has exactly one target and that target is not exactly one instruction length ahead. A one-length target is left as the original instruction. The pop count is the old pop count minus the replacement pop count: one `pop`, one `pop2`, or `popCount / 2` `pop2` instructions plus a final `pop` when odd. The same `pop2` instance is reused in that array. `jsr`/`jsr_w` take that goto path when the subroutine does not return or has exactly one caller. Otherwise, if the instruction after the `jsr` is untraced, that offset is replaced by `goto 0`. An `astore`/`astore_0`..`astore_3` at a subroutine start is deleted when the subroutine does not return or has exactly one caller. That delete does not notify the optional extra visitor. Replacements, the exception sequence, and the infinite loop do: the original instruction is visited with a null method and code attribute. An enum-switch rewrite visits the replacement switch instead.
  - [P3] A `tableswitch` or `lookupswitch` that was not rewritten to `goto` is treated as an enum switch when its top producer is a single unedited `iaload` and the array reference under that load is particular. The jump table is rebuilt by loading `mapping[index]`. A table case outside `[lowCase, highCase]` uses the default. A lookup case uses `Arrays.binarySearch` on the case array, so an unsorted table misses and uses the default. The lookup is replaced by a `tableswitch` from `0` through `length - 1` (high case `-1` when the array is empty). The `iaload` becomes `swap`, `pop`. Branch targets that partial evaluation does not contain are rewritten to the last recorded target minus the switch offset, including the default. An empty target list asks for a negative index and aborts the method. Redundant defaults are then trimmed: a table drops leading and trailing default slots and shifts `lowCase`/`highCase`, and a lookup drops default entries in place. If nothing non-default remains, the switch becomes `goto` of the default offset, with the usual pop inserted.
  - [P3] The default constructor uses a fresh `PartialEvaluator` and no extra visitor. Zel has no equivalent evaluator, constant folding, subroutine `jsr` rewrite, enum-switch flattening, switch cleanup, or explicit `ArithmeticException` / `NullPointerException` instruction sequences.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/InitializationFinder.java`
- Bazel lines: 349
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bytecode walk that pairs each `new` with the `<init>` that initializes it, tracks still-uninitialized creation offsets along forward branches (cleared at exception handlers and after a branch), or records the super/`this` initializer offset of `<init>` methods.
  - [P3] The query surface (`isInitializer`, `superInitializationOffset`, `creationOffset`, `isInitializedBefore`, and per-offset `isInitializer`) is absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/InstructionUsageMarker.java`
- Bazel lines: 1789
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no partial-evaluation pass that marks necessary bytecode instructions, unused invoke parameters, side-effecting and infinite-loop instructions, verifier-required variable initializations, stack-consistency producers, or extra push/pop instructions.
  - [P3] Query APIs for traced and necessary instructions, extra push/pop offsets, stack entries that are necessary or unwanted, and branch targets are absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/LivenessAnalyzer.java`
- Bazel lines: 500
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No backward dataflow over partial-evaluation traces that records which of the first 64 locals are alive before and after each traced instruction.
  - [P3] Loads do not mark a local alive before the instruction, and stores do not clear it before the store while forcing it alive immediately after.
  - [P3] Branch-target liveness is not unioned into the instruction after-state, and growth of before-liveness when the offset is not the latest branch origin does not force another pass.
  - [P3] Liveness at an exception-handler entry is not copied across every traced instruction in the try range, and that copy does not schedule another pass.
  - [P3] At the super-initializer call offset, local 0 (`this`) is not forced alive.
  - [P3] Category-2 values (long/double) are not recorded, and the following slot is not marked alive and category-2.
  - [P3] Locals at index 64 and above are not reported alive solely because the partial evaluator still holds a value, and the traced-instruction query is absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/LoadingInvocationUnit.java`
- Bazel lines: 164
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no invocation unit that, while evaluating a reference, returns a previously stored field class value, field value, method parameter value, or method return value when one is present.
  - [P3] The three independent load switches (field values, method parameter values, and method return values) are absent, including the default that turns all three on.
  - [P3] Field and method-return loads that require a resolved referenced member, the parameter load keyed by method and parameter index, and the fall-through to the basic invocation unit when nothing was stored are absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/ParameterTracingInvocationUnit.java`
- Bazel lines: 171
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no invocation unit that stores each method parameter by index and, for a reference return, rebuilds its trace from the callee’s returned-parameter bits, external-value flag, and new-instance flag.
  - [P3] Missing the return-type check that keeps a stored parameter trace only when that value may be an instance of the callee return type, then generalizes those traces onto the delegated return value.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/PartialEvaluator.java`
- Bazel lines: 1532
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bytecode partial evaluator. ProGuard walks each `CodeAttribute` from offset 0, abstractly executing instructions and recording locals and operand stack before and after every visited offset, plus whether that offset was executed.
  - [P3] Join points merge frames by generalization. A path stops when locals, stack, and branch targets are unchanged and the offset was already executed in a generalized context. After five evaluations of the same offset, the current frame is generalized into the accumulated one (most recent variable values stay last, for `ret`) and execution continues.
  - [P3] Branch targets and origins are accumulated. A branch with no targets ends the block, one definite target is followed immediately, and multiple targets are queued as separate blocks with copied frames. `evaluateAllCode` selects a branch unit that still evaluates unreachable or unnecessary branches and casts. An optional visitor runs immediately before each instruction.
  - [P3] `jsr` and `jsr_w` run in a nested evaluator so values live across the call do not clash between invocations. `ret` queues the continuation on the caller. Subroutine results are generalized back over the whole method.
  - [P3] Exception handlers run only when some traced instruction in the protected range may throw, or for every traced instruction when `evaluateAllCode` is set. Handler locals are the generalization of traced try frames, including the frame after the last traced instruction when that flag is set. The catch stack is supplied by the invocation unit. Handlers repeat until entering one no longer increases its evaluation count.
  - [P3] Parameters come from the invocation unit. Each slot’s producer is the parameter index, counting category-2 types as two slots. Unexpected evaluation failures print the class, method, and instruction to stderr and are rethrown. Callers can query traced ranges, branch and subroutine structure, creation and initializer sites, frames, and branch origins and targets, and can visit only traced or only untraced instructions.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/ReferenceTracingInvocationUnit.java`
- Bazel lines: 210
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no partial-evaluation invocation decorator that tags reference values with an origin (instruction offset or parameter index).
  - [P3] Caught exceptions, field and field-class loads, and method returns (including invokedynamic) are not stamped with the offset recorded by `enterExceptionHandler` and `invokeMember`, combined with the exception-handler, field-value, or method-return flag.
  - [P3] Method parameter values are not tagged with the parameter index (not a local-variable slot, so category-2 values are ignored) plus the method-parameter flag.
  - [P3] Non-reference values are not left untagged, existing traces are not replaced by unwrapping then rewrapping, setters are not forwarded without tracing, and exception/member entry is not routed through the superclass while only the value getters and setters use the delegate.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/ReferenceTracingValueFactory.java`
- Bazel lines: 307
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no value factory that attaches the current instruction-offset trace to newly created reference values, sets that trace from `aconst_null`, `newarray`, `athrow`, `aaload`, `ldc`/`ldc_w`, `new`, `anewarray`, `multianewarray`, field gets, invokes, and `checkcast` (and clears it for other instructions), or preserves versus replaces the trace when casting a traced reference.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/SimpleEnumArrayPropagator.java`
- Bazel lines: 106
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On each visited class, Bazel reads the stored value of every field whose descriptor is `[I` (last match wins) and, when one exists, sets the optimization return value of every `()[I` method that has optimization info.
  - [P3] The propagated value is a new `int[]` reference with the same length as that field array, not the original array, so later element overwrites do not change the method return value.
  - [P3] Zel has no bytecode evaluation pass that copies a simple enum `int[]` field into the `values()`-style method return value.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/SimpleEnumClassChecker.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never marks a program class as a simple enum when it declares `<init>(Ljava/lang/String;I)V`, and only when that class already has program-class optimization info.
  - [P3] Zel never clears that mark when any field or method has neither `ACC_PRIVATE` nor `ACC_STATIC`. Library classes are ignored by the checker, and that no-op is also absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/SimpleEnumClassSimplifier.java`
- Bazel lines: 183
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not clear `ACC_ENUM` on a visited program class.
  - [P3] Zel does not remove that class's `valueOf` method when one exists (any descriptor).
  - [P3] Zel does not rewrite `<clinit>` so `new` / `dup` / `ldc` name / `iconst_<ordinal>` / `invokespecial <init>` becomes `sipush ordinal; iconst_1; iadd`.
  - [P3] Zel does not rewrite the encrypted-name form `iconst_<ordinal>; invokespecial <init>` into `iconst_0; invokespecial <init>; pop; sipush ordinal; iconst_1; iadd`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/SimpleEnumDescriptorSimplifier.java`
- Bazel lines: 797
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no pass that rewrites simple-enum descriptors on program fields and methods, signatures, local-variable tables and type tables, or string, class, invokedynamic, and method-type constants.
  - [P3] A leading simple-enum type is not replaced by `I` with its array dimensions kept, and later simple-enum type arguments are not replaced by `java/lang/Integer`, with the referenced-class list shrunk to match.
  - [P3] Fields and non-`<init>` methods are not renamed with the special member separator plus the absolute hex descriptor hash, and simplified fields do not drop `ACC_ENUM`, the referenced class, or the optimization value when the field is not kept.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/SimpleEnumUseChecker.java`
- Bazel lines: 831
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no partial-evaluation pass that unmarks enums too complex to simplify. The Java checker clears the simple-enum mark for bootstrap-method handles and arguments, for every referenced class of an annotation type, and for string, class, method-handle, method-type, and ref constants.
  - [P3] It also clears the mark from traced bytecode: storing a simple enum into a wider array (`aastore`), returning it as a wider type (`areturn`), using it as a monitor, storing or passing it as a wider field or argument type, `invokevirtual` of anything other than `ordinal()I` or `clone()Ljava/lang/Object;`, `invokespecial`/`invokestatic`/`invokeinterface` of `valueOf`, and `checkcast`/`instanceof` of a different type. `checkcast` inside a static `values` or `valueOf` method of a simple enum does not unmark the checked type.
  - [P3] Reference compares (`if_acmpeq`/`if_acmpne`) of unequal types unmark both operands. At branch and exception targets, a reference stack slot or local with multiple producers unmarks those producers when the consumed type is not a simple enum, skipping exception-handler producers on the stack and method-parameter producers in locals.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/SimpleEnumUseSimplifier.java`
- Bazel lines: 861
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard pass that, after partial evaluation, rewrites traced uses of `SimpleEnumMarker` classes from reference operations to `int` operations, and it does not skip non-static methods of those classes.
  - [P3] No rewrites of `aaload`/`aastore`/`areturn`/`aload*`/`astore*` (the last only when the store is not a subroutine start), `anewarray` of a non-array simple enum, deletion of `checkcast`, `instanceof` to `iconst_1`, `ifacmpeq`/`ifacmpne` to `ificmpeq`/`ificmpne`, or `ifnull`/`ifnonnull` to `ifeq`/`ifne`.
  - [P3] No `invokevirtual` replacement of `ordinal()` with `iconst_1; isub`, and no null-producer fixup on `putstatic`/`putfield` or any `invoke*` when a non-primitive, non-array parameter or field type is a simple enum.
  - [P3] No conversion of always-null producers to `iconst_0` (`aconst_null` and `aload*` replaced; other producers kept, then `pop` and `iconst_0`; variable stores skip parameters and exception handlers and change `astore` to `istore`), and no `pop`/`pop2` insertion when a replacement pops fewer stack values. Zel only records ProGuard tool flags and validates library ProGuard configs.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/StoringInvocationUnit.java`
- Bazel lines: 192
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no invocation unit that, when enabled, generalizes a referenced field’s class and value, a method’s parameter value at a given index, or a method’s return value onto that member, skipping the write when the member is kept and when a field or parameter reference has no resolved member.
  - [P3] Zel has no readers that return the stored referenced class, field value, parameter value, or return value from member optimization info.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/TracedBranchUnit.java`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no branch unit that clears a fixed flag on reset, sets it on an unconditional branch or an ALWAYS conditional (replacing traced targets via `branch`), records a MAYBE target only while unfixed, and on any other conditional only marks the unit called without adding a target.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/evaluation/VariableOptimizer.java`
- Bazel lines: 357
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no liveness-based local-slot optimizer that trims `LocalVariableTable` and `LocalVariableTypeTable` ranges to traced liveness (leaving an unused variable's start PC unchanged and its length at 0), then remaps non-parameter locals below index 64 onto the smallest earlier slot that is never alive at the same time (skipping category-2 variables; leaving slot 0 alone on instance methods unless `reuseThis` is set), updates liveness as it goes, and either remaps the code and notifies an optional extra member visitor or, if nothing remapped, only cleans empty variables.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/DuplicateJsonFieldNameChecker.java`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On a program class, Bazel checks each `OptimizedJsonFieldCollector` mode (`serialize`, then `deserialize`) separately. For each mode it collects that class's JSON names (`@SerializedName` values, otherwise the Java field name) and sets `hasDuplicateJsonFieldNames` when any name repeats across those values.
  - [P3] The first duplicate returns immediately, so later modes are not scanned, and the checker never clears the flag. A library-class visit leaves the flag unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/FieldSignatureCollector.java`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no field-signature collector. The Java visitor starts with a null signature, ignores every non-signature attribute, and on `visitSignatureAttribute` replaces the stored value with `signatureAttribute.getSignature(clazz)`, so a later signature overwrites an earlier one and `getFieldSignature` returns that string or null if none was seen.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonAnnotationCleaner.java`
- Bazel lines: 91
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no pass that, after Gson optimization, deletes field annotations that Gson no longer needs.
  - [P3] Program classes are visited and library classes are skipped. When neither `setFieldNamingPolicy` nor `setFieldNamingStrategy` was used, `@SerializedName` on fields is marked for removal.
  - [P3] `@Expose` on fields is always marked for removal. Marked annotations are deleted from field attributes, then remaining field annotation visitor marks are cleared.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonBuilderInvocationFinder.java`
- Bazel lines: 533
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not scan bytecode for `GsonBuilder` calls or record which builder settings are used.
  - [P3] Missing sticky flags for `setVersion`, `excludeFieldsWithModifiers`, `generateNonExecutableJson`, `excludeFieldsWithoutExposeAnnotation`, `serializeNulls`, `disableInnerClassSerialization`, `setLongSerializationPolicy`, `setFieldNamingPolicy`, `setFieldNamingStrategy`, `setExclusionStrategies`, `addSerializationExclusionStrategy`, `addDeserializationExclusionStrategy`, and `serializeSpecialFloatingPointValues`. Each flag becomes true on the first matching `invokevirtual` and stays true.
  - [P3] Missing the `registerTypeAdapter` and `registerTypeHierarchyAdapter` path. It runs only when both class visitors are set, partially evaluates the method once, and requires exactly one program-class type argument and one program-class adapter argument. If the adapter implements `InstanceCreator`, the domain class is sent to the instance-creator visitor; otherwise it is sent to the type-adapter visitor. A library adapter class is ignored.
  - [P3] `enableComplexMapKeySerialization` is built into an instruction sequence and then discarded, so that call is not detected.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonClassConstants.java`
- Bazel lines: 181
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no table of Gson internal names and JVM descriptors that an optimizer can match: `GsonBuilder` configuration methods, `Gson` fields and `toJson`/`fromJson`/`getAdapter` overloads, `TypeToken`, `Excluder`, `InstanceCreator`, `TypeAdapter`, `FieldNamingPolicy.IDENTITY`, `JsonReader`/`JsonWriter` stream methods, `JsonSyntaxException`, `JsonToken.NULL`/`BOOLEAN`, and the `Expose`, `JsonAdapter`, and `SerializedName` annotation descriptors.
  - [P3] Zel’s only Gson-related behavior is Bazel lockfile JSON (lenient parse errors and HTML escaping), which does not identify or rewrite Gson bytecode.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonConstructorPatcher.java`
- Bazel lines: 237
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Gson constructor patcher that, for a method whose descriptor contains `Ljava/util/List;`, finds the first `java.util.List.addAll(java.util.Collection)` and the `aload` that produces its argument, then inserts after the first `java.lang.Object.<init>()V` a replacement of that local with `new ArrayList(thatLocal)` followed by `List.add` of a new `proguard/optimize/gson/_OptimizedTypeAdapterFactory`, and when `addExcluder` is set stores local 1 into `com/google/gson/Gson.excluder`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonContext.java`
- Bazel lines: 99
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not build a Gson-usage context over a program class pool: it never keeps the non-`com/google/gson/**` classes separate from the Gson library itself.
  - [P3] Zel does not scan those classes’ instructions for `GsonBuilder` calls to record runtime settings, instance-creator classes, and type-adapter classes.
  - [P3] Zel does not scan the same classes for Gson serialize and deserialize calls to collect domain classes, using the type-adapter pool and an optional note printer.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonDeserializationInvocationFinder.java`
- Bazel lines: 236
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bytecode visitor that matches `Gson.fromJson` and records the domain classes being deserialized.
  - [P3] No matchers for the five overloads (`String`/`Class`, `String`/`Type`, `Reader`/`Class`, `Reader`/`Type`, `JsonReader`/`Type`), and no partial evaluation of the `Class` or `Type` stack argument to find the producing instructions.
  - [P3] No walk of those producers to recover target types, no program-class-pool visit of each recovered type, and no note when the deserialized type cannot be derived.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonDeserializationOptimizer.java`
- Bazel lines: 685
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that injects Gson `fromJson$<classIndex>` and `fromJsonField$<classIndex>` into a program class. Library classes are ignored. The class index is the unboxed `deserializationInfo.classIndices` value for the class name, so a missing entry throws. After both methods are added, references are linked and the class edit is finished against the program and library pools.
  - [P3] No zel pass clears `ACC_PRIVATE` and sets `ACC_PUBLIC` on the visited class (a protected class also keeps `ACC_PROTECTED`). Every existing `<init>()V` has its access flags replaced with public. If there is no `<init>()V`, a public synthetic one is added that calls `super.<init>()V` and returns, then receives program-member optimization info.
  - [P3] No injected public synthetic `fromJson$(Gson, JsonReader, _OptimizedJsonReader)V` that calls `beginObject`, then while `hasNext` stores `_OptimizedJsonReader.b(JsonReader)` in a new local at the code attribute's `maxLocals` and calls `this.fromJsonField$(gson, reader, index)`, then `endObject` and returns. That edit also stores this class's `ClassJsonInfo` and its Java-to-JSON field-name map for the field method.
  - [P3] No protected synthetic `fromJsonField$(Gson, JsonReader, int)V`. When that map is non-empty, `JsonReader.peek()` is compared to `JsonToken.NULL`: a null token takes the null path and any other token is deserialized. `lookupswitch` cases are one per JSON name, sharing one label per Java field, sorted by ascending JSON field index using signed subtraction. The default case is the code after the field bodies.
  - [P3] With `excludeFieldsWithoutExposeAnnotation` captured at construction, the first switch contains only exposed Java fields. A second switch of the other mapped fields runs only when `gson.excluder.requireExpose` is 0. Without that setting, one switch contains every mapped Java field. Each case index comes from `jsonFieldIndices`; a missing index throws on unboxing.
  - [P3] Case bodies are written only for non-synthetic, non-static program fields whose name has a switch label, and `ACC_FINAL` is cleared on those fields. Descriptors `B`, `S`, `I`, and `Ljava/lang/String;` use the inline primitive-integer or string deserializer when `canDeserialize` allows it. Every other descriptor loads a Gson `TypeAdapter` and `read`s it: primitives use the numeric wrapper class; other types use the field descriptor. A non-null field signature adds a generated `TypeToken` subclass to the program pool, initializes its references, records it in the injected-class multimap, and calls `getAdapter(TypeToken)` instead of `getAdapter(Class)`. The read value is checkcast, and `ZBCSIJFD` are unboxed before `putfield`.
  - [P3] On a null token, a non-primitive field is set to null and a primitive field is left unchanged, then `nextNull` runs. A matched field jumps to the shared return and does not fall through. An unmatched index, or a class with no mapped fields, calls `super.fromJsonField$<superIndex>` when the superclass is not `java/lang/Object` (a missing super index becomes the method name `fromJsonField$null`) and otherwise calls `JsonReader.skipValue`.
  - [P3] Searches of `/workspace/zel-cursor` found no port of this injector, `fromJson$`, or `_OptimizedJsonReader` field dispatch. Zel's Gson code is lockfile and registry JSON, and its ProGuard surface is flags plus `proguard_allowlister.py`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonDomainClassFinder.java`
- Bazel lines: 273
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no GSON domain-class visitor. For a program class absent from the domain pool, the Java finder silently drops local or anonymous classes, then stops at the first failure on the class and its superclasses (not interfaces): a library superclass other than `java/lang/Object` or `java/lang/Enum`; a program class matching `com/google/gson/**`; a field with a generic type parameter; duplicate JSON field names; a class present in the registered type-adapter pool; or a `Lcom/google/gson/annotations/JsonAdapter;` annotation on the class or, deeply, its members. Each failure, when a note printer is set, prints the reason and ` You should consider keeping this class and its fields.`, and the reason is passed through `externalClassName` on the internal name concatenated with the sentence, so `/` becomes `.`. A non-interface that passes is added to the domain pool, then non-synthetic fields are followed through their descriptors and signature attributes with the same visitor, and superclasses and subclasses are visited the same way. Interfaces are left out of the pool and their fields are not followed. Library classes are ignored. A class already in the pool is skipped entirely.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonInstrumentationAdder.java`
- Bazel lines: 106
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No instruction visitor inserts, before `return` and `areturn`, an `ldc` of `"Type token cache after invoking " + className + "#" + methodName + descriptor + ":"`, `aload_0`, `getfield typeTokenCache Ljava/util/Map;`, and `invokestatic proguard/optimize/gson/_GsonUtil.dumpTypeTokenCache(Ljava/lang/String;Ljava/util/Map;)V`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonOptimizer.java`
- Bazel lines: 320
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No pass discovers Gson `toJson`/`fromJson` uses, derives domain classes (including nested field and element types), and bails out when that pool is empty.
  - [P3] No rewrite makes `Gson` and `Excluder` fields public or clears `final` on the `Gson` class.
  - [P3] No collection of separate serialize and deserialize field sets, deletion of `@SerializedName` and `@Expose`, or assignment of random class and field indices.
  - [P3] No injection of the optimized type-adapter, util, reader, writer, and factory templates, or of the index-to-name tables in the reader and writer implementations (so JSON names stay out of domain classes in plaintext).
  - [P3] No injection of reflection-free serialize and deserialize methods into non-enum domain classes, type adapters, or the type-adapter factory, and no registration of that factory from the `Gson` constructor.
  - [P3] No addition of a public `excluder` field when it is missing (Gson 2.1 compatibility), and no verbose count of optimized serializable classes. The Java `DEBUG` instrumentation of `toJson`/`fromJson` is compiled out (`DEBUG` is false).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonRuntimeSettings.java`
- Bazel lines: 65
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel type records which `GsonBuilder` options are in use. The Bazel object holds public booleans, all defaulting to false: `setVersion`, `excludeFieldsWithModifiers`, `generateNonExecutableJson`, `excludeFieldsWithoutExposeAnnotation`, `serializeNulls`, `disableInnerClassSerialization`, `setLongSerializationPolicy`, `setFieldNamingPolicy`, `setFieldNamingStrategy`, `setExclusionStrategies`, `addSerializationExclusionStrategy`, `addDeserializationExclusionStrategy`, and `serializeSpecialFloatingPointValues`.
  - [P3] Those flags are the whole behavior (no methods). The class deliberately does not track complex map-key serialization, pretty printing, lenient parsing, HTML escaping, date format, or type-adapter registration, because Gson’s own map, date, reader, and writer adapters cover them.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonSerializationInvocationFinder.java`
- Bazel lines: 266
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bytecode visitor that matches `Gson#toJson` and `Gson#toJsonTree` calls and records the domain classes being serialized.
  - [P3] The seven matched overloads are absent: `toJson(Object)`, `toJson(Object, Type)`, `toJson(Object, Appendable)`, `toJson(Object, Type, Appendable)`, `toJson(Object, Type, JsonWriter)`, `toJsonTree(Object)`, and `toJsonTree(Object, Type)`.
  - [P3] When the call has no `Type` argument, a program class referenced by the object argument is not visited as a serialization domain class.
  - [P3] When the call has a `Type` argument, types recovered from that argument’s producers are not looked up in the program class pool and visited.
  - [P3] There is no note when a `toJson` `Type` argument’s serialized type cannot be derived.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/GsonSerializationOptimizer.java`
- Bazel lines: 517
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that clears `ACC_PRIVATE` and sets `ACC_PUBLIC` on a program class, then injects a public synthetic `toJson$<classIndex>` (beginObject, call `toJsonBody$`, endObject, return) and a protected synthetic `toJsonBody$<classIndex>` that writes each non-static, non-synthetic field present in `javaToJsonFieldNames`: skip a self-reference on an object field, and if `excludeFieldsWithoutExposeAnnotation` is set skip fields not in `exposedJavaFieldNames` when `gson.excluder.requireExpose` is true; write the name through `OptimizedJsonWriter.name` using `jsonFieldIndices` of the first JSON name; inline-serialize primitive boolean, `java.lang.Boolean`, primitive byte/short/int, and `String` when `canSerialize` allows it; otherwise load the field, box primitives, resolve a type adapter by `Class` or by a generated `TypeToken` subclass recorded in `injectedClassNameMap` when a signature exists, and `write` the value; then invoke the superclass `toJsonBody$<superIndex>` unless the superclass is `java.lang.Object`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/InlineDeserializer.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No check that decides, from which Gson builder calls are used, whether optimized field-deserialization bytecode can be injected.
  - [P3] No injection of that bytecode for a domain-class field, with locals `this`, `gson`, `jsonReader`, and `fieldIndex`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/InlineDeserializers.java`
- Bazel lines: 180
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard Gson optimizer that injects these inline `fromJson` field deserializers. Both implementations always report they can deserialize and ignore runtime Gson settings.
  - [P3] Primitive integer deserializer: load `this` and the `JsonReader`, call `nextInt()`, apply `i2b` only when the target type is `byte` and `i2s` only when it is `short` (plain `int` and a null target are stored unchanged), `putfield`, and on `NumberFormatException` only store the exception and throw `JsonSyntaxException` constructed with that throwable. Other exceptions are not caught.
  - [P3] String deserializer: `peek` the reader and compare the token to `JsonToken.BOOLEAN`. If it matches, `nextBoolean` then `Boolean.toString` before `putfield`. Otherwise `nextString` then `putfield`. There is no exception wrapper.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/InlineSerializer.java`
- Bazel lines: 71
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No hook that decides, from the program class pool and which Gson builder calls are in use, whether optimized bytecode can be injected for one domain field.
  - [P3] No hook that appends that field-serialization bytecode into a method body whose locals are the domain object, the `Gson` instance, a `JsonWriter`, and an optimized `JsonWriter`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/InlineSerializers.java`
- Bazel lines: 171
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Gson `toJson` bytecode inliners that always emit `JsonWriter.value(boolean)` for a primitive boolean field.
  - [P3] Zel does not gate a boxed `Boolean` inliner on `JsonWriter.value(Boolean)` being present (Gson 2.7+) and then emit that call.
  - [P3] Zel does not box primitive `int`/`short`/`byte` fields with `Integer.valueOf` and then call `JsonWriter.value(Number)`.
  - [P3] Zel does not emit `JsonWriter.value(String)` for a `String` field.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/LocalOrAnonymousClassChecker.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no checker that marks a program class local or anonymous when an InnerClasses entry for that class has outer-class index 0 or inner-name index 0.
  - [P3] There is no visit that clears the flag, walks every InnerClasses entry, and overwrites the flag only for the entry whose inner-class index equals the class’s this-class index, leaving non-program visits unchanged.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/MarkedAnnotationDeleter.java`
- Bazel lines: 205
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no attribute visitor that deletes runtime visible and invisible annotations, type annotations, and parameter annotations whose visitor info is reference-equal to a given mark. The walk deletes by index while compacting the array, so a marked annotation that shifts into a just-deleted slot is left in place. The member attribute is then removed by name when the annotation list is empty; a parameter-annotation attribute is removed only when every parameter’s remaining count is zero.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/OptimizedClassConstants.java`
- Bazel lines: 146
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not implement ProGuard’s Gson optimizer, so it has no contract for the injected template classes (`_GsonUtil`, `_OptimizedJsonReader`/`Impl`, `_OptimizedJsonWriter`/`Impl`, `_OptimizedTypeAdapter`/`Factory`/`Impl`) or the obfuscated method names and descriptors (`a`/`b`/`c` for the names map and next-field/value index, `fromJson$`, `fromJsonField$`, `toJson$`, `toJsonBody$`, `getTypeAdapter`, `dumpTypeTokenCache`, `create`).
  - [P3] Zel does not derive internal field/method types with `ClassUtil.internalTypeFromClassName` (`L…;`) for those templates, nor the fixed field names (`optimizedJsonReaderImpl`, `optimizedJsonWriterImpl`, `gson`, `optimizedJsonReader`, `optimizedJsonWriter`, `excluder`, `requireExpose`) and local-slot maps (`ReadLocals`, `WriteLocals`, `FromJsonLocals` including `MAX_LOCALS = 3`, `FromJsonFieldLocals`, `ToJsonLocals`).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/OptimizedJsonFieldCollector.java`
- Bazel lines: 252
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not register each visited program class in a Gson optimized-JSON info map with a fresh per-class record and a null class index, and it does not ignore library classes and non-field members.
  - [P3] Zel does not read `Lcom/google/gson/annotations/SerializedName;` on program fields, flatten every constant element (including array values such as `alternate`) into a Java-to-JSON name array in element order, or, when that annotation is absent, default the mapping to the Java field name alone. It also does not insert each resulting JSON name into a global field-index map with a null value. An empty collected name list stays empty and is not replaced by the Java name.
  - [P3] Zel does not apply `Lcom/google/gson/annotations/Expose;` per mode (`serialize` or `deserialize`): the field starts exposed, only the integer element whose name equals that mode is consulted, a value of `0` drops it, and a still-exposed field is added to a set of Java field names independently of the JSON name map.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/OptimizedJsonFieldVisitor.java`
- Bazel lines: 96
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not select Gson (de)serialization fields: on each program class it does not first delegate the class, then visit every non-transient field of a non-enum, and for an enum only the non-transient fields whose descriptor is that class’s own internal type (`L…;`).
  - [P3] Zel does not ignore library classes and non-field members, nor forward each matching program field to a member visitor with its owning program class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/OptimizedJsonInfo.java`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not keep ProGuard’s Gson optimizer tables: class name to a contiguous index (suffixes for generated `toJson`/`fromJson`), JSON field name to a contiguous index (name/index mapping in `_OptimizedJsonReader` and `_OptimizedJsonWriter`), or class name to per-class JSON info.
  - [P3] Zel has no `assignIndices` pass that overwrites those two maps with `0..n-1` in `HashMap` key-set order (each map starting at 0, class JSON infos left unindexed), and no per-class record of exposed Java field names plus Java-field-to-JSON-name arrays (first name written, later names accepted on read).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/OptimizedJsonReaderImplInitializer.java`
- Bazel lines: 115
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never rewrites the code of injected `_OptimizedJsonReaderImpl.a()Ljava/util/Map;` (the obfuscated `initNames` that the template implements as `return null`).
  - [P3] Bazel replaces instruction 0 (`aconst_null`) with `new java.util.HashMap` / `dup` / `<init>()V`, then one `put` per `deserializationInfo.jsonFieldIndices` entry: the JSON field-name string and `Integer.valueOf` of its index, discarding `put`'s return value. The following `areturn` stays, so the static map `a` is that `HashMap` (empty when there are no fields) instead of `null`.
  - [P3] Without that map, `_OptimizedJsonReaderImpl.b` cannot resolve `JsonReader.nextName()` to a field index (`a.get(name)`, or `-1` when absent) and would NPE on the uninitialized map. Non-code attributes are ignored.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/OptimizedJsonWriterImplInitializer.java`
- Bazel lines: 108
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard Gson optimizer that rewrites the injected `_OptimizedJsonWriterImpl` `a()()[Ljava/lang/String;` method (the `initNames` template that returns null).
  - [P3] On a code attribute, Bazel replaces instruction 0 with bytecode that allocates a `java.lang.String` array whose length is `serializationInfo.jsonFieldIndices.size()`, then for each map entry stores the JSON field name at that entry’s integer index (`dup`, `ldc` index, `ldc` name, `aastore`), and leaves the following return in place so the method returns that array.
  - [P3] An empty index map still replaces the null load with a zero-length `String[]`. Attributes other than code are ignored.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/OptimizedTypeAdapterAdder.java`
- Bazel lines: 161
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel visits program classes and, when `package.Optimized<ShortName>TypeAdapter` is absent from the program class pool, loads the `_OptimizedTypeAdapterImpl` template (`internal name + ".class"`), renames and initializes it for that domain class, adds it to the pool, wires references and the subclass hierarchy, and records the domain class in both the injected-class map and the type-adapter registry.
  - [P3] Library classes are ignored, an existing adapter class skips injection, and an I/O failure while reading the template is wrapped as a runtime exception.
  - [P3] Zel has no ProGuard Gson optimizer and does not inject or register per-domain optimized type adapters. Its Gson code only parses lockfile and registry JSON.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/OptimizedTypeAdapterFactoryInitializer.java`
- Bazel lines: 192
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not rewrite the `aconst_null` in `_OptimizedTypeAdapterFactory.create(Gson, TypeToken)TypeAdapter` into a dispatch over the domain-class to optimized-adapter registry.
  - [P3] Zel does not emit the runtime bail-outs that return null when `Gson.fieldNamingStrategy` is not `FieldNamingPolicy.IDENTITY`, `Excluder.modifiers` is not `STATIC|TRANSIENT`, either exclusion-strategy list is non-empty, or `Excluder.version` is not `-1.0` (those checks are emitted only when the matching `GsonRuntimeSettings` flags were seen).
  - [P3] Zel does not, on a raw-type match, allocate the registered adapter with `(Gson, optimizedJsonReaderImpl, optimizedJsonWriterImpl)` and return it, falling through to null when nothing matches.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/OptimizedTypeAdapterInitializer.java`
- Bazel lines: 552
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not rewrite a template `_OptimizedTypeAdapter` into a per-domain Gson `TypeAdapter`: it never renames the class constant to the concrete adapter name, nor rewrites local-variable descriptors that still name the template type.
  - [P3] For enum domain classes it does not clear `ACC_PRIVATE` and set `ACC_PUBLIC` on the enum so the adapter can read its constants.
  - [P3] It does not fill `read()` only when the class is in the deserialization index, or `write()` only when it is in the serialization index, then hand the rewritten class to a follow-on visitor.
  - [P3] Enum `read()` is absent: a `lookupswitch` on `OptimizedJsonReader.nextValueIndex()`, cases sorted by JSON-name index (every alias), each `getstatic` of the enum constant and `areturn`, default `aconst_null`/`areturn`.
  - [P3] Non-enum `read()` is absent: `JsonReader.peek` for `NULL` then `skipValue` and return null; otherwise `new`/`<init>` or, when an `InstanceCreator` is registered, `Gson.instanceCreators.get(Class).createInstance` plus `checkcast`; then `fromJson$<classIndex>(Gson, JsonReader, OptimizedJsonReader)` and return the instance.
  - [P3] Enum `write()` is absent: null becomes `JsonWriter.nullValue()`; otherwise identity-compare the value to each constant (map order, first JSON name only), push that name’s index or `-1`, and call `OptimizedJsonWriter.value(int)`.
  - [P3] Non-enum `write()` is absent: null becomes `JsonWriter.nullValue()`; otherwise `checkcast` to the domain type and call `toJson$<classIndex>(Gson, JsonWriter, OptimizedJsonWriter)`.
  - [P3] It does not replace instruction 0 of the template `read`/`write` bodies with that generated sequence.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/TypeArgumentFinder.java`
- Bazel lines: 252
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instruction visitor that, on `aload`, scans code from offset 0 up to that load for the last `astore` of the same local and then reads the partial-evaluator stack before that store to recover the producer offsets of the stored value.
  - [P3] Zel does not treat a `TypeToken.getType` method reference (`com/google/gson/reflect/TypeToken`) as a signal to load the invoked class from the program class pool and keep the last `Signature` attribute visited on that class, its fields, and its methods.
  - [P3] Zel does not turn that signature’s referenced classes into `typeArgumentClasses`, dropping nulls and `com/google/gson/reflect/TypeToken` and keeping every other referenced class name in visit order (an empty array when the signature is present but nothing remains).
  - [P3] Zel does not, on a constant instruction, replace `typeArgumentClasses` with a one-element array of a reference constant’s class name or a class constant’s name, while leaving the previous result unchanged for every other instruction and constant.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/TypeParameterClassChecker.java`
- Bazel lines: 83
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no check that, for a program class, walks every field signature and sets a sticky flag when the signature starts with `T` or contains `<T`, `;T`, `+L`, `+T`, or `[T`. ProGuard’s Gson domain-class finder uses that flag to refuse optimization of the class and its superclasses.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/TypeTokenClassBuilder.java`
- Bazel lines: 111
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never builds a public `com.google.gson.reflect.TypeToken` subclass named `<enclosing class name><field name>TypeToken` with a public `()V` constructor that calls `TypeToken.<init>()V`.
  - [P3] Zel never forces that class to version 1.5 or adds a `Signature` attribute whose value is `Lcom/google/gson/reflect/TypeToken<` plus the field signature plus `>;`.
  - [P3] Zel never marks the synthetic class with program-class optimization info or registers it as a subclass of `TypeToken` in the program class pool.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/_GsonUtil.java`
- Bazel lines: 143
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no injected helper that chooses a Gson `TypeAdapter` for a declared `Class` or `TypeToken` together with a runtime value.
  - [P3] When the runtime type is more specific and its adapter is not custom, a custom adapter registered for the declared type takes precedence; otherwise the runtime-type adapter is used. Zel does not implement that choice.
  - [P3] A non-null value narrows the type to `value.getClass()` when the declared type is `Object`, a type variable, or a class. Zel does not implement that narrowing.
  - [P3] An adapter is custom unless it is an `_OptimizedTypeAdapter` or a `ReflectiveTypeAdapterFactory.Adapter`. Zel does not make that distinction.
  - [P3] `dumpTypeTokenCache` prints a message and each cached `TypeToken` to `TypeAdapter` mapping to stdout. Zel has no such dump.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/_OptimizedJsonReader.java`
- Bazel lines: 71
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no injected reader contract that returns the next JSON field’s internal index from a Gson `JsonReader` (`b`, originally `nextFieldIndex`) or the next JSON value’s internal index (`c`, originally `nextValueIndex`), both throwing `IOException` when the read fails.
  - [P3] Optimized Gson deserialization therefore cannot switch on those integer indices instead of field and value strings. Zel’s Gson-related code only formats lockfile malformation messages and does not provide this lookup.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/_OptimizedJsonReaderImpl.java`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no injected optimized JSON reader that maps the next Gson field name (`JsonReader.nextName`) through a static `String` to `Integer` map and returns that index, or `-1` when the name is absent.
  - [P3] Zel has no equivalent lookup of the next JSON string (`JsonReader.nextString`) against that same map, with the same `-1` miss result and `IOException` propagation from the reader.
  - [P3] Zel does not provide the template whose obfuscated `initNames` (`a()`) returns `null` so later bytecode can install the name-to-index map in the static field `a`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/_OptimizedJsonWriter.java`
- Bazel lines: 69
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no injected writer contract whose method `b` writes the JSON field name for an integer index to a `JsonWriter` and fails with an I/O error when that write fails.
  - [P3] Zel has no injected writer contract whose method `c` writes the JSON string value for an integer index to a `JsonWriter` and fails with an I/O error when that write fails.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/_OptimizedJsonWriterImpl.java`
- Bazel lines: 81
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not inject an `_OptimizedJsonWriter` implementation whose static `String[]` (obfuscated field `a`, originally `names`) is produced by `a()` (`initNames`). That stub returns null so the optimizer can replace the `aconst_null` with a `String[]` of length `jsonFieldIndices.size()`, each JSON name stored at its internal index.
  - [P3] There is no `b(JsonWriter, int nameIndex)` that writes `a[nameIndex]` with Gson `JsonWriter.name` (object field name).
  - [P3] There is no `c(JsonWriter, int valueIndex)` that writes `a[valueIndex]` with Gson `JsonWriter.value` (JSON string), using that same table.
  - [P3] Both lookups are unchecked: a still-null table throws `NullPointerException`, an out-of-range index throws `ArrayIndexOutOfBoundsException`, and writer failures propagate as `IOException`. Zel’s JSON helpers only escape or serialize values; they do not emit a name or string by integer index through Gson.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/_OptimizedTypeAdapter.java`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no marker type for injected optimized Gson type adapters, so those adapters cannot be recognized or given the priority this interface exists to assign.
  - [P3] Adapter selection that treats an implementor of this marker as non-custom (a user-registered adapter on the declared type is kept ahead of an injected adapter on a more specific runtime type) has no counterpart.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/_OptimizedTypeAdapterFactory.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Gson `TypeAdapterFactory` whose `create(Gson, TypeToken)` returns null so later injected bytecode can dispatch to a domain-class optimized adapter.
  - [P3] Zel does not keep the two static singletons this factory constructs, `optimizedJsonReaderImpl` (`_OptimizedJsonReaderImpl`) and `optimizedJsonWriterImpl` (`_OptimizedJsonWriterImpl`).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/gson/_OptimizedTypeAdapterImpl.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Gson `TypeAdapter` subclass that implements the optimized-adapter contract and retains the `Gson` context plus optimized JSON reader and writer supplied to the constructor for later injected code.
  - [P3] `write` is a no-op and `read` always returns null without consuming the reader. Those bodies are injection points for generated `toJson$…` and `fromJson$…` calls on the domain class. Zel has no such stub and does not install or invoke that dispatch.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/AccessMethodMarker.java`
- Bazel lines: 193
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no pass that walks a method's constant instructions (string, class, field/method refs, method handles, and the bootstrap handles of dynamic and invokedynamic constants) and marks that method as accessing private members, protected members, or package-visible classes and members.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/BackwardBranchMarker.java`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instruction visitor that marks a method when a branch or switch target offset is negative.
  - [P3] There is no `branchesBackward` flag on method optimization info, and no setter or static query for it.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/CatchExceptionMarker.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never marks a method as catching exceptions when its code attribute has a non-empty exception table (`u2exceptionTableLength > 0`), which Bazel does through `ProgramMethodOptimizationInfo.setCatchesExceptions()`.
  - [P3] Zel has no query equivalent to `CatchExceptionMarker.catchesExceptions`, which reads `MethodOptimizationInfo.catchesExceptions()`. Other attributes are ignored.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/CaughtClassFilter.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that forwards `visitProgramClass` and `visitLibraryClass` only when `CaughtClassMarker.isCaught` is true (the class is an exception type that appears in an exception handler).
  - [P3] Unmarked program and library classes are therefore never dropped before a downstream visitor, because that filter is not implemented.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/CaughtClassMarker.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not mark visited program classes as caught exception types. `visitProgramClass` sets `ProgramClassOptimizationInfo.isCaught` to true and never clears it; `visitLibraryClass` does nothing.
  - [P3] Zel has no `isCaught` query. The Java method reads `ClassOptimizationInfo.isCaught()`, which is the stored flag on program classes and is unconditionally true on the base info used for library classes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/ClassOptimizationInfo.java`
- Bazel lines: 144
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class-attached optimization record with a one-way `hasNoSideEffects` flag (default false; `setNoSideEffects` only sets it true).
  - [P3] Missing conservative defaults: kept, instantiated, caught, and escaping are always true; simple enum, wrapper, and mergeable are always false; wrapped and target class are always null.
  - [P3] `isInstanceofed`, `isDotClassed`, and `hasSideEffects` all return `!hasNoSideEffects`; package-visible member containment and invocation are always true.
  - [P3] No attach/retrieve of this info as the class visitor info.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/CodeAttributeOptimizationInfo.java`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not attach optimization info to a method code attribute by replacing its visitor info, or read that info back by cast (null when unset; a different stored object would fail the cast).
  - [P3] Once that object is present, `isKept()` always returns true and cannot be cleared. `KeepMarker.isKept(CodeAttribute)` treats a missing object as not kept and a present one as kept. Zel has no always-kept marker for code attributes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/DotClassFilter.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that stores one delegate and, on `visitProgramClass` (lines 47–52) and `visitLibraryClass` (lines 56–61), forwards only when `DotClassMarker.isDotClassed` is true. A false result returns without calling the delegate. Both program and library classes use that same check. Nothing else in the vendored ProGuard tree constructs `DotClassFilter`.
  - [P3] Searches of `/workspace/zel-cursor` for `DotClass`, `dot_class`, `ClassVisitor`, `ClassConstant`, and `OP_LDC` found no optimizer that filters classes by `.class` or class-constant use. Zel's ProGuard surface is flags plus `crates/zel-bazel-tools/resources/tools/jdk/proguard_allowlister.py`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/DotClassMarker.java`
- Bazel lines: 92
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instruction walker that, for `ldc` and `ldc_w` only, treats a class constant as a `.class` use and sets the referenced class’s dot-classed optimization flag when that class has program optimization info. Other opcodes and non-class constants stay unmarked, and nothing exposes the flag that class merging uses to refuse merging two classes that are both referenced by `.class`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/DynamicInvocationMarker.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No instruction visitor marks a method when it sees an `invokedynamic` constant instruction.
  - [P3] No method-optimization flag or query records whether a method contains `invokedynamic`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/EscapingClassFilter.java`
- Bazel lines: 99
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that forwards `visitProgramClass` and `visitLibraryClass` to one visitor when `EscapingClassMarker.isClassEscaping` is true and to another when it is false.
  - [P3] A null chosen visitor is skipped, so the single-visitor constructor drops classes that are not marked escaping. Zel never splits or drops classes on that flag.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/EscapingClassMarker.java`
- Bazel lines: 221
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no code-attribute pass that optionally runs a reference-tracing partial evaluation and a reference-escape check, then visits only traced instructions. Non-code attributes are ignored.
  - [P3] A traced instruction that pushes exactly one value is considered when that value is escaping or returned. Returned values count even without a downcast check.
  - [P3] The stack top must be a reference that is not always null, not an array type, and has a known referenced class. That class is then walked with this class, superclasses, and interfaces included, and subclasses excluded.
  - [P3] Library classes in that walk are skipped. Program classes get a one-way escaping mark on program-class optimization info. A static query reads whether the class optimization info says the class is escaping.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/ExceptionInstructionChecker.java`
- Bazel lines: 354
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bytecode walker that reports whether a code range may throw, the offset of the first throwing instruction (or `endOffset` if none), or the offset after the last throwing instruction (or `startOffset` if none).
  - [P3] The live check is `Instruction.mayThrowExceptions()` (opcode table). The visitor that marks `idiv`/`ldiv`/`irem`/`lrem`, array loads and stores, `newarray`, `arraylength`, `athrow`, and `monitorenter`, plus field access, invokes (including `invokedynamic`), `new`, `anewarray`, `checkcast`, `instanceof`, and `multianewarray`, and the commented callee-hierarchy lookup, are not executed.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/FieldOptimizationInfo.java`
- Bazel lines: 91
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no per-field optimization record that defaults to kept, written, and read, never privatizable, and with no referenced class.
  - [P3] Zel does not store or retrieve a field value, or attach and read that record through the field's visitor info.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/FinalFieldAssignmentMarker.java`
- Bazel lines: 94
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instruction visitor that, on `putstatic` or `putfield`, resolves the field reference and records that the enclosing method assigns a final field when the resolved member has `ACC_FINAL` (unresolved members are ignored).
  - [P3] Zel has no method-optimization flag or query equivalent to `setAssignsFinalField` / `assignsFinalField`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/InstanceofClassFilter.java`
- Bazel lines: 62
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that forwards `visitProgramClass` and `visitLibraryClass` only when `InstanceofClassMarker.isInstanceofed` is true (the class is used in an `instanceof` test).
  - [P3] Unmarked program and library classes are therefore never dropped before a downstream visitor, because that filter is not implemented.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/InstanceofClassMarker.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instruction visitor that, for an `instanceof` constant instruction, resolves that instruction’s class-constant operand and marks the referenced class.
  - [P3] Marking is delegated only when the referenced class already carries `ProgramClassOptimizationInfo`; `setInstanceofed` only sets that flag true. Every other instruction is ignored.
  - [P3] There is no static `isInstanceofed` query. On program-class optimization info the flag starts false; on plain class optimization info the same query returns `!hasNoSideEffects`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/InstantiationClassFilter.java`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that forwards `visitProgramClass` and `visitLibraryClass` only when `InstantiationClassMarker.isInstantiated` is true (the class is the target of a `new` instruction).
  - [P3] Unmarked program and library classes are therefore never dropped before a downstream visitor, because that filter is not implemented.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/InstantiationClassMarker.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instruction visitor that, on `new` only, resolves the constant-pool class and marks that referenced class instantiated.
  - [P3] Marking is limited to classes whose optimization info is `ProgramClassOptimizationInfo`, and `setInstantiated` only sets that flag true.
  - [P3] There is no `isInstantiated` query that reads `ClassOptimizationInfo.isInstantiated` (true unless program optimization info says otherwise).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/MethodInvocationMarker.java`
- Bazel lines: 105
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never walks bytecode instructions to count method invocations.
  - [P3] Constant instructions do not resolve string constants or method references and increment an invocation count only on program methods that already carry optimization info.
  - [P3] There is no query for how many times a method was invoked by the instructions that were visited.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/MethodOptimizationInfo.java`
- Bazel lines: 304
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no method-attached optimization record. Side-effect, escaping-parameter, and external-return flags default false and the return value defaults null. Each setter only turns its flags on.
  - [P3] `setNoSideEffects` also sets no external side effects and no escaping parameters. `setNoExternalSideEffects` sets those two flags and leaves side effects unchanged.
  - [P3] Conservative defaults are fixed: kept, exception catching, backward branches, super and dynamic invokes, private/package/protected access, synchronized blocks, final-field assignment, every parameter used, every parameter escaped, every parameter returned, and new-instance returns are always true. Private, unused parameters, and non-empty-stack returns are always false. Invocation count is `Integer.MAX_VALUE`, parameter size is 0, used/escaped/returned bitmasks are `-1`, and parameter values are always null.
  - [P3] Escaping ignores the index and follows the no-escaping flag (bitmask `0` or `-1`). A parameter is modified only when there are side effects, and only index 0 when there are no external side effects (bitmask `0`, `1`, or `-1`). `modifiesAnything` and `returnsExternalValues` follow the external side-effect and external-return flags. `hasSideEffects` is the inverse of the no-side-effects flag.
  - [P3] Info is stored on and read from `MethodLinker.lastMember(method)` visitor info, not the method itself.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/MutableBoolean.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no mutable boolean flag that starts false, `set()` forces true, `reset()` forces false, and `isSet()` returns the current value.
  - [P3] ProGuard’s optimizer uses that flag as a shared repeat trigger so field, side-effect, and parameter-escape marking loops until the flag stays clear. Zel only names an external ProGuard tool and does not implement this convergence flag.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/NoEscapingParametersMethodMarker.java`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Nothing in zel marks a program or library method so that parameter-escape analysis treats every parameter, including `this`, as non-escaping without further analysis.
  - [P3] Nothing in zel ignores non-method members (fields) while applying that mark only to program and library methods.
  - [P3] Nothing in zel stores or reads a per-method “no escaping parameters” optimization flag.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/NoExternalReturnValuesMethodMarker.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that marks program and library methods, and ignores fields, as having no external reference return values.
  - [P3] There is no `hasNoExternalReturnValues` flag on method optimization info, no one-way setter, and no static query that reads it.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/NoExternalSideEffectMethodMarker.java`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that ignores fields and unconditionally marks both program and library methods as having no external side effects.
  - [P3] There is no method-optimization flag, setter, or static query that makes later side-effect analysis treat those methods as having no external side effects.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/NoSideEffectClassMarker.java`
- Bazel lines: 60
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that, on every class visit, marks that class's optimization info as having no side effects.
  - [P3] No static query that reads the class's no-side-effects flag from class optimization info.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/NoSideEffectMethodMarker.java`
- Bazel lines: 71
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that ignores fields and marks both program and library methods as having no side effects.
  - [P3] There is no mark that sets `hasNoSideEffects`, `hasNoExternalSideEffects`, and `hasNoEscapingParameters` together on the method's linked optimization info, and no static `hasNoSideEffects` query that reads the first of those flags.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/NonEmptyStackReturnMarker.java`
- Bazel lines: 110
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instruction visitor that marks a method when a return is unreachable or the stack before it is larger than the return width (2 for `lreturn`/`dreturn`, 1 for `ireturn`/`freturn`/`areturn`, 0 for `return`).
  - [P3] There is no `returnsWithNonEmptyStack` flag on method optimization info, and no setter or static query for it.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/NonPrivateMemberMarker.java`
- Bazel lines: 143
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel code marks members that cannot be made private: constant-pool refs to a member in another class or through another class name, string-constant member refs even in the same class, `<clinit>`, the parameterless `<init>`, and methods that may have implementations.
  - [P3] No zel query equivalent to `canBeMadePrivate` for fields or methods.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/OptimizationCodeAttributeFilter.java`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] ProGuard’s filter is an attribute visitor that ignores every non-code attribute and, on a code attribute, forwards only `visitCodeAttribute` to one of two visitors.
  - [P3] If `KeepMarker.isKept` is false (no `CodeAttributeOptimizationInfo`, or that info is not kept), it uses the optimization visitor. If the code attribute is kept, it uses the optional keep visitor. A null chosen visitor is a no-op.
  - [P3] The optimizer uses that split to run peephole and evaluation passes on optimizable code and to mark parameter usage inside kept code. Zel does not implement this filter or the keep check. It only names an external ProGuard tool.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/PackageVisibleMemberContainingClassMarker.java`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that marks a class as containing package-visible members when the class itself lacks `ACC_PUBLIC`.
  - [P3] Zel has no member visitor that, for a public class, marks it when any field or method has neither `ACC_PRIVATE` nor `ACC_PUBLIC` (package-private and protected members).
  - [P3] Zel has no program-class optimization flag write (`setContainsPackageVisibleMembers`) and no read of `containsPackageVisibleMembers` from class optimization info.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/PackageVisibleMemberInvokingClassMarker.java`
- Bazel lines: 124
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no constant visitor that, for a string, reference, or class constant whose referenced class is a different class, marks the referencing class when that class is not public, or (string and reference constants only) when the referenced member is neither public nor private.
  - [P3] Zel has no optimization-info flag or query corresponding to `invokesPackageVisibleMembers`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/ParameterEscapeMarker.java`
- Bazel lines: 929
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no analysis that marks method parameters as escaping, modified, or returned, or that marks a method as able to modify anything on the heap. Native methods are not treated as modifying, escaping, and returning every parameter.
  - [P3] There is no instruction-level propagation: array stores, `areturn` (including new instances and external values when the value may match the return type), `athrow`, field access, static initializers of referenced classes, and invokes (unknown callees, `invokedynamic`, and known callees whose parameters escape or are modified, including when a returned parameter later escapes or is modified).
  - [P3] Marks are not limited to program methods, do not ignore null references and primitive parameters, and do not set a repeat trigger when a newly set bit changes the optimization info.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/ParameterEscapedMarker.java`
- Bazel lines: 306
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no fixed-point walk of every method that partial-evaluates the code with reference tracing, then marks reference parameters that escape through `invokevirtual`, `invokespecial`, `invokestatic`, or `invokeinterface`.
  - [P3] A callee parameter is marked only when its `ProgramMethodOptimizationInfo` exists and the matching stack value is a reference that is not always null, and any producer is an already-escaped parameter of the caller or an instruction `ReferenceEscapeChecker` reports as instance-escaping. The deepest popped slot is parameter 0 (the receiver on instance calls). Marks are sticky and the pool is reprocessed until a pass adds none. Static queries return the per-parameter bit and the bitmask. Zel only names an external ProGuard tool and does not run this analysis.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/ParameterUsageMarker.java`
- Bazel lines: 308
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no visitor that records a method's parameter-slot count (descriptor size plus `this` for instance methods) on method optimization info.
  - [P3] Nothing optionally marks `this` and/or every other parameter as used, and nothing always marks all parameters of native methods or `this` on abstract methods.
  - [P3] Concrete instance methods that are synchronized, may have other implementations, or are `<init>` do not get `this` marked used, and library methods that may have implementations do not get every parameter including `this` marked used.
  - [P3] There is no code analysis that partially evaluates a method and marks parameters loaded by traced variable instructions when the producer value is a method parameter, including the extra slot for category-2 types.
  - [P3] There are no queries for parameter size, the used-parameter mask, whether any parameter is unused, or whether a given variable index is used.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/ProgramClassOptimizationInfo.java`
- Bazel lines: 222
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not attach a per-program-class optimization record (via visitor info) whose `isKept()` is always false.
  - [P3] Missing monotonic flags, defaulting false: instantiated, `instanceof`, `.class`, caught, escaping, contains package-visible members, and invokes package-visible members. `isSimpleEnum` is a writable boolean. `mayBeMerged` starts true and can only be cleared. `wrappedClass` and `targetClass` are independently replaceable class references.
  - [P3] Missing `hasSideEffects()`, which is true only when side effects were recorded and the inherited no-side-effects mark is still false. `setSideEffects()` does not clear that mark.
  - [P3] Missing `merge()`, which ORs those flags through the other info object's getters and does not change may-merge, wrapped class, or target class.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/ProgramClassOptimizationInfoSetter.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that, on each program class, installs a new program-class optimization-info object when visitor info is null or when overwrite is set.
  - [P3] The default constructor (overwrite false, so existing visitor info is left in place) and the explicit overwrite constructor are absent.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/ProgramFieldOptimizationInfo.java`
- Bazel lines: 184
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no program-field optimization record stored as the field's visitor info. `isKept()` is always false. Read and write both start true only when `ACC_VOLATILE` is set, and otherwise start false. `canBeMadePrivate` starts true and can only be cleared. A referenced class starts unset and is replaced or generalized under synchronization.
  - [P3] Value reset clears the stored value. Static fields then take a `ConstantValue` attribute through `ConstantValueFactory`; other attributes are ignored, and non-static fields skip attributes. If the value is still unset and either the `optimize.conservatively` system property is set or the field lacks `ACC_FINAL`, the value becomes the descriptor's default initial value. A final field with no constant stays unset outside that mode. `generalizeValue` stores an unset value or generalizes an existing one, under synchronization.
  - [P3] The copy constructor copies the value, written, read, privatizable, and referenced-class fields. The static getter casts the field's visitor info to this program-field type.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/ProgramMemberOptimizationInfoSetter.java`
- Bazel lines: 83
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not attach `ProgramFieldOptimizationInfo` or `ProgramMethodOptimizationInfo` to program members. ProGuard appears only as an external bytecode-optimizer label.
  - [P3] The default visitor leaves existing visitor info in place. Constructing it with overwrite replaces that info.
  - [P3] A program field is updated only when its own visitor info is null or overwrite is set. The new info marks volatile fields as read and written, then resets the value from the field, including a constant-value attribute.
  - [P3] A program method is updated only when `MethodLinker.lastMember` has null visitor info or overwrite is set. The new info is stored on that linked representative, with parameter slots sized from the descriptor (including `this` for instance methods).
  - [P3] A library field or method visit is not handled and throws `UnsupportedOperationException` through `SimplifiedVisitor.visitAnyMember`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/ProgramMethodOptimizationInfo.java`
- Bazel lines: 586
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not attach per-program-method optimization info. ProGuard stores it on `MethodLinker.lastMember` visitor info, sizes a parameter `Value` array from the descriptor (including `this` for instance methods, sharing an empty array when the count is 0), and `isKept()` is always false.
  - [P3] Missing sticky flags and counters: side effects (reported only when the inherited no-side-effects flag is clear), can-be-made-private (starts true), catches exceptions, backward branches, super and dynamic invokes, private/package/protected access, synchronized blocks, final-field assigns, non-empty-stack returns, invocation count from 0, modifies-anything (suppressed by no-external-side-effects), returns new instances, and returns external values (suppressed by no-external-return-values).
  - [P3] Missing parameter bitsets (used, escaped, escaping, modified, returned) with synchronized set, OR-merge, insert, and remove. Indices at or above 64 are treated as set and are not written. Unused-parameter detection requires every bit below `parameterSize` (or all 64 bits when `parameterSize` is at least 64). Insert and remove shift escaped, escaping, modified, and returned bits and the value array, and leave used-parameter bits and `parameterSize` unchanged.
  - [P3] Missing the modified-parameter rule: false when there are no side effects; only index 0 when there are no external side effects; escaped bits count as modified when `modifiesAnything` is set. `getModifiedParameters` does not OR in escaped bits. Escaping queries return none when the inherited no-escaping-parameters flag is set.
  - [P3] Missing parameter and return value generalization, and `merge`, which ORs exception, backward-branch, super/dynamic invoke, private/package/protected access, synchronized-block, and final-field flags from the other info. Zel only names an external ProGuard tool and does not implement this store.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/ReadWriteFieldMarker.java`
- Bazel lines: 223
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not mark program fields as read or written from `getfield`/`getstatic` (read), `putfield`/`putstatic` (written), or `ldc`/`ldc_w` string constants that reference a field (both read and written).
  - [P3] A newly applied read or written mark is stored only on `ProgramFieldOptimizationInfo` and sets a shared repeat trigger. Library fields and fields that already have the mark are left unchanged, and Zel has no field-optimization info or that trigger.
  - [P3] Constructor flags can disable read marks, write marks, or both. Visiting a field directly, rather than through an instruction, marks it both read and written. Zel implements neither path.
  - [P3] Zel only names an external ProGuard tool (`bytecode_optimizers` defaults to `Proguard`) and does not run this field read/write analysis.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/ReferenceEscapeChecker.java`
- Bazel lines: 465
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] After an optional partial evaluation, each traced instruction offset records whether the reference created or loaded there escapes the method, is returned, is modified, or is external to the method and its callees.
  - [P3] `aastore` marks the array modified and the stored reference escaping. Primitive `*astore` marks only the array modified. `areturn` marks the value returned, `athrow` marks it escaping, `putstatic` marks the stored reference escaping, and `putfield` marks the object modified and the stored reference escaping. `getstatic` and `getfield` mark that load itself external.
  - [P3] Each reference argument of an invoke is escaping or modified when the callee is unresolved or `ParameterEscapeMarker` says that parameter escapes or is modified. The invoke result is external when the callee is unresolved, or when it returns an external class or array. Producer traces skip method parameters and always-null references.
  - [P3] Zel only names ProGuard as an external optimizer and does not run this reference escape, return, modify, or external analysis.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/RepeatedClassPoolVisitor.java`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No class-pool visitor that always runs at least once, clears a shared mutable repeat flag, delegates one full pool visit, and repeats while that flag is set afterward.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/SideEffectClassChecker.java`
- Bazel lines: 83
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no checker for class-initialization side effects. ProGuard reports that accessing a member may initialize the referenced class only when that member is static or is an instance initializer (`<init>`), and then only when the referenced class is not marked as having no side effects, the referencing class does not extend or implement the referenced class, and the referencing class's set of side-effect superclasses and interfaces does not contain every side-effect superclass and interface of the referenced class (those sets are the hierarchy entries, including the class itself, that have initialization side effects).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/SideEffectClassFilter.java`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Forwards `visitProgramClass` and `visitLibraryClass` only when `SideEffectClassMarker.hasSideEffects` is true, and otherwise drops the class. Callers use that gate to collect superclasses and interfaces whose initialization has side effects, and to limit class merging.
  - [P3] Zel has no class-visitor filter and no initialization side-effect marker. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/SideEffectClassMarker.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Visiting a program class sets that class’s optimization side-effect flag, and `hasSideEffects` reads the same flag back. Library classes are not marked. Zel has no class-optimization side-effect marker or query.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/SideEffectInstructionChecker.java`
- Bazel lines: 337
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no per-instruction side-effect check. It only names an external ProGuard optimizer (`bytecode_optimizers` default `Proguard`, `proguard_top`) and does not classify JVM opcodes.
  - [P3] Returns, `ret`, `jsr`, and `jsr_w` count only when return instructions are included. The eight `*astore` opcodes count only when array stores are included. `athrow`, `monitorenter`, and `monitorexit` always count. Each query clears the flag and visits one instruction.
  - [P3] `idiv`, `ldiv`, `irem`, `lrem`, `fdiv`, `frem`, `ddiv`, `drem`, the `*aload` opcodes, `newarray`, `arraylength`, `anewarray`, `multianewarray`, and `checkcast` count only when the `optimize.conservatively` system property is set. That same mode treats `getfield`, `putfield`, `invokevirtual`, `invokeinterface`, and `invokedynamic` as side effects without inspecting the member. `getstatic`, `putstatic`, `invokespecial`, and `invokestatic` still inspect the member.
  - [P3] Unknown fields and methods count as side effects. A known program field counts when a write targets a field that is read, the field is volatile, or the class checker says the access may have side effects. A known program method counts when the method marker (including synchronization) or the class checker says so. Library fields always count. Library methods count unless marked as having no side effects. `invokedynamic` always counts.
  - [P3] Other instructions, including local loads and stores, most arithmetic, `new`, `instanceof`, and branches other than `jsr` and `jsr_w`, do not count.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/SideEffectMethodFilter.java`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no member visitor that ignores field visits and forwards program and library method visits only when the method is marked as having side effects.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/SideEffectMethodMarker.java`
- Bazel lines: 120
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not mark program methods as having side effects when they are native or synchronized.
  - [P3] Zel does not visit method instructions and mark the enclosing method when an instruction checker reports side effects.
  - [P3] Zel does not record that mark only on a program-method optimization record that did not already have side effects, set a repeat trigger when the mark changes, or mark the class when the method is `<clinit>`.
  - [P3] Zel has no query that reads a method’s side-effect flag from method optimization info.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/SimpleEnumFilter.java`
- Bazel lines: 95
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] ProGuard routes `visitProgramClass` to the simple-enum visitor when `SimpleEnumMarker.isSimpleEnum` is true and to the other visitor otherwise, and skips the call when the chosen visitor is null. A one-argument constructor leaves the other visitor null, so unmarked program classes are dropped.
  - [P3] Library classes are never treated as simple enums: `visitLibraryClass` goes only to the other visitor, and only when that visitor is non-null. Zel does not implement this class-visitor split. It only names an external ProGuard tool and related flags.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/SimpleEnumMarker.java`
- Bazel lines: 69
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A marker stores the constructor boolean and writes it with `setSimpleEnum` only from `visitProgramClass`. `visitLibraryClass` does nothing, so library classes are never marked.
  - [P3] `isSimpleEnum` reads class optimization info. Library info always returns false. Program info returns the stored flag, which stays false until a visit sets it.
  - [P3] Zel does not implement this marker or the simple-enum optimization flag. It only names an external ProGuard tool and related flags.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/SuperInvocationMarker.java`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On `invokespecial`, the enclosing method is marked when the methodref’s referenced class is not the current class and the name is not `<init>`. Other instructions do nothing. The mark is sticky on program-method optimization info and is read back through method optimization info.
  - [P3] Zel has no instruction visitor or optimization-info flag for super invocations. It only names an external ProGuard tool and related flags.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/SynchronizedBlockMethodMarker.java`
- Bazel lines: 69
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No instruction visitor that, on `monitorenter` or `monitorexit` only, sets the method's program optimization info synchronized-block flag, or a query that reads that flag (the library-method default reports a synchronized block as present).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/UnusedParameterMethodFilter.java`
- Bazel lines: 64
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No member visitor ignores program fields, library fields, and library methods, then forwards only program methods that `ParameterUsageMarker.hasUnusedParameters` marks as having unused parameters.
  - [P3] Optimizer passes that run only on those methods (parameter removal and related updates) are not reimplemented. Zel only names ProGuard as an external optimizer and keeps ProGuard config/flags.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/UnusedParameterOptimizationInfoUpdater.java`
- Bazel lines: 140
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On a method `CodeAttribute`, Zel does not walk parameters (including `this`) and drop those `ParameterUsageMarker` marks unused.
  - [P3] Each dropped parameter does not shift `ProgramMethodOptimizationInfo` (escaped, escaping, modified, returned, and the parameter array) by the count already removed, or add `ClassUtil.internalTypeSize` to the removed slot total.
  - [P3] The optional extra member visitor is not invoked once per removed parameter.
  - [P3] After the walk, the stored parameter size is not reduced by that slot total, and `updateUsedParameters(-1L)` does not mark every remaining slot used.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/UsedParameterFilter.java`
- Bazel lines: 84
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No `ParameterVisitor` routes `visitParameter` to a used or unused visitor from `ParameterUsageMarker.isParameterUsed((Method) member, parameterOffset)`, skipping the call when that visitor is null (the one-argument constructor leaves the unused visitor null).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/VariableUsageMarker.java`
- Bazel lines: 97
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No code-attribute visitor resets a per-method used-slot array to `u2maxLocals` and marks each variable-instruction index, plus the next slot when that instruction pops or pushes a category-2 value, with a query for whether a given index was marked.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/info/WrapperClassMarker.java`
- Bazel lines: 225
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not mark a program class as wrapping one class. ProGuard does so only when `subClasses` is null or empty, then stores that wrapped class on `ProgramClassOptimizationInfo`. `getWrappedClass` reads it back from `ClassOptimizationInfo`.
  - [P3] The class must have exactly one non-static field whose descriptor ends with `;` and is not an array. That field's non-null `referencedClass` is the wrapped class. A static field, a primitive, an array, an unresolved class type, or a second object field sets the counter to `Integer.MIN_VALUE` and rejects the class.
  - [P3] It then requires exactly one `<init>` or `<clinit>` whose descriptor has one argument, not counting `this`. Parameter slot 1 must already be a reference value that is always non-null. Any other initializer poisons the counter. Ordinary methods are ignored.
  - [P3] That initializer's code must be exactly 10 bytes and match `aload_0`, `aload_1`, `putfield` (any name and type), `aload_0`, `invokespecial` (any constant), `return`, and the `putfield` owner must be the initializer's class. Any other code attribute rejects the class. Non-code attributes are ignored.
  - [P3] Zel only names an external ProGuard tool (`bytecode_optimizers` defaults to `Proguard`) and does not run this wrapper-class analysis.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/BranchTargetFinder.java`
- Bazel lines: 723
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No code-attribute walk that repeatedly marks instruction offsets, `new` creations, `invokespecial` of `<init>`, branch origins and targets, and the instruction after an unconditional transfer (`return`, `athrow`, `ret`, `goto`/`goto_w`, or a switch) until subroutine marks stabilize.
  - [P3] No jsr/jsr_w or `ret` subroutine tracking: invocation and returning flags, first-mark-wins membership, backward-branch start shrinking, exception-handler inheritance from the try start, or per-subroutine end offsets copied back onto every member. Offset 0 is always a target, and the code-length offset is a branch-target sentinel.
  - [P3] Zel does not implement these queries. It only names an external ProGuard tool and related flags.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/ClassFinalizer.java`
- Bazel lines: 84
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On `visitProgramClass`, sets `ACC_FINAL` when the class is not already final, an interface, or abstract, `KeepMarker.isKept` is false, and `subClasses` is null. If an extra `ClassVisitor` was supplied, it then visits that now-final class. Library classes are never visited.
  - [P3] Zel has no peephole pass that finalizes unreferenced program classes. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/ClassMerger.java`
- Bazel lines: 820
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] ProGuard merges a visited program class into a fixed target only when the classes differ, neither is kept or already retargeted, the source is not an annotation, versions match, access is allowed (access modification, both public without package-visible members or invokes, or the same package), interface/abstract flags match or the source is the target's only subclass whose superclass is the target or the target's superclass, neither implements the other indirectly, interface pairs have the same other subinterfaces, side-effect, `instanceof`, and caught-exception superclass sets match, both are not `.class` references, fields do not clash, become unwanted, or shadow unless wrapper merging is on, non-abstract methods do not clash, abstract methods are not introduced unless aggressive interface merging is on, concrete methods are not overridden, non-private methods are not shadowed, neither has a `Signature` attribute, and non-synthetic sources have no `InnerClasses` or `EnclosingMethod` attribute unless wrapper merging is on.
  - [P3] A successful merge ANDs `interface`/`abstract` and ORs `public`/`super`/`annotation`/`enum` onto the target, copies interfaces that would not loop the hierarchy, copies members (wrapper mode keeps only static fields and drops `<init>`), copies attributes other than `BootstrapMethods`, `SourceFile`, `InnerClasses`, and `EnclosingMethod`, merges class optimization info, clones field optimization info while sharing method optimization info, records the target, and optionally revisits the source. Library classes are ignored. Unexpected failures are logged with both class names and rethrown.
  - [P3] Zel has no class-merger visitor or retarget chain. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/GotoCommonCodeReplacer.java`
- Bazel lines: 263
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no code-attribute pass that, for an unconditional `goto` or `goto_w` that is not itself a branch target, walks backward over identical bytes before the goto and its target, stops on mismatched bytes, instruction-boundary mismatch, overlap with the goto, a branch origin or target, an exception edge, an initializer, or `pop`/`pop2`/`arraylength` at the source side (exception edges also at the target), and records a length only when the target-side offset is already a branch target. When that length is positive and no exception start or end lies between the two offsets inclusive, it deletes that preceding instruction range plus the goto, and replaces the goto only when the shortened branch still skips code. An optional extra visitor then sees the original branch.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/GotoGotoReplacer.java`
- Bazel lines: 115
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] ProGuard rewrites an unmodified `goto` or `goto_w` whose target is an unmodified plain `goto` (not `goto_w`), skipping a zero offset and an offset equal to the instruction length, by replacing it with the same opcode and offset equal to the sum of the two branch offsets. An optional extra visitor then sees the original branch. Zel has no bytecode peephole rewriter for this. It only names an external ProGuard tool and related flags.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/GotoReturnReplacer.java`
- Bazel lines: 115
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bytecode peephole that replaces an unmodified `goto` or `goto_w` with a copy of the return it targets (`ireturn`, `lreturn`, `freturn`, `dreturn`, `areturn`, or `return`) when that target offset is also unmodified, then optionally notifies an extra visitor with the original branch.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/HorizontalClassMerger.java`
- Bazel lines: 91
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On each program class, resolves its superclass and visits that superclass's direct subclasses, offering each one to `ClassMerger` with the visited class as the merge target. `allowAccessModification`, `mergeInterfacesAggressively`, and the optional extra visitor are forwarded; wrapper-class merging is fixed off.
  - [P3] Zel has no sibling-class inlining pass. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/InstructionSequenceConstants.java`
- Bazel lines: 4772
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No peephole pattern/replacement catalog, built from the program and library class pools, that rewrites Java bytecode: local load/store/dup/pop deletion, arithmetic identities (including multiply-to-shift and compound `iinc`), redundant field get/put, cast folding, branch and constant-if folding plus duplicate `lookupswitch` cases, `String`/`StringBuffer`/`StringBuilder` and `valueOf` folding, `equals`/`toString` and wrapper `*Value` simplifications, `java.lang.Math` call narrowing, and Android `android.util.FloatMath` calls rewritten to `Math`. Wildcard field and method constants (`equals`, `toString`, primitive `*Value`) and matched-string replacement constants are part of that catalog.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/InstructionSequenceReplacer.java`
- Bazel lines: 875
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] ProGuard walks each instruction, resets the sequence matcher when the offset is a branch target or already edited, and replaces the match only when the matcher hits and every matched offset is still unmodified. A shorter or equal replacement overwrites the matched instructions and deletes the tail; a longer one overwrites the match and inserts the extra instructions after the last matched offset. An optional extra visitor then sees only the instruction that completed the match.
  - [P3] Replacement opcodes keep their kind and fill wildcards from the match: simple constants, variable indices, constant-pool indices, branch offsets, and `tableswitch`/`lookupswitch` cases and jump offsets. Constant indices that did not match are copied from the pattern pool. Special indices build a new string constant: `STRING_A_LENGTH` is the length of matched string A, and the boolean/char/int/long/float/double/string A+B markers concatenate `Boolean`/`Character`/`Integer`/`Long`/`Float`/`Double` text or the matched string (empty text when the type nibble is unrecognized). Integer-typed matched arguments are stringified with the corresponding `toString`.
  - [P3] Local `label()` and `catch_()` pseudo-instructions are not bytecode. Their offsets are `0x20000000` plus an incrementing id. On apply they become editor labels and exception handlers whose ids are made unique with the first matched offset shifted left by 8, and those unique ids are written back into the replacement jump-offset arrays. Zel has no bytecode sequence matcher or replacer. It only records ProGuard as an external optimizer (`bytecode_optimizers` default, `proguard_top`, and allowlisting of ProGuard spec files).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/InstructionSequencesReplacer.java`
- Bazel lines: 155
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No composite instruction visitor that builds one sequence replacer per pair, uses pair index 0 as the pattern and pair index 1 as the replacement only when the pair is longer than one sequence (otherwise a shared empty sequence that deletes the match), passes the same constant array to both sides, and shares one branch-target finder, code editor, and optional deleted-load visitor while forwarding every instruction visit to each replacer in order.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/LineNumberLinearizer.java`
- Bazel lines: 250
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no class visitor that, when any line-number entry has a non-null source, walks every method line-number table and shifts those inlined or merged numbers into non-overlapping ranges.
  - [P3] On a source change that is not the inlined-method end marker, ProGuard parses start and end lines from the source (`…:start:end`), chooses a shift past the highest used line (multiples of 1000 while that high-water mark is at most 50000, otherwise the exact next line), updates the high-water mark to `end + shift`, and adds the shift to the entry.
  - [P3] On the end marker it pops the enclosing block, rewrites that entry’s line and source to the enclosing line and source, and restores the enclosing shift. An empty stack leaves the marker unchanged.
  - [P3] Later entries with the same source only receive the current shift. Entries with a null source are not rewritten, but they still become the previous entry used to detect the next block boundary.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/MemberPrivatizer.java`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no peephole visitor that, for program fields and methods still allowed to become private, replaces the access flags with `ACC_PRIVATE` (clearing public, protected, and package-private, and also clearing `ACC_FINAL`) and then forwards each privatized member to an optional extra visitor.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/MethodFinalizer.java`
- Bazel lines: 92
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On `visitProgramMethod`, sets `ACC_FINAL` when the method is not private, static, final, or abstract and is not `<init>`, and either its class is final or the method is not kept and has no non-private override (same name and descriptor) in the subclass hierarchy (`subClasses == null` counts as not overridden). A final class is finalized even if the method is kept or overridden. After the flag is set, an optional extra member visitor is invoked. Library methods are not visited.
  - [P3] Zel has no peephole method finalizer, keep marker, or override search. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/MethodInliner.java`
- Bazel lines: 812
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bytecode method inliner. ProGuard copies each method body and replaces `invokevirtual`, `invokespecial`, `invokestatic`, and `invokeinterface` with the callee when every gate passes: the callee is a program method, not kept, private or static or final, and not synchronized, native, abstract, or `<init>`; it is not the caller and is not already on the inline stack; the caller class version is at least the callee's; it does not invoke super or invokedynamic unless it is the same class; it does not branch backward while an uninitialized object is live (`new` increments that count, a constructor starts at 1, and a rejected `<init>` decrements it); without access modification, private access stays in the same class and package access stays in the same package; private access is never inlined from a superclass into a subclass, and protected access only into the same class; a synchronized block is inlined only when the call is covered by a catch-all (`catch` type 0) or by no handler; it does not assign a final field, does not return with a non-empty stack, catches exceptions only when this is the outermost inline and the stack after a reachable invoke is empty, and its related static initializers have no side effects. The body is inlined only for a single recorded invocation, or otherwise only when the code is at most 32 bytes on Android or 8 on the JVM, and the estimated result stays under 2000 bytes for Java ME or 7000 for Java SE (both caps come from the shared properties `maximum.inlined.code.length` and `maximum.resulting.code.length`). Parameters are stored into fresh locals (`this` at the caller's max-locals offset; longs and doubles take two slots), returns become `goto_w` to the end unless the return is the last instruction (then it is dropped and a label is left), variable indexes shift by the caller's max locals, constants and exception handlers are copied into the caller, and line numbers are copied with source `Class.method+descriptor:low:high`, a dummy line 0 when the callee has none, and an end marker of line -1. Invalid line numbers are dropped, and a runtime failure is logged and skips that inline. After any inline, the code is rewritten, super and access markers are refreshed, optimization info is merged into a non-kept caller, and invocation counts of methods reached from the inlined body are incremented again. Library members are not inlined.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/NopRemover.java`
- Bazel lines: 89
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Deletes a `nop` (`OP_NOP`) through `CodeAttributeEditor.deleteInstruction` only when that offset is not already modified, then optionally forwards the removed instruction to `extraInstructionVisitor`. Every other instruction is left unchanged.
  - [P3] Zel has no bytecode peephole pass and no nop-removal visitor. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/PeepholeOptimizer.java`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On a code attribute, ProGuard optionally fills a branch-target finder, resets the code editor to the attribute's code length, runs the instruction visitor, then applies the editor's edits. Non-code attributes are ignored. A two-argument constructor leaves the finder unset, so that setup step is skipped.
  - [P3] Zel does not run this peephole setup-and-apply pass. It only names ProGuard as an external bytecode optimizer (`bytecode_optimizers` defaulting to ProGuard, plus `proguard_top` and related shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/ReachableCodeMarker.java`
- Bazel lines: 262
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no JVM code-attribute walk that marks instruction offsets reachable from the method entry, including conditional and unconditional branch targets and every switch case plus the default.
  - [P3] Zel does not stop fall-through at `ireturn`, `lreturn`, `freturn`, `dreturn`, `areturn`, `return`, `athrow`, `ret`, `goto`, `goto_w`, or switches, while still marking those instructions' targets.
  - [P3] Zel has no fixed-point pass that marks an exception handler only when its range contains a reachable instruction and the handler itself is not yet reachable, then repeats until no new handler is marked.
  - [P3] Zel has no query of whether one offset, or any offset in a half-open range, was reachable in the code attribute visited most recently.

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/RetargetedClassFilter.java`
- Bazel lines: 96
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On a program class, follows `ClassMerger.getTargetClass` to the last non-null merge target and, if one exists, visits `retargetedClassVisitor`; otherwise visits `otherClassVisitor`. A null chosen visitor is a no-op.
  - [P3] A library class is never treated as retargeted and is visited only through `otherClassVisitor` when that visitor is non-null.
  - [P3] Zel has no class-merge retarget marker and no visitor that splits classes on that mark. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/RetargetedInnerClassAttributeRemover.java`
- Bazel lines: 169
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On each program class, drops an `InnerClasses` attribute when `ClassMerger.getTargetClass` is set for that class. Otherwise it keeps only entries whose outer and inner class constants both reference a non-null class with no merge target, nulls the removed tail, and drops the attribute when no entries remain.
  - [P3] Drops an `EnclosingMethod` attribute when the class itself or `referencedClass` has a merge target. A null referenced class is treated as not retargeted. Every other attribute is kept, and the attribute array is compacted the same way.
  - [P3] Zel has no class-merge retargeting and no pass that strips `InnerClasses` or `EnclosingMethod` attributes. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/TargetClassChanger.java`
- Bazel lines: 507
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On a program class, retargets constant-pool, field, method, and attribute references through `ClassMerger.getTargetClass` (null stays null). Drops interfaces that are the class itself or that duplicate an earlier interface. If the class itself has a merge target, replaces `thisClass` with a newly added `ClassConstant` (fresh Utf8, not a reused pool entry) that still names the class, and clears `subClasses`. Otherwise registers the class as a subclass of its current superclass and interfaces.
  - [P3] Library classes only retarget field and method referenced classes. String constants that point at a class, and field/method/interface-method refs, also retarget the member with `findField` or `findMethod` when the class changes (string and annotation element values pass a null descriptor). InvokeDynamic, MethodType, class constants, signatures, local variables, and annotation element values update referenced classes; nested annotations and array element values are walked. Other constants and attributes are left unchanged.
  - [P3] Zel has no bytecode class-retargeting pass. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/UnreachableCodeRemover.java`
- Bazel lines: 143
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] After `ReachableCodeMarker` marks reachable offsets, every unreachable instruction is deleted through `CodeAttributeEditor.deleteInstruction` and the edits are written back onto the code attribute. An optional `extraInstructionVisitor` is invoked on each deleted instruction. Non-code attributes are ignored. A `RuntimeException` during the pass is printed with the class and method, then rethrown.
  - [P3] Zel has no unreachable-code removal pass. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/UnreachableExceptionRemover.java`
- Bazel lines: 156
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On a code attribute only, each exception handler whose range `[startPC, endPC)` contains no instruction with `Instruction.mayThrowExceptions()` (opcode bit `MAY_THROW_EXCEPTIONS`) is emptied by setting `endPC = startPC`. A non-null `extraExceptionInfoVisitor` then sees that already-empty handler.
  - [P3] The table is then compacted in place to entries with `startPC < endPC` (already-empty handlers are dropped too) and `u2exceptionTableLength` is updated. Other attributes are ignored.
  - [P3] Zel has no bytecode peephole pass and no exception-table rewriter. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/VariableShrinker.java`
- Bazel lines: 133
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no peephole pass that drops unused non-parameter locals from a non-abstract method's local frame and renumbers the remaining slots. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/VerticalClassMerger.java`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On each program class, visits that class's immediate subclasses and offers each to `ClassMerger` with the visited class as the merge target. `allowAccessModification`, `mergeInterfacesAggressively`, and the optional extra visitor are forwarded; wrapper-class merging is fixed off.
  - [P3] Zel has no subclass-inlining pass. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/WildcardConstantFilter.java`
- Bazel lines: 309
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Forwards integer, long, float, double, primitive-array, Utf8, and method-handle constants to the delegate with no wildcard check.
  - [P3] Skips the delegate for string, invokedynamic, field, method, and interface-method refs, class, method-type, name-and-type, module, and package constants when a referenced pool index is `>= InstructionSequenceMatcher.X` (`0x40000000`). String checks `u2stringIndex`. Refs check the class and name-and-type indices, then those constants. Invokedynamic checks the name-and-type index and recurses. Class, module, and package check the name index. Method-type checks the descriptor index. Name-and-type checks the name and descriptor indices. Utf8 entries are not walked.
  - [P3] Dynamic constants call that same check, but the checker does not override `visitDynamicConstant`, so `SimplifiedVisitor.visitAnyConstant` leaves the flag false and every dynamic constant is still forwarded. Bootstrap-method indices are never tested.
  - [P3] Zel has no bytecode peephole constant visitor that suppresses wildcard pool indices. ProGuard appears only as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/WrapperClassMerger.java`
- Bazel lines: 98
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On each program class whose superclass is `java.lang.Object`, looks up the wrapped class and, when that is a program class, inlines the wrapper into it through `ClassMerger`. Access modification and the optional extra visitor are forwarded. Aggressive interface merging is off and wrapper-class merging is on.
  - [P3] Zel has no wrapper-class inlining pass. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/optimize/peephole/WrapperClassUseSimplifier.java`
- Bazel lines: 275
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Rewrites code that uses a program class already retargeted by `ClassMerger`: `new Wrapper` plus `dup` deletes both; `new` plus `astore` becomes `aconst_null` and the `<init>` `invokespecial` becomes that `astore` plus `pop`; any other follower becomes `aconst_null` and the `<init>` becomes `pop` plus `pop`; with no remembered pop the `<init>` is deleted; wrapper `getfield` is deleted. Only `<init>` method refs, field refs, and class constants count, and library classes are ignored. An optional extra visitor is notified only for the simplified `new`.
  - [P3] Zel has no bytecode peephole pass and no wrapper-use simplifier. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/preverify/CodePreverifier.java`
- Bazel lines: 665
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On each code attribute, runs partial evaluation, initialization analysis, and liveness analysis, then deletes every untraced instruction. Unexpected runtime failures are printed with the class and method and rethrown.
  - [P3] At each traced branch or exception target, records a full frame of verification types for live locals and the stack, storing category-2 values as one slot. Uninitialized `this` stays live until the super `<init>` call. A single `new` producer that is not yet initialized becomes an uninitialized type. Null references become Null. Other references become object types, adding a class constant to the pool. Constructors force local 0 of the method-entry frame to UninitializedThis. A null local at method entry is a dummy UninitializedThis so it does not match offset 0. Dead locals become Top.
  - [P3] When the target is not Java Micro Edition, compresses those frames against the previous locals into SameZero, SameOne, LessZero (1–3 chopped), MoreZero (1–3 appended), or Full, and stores offset deltas. Writes `StackMap` (Micro Edition, full frames only) or `StackMapTable`, or deletes that attribute when there are no frames.
  - [P3] Zel has no bytecode preverifier. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/preverify/CodeSubroutineInliner.java`
- Bazel lines: 402
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no jsr/ret subroutine inliner. ProGuard copies non-subroutine code, recursively inlines returning `jsr`/`jsr_w` bodies, turns non-returning calls and mid-subroutine `ret` into `goto`, replaces subroutine starts and trailing `ret` with labels, and clips or splits exception ranges around those calls.
  - [P3] Zel only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/preverify/Preverifier.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Clears visitor info on every class in the program pool, then preverifies every method's code attributes. The version gate is major version 1.0 when `microEdition` is set, and 1.6 otherwise. `CodePreverifier` receives that same `microEdition` flag.
  - [P3] Zel has no class-pool preverification pass, no version gate, and no code preverifier. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/preverify/SubroutineInliner.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No pass clears visitor info on every class in the program pool, then visits every method attribute so `jsr`/`ret` subroutines in code attributes are inlined.
  - [P3] That cleanup is not version-gated. On Java SE (neither `microEdition` nor `android`), inlining runs only for program classes whose `u4version` is from Java 6 (`(50 << 16) | 0`) through `Integer.MAX_VALUE`. With either flag set, every program class is inlined, including pre-Java 6.
  - [P3] Library classes are never visited.

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/AnnotationUsageMarker.java`
- Bazel lines: 327
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Recursively marks runtime annotation attributes, parameter annotations, and element values (constants, enums, classes, nested annotations, and arrays) as used only when the annotation type and the element method are already used. Program classes and methods must already be marked; library classes and methods count as used. Annotation default attributes are always kept. Class-typed elements also mark their referenced classes through the parent usage marker. A class constant is marked only when its referenced class is used, and only if that constant is not already marked.
  - [P3] Zel has no class-file shrinker and no annotation usage marker. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/ClassShrinker.java`
- Bazel lines: 603
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On a program class, drops unused interfaces, nest-member indices, constant-pool entries (1-based, with the previous used-flag kept across a null long/double second slot), fields, methods, and attributes. A smaller field or method count sets `ACC_REMOVED_FIELDS` or `ACC_REMOVED_METHODS`. Remaining pool references and used bootstrap-method indices are remapped. Unused entries are also removed from inner classes, code attributes, local-variable tables, annotations (including parameter, nested, and array values), and the subclass list (replaced by a shorter array, or null when empty). An enclosing method whose referenced method is unused has its name-and-type index and reference cleared. Signature Utf8 constants replace unused referenced classes with `java.lang.Object` and shorten inner-class names after `.`.
  - [P3] A library class is otherwise left unchanged. Only its subclass list is compacted the same way.
  - [P3] Zel has no class-file shrinker. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/InnerUsageMarker.java`
- Bazel lines: 174
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On an `InnerClasses` attribute, marks an entry used only when both its inner and outer class constants count as used (a zero index, an unresolved class, or a library class counts as used; a program class counts only if `UsageMarker` already marked it). A newly used entry is marked along with its inner-name `Utf8`; if any entry is used, the attribute and its name `Utf8` are marked too. Other attributes are ignored, and an entry already marked used is not rechecked.
  - [P3] Zel has no ProGuard shrink usage marker and no inner-class attribute retention pass. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/InterfaceUsageMarker.java`
- Bazel lines: 152
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no shrink visitor that, for a used or possibly-used program class, walks its interface class constants and treats the class as used when any of those interfaces is used.
  - [P3] A possibly-used interface is not promoted to used when an interface is used, and that promotion does not also mark the interface's this-class constant and its superclass constant. Otherwise it is not cleared as unused so it is not reconsidered.
  - [P3] A library class is not treated as unconditionally used for this walk.
  - [P3] An unmarked class constant is not resolved through its referenced class and then marked together with its UTF-8 name when that class is used.

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/LocalVariableTypeUsageMarker.java`
- Bazel lines: 177
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no shrink visitor that marks a `LocalVariableTable` or `LocalVariableTypeTable`, plus its attribute-name constant, only when at least one entry stays, and that keeps an entry and its name and descriptor or signature constants only when every referenced program class is already used (library classes are ignored; any unused program class drops that entry).

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/NestUsageMarker.java`
- Bazel lines: 160
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no shrink visitor that marks a `NestHost` attribute used only when its host class constant is used, and that then also marks the attribute-name constant.
  - [P3] Zel has no shrink visitor that marks a `NestMembers` attribute used when any member class constant is used, and that then also marks the attribute-name constant.
  - [P3] Zel has no class-constant rule that, if the constant is unused, treats a referenced program class as used only when the usage marker already marks it, always treats a library class as used, and then marks that class constant and its Utf8 name.

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/ShortestUsageMark.java`
- Bazel lines: 183
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A keep mark stores a certain-or-preliminary flag, a reason string, an integer path cost (`depth`), and the causing class plus an optional member. A reason-only mark is certain, with depth 0 and no cause. A mark linked from a previous one is always certain; its depth is the previous depth plus the given cost, and it stores only this step's reason, class, and member. A copy keeps reason, depth, class, and member and replaces only the certain flag. Shorter means a strictly smaller depth. Cause checks `givenClass.equals(storedClass)`. The class visitor runs only when a class is set and the member is absent; the member visitor runs only when both are set.
  - [P3] Zel has no class-file shrinker and no shortest-usage mark. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/ShortestUsageMarker.java`
- Bazel lines: 277
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Extends the usage marker so each kept element stores a certain or preliminary shortest-usage mark (reason, depth, causing class, and optional member). The initial mark is certain at depth 0 with reason "is kept by a directive in the configuration.\n\n". Walking a program class body, field body, method body, or method hierarchy pushes a certain mark with cost 10000 ("is extended by "), 1 ("is referenced by "), 1 ("is invoked by "), or 100 ("implements "), then restores the previous mark. A shorter chain is a strictly smaller depth.
  - [P3] A used mark replaces a missing or non-shortest mark, or any uncertain mark. When promoting an uncertain mark and the current chain is not shorter, the previous reason and depth are kept and only flipped to certain. A possible mark is a copy of the current mark with certain false; it is stored only when there is no certain mark and the current chain is shorter than any existing uncertain mark. `isUsed` requires a certain shortest mark; `isPossiblyUsed` requires an uncertain one. The recursive-cause checker is present but not consulted (its only call is commented out).
  - [P3] Zel has no class-file shrinker and no shortest-usage marker. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/ShortestUsagePrinter.java`
- Bazel lines: 197
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Prints the external name of a program class, field, or method, then the stored shortest-usage reason and recursively the causing class (only when the mark has no member) or member. Non-verbose names are `ExternalName.member`; verbose names are `ExternalName: ` plus a full field or method description with access flags forced to 0. Program methods append every nested line-number range as ` (low:high)` before the newline. Library classes, fields, and methods print a fixed “is a library …” line and do not follow the mark. An entity the marker does not treat as used prints ` is not being kept.` and a blank line.
  - [P3] Zel has no class-file shrinker and no shortest-usage printer. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/Shrinker.java`
- Bazel lines: 196
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No program- and library-class-pool shrink step that requires a non-null `-keep` configuration and fails with `You have to specify '-keep' options for the shrinking step.` when it is absent.
  - [P3] No seed marking that uses a shortest-usage marker when `whyAreYouKeeping` is set and a plain usage marker otherwise, including automatic marking of parameterless constructors of seed classes, keep-specification visitors with shrinking on and optimization and obfuscation off, and a second usage-marker pass over every library class.
  - [P3] No retention of inner-class, nest, annotation, and local-variable-type attributes, or of interfaces, driven by classes already marked used.
  - [P3] No `-whyareyoukeeping` shortest-usage explanation on stdout, and no alphabetical print of classes and members that will be removed when `printUsage` is set.
  - [P3] No rebuild that drops unused program classes, shrinks members of used program and library classes, clears the original program pool, and returns the new pool.
  - [P3] No verbose original and final program-class counts, and no empty-output handling that prints a warning to stderr when `ignoreWarnings` is set or throws `The output jar is empty. Did you specify the proper '-keep' options?` otherwise, skipped only when `warn` is an empty list.

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/UsageMarker.java`
- Bazel lines: 1277
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Marks classes, members, constants, and attributes with `USED` or `POSSIBLY_USED` visitor info, and skips anything already `USED`. A program class marks its name and superclass constants, gives program interfaces a preliminary `POSSIBLY_USED` mark, fully marks library interfaces, marks `<clinit>` only when its code length is greater than 1, promotes members already marked `POSSIBLY_USED`, then marks attributes. A library class marks its superclass, interfaces, and every method, and does not mark library fields.
  - [P3] A program field or method is marked and its body visited only when its class is already used; otherwise it is `POSSIBLY_USED`. Methods also mark the same-name, same-descriptor hierarchy unless they are private, static, or `<init>`/`<clinit>`. That walk keeps default methods on program interface subclasses and the first concrete subclasses, and it skips private, static, and (when the method is public) abstract methods.
  - [P3] Pool entries mark their referenced classes and members. A `String` marks its class and member. A field or method reference marks the class that actually declares the member. `Dynamic` and `InvokeDynamic` mark the bootstrap-method entry, its handle, and its arguments. A constant instruction that is a `String` or `Class` also marks that class’s `()V` constructor. Signature Utf8 constants are marked, but the classes they reference are not.
  - [P3] Most attributes are kept with their pool references. Left unmarked for later visitors are the `BootstrapMethods` attribute except through a dynamic constant, the `InnerClasses` attribute itself (only the entry whose inner class is the current class), nest host and nest members, local-variable tables, and annotation attributes. Stack-map object types mark their class constants. A catch type at index 0 is skipped.
  - [P3] Zel has no class-file shrinker and no usage marker. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/UsagePrinter.java`
- Bazel lines: 169
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] With the unused-items flag set (the only `Shrinker` call, on program classes in alphabetical order), an unused class is printed as its external name alone. A used class is printed only when it has unused members, as a one-time `ExternalName:` header, then each unused member in class-file order: a field is four spaces plus the external field description; a method is four spaces, an optional `lowest:highest:` prefix from the code attribute’s line-number table, then the external method description. With the flag clear, only used classes are printed, as bare external names, and members are left unvisited. Library classes are skipped.
  - [P3] Zel has no class-file shrinker or usage printer. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/UsedClassFilter.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Delegates `visitProgramClass` and `visitLibraryClass` only when `UsageMarker.isUsed` is true for that class. Unused program and library classes are skipped with no error and no other visitor.
  - [P3] Zel has no class-file shrinker and no used-class visitor filter. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/shrink/UsedMemberFilter.java`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Delegates `MemberVisitor` calls only when `UsageMarker.isUsed` is true for that member. The same check applies to program fields, program methods, library fields, and library methods. Unused members are not visited.
  - [P3] Zel has no class-file shrinker and no used-member visitor filter. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/AndMatcher.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] ProGuard returns true only when both string matchers accept the same `[beginOffset, endOffset)` window, and it skips the second matcher when the first returns false.
  - [P3] Zel has no ProGuard string-matcher library or list parser that combines a negated entry with the rest of the list this way. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/ArrayUtil.java`
- Bazel lines: 1745
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Prefix equality for boolean, byte, char, short, int, long, float, and double uses `!=` over the given length, so NaN is not equal to NaN and +0 equals -0. Object equality calls `equals` and throws if an element is null. `equalOrNull` treats both null as equal and exactly one null as unequal, then checks `array1.length` or the given size; a shorter second array throws.
  - [P3] Hashing rotates the accumulator left by 1, then XORs: booleans as 0/1, bytes masked with `0xff`, shorts with `0xffff`, chars and ints as-is, and objects via `hashCode` (null throws). Longs and raw double bits are folded with `(bits | (bits >>> 32))` and narrowed to int. Floats use raw int bits, so NaN payloads and signed zero stay distinct. A null object array hashes to 0.
  - [P3] Comparison is lexicographic with each type's signed compare (`Float.compare` and `Double.compare`, so -0 is less than +0 and NaN is greater than every value), then `Integer.compare` of the two lengths. Comparable elements treat null as less than non-null, then use `compareTo`.
  - [P3] `cloneOrNull` returns a shallow copy, or null. `extendArray` keeps the array when `length >= size`; otherwise it allocates an exact-size array, copies the prefix, and leaves the tail zero or null. `ensureArraySize` overwrites `[0, size)` when the array is already large enough. On growth it fills only when the initial value is not the Java default (false, 0, or null; -0.0 counts as 0). `add` and `insert` grow to `size + 1` and shift the logical tail. `remove` shifts by the physical length `array.length - index - 1`, then writes 0 or null at `size - 1`.
  - [P3] Zel has no ProGuard array utility. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/ClassNameParser.java`
- Bazel lines: 363
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no parser that compiles ProGuard internal class-name patterns into matchers. Missing are `%` (one of `VZBCSIFJD`), `?` (one character other than `/` or `;`), `*` (any run of those), `**` (any run other than `;`, with no continuation matcher when it ends the pattern), `L***;` (0–255 `[` then a primitive or `L`…`;`), `L///;` (any run other than `)`), and `<n>` backreferences. Also missing are recording each wildcard for later reference and the errors for a missing `>`, a non-numeric reference, an out-of-range index, and `<n>` when no wildcard list was supplied.

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/CollectionMatcher.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no string matcher that returns whether a full string, or the half-open slice `[beginOffset, endOffset)`, is a member of a caller-supplied set.

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/ConstantMatcher.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A `StringMatcher` that stores one boolean and returns it from every range match, ignoring the string and both offsets, including empty or out-of-range spans. The flag is fixed at construction. The public full-string `matches` throws `NullPointerException` on a null string because the base class reads `string.length()` before delegating. The range method itself does not read the string.
  - [P3] Zel has no ProGuard string-matcher hierarchy and no constant accept/reject matcher. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/Counter.java`
- Bazel lines: 34
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] ProGuard’s `Counter` is a one-method type whose only contract is `getCount()`, returning the current count as a 32-bit signed integer for visitors such as `ConstantCounter` and `AttributeCounter`. Zel has no equivalent type or call site.

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/EmptyStringMatcher.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A ProGuard `StringMatcher` whose range match is true exactly when `beginOffset == endOffset`. The string is never read, so a null string still matches when the offsets are equal, and an inverted or out-of-bounds range matches only when the two offsets are equal. The inherited full-string `matches(String)` is true only for `""` and throws on null because the parent reads `string.length()`.
  - [P3] Zel has no ProGuard string matcher. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/ExtensionMatcher.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `StringMatcher` that keeps a suffix and accepts it when `string.regionMatches(true, endOffset - suffix.length(), suffix, 0, suffix.length())` is true. The inherited `matches(String)` tests offsets `0` and `string.length()`.
  - [P3] `beginOffset` is ignored, so the matched characters may start before the window, including when `beginOffset` is past `endOffset`. A negative start, a span past the string, a suffix longer than `endOffset`, or an `int` wrap of `endOffset - suffix.length()` returns false. An empty suffix matches every `endOffset` in `[0, string.length()]`. A null string or a null suffix throws `NullPointerException`.
  - [P3] Case folding is Java's per UTF-16 code unit `regionMatches` ignore-case: identical units, else equal `Character.toUpperCase` results, else equal `Character.toLowerCase` of those uppercase results. Length is fixed, so `ß` does not match `SS`. The stored suffix is unchanged and a leading `.` is not added.
  - [P3] Searches under `/workspace/zel-cursor` found no `ExtensionMatcher`, `StringMatcher`, `regionMatches`, or case-insensitive offset suffix. ProGuard is only named as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/FileNameParser.java`
- Bazel lines: 121
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ProGuard file-name filter compiler. `parse` takes the literal text before the first wildcard as an exact prefix, then one wildcard and a recursively parsed remainder. `**` is recognized before `*`. `**` matches any length, including `/` and `File.pathSeparatorChar`. `*` matches any length and `?` matches exactly one character, and both reject `/` and `File.pathSeparatorChar`. A pattern with no wildcard is an exact match of the whole string. An empty pattern matches only the empty string.

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/FixedStringMatcher.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Matches the half-open window `[beginOffset, endOffset)` only when that window is at least as long as the stored fixed string and `String.startsWith(fixedString, beginOffset)` succeeds (UTF-16 code units). With no next matcher the window must be exactly that string. With a next matcher, the remainder `[beginOffset + fixedLength, endOffset)` (possibly empty) is passed to that matcher.
  - [P3] If there is no next matcher and the window is strictly longer than the fixed string but still starts with it, evaluation calls `matches` on a null next matcher and throws `NullPointerException`.
  - [P3] Zel has no ProGuard string-matcher pipeline. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags). `proguard_allowlister.py` only checks config-argument prefixes and does not compile or match keep-style fixed prefixes.

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/ListMatcher.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no ordered list of substring matchers over `[beginOffset, endOffset)` where the first matcher that accepts the slice decides the result.
  - [P3] A hit on a negated entry fails the whole match immediately and later entries are not tried. A hit on a non-negated entry succeeds immediately. With no negate array, any hit succeeds.
  - [P3] If no entry matches, a null negate array yields false. A present negate array yields its last flag, so a list whose last entry is negated matches strings that miss every matcher.
  - [P3] An empty matcher list with a null negate array does not match.

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/ListParser.java`
- Bazel lines: 137
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A string is split with `ListUtil.commaSeparatedList`, then each entry is parsed by a delegated `StringParser`. A leading `!` wraps that entry in `NotMatcher`. Entries are folded from the right: a negated entry is `AndMatcher` with the rest, any other entry is `OrMatcher`, so the leftmost entry is tested first and a positive match or a hit on a negated pattern returns without later entries. An empty list is `ConstantMatcher(true)` (matches every string). A null string becomes a null list and throws on `size()`.
  - [P3] Zel has no ProGuard `StringParser` or list/negation matcher. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/ListUtil.java`
- Bazel lines: 180
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel routine joins a list into a comma-separated string, returning null for a null list, and optionally single-quoting an element when it is empty or contains a space, `@`, `{`, `}`, `(`, `)`, `:`, `;`, or `,`.
  - [P3] No Zel parser splits a comma-separated string into a list, returning null for a null string: it skips Unicode whitespace, takes a single-quoted span (an unclosed quote runs to the end and the quotes are not kept), trims unquoted tokens, and drops empty unquoted tokens.

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/MatchedStringMatcher.java`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Matches a slice only when it starts, at `beginOffset`, with the string last captured by a `VariableStringMatcher`. A null capture fails. The slice must be at least that long. With no next matcher the slice must end exactly there. With a next matcher, the remainder from the end of the capture through `endOffset` is delegated to that matcher.
  - [P3] Zel has no ProGuard name-pattern matcher and no backreference to a previously captured wildcard. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/MultiValueMap.java`
- Bazel lines: 92
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel type maps each key to a set of values and also keeps a separate set of every value ever inserted.
  - [P3] Missing `put` and `putAll` (one key or many keys, one value or a value set). Inserting values under a new key creates that key's set even when the value set is empty, and duplicate values collapse.
  - [P3] Missing `get`, which returns the live per-key set or null when the key is absent, and `getValues`, which returns the live union of inserted values and does not track later mutation of a per-key set.

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/NameParser.java`
- Bazel lines: 224
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No parser builds a matcher chain for one ProGuard name pattern: a literal prefix, then `*` (length 0 through `Integer.MAX_VALUE`) or `?` (exactly one character), with the remainder parsed recursively and an empty pattern matching only the empty string.
  - [P3] No shared variable-matcher list records each `*` and `?` before the suffix is parsed, so later `<n>` (1-based) can replay an earlier wildcard. A missing `>` or an index outside `1..=list.len()` is an error. A non-numeric `<...>` is literal text. With no list, `<...>` is never special and `*` / `?` are not recorded.

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/NotMatcher.java`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `NotMatcher` wraps a `StringMatcher` and returns the negation of `matcher.matches(string, beginOffset, endOffset)` for that inclusive-exclusive substring. Zel has no string-matcher combinator and no negation of a substring match. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/ObjectUtil.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `equal` is null-safe: both null is true, exactly one null is false, otherwise it returns `object1.equals(object2)` (value equality, not reference identity).
  - [P3] `hashCode` returns `0` when the object is null and `object.hashCode()` otherwise.
  - [P3] `compare` orders null before a non-null (`-1` if only the first is null, `1` if only the second is null), returns `0` when both are null, and otherwise returns `object1.compareTo(object2)` unchanged (any `int`, not clamped to -1, 0, or 1).
  - [P3] Zel has no ProGuard object utility and no equivalent null-safe equal, hash, or compare helpers. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/OrMatcher.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `OrMatcher` stores two `StringMatcher`s and returns whether `matcher1.matches(string, beginOffset, endOffset)` or `matcher2.matches(string, beginOffset, endOffset)` is true for that same inclusive-exclusive substring. Java `||` short-circuits, so `matcher2` is not called when `matcher1` matches. Zel has no string-matcher combinator and no disjunction of a substring match. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/PrintWriterUtil.java`
- Bazel lines: 136
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no writer that returns a caller-supplied console `PrintWriter` when the file is the `Configuration.STD_OUT` singleton (`new File("")`, reference equality) and otherwise opens a non-auto-flush UTF-8 `PrintWriter` over a `BufferedWriter` and `FileOutputStream` with the given append flag.
  - [P3] `createPrintWriterOut` binds that console writer to `System.out` with auto-flush. `createPrintWriterErr` binds it to `System.err` with auto-flush and always passes append `false`. Append is ignored for the stdout sentinel.
  - [P3] Closing flushes and does not close the writer when the file is that same sentinel. Any other file is closed. The display name is the literal `standard output` for the sentinel, otherwise the canonical path, or `getPath()` if canonicalization throws `IOException`.

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/SettableMatcher.java`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `SettableMatcher` is a `StringMatcher` whose delegate starts unset and can be assigned or replaced later with `setMatcher`. A range match forwards to that delegate's inclusive-exclusive `matches(string, beginOffset, endOffset)`. An unset or null delegate throws `NullPointerException`. The inherited full-string `matches(String)` reads `string.length()` first, so a null string throws before the delegate runs.
  - [P3] Zel has no ProGuard string matcher and no late-bound substring matcher. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/StringMatcher.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Full-string `matches(String)` is defined as a substring match over `[0, string.length())`. A null argument throws before the abstract method runs, because `string.length()` is evaluated first. An empty string is the zero-length range `[0, 0)`. The range method is abstract: inclusive begin offset, exclusive end offset, and this type supplies no match criterion of its own.
  - [P3] Zel has no string-criterion type with that full-string-to-range delegation. It only names ProGuard as an external tool (`proguard_top`, `bytecode_optimizers`, and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/StringParser.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no parser abstraction whose `parse` method builds a string matcher from a regular expression.

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/StringTransformer.java`
- Bazel lines: 49
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] ProGuard's `StringTransformer` is a one-method contract: `transform(String)` returns the transformed string. The shared `IDENTITY_TRANSFORMER` returns the same reference it was given, including null, with no copy and no exception.
  - [P3] Callers such as `MemberAdder` apply a non-null transformer to member names, and `DefaultInterfaceMethodConverter` supplies its own renamer. Zel has no string-transformer type or those call sites. ProGuard appears only as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/StringUtil.java`
- Bazel lines: 225
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] ProGuard `StringUtil` encodes and decodes modified UTF-8 over UTF-16 code units: U+0000 is `C0 80`, units below U+0080 are one byte, units below U+0800 are two, and every other unit (including each surrogate) is three bytes, with no 4-byte form. Decode (`getString` / `getStringRepresentation`) reads only `size` bytes, treats a lead below `0xC0` as one character and any lead at or above `0xE0` as three, ignores continuation flag bits, and on a short sequence throws `UnsupportedEncodingException` naming the lead byte in hex and a partial string one `char` longer than the characters stored. `join` puts the separator only between elements, and `StringBuilder` turns a null element or separator into the text `null`. `toHexString` writes lowercase hex of every byte; the `size` overload only sets the initial buffer capacity and still walks the whole array.
  - [P3] Zel has no modified-UTF-8 codec and no ProGuard string utility. It only names ProGuard as an external tool (`proguard_top` and related Android shrink flags).

### `third_party/java/proguard/proguard6.2.2/src/proguard/util/VariableStringMatcher.java`
- Bazel lines: 268
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no matcher that accepts a span only when its length is between an inclusive minimum and maximum and every character is in an optional allow-list, or, when that list is absent, outside an optional deny-list (any character when both lists are absent).
  - [P3] With no successor, the whole remaining slice must meet those constraints. With a successor, matching is shortest-first: the successor is tried at the minimum length, then after each further allowed character up to the maximum, and the first disallowed character fails the match immediately.
  - [P3] There is no remembered capture of the last matched span, and no lookup that returns that span or fails when none was recorded.

### `third_party/pprof/profile.proto`
- Bazel lines: 192
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no schema, encoder, or decoder for a `perftools.profiles.Profile`: `sample_type`, `sample`, `mapping`, `location`, `function`, `string_table`, `drop_frames`, `keep_frames`, `time_nanos`, `duration_nanos`, `period_type`, `period`, `comment`, and `default_sample_type`.
  - [P3] The on-disk form is a gzip-compressed protobuf. `string_table[0]` is `""`. Each `sample.location_id` (leaf at index 0) and each nonzero `Location.mapping_id` refers to exactly one record. Every sample's `value` list has the same length as `sample_type`, and merged samples are the elementwise sum. Unset fields are defaults, and values are stored unsampled with enough data to recover the original samples.
  - [P3] A `Label` has at most one of `str` or `num`, and `num_unit` only when `num` is set. `drop_frames` and `keep_frames` are string-table regular expressions that drop matching frames and their successors, or keep them. An unset `default_sample_type` means the last sample value.
  - [P3] `Mapping` is an address range `[memory_start, memory_limit)` with `file_offset`, `filename`, `build_id`, and `has_functions`, `has_filenames`, `has_line_numbers`, and `has_inline_frames`. A `Location` address lies in that range, and its `line` list is outermost-last for inlines. `Function` ids are unique and nonzero, with `name`, `system_name`, `filename`, and `start_line` string-table indexes.

### `third_party/py/abseil/absl/__init__.py`
- Bazel lines: 13
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Importing `absl` loads a regular package whose module body defines no names, sets no `__all__`, and runs no import-time logic. Zel has no `absl` package root that does this.
  - [P3] That empty package is what lets `absl.app`, `absl.command_name`, `absl.flags`, `absl.logging`, and `absl.testing` resolve as submodules. Zel does not provide that package layout.

### `third_party/py/abseil/absl/app.py`
- Bazel lines: 480
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No `app.run` entry point that parses flags, installs the Abseil log handler, runs deferred init callbacks, calls `main`, and exits with its return code.
  - [P3] No `--help`/`-?`, `--helpshort`, `--helpfull`, or `--helpxml` flags that print module or full usage (or XML help) and exit 1.
  - [P3] Flag parse failures do not print `FATAL Flags parsing error:` plus a hint to pass `--helpshort` or `--helpfull` and exit 1.
  - [P3] No `--only_check_args` success exit, one-shot flag registration, or default verbosity bump to INFO after a successful parse.
  - [P3] No `--run_with_pdb`, `--pdb`/`--pdb_post_mortem` (tty-only), or `--run_with_profiling`/`--profile_file`/`--use_cprofile_for_profiling` wrappers around `main`.
  - [P3] No `UsageError` path that prints short usage and exits with the error's exit code, and no pluggable exception handlers invoked on other abnormal exits.
  - [P3] No process-name rewrite, faulthandler enablement, or `call_after_init` callbacks that run immediately once init has finished.

### `third_party/py/abseil/absl/command_name.py`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `make_process_name_useful` sets the Linux kernel command name to `basename(argv[0])`. Zel never changes it.
  - [P3] `set_kernel_process_name` encodes a non-bytes name as ASCII with `?` for non-ASCII, then writes the first 15 bytes to `/proc/self/comm`. On `EnvironmentError` it falls back to `prctl(PR_SET_NAME)` via `libc.so.6`, ignores the return value, and returns silently if ctypes, libc, or `prctl` is missing. Zel has no `/proc/self/comm` write and no `prctl` fallback.

### `third_party/py/abseil/absl/flags/__init__.py`
- Bazel lines: 223
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Importing this module installs the process-wide `FLAGS` registry and the public `DEFINE_*`, validator, exception, flag, parser, and helper API that Bazel Python tools use. Zel has no such package.
  - [P3] On import it sets `FLAGS_MODULE` to this module, disclaims the module from key-flag adoption, and registers `--flagfile` (default `''`, insert definitions from a file) and `--undefok` (comma-separated undefined names that are allowed; valued flags must use `--flag=value`) on a separate `SPECIAL_FLAGS` registry. Zel does not do that.

### `third_party/py/abseil/absl/flags/_argument_parser.py`
- Bazel lines: 629
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Abseil Python flag argument-parser or serializer implementation.
  - [P3] Missing behaviors include the shared parser cache, string parsing that rejects non-strings, bounded float and int conversion (`0x` hex and `0o` octal, bounds help text), bool parsing of `true`/`t`/`1` and `false`/`f`/`0` plus only integer 0 and 1, case-sensitive and case-insensitive string enums and `Enum` members, CSV and whitespace (optional comma) list parsing with strip and empty-input handling, and string, joined-list, CSV, and enum-name serialization including XML `lower_bound`, `upper_bound`, and `list_separator` elements.

### `third_party/py/abseil/absl/flags/_defines.py`
- Bazel lines: 927
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Python `absl.flags` registry. Nothing registers a flag on a `FlagValues` object, attributes it to an explicit `module_name` or the caller's stack, rejects a required flag whose default is not `None`, marks it required, and returns a `FlagHolder` that treats the value as non-`None` when a default is set or the flag is required.
  - [P3] No `set_default` that replaces the default and also the current value only while the flag is still using that default.
  - [P3] No key-flag tracking: `declare_key_flag` (name or holder, special flags such as `--flagfile` and `--undefok` via `SPECIAL_FLAGS`, `ValueError` if the flag is undefined), `adopt_module_key_flags` (requires a module object and also adopts special flags when the module is the flags module), or `disclaim_key_flags` (the caller module can define no further flags, for every `FlagValues`).
  - [P3] No typed `DEFINE_*` helpers: string, boolean (`--no<name>`, value `None`/`True`/`False`), float and integer with optional lower and upper bounds checked by a validator, enum, enum class (case-insensitive by default), CSV list, or whitespace-separated list with optional comma compatibility.
  - [P3] No repeatable multi-value flags (string, integer, float, enum, enum class; a scalar default becomes a one-element list) and no `DEFINE_alias` that forwards parse and value get/set to an existing flag, errors if that flag is missing, rejects a duplicate alias name, and uses help text `Alias for --<name>.`

### `third_party/py/abseil/absl/flags/_exceptions.py`
- Bazel lines: 108
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No absl.flags exception hierarchy: `Error`, `CantOpenFlagFileError` (flagfile missing or unreadable), `IllegalFlagValueError`, `UnparsedFlagAccessError` (read before parse), `ValidationError`, or `FlagNameConflictsWithMethodError`.
  - [P3] No `DuplicateFlagError.from_flag`: it does not resolve the first defining module (default `<unknown>`), the second module from the caller or another flag set, or the first flag's help, and it does not format `The flag '%s' is defined twice. First from %s, Second from %s. Description from first occurrence: %s`.
  - [P3] No `UnrecognizedFlagError` with `flagname`, `flagvalue` defaulting to empty, and the tip `. Did you mean: a, b ?` (comma-joined suggestions, space before `?`) on `Unknown command line flag '%s'`.
  - [P3] Import does not register the module in a disclaim set so flag-definition lookup skips it.

### `third_party/py/abseil/absl/flags/_flag.py`
- Bazel lines: 488
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No per-flag object that parses a value, increments `present`, rejects a repeat when `allow_overwrite` is false (`flag --name=arg: already defined as ...`), resets with `unparse`, or serializes booleans as `--name` / `--noname` and other values as `--name=<serialized>` (empty string when the value is `None`).
  - [P3] No default path that parses a non-`None` default, keeps `using_default_value`, and sets `default_as_str` from the serializer or from `repr('true')` / `repr('false')` for booleans. Empty help is not rewritten to `(no help available)`.
  - [P3] No `BooleanFlag`, `EnumFlag`, `EnumClassFlag`, `MultiFlag`, or `MultiEnumClassFlag`: enum help is not prefixed with `<a|b>:`, multi values are not always a list that appends on later occurrences (`present` grows by the number of new items), multi serialization is not newline-joined, and the type string is not `multi <parser type>`.
  - [P3] No flag XML (`key`, `file`, `name`, `short_name`, `meaning`, serialized default and current value, `type`, `enum_value`). `allow_hide_cpp` and `allow_override_cpp` are not mutually exclusive. Identity equality, a `TypeError` in boolean context, and the pickle and shallow-copy bans are absent.

### `third_party/py/abseil/absl/flags/_flagvalues.py`
- Bazel lines: 1418
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `FlagValues` registry that stores flags under long and short names, hides names from attribute access, and returns parsed values only after parsing (unparsed access raises).
  - [P3] Zel does not track flags or key flags by module name and module id, or look up which module defined a name.
  - [P3] Zel does not apply this registration policy: reject non-flags, empty names, and names containing spaces; reject method-name clashes unless explicitly allowed; duplicate flags error unless override is allowed; a same-named re-import with a different module id is a no-op; short-name registration and cleanup of overridden flags are separate from the long name.
  - [P3] Zel does not implement this argv scan: GNU vs POSIX stop-at-nonoption, `--` termination, `--flag=value` and `--flag value`, boolean `--flag`/`--noflag`, `--undefok` (including `no` prefixes), `known_only`, and unknown-flag errors with suggestions.
  - [P3] Zel does not expand `--flagfile`/`-flagfile` (including the next-token form) with `~`, drop blank and `#`/`//` lines, inline nested flagfiles, warn and skip circular files, and let the later occurrence win by splice position.
  - [P3] Zel has no retired-flag callback, insertion-ordered single- and multi-flag validators, `set_default` that updates the current value only while it is still the default, or `unparse_flags` that restores the pre-parse state and blocks later reads.
  - [P3] Zel does not emit module-grouped help (main module first, special `--flagfile`/`--undefok` section), key-flag module help, gflags-shaped XML documentation, or flagfile text from current values (`flags_into_string` / append to a file).
  - [P3] Zel has no `FlagHolder` that refuses `==` and truthiness, reads through the registry, and rejects mixed `FlagValues` in one resolution, and no rule that `FlagValues` cannot be pickled or shallow-copied while a deepcopy copies the registry.

### `third_party/py/abseil/absl/flags/_helpers.py`
- Bazel lines: 401
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `get_calling_module_object_and_name` walks `sys._getframe` up to the recursion limit, skips modules in `disclaim_module_ids` (this module is pre-inserted), and names `__main__` with `sys.argv[0]`. It raises `AssertionError` when no caller remains. Zel has no stack-based flag-defining module.
  - [P3] `create_xml_dom_element` stringifies the value, lowercases booleans (`true`/`false`), strips XML 1.0-illegal characters (C0 controls except tab/LF/CR, U+007F-U+0084, U+0086-U+009F, surrogates, U+FFFE, U+FFFF), and appends a text node. Zel does not build these flag XML elements.
  - [P3] `get_help_width` returns 80 when stdout is not a tty or `termios`/`fcntl` is missing. Otherwise it uses `TIOCGWINSZ` columns when they are at least 40, then `int(COLUMNS)` (default 80). `TypeError`, `IOError`, and `struct.error` return 80. A non-numeric `COLUMNS` raises `ValueError`. Zel has no help-width helper with that 40-column floor.
  - [P3] `get_flag_suggestions` returns nothing for attempts of length 2 or less or an empty long-option list. It compares the attempt to each option prefix of the same length with Damerau-Levenshtein (insert, delete, substitute, adjacent transposition), drops matches whose distance is at least half the attempt length, and returns every name tied for the smallest distance, sorted by distance then name. Zel has no prefix suggestion of that form.
  - [P3] `text_wrap` expands tabs to 4 spaces, keeps blank lines, wraps the first paragraph with `firstline_indent` and later paragraphs with `indent`, and raises `ValueError` when either indent is at least `length` (`get_help_width()` when `length` is omitted). Zel has no wrapping of that form.
  - [P3] `flag_dict_to_args` emits `--name` for `None` and `True`, `--noname` for `False`, and `--name=value` for `str` and `bytes`. Other iterables become one comma-joined `--name=a,b` unless the name is in `multi_flags`, which repeats `--name=item`. A non-iterable falls through to `--name=<value>`. Zel has no dict-to-argv conversion of that form.
  - [P3] `trim_docstring` follows PEP 257 (tab size 8, first line ignored for the indent, trailing whitespace stripped, leading and trailing blank lines removed). `doc_to_help` strips the docstring, turns whitespace-only lines into empty lines, trims it, and replaces a newline between non-whitespace with a space. Zel does not reformat flag docstrings that way.

### `third_party/py/abseil/absl/flags/_validators.py`
- Bazel lines: 352
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `register_validator` and the `@validator` decorator resolve a flag name or `FlagHolder` against a `FlagValues` (default `FLAGS`), reject a holder whose registry does not match a non-default `flag_values`, and append one checker to that flag. The checker gets the flag value. Returning false uses the message (default `Flag validation failed`). A raised `ValidationError` replaces that message. The decorator returns the original function.
  - [P3] `register_multi_flags_validator` and `@multi_flags_validator` do the same for a list of names or holders. Mixed `FlagValues`, or a string name whose registry disagrees with a holder, raises `ValueError`. The checker receives a dict of name to value. The register default message is `Flags validation failed`. The decorator default is `Flag validation failed`.
  - [P3] `_add_validator` appends the same validator onto each named flag's `validators` list so it runs at initial parse and again after each change of those flags. A missing name raises `KeyError`.
  - [P3] `mark_flag_as_required` warns at `stacklevel` 2 when the default is not `None`, then requires `value is not None` (`False`, `0`, and `''` pass) with `Flag --{name} must have a value other than None.` `mark_flags_as_required` applies that per name and does not reject mixed registries in one call.
  - [P3] `mark_flags_as_mutual_exclusive` warns for each non-`None` default, then allows at most one non-`None` value, or exactly one when `required` is true. Failure raises `ValidationError`: `At most` or `Exactly` `one of ({comma-separated names}) must have a value other than None.`
  - [P3] `mark_bool_flags_as_mutual_exclusive` raises `ValidationError` unless every flag's `.boolean` is true, then counts truthy values with `bool(val)`. At most one truthy value is allowed, or exactly one when `required` is true, with the message `... must be True.`

### `third_party/py/abseil/absl/flags/_validators_classes.py`
- Bazel lines: 172
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No validator object that stores a checker and an error message, assigns a process-wide monotonic insertion index, and raises `ValidationError` with that message when the checker returns false (a checker exception propagates unchanged).
  - [P3] No single-flag validator whose checker receives only that flag's current value, whose flag-name list is `[flag_name]`, and whose failure text is `flag --{name}={value}`.
  - [P3] No multi-flag validator whose checker receives a dict of each named flag's current value (in registration order), whose name list is the original name list, and whose failure text is `flags {name}={value}, {name}={value}` with no `--` prefix.

### `third_party/py/abseil/absl/flags/argparse_flags.py`
- Bazel lines: 388
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `ArgumentParser` is an `argparse.ArgumentParser` that inherits `absl.flags.FLAGS` unless `inherited_absl_flags=None`. Any `prefix_chars` other than `-` raises `ValueError`. Zel has no argparse parser that injects absl flags.
  - [P3] With help enabled it adds suppressed `--helpshort` (same action as `-h`/`--help`) and `--helpfull`, which prints argparse help plus other modules' absl help (main-module key flags already printed are omitted, special flags included) and then exits. It does not define `--helpxml`. Zel has neither flag.
  - [P3] Inherited flags skip `help`, `helpshort`, `helpfull`, `helpxml`, `flagfile`, and `undefok`, and define each flag only under its regular name. The long form is `--name` only; `short_name` is a single dash. Help is suppressed unless the flag is a key flag of `sys.argv[0]`, and `%` in help is escaped. Booleans accept only `--name` / `--noname` (`nargs=0`), not `--name=true/false`. Parsed values stay on the absl flag (`parse`, `using_default_value=False`) and are omitted from the argparse namespace. Zel does not define absl flags this way.
  - [P3] `parse_known_args` defaults to `sys.argv[1:]`, expands `--flagfile` with `read_flags_from_files(..., force_gnu=True)` before argparse, restores `undefok` if a subparser drops it, then strips remaining unknown args named by comma-separated `--undefok` (also each `no`+name, `-`/`--`, and `name=value` on the same token). It deletes `namespace.undefok`, calls `mark_as_parsed()`, and turns `IllegalFlagValueError` from `validate_all_flags()` into `parser.error`. Zel does none of this.

### `third_party/py/abseil/absl/logging/__init__.py`
- Bazel lines: 1240
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `info`, `debug`, `warning`, `error`, `fatal`, `exception`, `log`, and `vlog` write through a logger named `absl`. `fatal` marks the record and the handler flushes, then calls `os.abort()`. A `CRITICAL` record that is not from that logger is emitted as `ERROR` with the prefix `CRITICAL - ` and does not abort. `warn` logs a warning and also emits a `DeprecationWarning`. Zel has no such Python logging facade.
  - [P3] `--verbosity` / `-v` defaults to `-1`. Values at or below debug (`1`) map onto the standard levels (`1` debug, `0` info, `-1` warning, `-2` error, `-3` fatal). Values above `1` map to `DEBUG - (value - 1)`. If the absl handler is on the root logger, the absl logger is set to `NOTSET` and the root level is updated. Otherwise only the absl logger level changes. `set_verbosity` accepts an int or a case-insensitive `debug`, `info`, `warning`, `warn`, `error`, or `fatal`. `level_debug`, `level_info`, `level_warning`, `level_warn`, and `level_error` compare that flag with `>=`, not the logger's effective level. `--logger_levels` is a comma-separated `name:level` list (or a mapping) applied with `logging.getLogger(name).setLevel`. Zel defines none of these flags or mappings.
  - [P3] `--logtostderr` (default false) sends records only to stderr. Otherwise they go to the log stream, and also to stderr when `--alsologtostderr` is set or the level is at or above `--stderrthreshold`. That threshold defaults to `fatal`, accepts case-insensitive `debug`, `info`, `warning`, `warn` (stored as `warning`), `error`, and `fatal`, plus `'0'` through `'3'`, and rejects anything else. `--log_dir` defaults to `TEST_TMPDIR` or empty. Before flags are parsed, the first log writes `WARNING: Logging before flag parsing goes to stderr.` and the record goes to stderr. If the root logger has no handlers, `log` calls `logging.basicConfig()` first. `--showprefixforinfo` (default true) drops the prefix only for `INFO` when verbosity is `0` and the stream is stderr. Zel has none of this routing.
  - [P3] Each line is prefixed with `Smmdd hh:mm:ss.uuuuuu thread file:line] ` using local time, an unsigned thread id (`threading.get_ident()` masked to an unsigned long), and severity `I`, `W`, `E`, or `F`. `ABSL_LOGGING_PREFIX_REGEX` matches that prefix. The caller frame skips `logging/__init__.*` and any `(filename, function[, firstlineno])` registered through `skip_log_prefix`. Zel does not emit this prefix or skip those frames.
  - [P3] `find_log_dir` uses the explicit directory, otherwise `--log_dir`, otherwise the first writable directory among `/tmp/` and `./`, and raises `FileNotFoundError` if none is writable. The log basename is `{program}.{hostname}.{user}.log.INFO.{YYYYmmdd-HHMMSS}.{pid}`. An omitted program name becomes `py_` plus `argv[0]` without its extension. The username falls back to the uid, or `unknown`, when `getpass.getuser()` raises `KeyError`. A `{program}.INFO` symlink is replaced on a best-effort basis. `get_log_file_name` returns the stream name, or `''` for stderr, stdout, or a stream with no name, and rejects levels other than the five absl levels. `flush` ignores `EnvironmentError` and `ValueError`. `close` flushes and closes streams that are not stderr, stdout, or a tty. Zel does not create these files or symlinks.
  - [P3] `log_every_n` logs the 1st call and then every `n`th call after that. `log_first_n` logs while the count is below `n`. `log_every_n_seconds` logs the first call and later calls once `timeit.default_timer()` has advanced by at least `n` seconds. All three key off the caller frame. `log_if` logs only when the condition is true. The counters and timers are process-global and are never dropped. Zel has none of these.
  - [P3] `use_absl_handler` adds the handler to the root logger, removes existing stderr `StreamHandler`s once, and reapplies verbosity and logger levels. The absl logger still emits when `Logger.disabled` is set. `use_python_logging` switches back to the Python handler and, unless `quiet` is set, logs `Restoring pure python logging`. Zel does not install or switch this handler.

### `third_party/py/abseil/absl/logging/converter.py`
- Bazel lines: 214
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not convert Abseil Python levels (`FATAL=-3`, `ERROR=-2`, `WARNING=-1`, `INFO=0`, `DEBUG=1`) to C++ (`0` when `level >= 0`, otherwise `-level`) or to standard levels (`50/40/30/20/10`), including clamping below `FATAL` and the vlog formula `10 - level + 1` for absl levels above `DEBUG`.
  - [P3] Zel has no inverse `standard_to_absl` / `standard_to_cpp` (negative standard levels clamped to `0`; bands below 10, 20, 30, 40, and 50) and no case-insensitive `string_to_standard` (`debug`/`info`/`warning`/`warn`/`error`/`fatal`).
  - [P3] Zel has no `get_initial_for_level` (`I` below 30, `W` below 40, `E` below 50, otherwise `F`). Non-integer levels do not raise `TypeError`.

### `third_party/py/abseil/absl/testing/__init__.py`
- Bazel lines: 13
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] License-only package marker: importing `absl.testing` defines no names and does not import `absltest`, `parameterized`, `flagsaver`, `xml_reporter`, `_bazelize_command`, or `_pretty_print_reporter`. No file under `/workspace/zel-cursor` provides that package.

### `third_party/py/abseil/absl/testing/_bazelize_command.py`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `get_executable_path` is absent: nothing resolves a data-dependency `py_binary` name to a filesystem path.
  - [P3] On Windows there is no lookup that appends `.exe`, opens `FLAGS.test_srcdir/MANIFEST`, matches `{TEST_WORKSPACE}/{name}` on a two-token line, returns the second token, and raises `RuntimeError` when the entry is missing.
  - [P3] On other platforms there is no walk from the module file up `__name__.count('.') + 1` directories to a root, then a join with `py_binary_name`.

### `third_party/py/abseil/absl/testing/_pretty_print_reporter.py`
- Bazel lines: 91
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No unittest result that forces the base reporter to verbosity 0 and, when the requested verbosity is greater than 0, prints a flushed `[ RUN ]`, `[ OK ]`, `[ FAILED ]`, or `[ SKIPPED ]` line using the test id with a leading `__main__.` removed.
  - [P3] Success and expected failure both report `OK`; error, failure, and unexpected success all report `FAILED`.
  - [P3] No runner that, when `run_for_debugging` is set, calls `test.debug()` and returns a fresh empty result instead of the normal result-collecting run.

### `third_party/py/abseil/absl/testing/absltest.py`
- Bazel lines: 2577
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Importing the module monkey-patches `unittest.TestResult.wasSuccessful` so any `unexpectedSuccesses` counts as failure. If that probe still reports success, it writes a stderr warning that the hotfix failed. Zel has no Python unittest result patch.
  - [P3] `expectedFailureIf` drops `reason` and returns `unittest.expectedFailure` when `condition` is true, otherwise the identity decorator. It also re-exports `skip`, `skipIf`, `skipUnless`, `SkipTest`, and `expectedFailure`. Zel has none of these decorators.
  - [P3] `skipThisClass` raises `TypeError` if `reason` is a class or the target is not a `TestCase`. It replaces `setUpClass` so only the decorated class raises `SkipTest`; subclasses call the shadowed `setUpClass.__func__` or `super().setUpClass`. Zel has no class-only skip.
  - [P3] Flags, all `allow_override_cpp`: `test_srcdir` defaults to `TEST_SRCDIR` or `''`; `test_tmpdir` to `TEST_TMPDIR` or `<temp>/absl_testing`; `test_random_seed` to `301`, or `int(TEST_RANDOM_SEED)` when that parses; `test_randomize_ordering_seed` defaults to `''`; `xml_output_file` defaults to `''`. Zel does not define these flags.
  - [P3] Ordering seed reads the flag when present, else `TEST_RANDOMIZE_ORDERING_SEED`. Empty or `0` disables randomization, `random` picks an int in `[1, 4294967295]`, a positive int is used, and anything else raises `ValueError`. Zel does not shuffle test-method order from that flag or variable.
  - [P3] `TestLoader.getTestCaseNames` raises `TypeError` for a method named `Test` plus another uppercase letter whose spec is exactly one `self` argument. When a seed is set it logs the seed and shuffles names with `random.Random(seed)`. Zel has no such loader.
  - [P3] `main` prints `Running tests under Python X.Y.Z: <executable>` to stderr, forces `alsologtostderr` default true unless already set false, and runs inside `absl.app.run`. Already inside `app.run`, it stubs every flag `parse` to a no-op, re-parses `sys.argv`, then restores parsers. It registers `faulthandler` for `SIGTERM` with `chain=True` and ignores failure. `run_tests` exits `sys.exit(not result.wasSuccessful())`. Zel has no `absltest.main`.
  - [P3] `TESTBRIDGE_TEST_ONLY` is `shlex.split` and inserted into argv: positional args on Python before 3.7, and one `-k=<token>` per token on 3.7+. `TESTBRIDGE_TEST_RUNNER_FAIL_FAST=1` inserts `--failfast`. Zel exports `TESTBRIDGE_TEST_ONLY` from the runner and does not translate either variable into unittest argv.
  - [P3] Sharding writes an empty `TEST_SHARD_STATUS_FILE` when that variable is set and `sys.exit(1)` on `IOError`. With `TEST_TOTAL_SHARDS` set, a `TEST_SHARD_INDEX` outside `[0, total)` exits 1. Names are bucketed by cycling shards over the sorted names, and the shared iterator continues across classes, then the shard's names are returned in the original loader order. Zel only checks that the status file was touched; it does not partition Python methods.
  - [P3] XML path priority is `--xml_output_file`, then `XML_OUTPUT_FILE`, then `dirname(TEST_TMPDIR)/test_detail.xml` when `RUNNING_UNDER_TEST_DAEMON` is set, then `TEST_XMLOUTPUTDIR/<argv0 stem>.xml`. It creates the parent directory, ignoring `EEXIST`, truncates the file before the run, buffers the report in memory, and writes it at the end. A runner without `set_default_xml_stream` is replaced, with a stderr warning. A randomization seed is stored as the `test_randomize_ordering_seed` testsuites property. `FLAGS.pdb_post_mortem` sets `run_for_debugging` only on the pretty-print runner. Zel does not run this reporter.
  - [P3] `create_tempdir` and `create_tempfile` live under `TEST_TMPDIR/<qualname without __main__.>/<method>`, after creating `TEST_TMPDIR` if missing. A relative name's first path segment is deleted and recreated; unnamed paths use `mkdtemp` or `mkstemp`. Existing files are chmod'd user-writable and then truncated. An empty file is `write_bytes(b'')`. Cleanup is `always` (default, via `addCleanup`), `success` (only if failure, error, and unexpected-success counts are unchanged; before 3.11 it feeds `outcome.errors` into a fresh result), or `never`. Zel has no per-method temp helpers or success-only cleanup.
  - [P3] `enter_context` on the instance uses an `ExitStack` closed in `addCleanup`; on the class it needs `addClassCleanup` and raises `AssertionError` if `setUp` or `setUpClass` did not install the stack. Contexts are exited in reverse order with no exception info. Zel has no equivalent.
  - [P3] `shortDescription` returns `id()` with a leading `__main__.` removed, plus the first docstring line when present. `maxDiff` is 1600 and `longMessage` is true. `fail` formats an optional prefix through `_formatMessage`.
  - [P3] Extra assertions absent from Zel include prefix and suffix checks, `assertEmpty` and `assertNotEmpty` via `len` rather than truthiness, `assertLen` requiring `Sized`, `assertSequenceAlmostEqual` (length mismatch fails immediately; per-index `assertAlmostEqual` errors, capped at 30), set subset and disjoint checks, `assertSameElements` (rejects `str` and `bytes`, ignores duplicates, falls back to a sorted list difference for unhashable items), `assertItemsEqual` as `assertCountEqual`, multiline `ndiff` with an optional `line_limit`, inclusive `assertBetween`, substring `assertRegexMatch` with `re.MULTILINE` after joining regexes and coercing `str`/`bytes`, in-order string search that resumes at the match index rather than after it, subsequence and exact-subsequence checks, `assertTotallyOrdered` over equality groups including hash equality when both sides are hashable, dict diffs split into missing, unexpected, and differing entries, URL equality with sorted params and order-insensitive query keys, and `assertSameStructure` / `assertJsonEqual` walking mappings, sequences, and sets while treating mixed container types of the same kind as compatible and capping reported problems by `maxDiff // 80`.
  - [P3] `get_command_stderr` runs the command with `stdout` merged into `stderr` and returns `(wait status, output)`. `env=None` becomes `{}`, so the child does not inherit the parent environment. A string command uses `shell=True`. On Windows `close_fds` is forced false. Non-Windows argv lists are quoted with single quotes for the failure text. `assertCommandSucceeds` requires exit 0 and stderr matching a regex (default `b''`, and `str` regexes are UTF-8 encoded). `assertCommandFails` requires a non-zero exit and a regex match. Zel has no such command assertions.

### `third_party/py/abseil/absl/testing/flagsaver.py`
- Bazel lines: 194
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No decorator, context manager, or explicit save/restore API that snapshots every flag and puts the previous values back after a call or block.
  - [P3] No keyword overrides or `(FlagHolder, value)` pairs applied only for that scope, and no rejection of classes, mixed positional and keyword arguments, or setting the same flag twice.
  - [P3] No snapshot of each flag's attribute dict, current value, and validator list, and no restore that writes the value back before replacing that dict.
  - [P3] No restore that deletes flags registered after the snapshot. The context manager also does not roll the snapshot back and re-raise when applying overrides fails.

### `third_party/py/abseil/absl/testing/parameterized.py`
- Bazel lines: 706
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Python `unittest` parameterization library: no `parameters`, `named_parameters`, or `product` decorators, and no `TestCase` / `TestGeneratorMetaclass` that expands one method or class into isolated cases with their own `setUp`/`tearDown`.
  - [P3] No generation of indexed names (`testFoo0`) versus named suffixes (`testcase_name` or a leading string), including the `test_` underscore join, duplicate-name failure, or empty-parameter `NoTestsError`.
  - [P3] No invocation rules for dict kwargs, tuple positional args, a bare single argument, or a single non-tuple iterable of cases, and no cartesian product of kwargs-dict sequences and named value lists with key-overlap checks.
  - [P3] No parameter text on `id()` and `__str__` (address-stripped reprs), no async wrapper for coroutine tests, and no `CoopTestCase` cooperative metaclass.

### `third_party/py/abseil/absl/testing/xml_reporter.py`
- Bazel lines: 558
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `TextAndXMLTestRunner` is a `unittest.TextTestRunner` that still prints text results and, in `printErrors`, writes `<?xml version="1.0"?>` plus a JUnit tree. With no XML stream it is an ordinary text runner. Zel has no Python unittest XML reporter.
  - [P3] Each case is `status="run"` `result="completed"`, or `status="notrun"` `result="suppressed"` when skipped, with no `<skipped>` child. `time` is `%.3f` seconds. `timestamp` is UTC ISO-8601 and is omitted when the epoch time is missing or negative.
  - [P3] Cases are grouped by class name (`_ErrorHolder` uses the last dotted component of the parsed class; `_SubTest` uses the parent case's class). Suite time is max(start+duration)-min(start). The outer `testsuites` `name` is empty and its time is wall-clock end-start. Only the first `failure` or `error` on a case increments that suite count.
  - [P3] Expected failures stay successful and, if the test has `recordProperty`, store the traceback as `EXPECTED_FAILURE`. An unexpected success is an `error` with empty `message` and `type` and CDATA `Test case <id> should have failed, but passed.` A subtest whose exception subclasses `failureException` is a `failure`; any other exception is an `error`.
  - [P3] Pending results are keyed by `id(test)` under a re-entrant lock. `stopTest` with nothing pending writes `No pending test case: ...` to stderr and records nothing. Fixture errors still pending at `stopTestRun` are added to the suite. Sorted `<properties>` on `testsuites` come from `set_testsuites_property`.
  - [P3] Attribute values escape `"`, `'`, newline, tab, CR, and space (`&#x20;`), and illegal XML controls as `\xNN`. CDATA uses the same control escapes and turns `]]>` into `]] >`. A `None` attribute is omitted.

### `third_party/py/abseil/setup.py`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Importing or executing this file calls `setuptools.setup` at module scope. On `ImportError` it runs `ez_setup.use_setuptools()` and imports `setuptools` again. Zel has no setuptools bootstrap and no `absl-py` setup call.
  - [P3] `sys.version_info < (3, 6)` raises `RuntimeError` with the text `Python version 3.6+ is required.`
  - [P3] The first two dot-separated fields of `setuptools.__version__` are parsed with `int`. When that pair is at least `(24, 2)`, setup receives `python_requires='>=3.6'`. A non-numeric field raises `ValueError`. A one-field version leaves `python_requires` unset.
  - [P3] `long_description` is the sibling `README.md` of `os.path.realpath(__file__)`, read as bytes and decoded as strict UTF-8. A missing file or invalid UTF-8 raises.
  - [P3] The setup metadata is name `absl-py`, version `1.3.0`, description `Abseil Python Common Libraries, see https://github.com/abseil/abseil-py.`, markdown content type, author `The Abseil Authors`, URL `https://github.com/abseil/abseil-py`, license `Apache 2.0`, `include_package_data=True`, classifiers for Python 3.6 through 3.11, and `find_packages` excluding `*.tests`, `*.tests.*`, `tests.*`, and `tests`.

### `third_party/py/concurrent/__init__.py`
- Bazel lines: 3
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On import, the package `__path__` is replaced with `pkgutil.extend_path(__path__, __name__)`, so every other `sys.path` entry that also provides `concurrent` (directories with `__init__.py`, plus paths listed in `*.pkg` files) is merged in and duplicates are dropped.
  - [P3] That keeps `concurrent` a namespace package: this tree's `futures` backport and the stdlib `concurrent` package can both be imported. Zel has no `concurrent` package init and no `pkgutil.extend_path` equivalent that performs this merge.

### `third_party/py/concurrent/futures/__init__.py`
- Bazel lines: 18
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `concurrent.futures` package (or other module) that re-exports this public surface: `FIRST_COMPLETED`, `FIRST_EXCEPTION`, `ALL_COMPLETED`, `CancelledError`, `TimeoutError`, `Future`, `Executor`, `wait`, `as_completed`, `ProcessPoolExecutor`, and `ThreadPoolExecutor`.
  - [P3] Nothing else submits callables to a thread-pool or process-pool executor and returns futures that support cancellation, timeouts, `wait`, and `as_completed`.

### `third_party/py/concurrent/futures/_base.py`
- Bazel lines: 575
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `Future` starts `PENDING`. `cancel` succeeds only before `RUNNING` or `FINISHED` (a second cancel returns true), notifies waiters, and runs done callbacks. `set_running_or_notify_cancel` moves a pending future to `RUNNING`, or a cancelled one to `CANCELLED_AND_NOTIFIED` and wakes `wait`/`as_completed` without running the work. Any other state logs a critical line and raises `RuntimeError`. `set_result` and `set_exception` mark `FINISHED`, wake waiters, and run callbacks. Zel has no state machine of that form.
  - [P3] `result` waits up to `timeout`, raises `CancelledError` when cancelled, re-raises the stored exception, and raises `TimeoutError` if still unfinished. `exception` returns that exception or `None` under the same wait, cancel, and timeout rules. Zel has no waits of that form.
  - [P3] `add_done_callback` runs immediately if the future is already cancelled or finished. Otherwise it runs later, in add order, after cancel or completion. A callback exception is logged on `concurrent.futures` and swallowed. Zel has no callback list of that form.
  - [P3] `wait` returns `(done, not_done)`. `FIRST_COMPLETED` returns as soon as one future is finished or notified-cancelled. `FIRST_EXCEPTION` returns early only when a non-cancelled finished future has an exception; otherwise it matches `ALL_COMPLETED`. A plain `CANCELLED` future stays not-done until the worker notifies. A timeout returns the partial split. An unknown `return_when` raises `ValueError`. Zel has no wait of that form.
  - [P3] `as_completed` yields futures already finished or notified-cancelled, then each later completion. On timeout it raises `TimeoutError` with the unfinished count. Zel has no iterator of that form.
  - [P3] `Executor.submit` raises `NotImplementedError`. `map` submits every zipped argument immediately, yields `result` in input order with a shrinking deadline, and cancels leftovers in `finally`. `shutdown` is a no-op. The context manager always calls `shutdown(wait=True)` and does not swallow the block exception. Zel has no executor base of that form.

### `third_party/py/concurrent/futures/_compat.py`
- Bazel lines: 101
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel vendors a Python 2 `namedtuple` factory (pythonfutures 2.1.2) used only when `collections.namedtuple` is missing. Zel has no named-tuple type and no `concurrent.futures` backport.
  - [P3] Callers can pass field names as a comma- or whitespace-separated string or as any sequence of names. Each name is stringified, then the type name and every field are rejected unless they are non-empty, alphanumeric or `_`, not a keyword, and not digit-leading. Field names that start with `_` or repeat are also rejected.
  - [P3] The generated tuple subclass sets `__slots__`, `_fields`, and a `Type(field, ...)` docstring. Construction is positional or keyword. `_make` builds from an iterable and raises `TypeError` when the length is not the field count. `__repr__` prints `Type(name=value, ...)`. `_asdict` returns a field-to-value dict. `_replace` copies the tuple with named overrides and raises `ValueError` on unknown names. `__getnewargs__` returns the plain tuple for pickling, and `__module__` is taken from the caller frame when `sys._getframe` exists. Field access is an `itemgetter` property.

### `third_party/py/concurrent/futures/process.py`
- Bazel lines: 345
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No `ProcessPoolExecutor`: a pool of worker processes (default size `cpu_count()`) that runs submitted callables out of process and completes a `Future` with the return value or the raised exception.
  - [P3] No in-process work-id queue plus pending-work map, and no daemon management thread that moves not-yet-cancelled items onto a call queue of size `max_workers + 1` (`EXTRA_QUEUED_CALLS`). Items already on that queue can no longer be cancelled.
  - [P3] Workers block on the call queue with a 0.1s timeout and exit only after a shutdown event is set and the queue is empty. The management thread applies results, and on interpreter shutdown, executor collection, or `shutdown`, joins the processes once nothing is pending.
  - [P3] `submit` after shutdown raises `RuntimeError`; `shutdown(wait=True)` joins the management thread and then drops the queues and process handles. An `atexit` hook joins those management threads. Zel has no equivalent API or lifecycle.

### `third_party/py/concurrent/futures/thread.py`
- Bazel lines: 144
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `ThreadPoolExecutor(max_workers)` keeps an unbounded queue and daemon workers. `submit` holds the shutdown lock and raises `RuntimeError('cannot schedule new futures after shutdown')` once shut down; otherwise it enqueues a work item and starts one daemon thread while `len(threads) < max_workers`. Dead threads stay in that set, so a crashed worker is never replaced, and idle-versus-queue sizing is not done.
  - [P3] A worker `get`s with a 0.1s timeout. On timeout it exits only if the interpreter is shutting down, the executor weakref is dead, or `executor._shutdown` is set; otherwise it drops the executor reference and keeps polling. A dequeued item runs only when `future.set_running_or_notify_cancel()` succeeds, then `set_result` or `set_exception` (`BaseException` taken from `sys.exc_info()[1]`). Any other `BaseException` in the worker is logged critical as `Exception in worker` and that thread ends.
  - [P3] `shutdown(wait=True)` sets `_shutdown` and joins every worker, so already queued items still run until the queue is empty. `wait=False` only sets the flag. An `atexit` handler sets a process-wide `_shutdown` and joins every live worker weakref. Each new executor drops dead entries from that global weakref set.

### `third_party/py/dataclasses/dataclasses/__init__.py`
- Bazel lines: 1056
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `@dataclass` reads PEP 526 annotations, merges base `__dataclass_fields__` in reverse MRO order, and adds `__init__`, `__repr__`, `__eq__`, ordering, and frozen `__setattr__`/`__delattr__` only when those names are absent from the class `__dict__`. `order=True` with `eq=False` raises `ValueError`. A frozen class cannot inherit a non-frozen dataclass, or the reverse. Zel does not generate these methods or enforce that inheritance rule.
  - [P3] `field()` rejects both `default` and `default_factory`. `ClassVar` (typing 3.6 `_ClassVar` or 3.7+ `_GenericAlias`) and bare `InitVar` are stored but omitted from `fields()`, and both reject a default factory. A real field whose default is a `list`, `dict`, or `set` raises `ValueError`. A `Field` class attribute with no annotation raises `TypeError`. Zel has no `Field`, `InitVar`, or `MISSING`.
  - [P3] Generated `__init__` rejects a non-default init field after a default, calls `default_factory` when the argument is the `<factory>` sentinel or when `init=False`, assigns frozen fields with `object.__setattr__`, skips `InitVar` assignment, and calls `__post_init__` with `InitVar` values. The parameter is named `__dataclass_self__` when a field is named `self`. Zel does not build this constructor.
  - [P3] Comparisons and `__hash__` use only real fields with `compare` (or `hash` when it is not `None`) and return `NotImplemented` for a different class. Default `eq=True` and `frozen=False` sets `__hash__ = None` unless the class already defined `__hash__`. `unsafe_hash=True` overwrites a missing `__hash__` and raises `TypeError` if one exists. Frozen instances raise `FrozenInstanceError` on assign or delete of a known field or when `type(self)` is the frozen class. Zel does not implement this hash table or frozen error.
  - [P3] `fields` and `is_dataclass` key off `__dataclass_fields__`. `asdict` and `astuple` require an instance, recurse into dataclass values and `list`/`tuple`/`dict`, deepcopy other values, and honor `dict_factory`/`tuple_factory`. `replace` copies init fields, rejects `init=False` overrides, and reconstructs through `__init__`. `make_dataclass` builds a class with `typing.Any` for bare names and passes it to `dataclass`. An empty `__doc__` is replaced with the class name plus `inspect.signature`. Zel has none of these helpers.

### `third_party/py/frozendict/frozendict/__init__.py`
- Bazel lines: 64
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no immutable hashable mapping: construction from a mapping, an iterable of pairs, or keyword arguments, plus lookup, containment, iteration, and length.
  - [P3] No `copy` that returns a new instance with keyword overrides, no `<ClassName {dict}>` repr, and no cached hash that XORs `hash((key, value))` for each pair.
  - [P3] No order-preserving `FrozenOrderedDict` subclass, and no Python <2.7 path that drops that subclass when `OrderedDict` is unavailable.

### `third_party/py/mock/__init__.py`
- Bazel lines: 2367
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not provide the mock 1.0.1 library: `Mock`, `MagicMock`, `NonCallableMock`, `NonCallableMagicMock`, and `PropertyMock` (call recording, `assert_*`, spec/wraps, child mocks, and magic methods).
  - [P3] Zel does not provide `patch` / `patch.object` / `patch.dict` / `patch.multiple`, `mock_open`, `create_autospec`, `sentinel`, `call`, `ANY`, or `DEFAULT`.

### `third_party/py/mock/docs/conf.py`
- Bazel lines: 208
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Sphinx config that inserts the parent directory on `sys.path` and reads `mock.__version__` so the docs build imports the vendored mock package beside `docs/`.
  - [P3] No `sphinx.ext.doctest` setup. The global setup imports `mock` (including a star import), `unittest2`, and `__main__`, appends the current directory to `sys.path` when it is absent, stores the real `__main__` under `sys.modules['__main']`, and replaces `sys.modules['__main__']` with a `ProxyModule` whose `__dict__` is the doctest globals. Cleanup copies `sys.modules['__main']` onto itself and does not put the real module back.
  - [P3] No HTML settings for this book: theme `nature` with empty options, source suffix `.txt`, master document `index`, project `Mock`, copyright `2007-2012, Michael Foord & the mock team`, `version` as the first three characters of `__version__` and `release` as the full string, `today_fmt` `%B %d, %Y`, `add_module_names` false, Pygments style `friendly`, last-updated format `%b %d, %Y`, module index off, and HTML Help basename `Mockdoc`.
  - [P3] No LaTeX settings: 12pt font, one manual from `index` to `Mock.tex` titled `Mock Documentation` by `Michael Foord`, and the LaTeX module index off. `exclude_trees` is an empty list.

### `third_party/py/mock/html/_static/doctools.js`
- Bazel lines: 247
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Sphinx browser helpers for this vendored mock HTML: `$u = _.noConflict()`, `urldecode` (`decodeURIComponent` plus `+` to space), `urlencode`, multi-value query parsing into string arrays, and linear `contains`.
  - [P3] No case-insensitive `highlightText` that wraps a match in a span, skips `button`/`select`/`textarea`, and skips a parent that already has the highlight class.
  - [P3] No `Documentation` i18n catalog (`gettext`/`ngettext`, plural expression from the catalog, locale), permalink paragraph links on the first header under `div[id]` and on `dt[id]`, or the Firefox hash `location.href += ''` reload after 10 ms.
  - [P3] No search-term highlighting from the `highlight` query param inside `div.body` (10 ms delay), "Hide Search Matches" link, or `hideSearchWords` (fade the link, strip the `highlighted` class).
  - [P3] No domain-index toggler that shows `img.toggler`, flips `tr.cg-<id>` and swaps `minus.png`/`plus.png`, and auto-clicks when `DOCUMENTATION_OPTIONS.COLLAPSE_INDEX` is set.
  - [P3] No `makeURL` (`URL_ROOT + '/' + relative`) or `getCurrentURL` (pop path segments for each `..` in `URL_ROOT`, then return the last path piece without its final character), and no `document.ready` call to `Documentation.init`.

### `third_party/py/mock/html/_static/jquery.js`
- Bazel lines: 154
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel vendors jQuery 1.4.2 (including Sizzle) as the browser script the prebuilt Python mock Sphinx pages load from `_static/jquery.js`. It supplies `$`/`jQuery`: CSS selectors, DOM traversal and mutation, events, AJAX, effects, and the helpers `doctools.js` and `searchtools.js` call for search, permalinks, and navigation.
  - [P3] Zel does not ship this library or the mock HTML docs, so none of that client-side documentation behavior exists.

### `third_party/py/mock/html/_static/searchtools.js`
- Bazel lines: 559
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Sphinx browser full-text search: no Porter stemmer, no `q`-parameter search against an index of terms, objects, filenames, and titles, no stopword, numeric, and `-excluded` term handling, no important/object/regular/unimportant result ranking, and no source-excerpt summaries with keyword highlighting.

### `third_party/py/mock/html/_static/toc.js`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No zel code binds a click on `#toc_button` to show or hide `#sphinxsidebar`.
  - [P3] No zel code shifts `div.document` to `left: 0px` and drops the `open` class when the sidebar is hidden, or to `left: 230px` and adds `open` when it is shown.
  - [P3] No zel code runs that setup from a document-ready handler.

### `third_party/py/mock/html/_static/underscore.js`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel vendors Underscore.js 0.5.5 as a Sphinx static script for the mock HTML docs. It installs a global `_` (and `exports._` under CommonJS) with collection, array, function, and object helpers, ERB-style `template` compilation, OOP chaining (`chain`/`value`), `noConflict`, and `uniqueId`.
  - [P3] Zel has no mock documentation tree and no equivalent browser utility, so that script and `_` API are absent.

### `third_party/py/mock/html/searchindex.js`
- Bazel lines: 0
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel's mock HTML docs load this file with `Search.loadIndex("searchindex.js")`. It is a single-line `Search.setIndex({...})` script (21542 bytes, no trailing newline) that is the Sphinx client-side search corpus.
  - [P3] The index covers ten pages (`index`, `compare`, `getting-started`, `magicmock`, `changelog`, `patch`, `helpers`, `examples`, `sentinel`, `mock`) and object entries for the `mock` module, `mock.patch`, and `mock.Mock`.
  - [P3] Zel has no `Search.setIndex` or `Search.loadIndex` path and no vendored `third_party/py` mock documentation search index, so that browser search corpus is absent.

### `third_party/py/mock/setup.py`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no setuptools or distutils entry point that registers a distribution named `mock`, with the version taken from `mock.__version__` and `py_modules=['mock']`.
  - [P3] Zel does not read a sibling `README.txt` as the long description, and it does not publish this package's author, email, summary, keywords, URL, or classifier list.
  - [P3] Zel does not set `tests_require=['unittest2']` and `test_suite='unittest2.collector'` when setuptools imports, and it has no fallback to `distutils.core.setup` when setuptools is missing.

### `third_party/py/mock/tests/__init__.py`
- Bazel lines: 3
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The Bazel file is a comment-only package marker that makes `mock.tests` a regular importable Python package and defines no symbols. Zel has no Python mock test package or equivalent package marker.

### `third_party/py/mock/tests/_testwith.py`
- Bazel lines: 181
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `with patch(target, new)` swaps the named attribute for the block and puts the original back on a normal exit and after an exception. The exception is not swallowed.
  - [P3] `with patch(target) as m` binds a `MagicMock`, and the target equals that mock inside the block. Passing an explicit new value makes both the target and the `as` name that value.
  - [P3] `patch.object` on a class attribute installs a mock and restores the original string afterward.
  - [P3] Nested `patch` calls on two attributes yield both mocks and restore both. Re-patching the same attribute restores the outer mock when the inner block ends, then the original value when the outer block ends.
  - [P3] A `Mock` used as a context manager calls `__enter__` with no arguments, binds `as` to `__enter__.return_value`, and calls `__exit__(None, None, None)` on a clean exit.
  - [P3] A `MagicMock` context manager records `__enter__` and `__exit__` and lets a `TypeError` raised in the block propagate.
  - [P3] `patch.dict` inserts the given keys for the block and restores the original dict afterward, including when a `NameError` is raised inside.
  - [P3] `mock_open()` patched onto `open` (`create=True`) is the patched object, records `open('foo')` once, and as a context manager records `call('foo')`, `__enter__`, `read`, and `__exit__(None, None, None)`, with `as f` equal to the mock's return value. `mock_open(existing)` configures that mock. `read_data='foo'` makes `read()` return `'foo'`.
  - [P3] Zel has no `mock` package, `patch` / `patch.object` / `patch.dict`, or `mock_open`.

### `third_party/py/mock/tests/support.py`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On Python 3.2 and newer, import binds the name `unittest2` to the stdlib `unittest`. On older versions it imports the third-party `unittest2` package. Zel has no such version-selected unittest alias.
  - [P3] If the builtin `callable` is missing, `callable(obj)` is defined as `hasattr(obj, '__call__')`. Otherwise the builtin is kept. Zel has no equivalent polyfill.
  - [P3] `inPy3k` is true exactly when the major version is 3, and `with_available` is true exactly when the version is at least 2.5. Zel does not expose those flags.
  - [P3] `is_instance(obj, klass)` returns `issubclass(type(obj), klass)`, so the check uses the object's type and does not read `__class__`. Zel has no such helper.
  - [P3] `SomeClass` is a new-style class with `class_attribute` set to `None` and a `wibble` method that returns `None`. `X` is an empty new-style class. Zel has neither fixture.
  - [P3] If the builtin `next` is missing, `next(obj)` is defined as `obj.next()`. Otherwise the builtin is kept. Zel has no equivalent polyfill.

### `third_party/py/mock/tests/support_with.py`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `nested` is absent: import `contextlib.nested` when present, otherwise enter managers left to right, yield their `__enter__` results, exit in reverse, and let a true `__exit__` clear the active exception before re-raising whatever remains.
  - [P3] `catch_warnings` is absent: import `warnings.catch_warnings` when present, otherwise copy `warnings.filters`, optionally replace `showwarning` to record `WarningMessage`s, restore both on exit, and reject a second enter or an exit without enter. The fallback ignores its `module` argument and always uses `sys.modules['warnings']`.
  - [P3] `examine_warnings` is absent: it wraps a no-argument function so the call runs inside `catch_warnings(record=True)` and receives the recorded warning list.

### `third_party/py/mock/tests/testcallable.py`
- Bazel lines: 158
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not vendor mock 1.0.1. There is no `NonCallableMock`, `NonCallableMagicMock`, `CallableMixin`, `Mock`, `MagicMock`, `patch`, or `create_autospec`.
  - [P3] Calling `NonCallableMagicMock()` or `NonCallableMock()` raises `TypeError`, the object has no `__call__`, and `repr` includes the class name. Zel has no non-callable mock.
  - [P3] `MagicMock` subclasses `Mock`, and `NonCallableMagicMock` subclasses `NonCallableMock`. A child of `NonCallableMock` is a `Mock` subclass, and a child of `NonCallableMagicMock` is a `MagicMock` subclass. Subclasses of `Mock` or `MagicMock` produce children of that same subclass. Zel has no such hierarchy.
  - [P3] `patch` of a non-callable class with `spec=True` or `spec_set=True` returns a callable mock whose instance is a `NonCallableMagicMock` (not a `CallableMixin`) and raises `TypeError` when called. `spec` or `spec_set` of an instance makes the patched object itself non-callable. Zel has no `patch` spec.
  - [P3] `patch` with `spec` or `spec_set` set to a callable class, including a subclass, multiple inheritance, and old-style classes with `__call__`, returns a `MagicMock` instance that inherits the spec (`AttributeError` on an unknown name), is itself callable, and whose call result has no spec, so further calls and child calls are recorded. Zel does not do this.
  - [P3] `create_autospec` of a non-callable class yields a callable mock whose instance raises `TypeError`. Autospec of an instance is not callable. `create_autospec(SomeClass, instance=True)` is not callable, `wibble()` is recorded, and extra positional arguments raise `TypeError`. Zel has no autospec signature check.

### `third_party/py/mock/tests/testhelpers.py`
- Bazel lines: 940
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No `ANY` sentinel that compares equal to every object, including `datetime` values and objects whose `__eq__` returns false. `repr` and `str` are `<ANY>`, and it matches positional and keyword arguments in `assert_called_with` and in `mock_calls` in either comparison order.
  - [P3] No `_Call` / `call` value. An empty call equals every name/args/kwargs tuple. A call with an empty name matches any name when the arguments agree. A non-empty name must match. Arguments and keyword arguments must match. Equality is symmetric with `!=`, and non-tuples never match. `two=True` exposes a length-2 `(args, kwargs)` pair that still equals the normal three-tuple form.
  - [P3] No call formatting or recording. `repr(call)` and `str(call)` are `call`. Instances render as `call()`, `call.foo()`, or `call(1, 2, 3, a='b')`. Chained reprs keep only the last call's arguments (`call(1)(2)` is `call()(2)`, `call(1).foo.bar(a=3).bing` is `call().foo.bar().bing`). `call()` is `('', (), {})`, attribute calls use dotted names, and a chain such as `call(1).foo(2).bar(3, a=4)` is `('().foo().bar', (3,), {'a': 4})`. `call_list()` expands the chain, including `int(...)` as `__int__()`. `call == ANY`, so `mock_calls` can equal `[ANY]`.
  - [P3] No `create_autospec` signature checks for classes, instances, functions, nested attributes, list instances (treated as the list type, not as a spec of strings), builtins, descriptors, `__slots__`, properties, or `None` attributes. Missing attributes raise `AttributeError`. Too few or too many arguments raise `TypeError`. `spec_set=True` also blocks new attributes. `return_value` overrides the result of calling a function or class spec. `reset_mock` clears magic-method counts such as `__int__`. Class and instance specs share method signatures. A class mock is callable from `__init__` and is not the same object as its return value. An instance spec is not callable unless the spec defines `__call__`. Nested classes, recursive specs, and function attributes copied onto the spec are checked separately, so shared attribute identity on the real class is not shared on the mock. Speccing `int`, `str`, `dict`, `list`, `set`, `float`, `complex`, and `bool` values and types does not recurse forever. A data descriptor that raises `AttributeError` on the class is skipped and left unspecced. A property or slot is an unspecced `MagicMock`. On Python 3, keyword-only parameters are enforced. On Python 2, old-style classes get the same signature checks. A class with no `__init__` can still be specced.
  - [P3] No `_CallList`. `call_args_list` would be one, and it would contain both single calls and contiguous call subsequences, but not absent calls. `str(mock_calls)` would be the multiline `[call(...),\n call.foo(...), ...]` form, including intermediate calls in a chain.
  - [P3] No `PropertyMock`. Patching an attribute with it records a no-arg call on read and `call(value)` on write. Used as a class attribute, a read calls it once and returns a `MagicMock` that is not itself a `PropertyMock`.

### `third_party/py/mock/tests/testmagicmethods.py`
- Bazel lines: 486
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no Python `Mock`/`MagicMock` magic-method protocol. Deleting `__getitem__` is not modeled, and subscripting after that delete is not specified to raise `TypeError`.
  - [P3] Assigning a callable magic method does not wrap it so the mock is passed as `self`, and assigning the mock object itself is not left unwrapped.
  - [P3] Magic methods are not isolated per mock. Default `repr` (`<Mock id='...'>`), `str` (`object.__str__`), and Python 2 `unicode` are absent, including overrides of `__repr__`, `__str__`, and `__unicode__`.
  - [P3] Unset `__getitem__`, `__setitem__`, and `__delitem__` do not raise `TypeError`, and there is no delegation once they are assigned.
  - [P3] `__add__`, `__iadd__`, and `__radd__` are absent, including in-place `+=` keeping the same mock. Python 2 `__truediv__`/`__rtruediv__` and the Python 3 absence of those attributes on `MagicMock` are absent.
  - [P3] Default `hash` delegation to `Mock.__hash__`, default `bool` true, and `__bool__`/`__nonzero__` overrides are absent.
  - [P3] Default ordering (Python 2 like `object()`, Python 3 `TypeError` between `MagicMock`s) and custom rich comparisons are absent.
  - [P3] Identity equality (`mock == mock` is `True`, other objects `False`, results are `bool`) and custom `__eq__`/`__ne__`, including `MagicMock` return values coerced to `bool`, are absent.
  - [P3] Unset `len`, `iter`, and `in` do not raise `TypeError`. `MagicMock` does not expose the `_magics` set, reject unknown names, or swap `__bool__`/`__nonzero__` by language version.
  - [P3] `MagicMock` numeric and container defaults are absent: `int` 1, `complex` 1j, `float` 1.0, `long` 1, `len` 0, empty iteration, `not in`, `hash`/`str`/`unicode` like `object`, `bool` true, and `oct`/`hex` (`'1'`/`'0x1'` on Python 2, `'0o1'`/`'0x1'` via `__index__` on Python 3).
  - [P3] `__cmp__` is not refused on `MagicMock` and not honored on `Mock`. `spec` and `spec_set` do not hide magic methods absent from the spec, allow only those on the spec, or make `MagicMock(spec=Iterable)` iterate empty.
  - [P3] Setting `__setattr__` does not raise `AttributeError` with message `Attempting to set unsupported magic method '__setattr__'.`
  - [P3] Attribute access and calls do not yield `MagicMock` children. Magic methods are not themselves `MagicMock`s (or the `MagicMock` subclass), chained indexing is absent, and `reset_mock` does not clear a magic method's `called` flag.
  - [P3] `__dir__` overrides, `TypeError` when a real bound method is used as `__iter__`, class-level descriptor `return_value` (for example `type(m).__str__`), and reuse of a list `__iter__.return_value` versus exhaustion of an iterator return value are absent.

### `third_party/py/mock/tests/testmock.py`
- Bazel lines: 1351
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel does not vendor or reimplement the Python `mock` package these tests exercise (`Mock`, `MagicMock`, `NonCallableMock`, `NonCallableMagicMock`, `create_autospec`).
  - [P3] No counterpart for mock construction and call state: `called`, `call_count`, `return_value`, `call_args`, `call_args_list`, `method_calls`, `mock_calls`, parent and child tracking, or `repr` (name chains, calls, `spec` and `spec_set`).
  - [P3] No counterpart for `side_effect` as a callable, iterator, or exception, including `DEFAULT` falling through to `return_value`, or for `reset_mock` clearing history while keeping children, side effects, and return-value identity.
  - [P3] No counterpart for `assert_called_with`, `assert_called_once_with`, `assert_has_calls` (ordered and any order), `assert_any_call`, or their failure messages.
  - [P3] No counterpart for auto child attributes, `spec` and `spec_set` restrictions, `wraps`, `configure_mock`, `mock_add_spec`, `attach_mock`, magic methods in `mock_calls`, `dir` filtering, attribute deletion, `copy`, or pickle.

### `third_party/py/mock/tests/testpatch.py`
- Bazel lines: 1809
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no `mock.patch` / `patch.object` implementation: no temporary attribute replacement (including `None`), no restore-after-call, and no lazy target lookup until the patched callable runs.
  - [P3] No default `MagicMock` replacement, and no `spec`, `spec_set`, list/boolean spec, inherited spec, or `autospec` (functions, objects, keywords, `new`, call signatures, and repr).
  - [P3] No `create` flag: missing attributes are not refused by default and not installed-then-deleted when creation is requested. Static methods, `__slots__`, descriptors, and get/set/delete proxies are not patched.
  - [P3] No `patch.dict` for real mappings or mapping-like containers, including `clear`, string targets, keyword values, and restore on exit.
  - [P3] No `patch.multiple` (several attributes, create, spec, `new_callable`, stacked and class decorators, or rollback when a later patch fails).
  - [P3] No patcher `start`/`stop`, error on `stop` without `start`, or `stopall`. Class decorators do not wrap only `test*` (or a custom prefix) and inject mocks.
  - [P3] No `new_callable` factory with forwarded keywords, incompatibility with `new` and `autospec`, or restoration when the factory or a nested patch raises. Exception propagation, lazy imports, and `wraps` on patch/object/dict/multiple are absent.

### `third_party/py/mock/tests/testsentinel.py`
- Bazel lines: 33
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no sentinel factory that caches one object per attribute name, so repeated `sentinel.whatever` is not the same value and `sentinel.whatever` is not distinct from `sentinel.whateverelse`.
  - [P3] Zel has no sentinel whose string form is `sentinel.<name>`.
  - [P3] Zel has no `DEFAULT` that is the same object as `sentinel.DEFAULT`.
  - [P3] Zel has no `sentinel.__bases__` access that raises `AttributeError`.

### `third_party/py/mock/tests/testwith.py`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] On Python 2.5 and newer this module loads the `with`-statement suite. Zel has no `patch` context manager that replaces an attribute for the block, restores it on exit, and still restores it when the block raises without swallowing the exception.
  - [P3] No `with patch(...) as mock` that binds a `MagicMock` or a caller-supplied replacement, and no `patch.object` that restores the original attribute on exit.
  - [P3] No nested patches of two targets, and no double patch of the same attribute where the inner block restores the outer mock and the outer block restores the original.
  - [P3] No `patch.dict` context manager that applies mapping updates for the block and restores the original mapping on exit or when the block raises `NameError`.
  - [P3] No mock-as-context-manager protocol: `with mock as m` does not call `__enter__` and `__exit__(None, None, None)` or bind `m` to `__enter__`'s return value, and a `MagicMock` used as a context manager does not record `__enter__` and `__exit__` when the body raises `TypeError`.
  - [P3] No `mock_open`: patching `open` does not record the `open(path)` call; `with open(...) as f` does not record `__enter__`, `read`, and `__exit__(None, None, None)` with `f` as the handle; an explicit mock is not left as the patched `open`; and `read_data` does not make `read()` return that string.
  - [P3] On Python 2.4 this module does not substitute one skipped `testWith`. Zel has no such version gate.

### `third_party/remoteapis/build/bazel/remote/asset/v1/remote_asset.proto`
- Bazel lines: 495
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel defines no Remote Asset API: no `Qualifier`, `Fetch` (`FetchBlob` / `FetchDirectory`), or `Push` (`PushBlob` / `PushDirectory`) messages or RPCs.
  - [P2] No fetch contract: URI/URN to CAS digest resolution, unique unordered qualifiers, at least one URI with any-URI match, timeout that may outlive the RPC, `oldest_content_accepted`, SHA256 default digest function, inline fetch `status` versus RPC status, `expires_at`, or asset transforms.
  - [P2] No push contract: URI/qualifier association with a blob or directory digest, `expire_at`, `references_blobs` / `references_directories` lifetime hints, or digest-function inference from hash length.
  - [P2] `--experimental_remote_downloader` and related flags exist only as option text. Repository downloads do not call this API.

### `third_party/remoteapis/build/bazel/remote/execution/v2/remote_execution.proto`
- Bazel lines: 2511
- Zel counterpart: `/workspace/zel-cursor/crates/zel-proto/src/reapi.rs`
- Verdict: partial
- Differences:
  - [P2] `ContentAddressableStorage.SplitBlob` and `SpliceBlob` are not implemented. `ChunkingFunction`, `PriorityCapabilities`, `FastCdc2020Params`, and `RepMaxCdcParams` are absent, and `CacheCapabilities` drops `cache_priority_capabilities`, `max_cas_blob_size_bytes`, `split_blob_support`, `splice_blob_support`, and both CDC parameter fields, so chunked CAS and advertised priority or blob-size limits are ignored.
  - [P1] `GetTree` is a server stream in the proto. The client issues it as a unary RPC and only follows `next_page_token` with further unary calls, so a server that streams more than one `GetTreeResponse` fails the call.
  - [P2] Decode drops `ExecuteResponse.server_logs`, `ExecutedActionMetadata.virtual_execution_duration` and `auxiliary_metadata`, `ExecuteOperationMetadata.partial_execution_metadata` and `digest_function`, `Command.output_directory_format`, and `ExecutionCapabilities.execution_priority_capabilities`.
  - [P1] Platform requirements are written only on `Command.platform`. `Action.platform` stays unset, so a v2.2 server that prefers the action field sees no platform.
  - [P2] Only SHA-256 and MD5 are implemented. SHA-1, SHA-384, SHA-512, VSO, Murmur3, SHA256TREE, BLAKE3, and `GITSHA1` (enum 10, not even named) cannot be produced or checked. Batch and ByteStream transfers stay identity-compressed.
  - [P2] Service comments say clients should honor `google.rpc.RetryInfo`. Retries use a local delay and never read that detail.
  - [P3] The unused `remote_execution` module in `/workspace/zel-cursor/crates/zel-proto/src/lib.rs` encodes `Action.do_not_cache` as field 6, which is `timeout` in this proto, and omits `timeout`, `salt`, `platform`, and `output_paths`.
