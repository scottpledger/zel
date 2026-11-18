# Bazel vs zel code differences: starlark language builtins

Category rule: Starlark interpreter/API/doc extraction code (`net/starlark`, `starlark*`) and `src/main/starlark/` builtins and tests.

- Failure entries: 316
- Missing: 151
- Partial: 165
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/ActionApi.java`
- Bazel lines: 148
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Starlark `Action` value, so `mnemonic`, `inputs`, `outputs`, `argv`, `args`, `content`, `substitutions`, `env`, and `execution_info` cannot be read.
  - Bazel returns `None` for `argv`, `args`, `content`, and `substitutions` when that action kind does not support them. `content` is also `None` when a parameter-file write still has inputs, because directory expansion is unknown at analysis.
  - `execution_info` exists only with `--experimental_google_legacy_api`. Without that flag the field is absent.
  - `inputs` and `outputs` are depsets (`outputs` in stable order). `env` and `substitutions` are insertion-ordered dicts. The internal action record stores inputs and outputs as vectors and env, execution requirements, and lowered template substitutions in a `BTreeMap` (sorted keys). File-write `content` is a Java string written as Latin-1 bytes. Zel renders those bytes as UTF-8.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/ActionsInfoProviderApi.java`
- Bazel lines: 28
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - Calling `Actions(...)` fails in Bazel: this provider type declares no methods and `ActionsProvider` has no `selfCall` constructor. Zel binds the `.bzl` global `Actions` as a generic `Value::Provider`, so the call succeeds and builds a `ProviderInstance` from keyword arguments, silently dropping positional arguments.
  - Bazel only materializes `Actions` as a struct whose sole field is `by_file` (output artifact to the first action that produced it, `HashMap.putIfAbsent`). Zel never builds that map, stores arbitrary keywords in a `BTreeMap`, and does not implement `ctx.created_actions()` (`Actions` when the rule is `_skylark_testable`, otherwise `None`).

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/BuildConfigurationApi.java`
- Bazel lines: 123
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_host.rs`
- Verdict: partial
- Differences:
  - `bin_dir` and `genfiles_dir` are missing on the configuration object, so attribute access fails. Bazel returns the main-repository output roots (`bazel-out/<mnemonic>/bin`, and `genfiles` when `--incompatible_merge_genfiles_directory` is off; `--experimental_sibling_repository_layout` changes the root path).
  - `host_path_separator` is always `:`. Bazel returns `;` on Windows and `:` otherwise.
  - `default_shell_env` is the fixed action environment in sorted key order. On Windows, Bazel stores the MSYS/System32 `PATH` when `--incompatible_strict_action_env` is on, and the client `PATH` when it is off. Zel stores `/bin:/usr/bin:/usr/local/bin` when strict and leaves `PATH` inherited otherwise. The returned dict is mutable; Bazel returns an immutable map.
  - `test_env` always returns the fixed `--test_env` entries. Bazel returns an empty map when `TestOptions` are trimmed (`--trim_test_configuration`, the default for non-test rules).
  - `short_id` is the output-directory mnemonic (`<cpu>-<mode>[-exec][-ST-<hash>]`). Bazel returns the first 7 hex characters of the configuration checksum, which covers every option.
  - `stamp_binaries()`, `has_separate_genfiles_directory()`, `is_sibling_repository_layout()`, `runfiles_enabled()`, and `disabled_features()` run from any `.bzl` and ignore extra arguments. Bazel rejects callers outside the builtins repository and the internal allowlist with `file '%s' cannot use private API`, and rejects extra arguments.
  - `stamp_binaries()` is false for every exec configuration even when the `stamp` option is still true. Bazel returns only `CoreOptions.stampBinaries`.
  - `has_separate_genfiles_directory()` is always false. Bazel returns the inverse of `--incompatible_merge_genfiles_directory` (default true, so the default matches).
  - `is_sibling_repository_layout()` is always false. Bazel follows `--experimental_sibling_repository_layout`.
  - `runfiles_enabled()` is off only for the exact strings `0`, `false`, and `no`, and for unset or `auto` only when the binary is compiled for Windows. Bazel lowercases the tri-state, also treats `f` and `n` as off, and treats `auto` as off when the current OS is Windows.
  - `disabled_features()` is missing and the lookup fails. Bazel returns an immutable set of default-off feature names from `--features` (the exec configuration uses `--host_features`), with `no_layering_check` recorded as disabling `layering_check`, in sorted order.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/CommandLineArgsApi.java`
- Bazel lines: 522
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/args.rs`
- Verdict: partial
- Differences:
  - `map_each`, depset flattening, and string conversion run at the call. The API defers them to execution and discards `print()` from `map_each`.
  - `allow_closure` is ignored, so a nested `map_each` is always accepted. The API rejects it unless `allow_closure` is true.
  - Labels are always canonical (`//pkg:name`, `@@canonical//pkg:name`), not the apparent repo form (`@repo//pkg:name`). Values other than string, int, `File`, and `Label` are rejected instead of stringified. `add` appends a directory `File`’s path; the API requires `add_all` or `add_joined`. A two-argument arg name is stringified (int, `File`, `Label`) instead of being an unprocessed string. `None` becomes `"None"` only as the two-argument `value`.
  - `expand_directories` runs only for `add_all` without `map_each`, and only later via a tree placeholder. With `map_each`, directories are not expanded first, and the extra argument is a dummy passed only when a user function has two required positional parameters, not whenever it declares two or more. `add_joined` never expands directories. An empty tree still keeps the arg name and `terminate_with` under `omit_if_empty`, and an empty `before_each` is dropped when that tree is expanded.
  - `add_all` dedupes input values and mapped strings before `format_each`. `add_joined` dedupes only input values, so duplicate strings from `map_each` stay in the join. The API dedupes derived strings after directory expansion for both methods.
  - Named-only `add.format`, `add_joined.join_with`, and `use_param_file.use_always` are accepted positionally. Bool parameters use truthiness (`"false"` is true). A non-string `join_with` is reported as missing.
  - `param_file_arg` is a raw `%s` substring check and replace, so `%%` is not an escaped percent (`100%%-%s` stays `100%%-<path>`) and `%d` or a trailing `%` is accepted. With `use_always` false, spill uses a fixed 128 KiB argument-byte threshold.
  - `set_param_file_format("flag_per_line")` is stored, but the file is written like `"multiline"` (every argument plus a newline). The API writes only `--` flags, joins each flag and its value with `=`, and leaves other arguments on the command line. `"shell"` and `"multiline"` follow the documented quoting, and the default format is shell.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/DefaultInfoApi.java`
- Bazel lines: 192
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_driver.rs`
- Verdict: partial
- Differences:
  - `DefaultInfo` is a generic provider call: positional arguments are ignored, and unknown keywords are stored on the instance. The API constructor accepts only the keyword parameters `files`, `runfiles`, `data_runfiles`, `default_runfiles`, and `executable`, each defaulting to `None`.
  - `allowedTypes` (`depset` or `None`, `runfiles` or `None`, `File` or `None`) are checked only when a rule return value is finalized. A bad argument does not fail at the `DefaultInfo(...)` call, and an explicit `None` is treated as an omitted parameter.
  - The call result is the kwargs map, so `runfiles` and `executable` are fields and `files_to_run` is absent. The API instance always exposes `files`, `files_to_run`, `data_runfiles`, and `default_runfiles`, and each may be `None`. Omitted parameters are missing attributes on the Zel value.
  - Rule finalization always publishes those four fields as values. Omitted `files` becomes a new default-order depset of the predeclared outputs plus the executable; a supplied depset is flattened and deduplicated by exec path. `files_to_run` is always a `FilesToRunProvider`. Omitted runfiles become empty runfiles, with the executable merged into both sets when `runfiles` is set and into `default_runfiles` when `data_runfiles` or `default_runfiles` is used on an executable or test rule.
  - Provider fields are a `BTreeMap`, so iteration order is sorted by name (`data_runfiles`, `default_runfiles`, `files`, `files_to_run`) rather than the API declaration order (`files`, `files_to_run`, `data_runfiles`, `default_runfiles`).

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/DirectoryExpander.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/cc_native.rs`
- Verdict: partial
- Differences:
  - `expand` on a directory always returns a one-element list of that directory. The interface returns the files recursively underneath a directory, and raises `EvalException` when that directory is not an action input.
  - A non-`File`, a missing argument, or a keyword is rejected with `TreeArtifactExpander.expand: expected a File` after reading only positional index 1 (extra positionals are ignored). The interface takes exactly one positional, non-named `File` and fails in Starlark argument checking before `list` runs. A plain file still returns a one-element list of itself.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/ExecGroupApi.java`
- Bazel lines: 23
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader_host.rs`
- Verdict: partial
- Differences:
  - Bazel’s Starlark type name is `exec_group`. Zel stores the value as a plain struct, so `type()` is `struct` and ordered comparisons report `struct <=> struct`.
  - The interface has no fields or methods: `dir` is empty and attribute access fails. Zel exposes every constructor keyword plus `_exec_group`, `dir` lists those keys, and `+` merges two structs.
  - `repr`/`str` is `<unknown object com.google.devtools.build.lib.packages.DeclaredExecGroup>`. Zel prints `struct(...)` with `BTreeMap` key order, including `_exec_group = True`.
  - The value is unhashable (`unhashable type: 'exec_group'`). Zel structs are hashable when nested values are, including lists, so the value can be a dict key.
  - `==` uses Java record equality on the toolchain map, exec constraints, and copy-from-default flag, and only against the same Starlark type. Zel equates any struct with the same fields, so the synthetic `_exec_group` flag participates and a hand-built struct can compare equal.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/ExpandedDirectoryApi.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Starlark `ExpandedDirectory` value exposing struct fields `directory` (the expanded input directory as a `File`) and `children` (the list of files in that directory).

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/FileApi.java`
- Bazel lines: 144
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/objects.rs`
- Verdict: partial
- Differences:
  - `dirname` is the parent of the exec path via `getSafePathString()`: a single-segment relative path is `"."`, and a null parent (empty path or filesystem root) is `"/"`. Zel slices `exec_path` at the last `/` and returns `""` when there is no slash or the only slash is at index 0.
  - `dirname`, `path`, and `root` take Starlark semantics and run the exec path through `PathMapper` (`path` is the mapped exec path; `root.path` is the mapped root). Zel returns the stored exec path and an unmapped root path string.
  - `root` is a `root` value, equal by filesystem root, exec path, and root type. Zel returns a struct equal by that path string, so distinct source roots (both path `""`) compare equal.
  - `basename` and `extension` come from `execPath`. Zel uses `root_relative_path`. `getFileExtension` ignores a `.` at index `<= driveStrLength`, so a relative path whose only dot is the first character (`.bashrc`) has extension `""`; Zel’s `rfind('.')` yields `bashrc`.
  - `owner` may be `None` (`NULL_OWNER`). Zel always returns a `Label`.
  - `is_directory` is true for a tree artifact or a fileset. Zel is true only when `is_tree_artifact` is set.
  - `short_path` follows the artifact root: a legacy root rewrites a root-relative path that starts with `external` to `../…`; a non-legacy external root prepends `../` plus exec-path segment 1. Zel rewrites from the owner label’s repository, stripping an `external/<repo>/` prefix when present, and has no sibling-repository root case.
  - `tree_relative_path` returns the parent-relative path for a tree-file artifact and otherwise fails with `tree_relative_path not allowed for files that are not tree artifact files.` Zel always fails, with `'tree_relative_path' is only available on files inside a tree artifact; '<exec path>' is not`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/FileProviderApi.java`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Starlark `file_provider.files_to_build` is a struct field that returns a depset of files. Zel has no `file_provider` value or `files_to_build` field, so that access is absent.
  - The only `FileProvider` value is an opaque query host object with no fields. The same file set is stored on `DefaultInfo.files` and exposed as `DefaultInfo.files` and `target.files`, not through this provider.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/FileRootApi.java`
- Bazel lines: 38
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/objects.rs`
- Verdict: partial
- Differences:
  - Zel exposes a generic `struct`, so `type()` is `"struct"` and `str`/`repr` are `struct(path = "...")`. Bazel’s type is `root`, and `str`/`repr` are `"<source root>"`, `"<derived root>"`, or `"<mapped root>"` with no path in the text.
  - Equality and dict/set hashing use only the `path` string. Bazel `ArtifactRoot` equality is filesystem root + exec path + root type, and a mapped root is never equal to an unmapped root, so distinct source roots (all `path == ""`) compare equal in Zel and unequal in Bazel.
  - Struct `+` merges fields. A `root` has no `+`. `len` returns the field count (`1`). `"path" in value` is true. `json.encode` emits `{"path":"..."}`. Those operations fail on a Bazel `root`.
  - A missing field is reported as a `struct` error that lists `Available attributes: path`. Bazel reports that a `root` has no such field or method.
  - `args.add` on a Bazel root emits `path` through `CommandLineItem`. Zel rejects the struct and will not convert type `struct` to a command-line argument.
  - `File.root.path` is never path-mapped. Bazel applies `PathMapper.mapRoot` first, so `path` can be the mapped root exec path.
  - Zel’s `path` is `""` for sources; otherwise it strips `root_relative_path` as a raw string suffix of the exec path and trims every trailing `/`, or returns the whole exec path when that suffix is absent. Bazel returns the root’s stored exec path, which is empty for every source root, including external ones.
  - `ctx.bin_dir.path` and `ctx.genfiles_dir.path` are always `bazel-out/<mnemonic>/bin`. Bazel uses that only when genfiles are merged and sibling repository layout is off; otherwise genfiles is `bazel-out/<mnemonic>/genfiles`, and sibling layout inserts the repository (`bazel-out/<repo>/<mnemonic>/bin` or `genfiles`). `configuration.bin_dir` and `configuration.genfiles_dir` are absent on Zel’s configuration object.
  - `path` is a Rust UTF-8 string. Bazel returns the internal `PathFragment` string (Latin-1 bytes in a Java `String`) with no Unicode conversion.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/FilesToRunProviderApi.java`
- Bazel lines: 58
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_driver.rs`
- Verdict: partial
- Differences:
  - Analysis-built values always expose a fourth Starlark field, `_runfiles`. The Java API only defines `executable`, `runfiles_manifest`, and `repo_mapping_manifest`, each a File or None.
  - Zel predeclares `FilesToRunProvider` as a callable provider. A call succeeds and stores only the kwargs given, so a missing name errors (`field '…' not found`) instead of being None. This builtin has no Starlark constructor, so those three fields are always present.
  - `default_info_provider` always sets both manifest fields to None. Starlark executable and test rules set both to files whenever default runfiles are non-empty, and do not honor `--nobuild_runfile_manifests`. The Java getters are null independently when there is no runfiles support, when the runfiles manifest artifact is null, or when there is no repo-mapping artifact.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/FilesetEntryApi.java`
- Bazel lines: 22
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel declares an empty undocumented Starlark value type named `FilesetEntry` (`StarlarkBuiltin`, no fields or methods). Zel registers no such type.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/FragmentCollectionApi.java`
- Bazel lines: 50
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_host.rs`
- Verdict: partial
- Differences:
  - Starlark type name is `fragments`. Zel’s `ctx.fragments` is a generic struct, so `type()` is `struct`.
  - An unknown field is `There is no configuration fragment named '<name>'. Available fragments: <fields>` where `<fields>` is `getFieldNames()` joined by `', '` and wrapped in one pair of quotes (`'cpp', 'java'`; no names yields `''`). Zel reports `'struct' value has no field or method '<name>'`, then a newline and `Available attributes: ` plus names joined by `", "` with no quotes.
  - That list is `getFieldNames()` in collection order (for a rule, every Starlark-visible fragment, in Java fragment-class name order). Zel lists only fragments declared on the rule or aspect, sorted as `BTreeMap` keys.
  - This message runs only when the field value is null (name is not a fragment). A known fragment the rule did not declare throws before that (`has to declare '…' as a required fragment`). Zel uses the same missing-struct-field error for every undeclared name, and a declared name that is not a real fragment is an empty struct.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/LateBoundDefaultApi.java`
- Bazel lines: 33
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Starlark builtin type named `LateBoundDefault`. `configuration_field()` returns a plain struct, so `type()` is `"struct"` and `fragment`, `name`, and `_configuration_field` are readable fields. Bazel values of this type are opaque `StarlarkValue`s: `type()` is `"LateBoundDefault"` and the interface defines no fields or methods.
  - Those structs compare equal by field contents and are hashable dict keys. A Bazel `LateBoundDefault` uses identity equality and is unhashable (`isImmutable()` stays false, so `checkHashable` fails with `unhashable type: 'LateBoundDefault'`).

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/LicenseApi.java`
- Bazel lines: 29
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel exposes a documented Starlark builtin type named `License` (a `StarlarkValue` with no methods). Zel has no `License` value; `LICENSE` attributes are `StringList`, and the `License.NONE` default is dropped (`gen_default` returns `None`).

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/MacroFunctionApi.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader_host.rs`
- Verdict: partial
- Differences:
  - `type()` on a symbolic-macro value is `"host_object"` (`Value::Host("macro", …)` misses the `type_name` match). `@StarlarkBuiltin(name = "macro")` makes Bazel return `"macro"`.
  - The interface adds no fields or methods, so Bazel attribute access fails with `'macro' value has no field or method '…'`. Zel routes `attr:macro.…` and `method:macro.…` through the host hook and fails as `unknown native function '…'`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/MaterializedDepsInfoApi.java`
- Bazel lines: 64
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Starlark builtin `MaterializedDepsInfo`: the self-call constructor that requires a `deps` sequence (positional and named) and the `deps` struct field are absent.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/NativeComputedDefaultApi.java`
- Bazel lines: 26
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel registers an empty, undocumented Starlark builtin type named `NativeComputedDefault` (`documented = false`) with no methods or fields, so values of that type are a distinct Starlark type. Zel has no such type; a user function is the only computed-default form, and a builtin function passed as an attribute default is dropped rather than stored as a native computed default.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/OutputGroupInfoApi.java`
- Bazel lines: 54
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`
- Verdict: partial
- Differences:
  - The annotated constructor takes only `extraKeywords` (`kwargs`, default `{}`) and no positional parameters, so Bazel fails the call with `OutputGroupInfo() got unexpected positional argument`. Zel’s `Value::Provider` call drops positional arguments and still returns a provider built from the keyword map (empty if only positionals were passed).
  - Bazel’s constructor throws `EvalException` unless every keyword value is a sequence or depset of `File`s, and it stores each accepted sequence as a stable-order depset. Zel’s call always succeeds and keeps the original list, tuple, or depset on the instance; the sequence/depset/`File` check runs later, only when analysis applies a rule or aspect result.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/PackageSpecificationProviderApi.java`
- Bazel lines: 43
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/lib.rs`
- Verdict: partial
- Differences:
  - `PackageSpecificationInfo.contains(target)` is not implemented. A call fails with `type 'PackageSpecificationInfo' has no method 'contains'` instead of returning whether the target's package is in the group's transitive package specifications.
  - The provider instance stores only a `label` field and does not carry those specifications, so direct packages, `//pkg/...`, negatives, and `includes` are not evaluated.
  - Bazel accepts a `Label` or a canonical label string and rejects other types and invalid label strings. Zel never performs that check.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/RuleFunctionApi.java`
- Bazel lines: 35
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/value.rs`
- Verdict: partial
- Differences:
  - Bazel values of this type report `type()` as `rule`. Zel native rules are `BuiltinFunction("host:rule.NAME")`, so `type()` is `builtin_function_or_method`, and `repr` is `<built-in method NAME of rule value>` because the name is split on the first `.`. Bazel prints `<built-in rule NAME>`.
  - Starlark rule values are `Host("rule", index)`. `type()` is `rule`. `repr` and `str` are `<rule {index}>` (the registry index) both before and after export. Bazel prints `<rule>` before export and `<rule {class name}>` after.
  - `RuleCallable` is not hashable. Bazel rule functions are immutable and hashable. Equality compares only `rule_class`, so distinct callables with the same class name compare equal and `executable` / `test` are ignored. Bazel uses object identity for native rules and the export identity token for Starlark rules. A call drops positional arguments, records kwargs, and returns `None` without adding a package target. Bazel rejects a non-empty positional argument list and adds the rule to the package, still returning `None`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/RunEnvironmentInfoApi.java`
- Bazel lines: 111
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_driver.rs`
- Verdict: partial
- Differences:
  - Positional arguments are accepted (`environment`, then `inherited_environment`). Zel keeps only keywords for this provider, so a positional call is stored with no fields and becomes an empty environment and an empty inherited list.
  - Omitted fields match the defaults `{}` and `[]` only when the provider is read during analysis. The Starlark value does not materialize those fields, and an explicit `None` is treated as omitted instead of failing the dict or sequence cast.
  - String-dict and string-sequence checks run when a rule returns the provider, not at the call. Unknown keywords are kept. A depset is accepted for `inherited_environment`; Bazel allows only a sequence.
  - Fixed variables are copied into a `BTreeMap`, so key order is sorted rather than Starlark dict insertion order. Inherited names stay ordered, including duplicates, until later copied into a `BTreeSet`.
  - Returning this provider from a non-executable, non-test rule is a rule error in Bazel and the provider is dropped. Zel records it and analysis succeeds.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/RunfilesApi.java`
- Bazel lines: 101
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_actions.rs`
- Verdict: partial
- Differences:
  - `merge` and `merge_all` take positional-only `other`. Zel also accepts the keyword `other`.
  - `merge_all` accepts only a sequence of runfiles. Zel also accepts a depset.
  - An empty `merge`, or a `merge_all` with one non-empty object, returns that existing instance (`isEmpty` ignores empty-file suppliers). Zel always returns a new value, so `==` differs. Zel `is_empty` also counts `empty_filenames`, which changes the nested-set short-circuit.
  - Bazel raises if the merged artifacts, symlinks, or root-symlink nested sets exceed `--nested_set_depth_limit`. Zel does not.
  - `files` is the compile-order artifact nested set. Zel returns a flat default-order depset of `artifacts`, drops nesting, and skips later artifacts with the same exec path.
  - `symlinks` and `root_symlinks` are stable-order nested sets of distinct `SymlinkEntry` instances (no `equals`, so duplicate paths remain). Zel stores them in a `BTreeMap` (`or_insert` keeps the first target) and emits a depset in sorted key order, so conflicting paths are dropped and equal struct elements collapse.
  - `empty_filenames` is a stable-order depset from `EmptyFilesSupplier.getExtraPaths` over symlink paths and artifact runfiles paths. Merge keeps one supplier and fails if two non-dummy suppliers differ. Zel returns the stored string list and appends unseen names, with no supplier check and no recompute against the manifest.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/SplitTransitionProviderApi.java`
- Bazel lines: 22
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel marks a split configuration transition as a Starlark value and defines no methods or fields on that type. Zel has no corresponding type.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkActionFactoryApi.java`
- Bazel lines: 1230
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_actions.rs`
- Verdict: partial
- Differences:
  - `map_directory` is absent, so the call fails as an unknown `actions` method. Its dict arguments, defaults (`additional_inputs`/`additional_params` `{}`, `use_default_shell_env` false, `None` for env, mnemonic, resource set, exec group, toolchain, and execution requirements), and top-level implementation callback are not implemented.
  - Keyword-only parameters are accepted positionally for `declare_*`'s `sibling`, both `do_nothing` arguments, every `symlink` argument, `write`'s `mnemonic` and `execution_requirements`, all of `expand_template`, and all of `transform_version_file` / `transform_info_file`. `run` and `run_shell` reject positionals.
  - `is_executable` and `use_default_shell_env` use Starlark truthiness, so non-bool values are accepted. `inputs=None` and `tools=None` are treated as omitted (`[]` / unbound) instead of a type error.
  - `do_nothing` does not type-check `inputs` (default `[]`, sequence or depset) and does not register an action.
  - `expand_template`'s `substitutions` defaults to `{}` but is required here. A non-`TemplateDict` `computed_substitutions` is ignored, and duplicate substitution keys are kept in insertion order instead of failing.
  - `env` and `execution_requirements` are stored in a `BTreeMap` (sorted keys), not Starlark dict insertion order.
  - `run`/`run_shell` mnemonics allow `_` and only ASCII alphanumerics. The API's letter-or-digit check allows Unicode letters and rejects `_`.
  - `run` ignores `unused_inputs_list`, `shadowed_action`, and `toolchain` (no shadowed inputs/env merge, no toolchain exec group, no toolchain/`exec_group` mismatch error). `input_manifests` stays ignored. An extra `stdout` argument is accepted and allows empty `outputs`.
  - `tools` and a `File` `executable` do not automatically attach `ctx.executable` / `FilesToRunProvider` runfiles. An executable string is not path-normalized. `run_shell` always uses `/bin/bash -c` and always rejects a sequence `command`, including when `--incompatible_run_shell_command_string` is false.
  - `resource_set` callbacks run at analysis with `"linux"` or `"osx"` and the input count before tools are added. The documented `local_test` default is 1; the Rust default is 0. Omitted keys are not filled, and `local_test` is truncated with `as i32`.
  - `symlink` accepts `use_exec_root_for_source` and `target_type` but does not apply them (no exec-root source target, no Windows file-vs-directory symlink kind).
  - `transform_version_file` / `transform_info_file` call the callback at analysis on a fixed unstamped key set, and the version output is an ordinary file, so it is not volatile. `declare_shareable_artifact`'s `artifact_root` is ignored. `declare_directory` after `declare_file` on the same path, and `declare_symlink` after another declaration, are not rejected.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkAspectApi.java`
- Bazel lines: 31
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/aspect.rs`
- Verdict: partial
- Differences:
  - `type()` on an aspect value is `Aspect`. Zel host kind `aspect` is not a `type_name` arm, so `type()` is `host_object` (same string in `getattr` and in `unsupported comparison: host_object <=> host_object`).
  - A missing field is `'Aspect' value has no field or method '<name>'`. Zel routes `aspect.name` to `attr:aspect.<name>`: in a `.bzl` file that is `attr:aspect.<name>() can only be used while evaluating a BUILD file ...`, and in a BUILD file `unknown native function 'attr:aspect.<name>'`.
  - `str`/`repr` of a Starlark aspect is `<aspect>` and of a native aspect `<native aspect>`. Zel prints `<aspect {registry index}>`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkAspectPropagationContextApi.java`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Starlark `propagation_ctx` is built or passed to `propagation_predicate`, `attr_aspects`, or `toolchains_aspects`.
  - `attr` (aspect public parameters only) is absent.
  - `rule.label`, `rule.attr` entries as structs of `value` and `is_tool` (dependency attributes as labels), and `rule.qualified_kind` (`file_label` as None for native rules, plus `rule_name`) are absent.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkAttrModuleApi.java`
- Bazel lines: 1375
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader_host.rs`
- Verdict: partial
- Differences:
  - `attr.label_list_dict` is missing (`attr.label_list_dict() is not supported`). `dormant_label` and `dormant_label_list` stay unavailable even with `--experimental_dormant_deps` (error is "not supported", not the experimental-flag error). `attr.license` stays unavailable even with `--incompatible_no_attr_license=false`.
  - Keyword sets differ: `bool` accepts `values`; `output` and `output_list` accept `configurable` and stay configurable (`select()` is allowed). `label` rejects `skip_validations`, `materializer`, and `for_dependency_resolution`. `label_list` and `label_keyed_string_dict` reject `skip_validations` and `for_dependency_resolution`; `label_list` also rejects `materializer`. `string_keyed_label_dict` rejects `for_dependency_resolution`. Unbound versus explicit `for_dependency_resolution` is not recorded. Explicit `configurable` on a rule is not rejected, and `configurable=True` is treated the same as omitting it.
  - Positional binding does not follow the API. Keyword-only calls such as `attr.int(1)` store the first positional as `default` and drop the rest; a keyword `default` overwrites that with no multiple-values error. `attr.string_list(True)` and `attr.int_list(True)` set `default` instead of `mandatory`. `attr.label_list(False)`, `attr.string_dict(False)`, and the other leading `allow_empty` parameters set `default` instead of `allow_empty=False`. `attr.output_list(False)` ignores the positional, so `allow_empty` stays true.
  - Argument types are not checked at the call. `mandatory` is set only for `Bool(true)`. `allow_empty` forces non-empty only for `Bool(false)`. Non-string `values`, `allow_files`, and `allow_rules` sequences are ignored. `attr.int(values=[...])` never restricts the value, and `int_list` elements are `i64` with no signed 32-bit range check. `executable=True` without `cfg` is accepted. `allow_files` and `allow_single_file` may both be set. `flags` is accepted and discarded. `doc` is accepted and not stored.
  - Omitted `allow_files` means no files only for `label` and `label_list`. `string_keyed_label_dict` and `label_keyed_string_dict` keep any file allowed. String-dict attribute values are `BTreeMap`s, so keys are sorted rather than kept in insertion order. Unexpected keywords are reported one at a time in sorted key order. The returned value's type is `struct`, not `Attribute`.
  - Repository-rule and tag-class evaluation in `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs` does expose `label_list_dict`, but keeps only `default`, truthy `mandatory`, `values`, truthy `allow_empty` (default true), and `doc`, and uses the first positional as `default` except for `label` and `label_list`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkAttributesCollectionApi.java`
- Bazel lines: 74
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_aspect.rs`
- Verdict: partial
- Differences:
  - Zel’s `ctx.rule` struct has only `kind`, `attr`, `file`, `files`, and `executable`. `toolchains` (default exec group of the rule the aspect applies to), `exec_groups` (that rule’s exec groups by name), and `var` (string-to-string configuration variables) are absent, so those reads fail. Bazel still returns `toolchains` and `exec_groups` when the base target has no toolchain context, as not-valid sentinel objects.
  - The Starlark type is `rule_attributes`. Zel reports `struct`, so a missing field errors as `'struct' value has no field or method '…'` and lists available attributes. Field order and `repr` follow `BTreeMap` keys (`attr`, `executable`, `file`, `files`, `kind`) and print as `struct(...)`.
  - Reads of `attr`, `executable`, `file`, `files`, `kind`, `toolchains`, and `exec_groups` fail after the aspect implementation returns (`checkMutable("rule.<field>")`). Zel returns the captured struct fields with no freeze check.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkBuildApiGlobals.java`
- Bazel lines: 116
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader_host.rs`
- Verdict: partial
- Differences:
  - `visibility()` ignores every argument and returns `None`. It does not require `--experimental_bzl_visibility`, `.bzl` initialization, a top-level call, or a single call, and it never records load visibility, so `public`, `private`, `//pkg`, `//pkg/...`, and `[]` do not change who may load the module.
  - `value` is a required positional-only string or list of strings. Zel also accepts zero arguments, extra positionals, `value=`, tuples, and non-strings. Bazel rejects those, rejects negative patterns and reports invalid package specs, treats `[]` as private, and always parses `public` / `private` / `//...` as current-repository load visibility.
  - `configuration_field` does not require `.bzl` initialization, so a helper invoked while evaluating a BUILD file succeeds. Bazel fails that call.
  - Unknown fragment or field names are accepted. Bazel fails at the call. Zel stores the two strings and later resolves them from a smaller field table, then a same-named build option, otherwise no label, and does not capture the tools repository used for default labels.
  - Missing or non-string parameters fail as `configuration_field() missing or non-string argument '...'`. Extra positionals are ignored, and a keyword plus a positional for the same parameter keeps the keyword. The result is a struct with `fragment`, `name`, and `_configuration_field`, not a late-bound default.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkNativeModuleApi.java`
- Bazel lines: 372
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader_host.rs`
- Verdict: partial
- Differences:
  - `glob` and `exports_files` take only one positional argument. Bazel binds every parameter positionally (`exclude`, `exclude_directories`, `allow_empty`, `visibility`, `licenses`).
  - `subpackages` is keyword-only, and `existing_rule` / `package_relative_label` are positional-only. Zel accepts `subpackages(include)` positionally and `name=` / `input=` by keyword.
  - Omitted `glob(allow_empty)` uses `LoaderConfig.allow_empty_glob`, which defaults to true (`loading.rs` treats a missing `--incompatible_disallow_empty_glob` as false). Bazel’s unbound default follows that flag, whose default is true, so an empty match fails.
  - `repository_name` is always callable. Bazel exposes it only while `--incompatible_enable_deprecated_label_apis` is set.
  - `module_name` and `module_version` always return `None`.
  - `package_default_visibility` returns the stored default, or `//visibility:private` when unset. Bazel also concatenates the package (`//pkg:__pkg__`), and a private default becomes that label alone.
  - `existing_rules` is a mutable dict iterated in `BTreeMap` name order. Bazel returns an immutable insertion-ordered view. `existing_rule` attribute keys follow `BTreeSet` order. Bazel uses rule-attribute declaration order, drops computed defaults, turns lists into tuples (including inside `select`), and keeps `select` keys as labels. Zel fills computed defaults with the type’s zero value and runs `select` through `to_value` (lists stay lists, keys and labels become canonical strings). A main-repo label seen from another repo is `//pkg:name` rather than `@@//pkg:name`.
  - `exports_files(..., licenses=...)` accepts the argument and discards it, so a second license declaration is not rejected.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkRuleContextApi.java`
- Bazel lines: 691
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule.rs`
- Verdict: partial
- Differences:
  - `super`, `created_actions`, `check_placeholders`, `package_relative_label`, `info_file`, and `version_file` are unknown ctx fields or methods. `created_actions` never returns `None` for a rule that is not `_skylark_testable`.
  - `ctx.rule` and `ctx.aspect_ids` succeed on a rule implementation (a synthetic `{kind, attr}` struct and an empty list). Bazel raises unless the context is an aspect. On an aspect, `ctx.outputs` is an empty struct. Bazel raises `'outputs' is not defined`.
  - `ctx.build_file_path` always returns `<package>/BUILD`, including when `--incompatible_stop_exporting_build_file_path` is set. Bazel returns the package metadata path of the real BUILD file (`BUILD.bazel` included) and raises when that flag is enabled.
  - `coverage_instrumented` ignores `target` (default `None`) and returns whether coverage collection is on. Bazel returns false when coverage is disabled. Otherwise it applies `--instrumentation_filter` and `--instrument_test_targets` to the current rule, or returns false for a non-rule or Starlark target with no instrumented-files provider.
  - `target_platform_has_constraint` accepts the keyword `constraint_value` and a raw `Label`. The API parameter is positional-only `constraintValue` and must be a `ConstraintValueInfo`.
  - `tokenize` splits on every Unicode whitespace character, so a newline separates tokens. Bazel splits only on space and tab. Inside double quotes, a backslash is dropped before the next Unicode scalar. Bazel keeps the backslash unless the next UTF-16 unit is `\` or `"`. A trailing backslash raises `unterminated escape`, and an open quote raises `unterminated quotation in: ...`. Bazel raises `backslash at end of string` or `unterminated quotation`, wrapped as `while tokenizing`.
  - `expand_location` maps only `srcs`, `deps`, `tools`, and `targets`, and each label expands to `DefaultInfo.files` in sorted path order. Bazel also maps `data` and alias labels, and with default `--incompatible_locations_prefers_executable` uses `DefaultInfo.executable` when it is set. `input` may be passed by keyword (Bazel takes it positionally only). `short_paths` is not allowlist-restricted, and a third positional argument is accepted and ignored.
  - `resolve_command` expands make variables even when `make_variables` is `None`. Bazel leaves the command unchanged in that case. `attribute=None` is reported as attribute `command`. The argv is always `["/bin/bash", "-c", command]`, and `execution_requirements` is ignored. Bazel builds the argv from the execution platform shell and those requirements. Tool inputs are only `DefaultInfo.files`. `label_dict` keys may be targets, and values that are not files are skipped.
  - `resolve_tools` returns the struct `{inputs, input_manifests}` and still succeeds when `--incompatible_disallow_ctx_resolve_tools` is set (Bazel default is true). Bazel returns the tuple `(depset of tool files, including runfiles, [])` or raises under that flag.
  - `runfiles` accepts a `transitive_files` depset of any order. Bazel rejects an order that is not compatible with the default order. `skip_conflict_checking` does not enable or disable symlink conflict errors, and it is not allowlist-restricted. Symlink maps are `BTreeMap`s, so keys are sorted. `collect_data` and `collect_default` follow Starlark truthiness for any value.
  - `features` and `disabled_features` are taken only from the rule `features` attribute (`-` selects the disabled list), as mutable lists. Bazel returns frozen lists of the features enabled or disabled for the rule, including package features and `--features`. `ctx.var` is a mutable dict of `COMPILATION_MODE`, `BINDIR`, `GENDIR`, `TARGET_CPU`, and extra toolchain variables, inserted in sorted key order. Bazel returns a frozen dict of the configuration make variables.
  - `build_setting_value` parses an `int` flag with `i64` and splits a `string_list` flag on commas, so an empty value becomes `[""]`. Bazel returns the typed Starlark value, and a multi-value setting's default is a one-element list.
  - After the implementation returns, field reads stay available. Bazel rejects them because the context is frozen. The frozen check runs for `runfiles` and action registration.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkRuleFunctionsApi.java`
- Bazel lines: 1233
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader_host.rs`
- Verdict: partial
- Differences:
  - `materializer_rule` is not defined, so the call fails instead of creating a materializer rule (`allow_real_deps` is unused).
  - `provider`: keyword-only `fields` and `init` are taken positionally, and extra positionals are ignored. `doc` and `init` are not type-checked. A `fields` dict drops non-string keys. Unexpected-field errors follow `BTreeMap` key order.
  - `macro`: `finalizer=True` is ignored (no end-of-package evaluation). `inherit_attrs="common"` and any other non-rule/non-macro value are ignored instead of inheriting common attributes or failing. Hidden `_` attributes are inherited, non-mandatory inherited defaults stay as declared instead of becoming `None`, and an `attrs` entry of `None` does not remove an attribute. `visibility` is not reserved, and `inherit_attrs` does not require `**kwargs`. The implementation is called with a sorted `BTreeMap`.
  - `rule`: `dependency_resolution_rule` is an unexpected keyword. `parent`, `extendable`, and `initializer` are stored and never applied, so attributes, providers, fragments, toolchains, and exec groups are not merged and the initializer does not run. `outputs` rejects a function, and `%{dirname}`, `%{basename}`, labels, outputs, and single-element lists are erased instead of substituted or rejected. Export does not require `*_test` exactly when `test` is set. `test`, `executable`, and `analysis_test` treat non-bools as false. `output_to_genfiles` uses truthiness. `host_fragments` and `_skylark_testable` are not applied. `compile_one_filetype` and `name` are accepted. Keyword-only arguments passed positionally are ignored. `build_setting` also adds a `help` attribute.
  - `aspect`: a flat `required_providers` or `required_aspect_providers` list is OR of single providers, not one AND-group, and a mix of providers and lists is not rejected. `toolchains_aspects`, `propagation_predicate`, `host_fragments`, `exec_groups`, and `subrules` are discarded. Function forms of `attr_aspects` and `toolchains_aspects` fail. A bare string is accepted for `attr_aspects`. Explicit attributes may be bool or int. Implicit `_` attributes need not be label-typed with defaults. Only `implementation` is read positionally.
  - `exec_group` does not check parameter names or sequence types and ignores positionals.
  - `subrule` drops nested `subrules`, turns a non-sequence `toolchains` or `fragments` into an empty list, and does not require private attributes to be label-typed. `implementation` is accepted positionally even though every parameter is keyword-only.
  - `Label` returns an existing label unchanged and parses a string in the defining `.bzl` context. Extra positional arguments are ignored.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkSubruleApi.java`
- Bazel lines: 42
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/aspect.rs`
- Verdict: partial
- Differences:
  - `getUserDefinedNameIfSubruleAttr` walks `subrules` in list order, drops empty results, and returns the first present user-defined name for `attributeName`. Zel stores subrule attributes under their original names and never maps a lifted rule attribute name back to that name or stops at the first declaring subrule.
  - When no subrule reports a name, Java returns an empty `Optional`. Zel has no corresponding `Option`, so callers cannot fall back from a lifted name to the user-defined name.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkTemplateContextApi.java`
- Bazel lines: 137
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never constructs a Starlark `template_ctx` or passes one into a `map_directory` expansion function.
  - `declare_file(filename, *, directory)` is absent: a relative filename must be declared inside a required keyword-only `File` directory and the method returns that `File`.
  - `args()` is absent: it returns an `Args` command-line builder bound to the current Starlark thread.
  - `run` is absent: keyword-only, with `outputs` (sequence of `File`), `inputs` defaulting to `[]` (sequence of `File` or depset), `executable` (`File`, string, or `FilesToRunProvider`), `tools` defaulting to `None` (sequence, depset, or `None`), `arguments` defaulting to `[]` (strings or `Args`), and `progress_message` defaulting to `None` (string or `None`).

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/SymlinkEntryApi.java`
- Bazel lines: 37
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_host.rs`
- Verdict: partial
- Differences:
  - Bazel `type()` is `SymlinkEntry`, unknown-field errors use that name, and `repr`/`str` is `SymlinkEntry(path = ..., target_file = ...)`. Zel builds a generic struct, so `type()` is `struct`, `repr`/`str` is `struct(path = ..., target_file = ...)`, and a missing field also lists `Available attributes: path, target_file`.
  - `==` and dict keys follow Java reference equality, so two entries with the same path and file are unequal. Zel compares and hashes the struct by field values, so the same `path` string and `target_file` compare equal.
  - `SymlinkEntry + other` is rejected (`unsupported binary operation: SymlinkEntry + …`). Zel struct addition merges fields and only fails when a name is in both operands (`cannot add structs: field '…' appears in both operands`); a disjoint right-hand struct succeeds.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/TemplateDictApi.java`
- Bazel lines: 120
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_actions.rs`
- Verdict: partial
- Differences:
  - `add`’s `key` and `value`, and `add_joined`’s `key` and `values`, are positional-only; `join_with`, `map_each`, `uniquify`, `format_joined`, and `allow_closure` are keyword-only. Zel accepts any of those names by position or keyword, and a keyword-only argument passed by position is dropped before the join.
  - `values` must be a depset. Zel also accepts a list or tuple.
  - `map_each` is required and is called with one argument. Zel treats a missing or `None` `map_each` as identity stringification, and calls a function with two or more required parameters with a dummy tree expander.
  - `uniquify` defaults to false and, when true, drops duplicate strings after `map_each`, keeping the first. Zel deduplicates the input elements before `map_each`, so identical mapped strings remain, and any truthy value counts as true (`None` selects the default).
  - `allow_closure` defaults to false and rejects a non-global `map_each` unless set. Zel accepts the flag and never applies that check.
  - Joining is deferred to template expansion. Zel runs `map_each` immediately and stores the joined string.
  - `format_joined` defaults to `None`. When set, Bazel uses Starlark `%` formatting on the Java UTF-16 string (so `%r` and other conversions apply, and `%` is found by code unit). Zel only substitutes one `%s` (`%%` is a literal percent) while scanning Unicode scalars, and rejects other conversions.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/TemplateVariableInfoApi.java`
- Bazel lines: 69
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - The constructor is a self-call with one mandatory parameter `vars` (positional or named) that must be a dict. Zel’s generic `Value::Provider` call keeps only keyword arguments and drops positionals, so `TemplateVariableInfo({'FOO': 'bar'})` builds an empty instance and a call with no arguments succeeds.
  - The only struct field is `variables`, an `ImmutableMap<String, String>` exposed as a new string-to-string dict. Zel does not map `vars` to `variables`; a `vars` keyword is stored under `vars`, and `variables` exists only if that keyword was passed, is returned by reference, and is not restricted to string keys and values.
  - Bazel rejects a non-dict `vars` (including `None`), extra positionals, and unexpected keywords. Zel accepts any keywords, ignores positionals, and stores non-dict values.
  - Bazel exposes this provider only as `platform_common.TemplateVariableInfo`. Zel also predeclares a top-level `TemplateVariableInfo`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/android/AndroidConfigurationApi.java`
- Bazel lines: 259
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The Starlark `android` configuration fragment is not implemented. A declared `android` fragment becomes an empty struct, so every struct field on this interface is absent.
  - Missing values: the boolean accessors; `incremental_dexing_shards_after_proguard` (`int`); the four dexopt `ImmutableList<String>` fields; `manifest_merger` and `get_output_directory_name` (`String`); and `apk_signing_method_v4` (`Boolean` or null, `allowReturnNones`).

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/apple/AppleConfigurationApi.java`
- Bazel lines: 134
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - `apple_cpus` is missing. Bazel returns `apple_split_cpu` (default `""`) plus tuples `ios_multi_cpus`, `visionos_cpus`, `watchos_cpus`, `tvos_cpus`, `macos_cpus`, and `catalyst_cpus`. A null or empty list is replaced by one host-dependent default (`sim_arm64` or `x86_64` for iOS, `sim_arm64` for visionOS, `arm64` or `x86_64` for watchOS and macOS, `sim_arm64` or `x86_64` for tvOS, `x86_64` for Catalyst). Struct field order follows a `HashMap`.
  - `apple_platform_type` is missing. Bazel returns that string (default `macos`).
  - `ios_sdk_version_flag`, `macos_sdk_version_flag`, `tvos_sdk_version_flag`, and `watchos_sdk_version_flag` are missing. Bazel returns a nullable `DottedVersion` (`None` when the option is null).
  - `single_arch_cpu` is `cfg.cpu` with every leading `darwin_` stripped. Bazel uses a non-empty `apple_split_cpu`, otherwise the first CPU of the platform list (with those defaults), then strips one leading `sim_` or `device_` prefix. An unknown platform type throws; zel still returns a string.
  - `single_arch_platform` is a device unless the type is ios, tvos, watchos, or visionos and `cfg.cpu` starts with `ios_sim`. Any other type, including an empty one, becomes macos (`MacOSX`) and does not fail. Bazel looks up `{type}_{arch}` (`darwin_{arch}` for macOS) in a fixed CPU set and throws if it is absent, so iOS `ios_x86_64`/`ios_i386` and tvOS, watchOS, and visionOS simulator CPUs get different `name`, `name_in_plist`, `platform_type`, and `is_device`.
  - The four `*_minimum_os_flag` fields are raw strings, or `None` when the option is missing or empty. Bazel returns a parsed `DottedVersion`, or `None` only for a null option; an empty or illegal version fails parsing. There is no `compare_to`, so order is string order (`1.10` vs `1.2`).
  - `xcode_version_flag` is `None` for a missing or empty value. Bazel returns `None` only for null and returns `""` when the flag is empty.
  - `prefer_mutual_xcode` and `include_xcode_exec_requirements` match the defaults `true` and `false` for absent or `true`/`false`/`yes`/`no`/`1`/`0` values. Any other string keeps that default. Bazel stores a real boolean and does not re-default an already parsed flag.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/apple/ApplePlatformApi.java`
- Bazel lines: 89
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - Zel’s value is a generic Starlark struct: `type()` is `struct`, and `repr` prints fields in sorted order (`is_device`, `name`, `name_in_plist`, `platform_type`). This API is the builtin type `apple_platform`; the implementing enum’s `repr` is the lowercased enum constant (`ios_device`, `ios_simulator`, and so on).
  - Any platform type other than `ios`, `tvos`, `watchos`, `visionos`, or `catalyst` (including `""`) gets `name` `macos` and `name_in_plist` `MacOSX`. `platform_type` stays the input string, or becomes `macos` when the input is empty. Bazel only has the fixed platforms; an unrecognized target CPU throws `IllegalArgumentException` (`No supported apple platform registered for target cpu …`).
  - Zel sets `is_device` false only when `apple_platform_type` is `ios`, `tvos`, `watchos`, or `visionos` and `cpu` starts with `ios_sim`; every other CPU uses the device platform. Bazel’s `is_device` is false for the registered simulator CPUs (`ios_x86_64`, `ios_i386`, `ios_sim_arm64`, `ios_sim_arm64e`, `tvos_x86_64`, `tvos_sim_arm64`, `tvos_sim_arm64e`, `watchos_i386`, `watchos_x86_64`, `watchos_arm64`, `watchos_sim_arm64e`, `visionos_sim_arm64`, `visionos_sim_arm64e`), so those CPUs get the simulator `name`, `name_in_plist`, and `is_device`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/apple/DottedVersionApi.java`
- Bazel lines: 45
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/natives.rs`
- Verdict: partial
- Differences:
  - `compare_to` takes one required positional-only argument (`named = false`). Zel reads only the first positional value and drops keywords, so extra positional arguments are ignored and the call still returns a comparison, and `compare_to(other=…)` does not bind `other` (keyword-only and mixed keyword calls fail the DottedVersion type check or succeed on the positional value).
  - The Starlark type is `DottedVersion`. Zel reports these host values as `host_object`, so `type()` and comparison errors that include the type name differ.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/apple/J2ObjcConfigurationApi.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Starlark configuration fragment `j2objc` with struct field `translation_flags` (`ImmutableList<String>`) is not implemented. `fragment("j2objc")` falls through to an empty struct, so the field is absent.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/apple/ObjcConfigurationApi.java`
- Bazel lines: 116
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - `ios_simulator_device`, `ios_simulator_version`, `signing_certificate_name`, `run_memleaks`, and `uses_device_debug_entitlements` are absent. Bazel returns `None` for an unset simulator device, simulator `DottedVersion`, or signing cert (`ios_signing_cert_name`); `run_memleaks` is `--ios_memleaks` (default false); device debug entitlements are `device_debug_entitlements` (default true) and compilation mode is not `opt`.
  - `copts_for_current_compilation_mode` is always `[]`. Bazel follows compilation mode: `dbg` adds the hardcoded debug copts unless `incompatible_avoid_hardcoded_objc_compilation_flags` (default true) and adds the glibcxx debug macros when `objc_debug_with_GLIBCXX`; `fastbuild` returns `experimental_objc_fastbuild_options` (default `-O0,-DDEBUG=1`); `opt` returns the hardcoded opt copts unless that avoid-hardcoded flag is set.
  - `target_should_alwayslink(ctx)` ignores `ctx` and the builtin allowlist and always returns `incompatible_objc_alwayslink_by_default` (default false). Bazel fails outside the default allowlist, returns an explicitly set `alwayslink` attribute when present, and otherwise returns `alwayslink_by_default`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/config/ComposedConfigurationTransition.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `compose` flattens nested composed transitions into one ordered chain (elements of the first, then the second) and stores the `and_then` caller location. Zel has no composed-transition value and no `and_then`; `transition()` is a single struct applied once.
  - Starlark `repr` of a composed transition is always `<transition object>`. Zel never produces that value.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/config/ConfigBootstrap.java`
- Bazel lines: 45
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - `config` is bound without `string_set`. `config.string_list(repeatable=True)` is not rejected when `flag` is false (Bazel errors unless `flag=True`). `flag`, `allow_multiple`, `repeatable`, and `exec_group` defaults are not applied; positional arguments are ignored and unknown keywords are kept. `config.target` and `config.none` accept arguments.
  - `config_common` is bound without `config_feature_flag_transition`. `toolchain_type` succeeds with no `name`, accepts `name` as a keyword, and ignores a positional `mandatory` (Bazel requires a positional string or Label `name`, and `mandatory` is keyword-only, default True).
  - `transition` and `analysis_test_transition` accept positional arguments and `transition` drops missing `implementation`, `inputs`, or `outputs` instead of failing. Inputs, outputs, and setting keys are not checked for string sequences, label syntax, duplicate keys, or `--experimental_*` / `--incompatible_*` names. A missing `analysis_test_transition` setting is reported as a missing positional argument.
  - `exec_transition` is inserted only if absent, and a call fails (`unknown native function 'exec_transition'`, or that it can only be used while evaluating a BUILD file). Bazel always binds it and rejects the call only outside builtins, after the same keyword-only `implementation` / `inputs` / `outputs` checks as `transition`, while allowing experimental and incompatible options.
  - Those bindings are stored in a `BTreeMap`, so `dir(config)` lists `string` before `string_list`. Bazel’s method map is ordered by Java method name, which lists `string_list` before `string`. A duplicate `put` throws; Zel `insert` overwrites the earlier empty `config`, `config_common`, `transition`, and `analysis_test_transition` placeholders.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/config/ConfigFeatureFlagProviderApi.java`
- Bazel lines: 58
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - `value` and `error` are struct fields that may be None. Zel’s `config_common.FeatureFlagInfo` is a schemaless provider, so only kwargs stored on the instance exist. A missing field fails lookup instead of returning None.
  - `is_valid_value(value)` is a method that returns a boolean. Zel does not implement that method. A call fails unless the instance stores a callable under that name.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/config/ConfigGlobalLibraryApi.java`
- Bazel lines: 159
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader_host.rs`
- Verdict: partial
- Differences:
  - `transition()` requires three named-only arguments (`implementation` callable, `inputs` and `outputs` sequences of strings). Zel accepts positional or keyword arguments, treats all three as optional, and does not type-check them.
  - Bazel rejects duplicate input or output keys, label spellings that canonicalize to the same setting, malformed or repo-invisible labels, `//command_line_option:experimental_*` and `incompatible_*` (except `incompatible_enable_cc_toolchain_resolution` and `incompatible_enable_apple_toolchain_resolution`), and options listed in `--incompatible_disable_transitions_on`. Zel keeps the lists as given.
  - Unexpected keywords are rejected in Bazel. Zel ignores them. If the same parameter is passed both positionally and by keyword, Zel keeps the keyword.
  - `analysis_test_transition(settings)` is named-only. Bazel casts dict keys to strings and validates them as outputs, allowing experimental and incompatible options. Zel also accepts a positional dict, reports a missing positional argument when `settings` is absent, and only checks that keys are strings when the transition is applied.
  - `exec_transition` matches `transition` but allows experimental and incompatible options and fails outside `@_builtins` with `file '...' cannot use private @_builtins API`. Zel registers the name and has no handler, so a `.bzl` call fails with `exec_transition() can only be used while evaluating a BUILD file`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/config/ConfigStarlarkCommonApi.java`
- Bazel lines: 76
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - `config_feature_flag_transition` is absent. Bazel takes one positional-only string attribute name and returns a rule transition that, when that attribute is set, replaces config feature-flag values with its label-keyed string dict (a no-op transition when it is unset) and adds the feature-flag allowlist checks.
  - `toolchain_type` binding differs. Bazel requires a positional-only `name` (`string` or `Label`) and a keyword-only bool `mandatory` defaulting to `True`, and rejects a keyword `name`, a second positional argument, a non-bool `mandatory`, and unexpected keywords. Zel reads `name` from a keyword or position 0 (keyword wins if both are passed), allows it to be missing or any type, reads `mandatory` only from keywords (default `True`), and ignores a second positional argument and other keywords. `toolchain_type("//pkg:t", False)` fails in Bazel and stays mandatory in Zel. A non-bool `mandatory` is stored and later treated as mandatory unless it is exactly `False`.
  - A string `name` is not parsed at the call. Bazel resolves it immediately against the evaluating `.bzl` package and repo mapping and fails the call on a label syntax error. Zel keeps the raw value and converts it only when exporting a rule's `toolchains` or `exec_compatible_with`; a failed conversion stays a string and is later dropped.
  - The result fields differ. Bazel exposes `toolchain_type` (a `Label`) and `mandatory` (a bool). Zel returns a struct with `name`, `mandatory`, and `_toolchain_type`, and no `toolchain_type` field. The same call shape is used for analysis-time `config_common.toolchain_type` in `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_host.rs`, which also does not parse the label.
  - `FeatureFlagInfo` is a provider constructor. Bazel requires a string `value` (positional or named) and records that string with a null error and an always-true validity check. Zel accepts any keywords, drops positional arguments, and does not require or type-check `value`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/config/ConfigurationTransitionApi.java`
- Bazel lines: 55
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader_host.rs`
- Verdict: partial
- Differences:
  - Bazel `and_then` returns a new transition that applies the receiver and then the argument, flattening already-composed operands into one ordered chain and recording the caller location. The original transitions are left unchanged.
  - Zel `transition()`, `analysis_test_transition()`, and `config.exec`/`target`/`none` values are plain structs with no `and_then` method, so the call fails as a missing attribute and never builds a composition.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/config/StarlarkConfigApi.java`
- Bazel lines: 215
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader_host.rs`
- Verdict: partial
- Differences:
  - `config.string_set` is not on the `config` module (`/workspace/zel-cursor/crates/zel-packages/src/loader.rs` exports only `int`, `bool`, `string`, `string_list`, `exec`, `target`, and `none`), so the call fails instead of building a string set (one instance of each element, comma-separated, or one element per occurrence when `repeatable` is true).
  - Keyword-only parameters are not enforced: positional arguments are dropped, unknown keywords are stored on the result, and `flag`, `allow_multiple`, `repeatable`, and `exec_group` are not checked as `bool` or `str | None`.
  - Omitted `flag`, `allow_multiple`, and `repeatable` stay absent instead of defaulting to `False`, and omitted `exec_group` stays absent instead of `None`, so `config.int()` is not the same value as `config.int(flag=False)`.
  - `flag=False` still accepts a command-line `--//pkg:setting=...`. Bazel rejects that unless `flag=True`.
  - `allow_multiple=True` on `config.string` does not accumulate an ordered list that keeps duplicates. The value stays one string and the last occurrence wins.
  - `repeatable=True` with `flag=False` does not fail. `string_list` always joins every command-line occurrence with commas and splits on commas, including a final empty string becoming `[""]`. Bazel keeps only the last occurrence when `repeatable` is false, appends each occurrence as a single element when it is true, and treats an empty assignment as an empty list.
  - `config.none()` does not clear configuration on an attribute `cfg`. Rule analysis treats both `config.none()` and `config.target()` as no-ops. Bazel `none()` unsets all flags. `target()` is the no-op.
  - `config.int` command-line values are parsed as `i64` and fail outside that range. Bazel accepts any Starlark int. Attribute defaults still reject values outside the signed 32-bit range.
  - `config.bool` command-line values accept only the exact strings `true`, `1`, `yes`, `false`, `0`, and `no`. Bazel also accepts `t`, `y`, `f`, and `n` in any letter case.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/config/StarlarkToolchainTypeRequirement.java`
- Bazel lines: 41
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader_host.rs`
- Verdict: partial
- Differences:
  - Starlark type name is `toolchain_type`. Zel stores a generic struct, so `type()` is `struct`.
  - Struct field `toolchain_type` returns a non-null `Label`. Zel has no `toolchain_type` field; the value is kept under `name` as the raw string or label, and `.toolchain_type` fails as a missing struct field.
  - Struct field `mandatory` is always a boolean. Zel copies the `mandatory` keyword as given and inserts `True` only when that keyword is absent, so the field can be a non-bool. Callers treat the requirement as optional only when the field is exactly `False`.
  - Zel also exposes `_toolchain_type`. Field order is `BTreeMap` key order (`_toolchain_type`, `mandatory`, `name`) rather than `toolchain_type` and `mandatory`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/core/Bootstrap.java`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No hook appends one slice of the Starlark build API into a shared map. Bazel passes a single `ImmutableMap.Builder<String, Object>` to `addBindingsToBuilder` for every bootstrap; `buildOrThrow` keeps insertion order and throws `IllegalArgumentException` when two helpers bind the same name.
  - A null key or null value is rejected by that builder (`NullPointerException`). The nearest Zel globals live in `EvalContext.globals`, a `BTreeMap<String, Value>`: a later insert replaces the earlier binding, iteration is sorted by key, and `Value` has no null.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/core/ContextAndFlagGuardedValue.java`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Access is allowed when the incompatible flag is false or the requesting `.bzl` matches an allowlisted `(repo, package prefix)`; zel binds the guarded symbols with no such check.
  - `--incompatible_stop_exporting_language_modules` is only a semantics key (default false) and does not hide the value or change accessibility when true.
  - Denied access does not raise `may only be used from one of the following repositories or prefixes: …` plus `--<flag>=false` (`flag` without its `+`/`-` prefix).
  - There is no compile-time client-data filter: empty repo matches only the empty repo name, otherwise `.*\b<quoted repo>\b.*`, and the package path must start with the allowlisted fragment.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/core/ContextGuardedValue.java`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel allows the wrapped value only when client data is a non-null `BazelCompileContext` and some allowlist `PackageIdentifier` matches: an empty allowed repository name matches only an empty caller repository name (`String.matches("")`); any other name must match the whole caller repository name against the `java.util.regex` pattern `.*\b` + `Pattern.quote(name)` + `\b.*` (ASCII `\b` / `[A-Za-z0-9_]`, so `rules_foo+1.2.3` matches `rules_foo` and `my_rules_foo_helper` does not, and `.` stays literal), and the caller package path must `startsWith` the allowed package fragment (string prefix plus a `/` boundary, so an empty fragment matches every package). `StarlarkSemantics` is ignored. Zel has no such allowlist.
  - Any other client data, including null and a non-compile context, or no matching entry, denies access. The error is `name + " may only be used from one of the following repositories or prefixes: "` plus `PackageIdentifier.toString()` values joined by `", "` in `ImmutableSet` iteration order (main repo is the package path only; other repos are canonical `repo//pkg`). Zel predeclared lookup has neither this denial nor this error.
  - When access is allowed, the guard returns the wrapped object unchanged, including null. Zel has no wrapper.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/core/StructApi.java`
- Bazel lines: 55
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/struct_value.rs`
- Verdict: partial
- Differences:
  - `createStruct` takes only `**kwargs` (default `{}`) and rejects positionals. The `struct` identifier arm builds a `StructValue` from keywords and ignores positional arguments, so `struct(1)` succeeds.
  - `selfCall` makes the provider value callable. A struct is built only when the callee identifier is `struct`. `f = struct; f(a = 1)` resolves `BuiltinFunction("struct")` and fails as an unknown builtin.
  - A repeated keyword (`struct(a = 1, **{"a": 2})`) errors before `createStruct`. That arm returns without checking `pending_dup_kw`, so the call succeeds and the `**` entry overwrites the explicit one in the `BTreeMap`.
  - The identifier `struct` is dispatched as the builtin even when that name is shadowed. Bazel calls the bound value.
  - `useStarlarkThread` records the caller location on the value. `StructValue` stores none.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/core/TransitiveInfoCollectionApi.java`
- Bazel lines: 45
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_host.rs`
- Verdict: partial
- Differences:
  - An alias dependency’s `label` is the alias rule’s own label, and `str`/`repr` are `<target ALIAS>`. Bazel’s `label` is the actual target’s label, and `str`/`repr` are `<alias target ALIAS of ACTUAL>`. An aspect-decorated target is `<target LABEL>`; Bazel prints `<merged target LABEL>`. `print` of a rule target is the same `<target LABEL>` text; Bazel’s debug print appends `, keys:[…]` with the provider keys.
  - Every `target_value` call allocates a new host id. `==` and dict/set hashing use that id, so two values for the same configured target are unequal and are distinct keys. Bazel uses the configured-target object’s identity, so the same target compares and hashes as one key.
  - A field name other than the special defaults is resolved by scanning provider instances in `BTreeMap` key order and returning the first field with that name. Bazel rejects every such name.
  - `actions` is not a field, so the read fails. Bazel returns the rule’s Starlark-visible action list. `output_groups` is not the `OutputGroupInfo` value; Bazel returns that provider when `--incompatible_disable_target_default_provider_fields` is false (the default).
  - Setting `--incompatible_disable_target_default_provider_fields` does not change field access. Bazel then rejects `files`, `files_to_run`, `default_runfiles`, `data_runfiles`, and `output_groups`.
  - `files_to_run` is always present, including `executable = None`. Bazel leaves `FilesToRunProvider` null on a non-executable target, so the read fails. A rule that passes only `runfiles` exposes both `default_runfiles` and `data_runfiles` as that runfiles value. Bazel returns it only from `default_runfiles` and the `data_runfiles` read fails.
  - `Provider in target` for an unexported provider is a boolean membership test of its anonymous key, and indexing that key can succeed. Bazel errors on both unless the provider was exported. A non-provider key fails in both.
  - `dir()` is always `actions`, `data_runfiles`, `default_runfiles`, `files`, `files_to_run`, `label`, and `output_groups`. Bazel lists `output_groups` only when that provider exists, `actions` only on rule targets, and also legacy string provider keys.
  - In `cquery`/`aquery` `--starlark:expr` (`/workspace/zel-cursor/crates/zel-query/src/starlark_out.rs`), indexing and `in` accept a string provider name and a `%Name` suffix match, and `providers_dict` is a field. The analysis `Target` rejects a non-provider key, and Bazel’s `Target` index/`in` accept only an exported provider.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CcBootstrap.java`
- Bazel lines: 64
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - Bazel predeclares both `cc_common` and `CcToolchainConfigInfo` as `None`. Zel binds `cc_common` to a struct whose only field is `internal_DO_NOT_USE`, and does not bind `CcToolchainConfigInfo`, so a strict `.bzl` reference is `name 'CcToolchainConfigInfo' is not defined` in every repository.
  - Bazel allows either name when `--incompatible_stop_exporting_language_modules` is false (the default) or the compiling `.bzl` repo matches `_builtins`, `bazel_tools`, `local_config_cc`, or `rules_cc` as a Java `\bname\b` regex (a `+` module suffix still matches) at any package, or the main repo whose package path starts at `tools/build_defs/cc` (segment boundary, so `tools/build_defs/cc_extra` does not match). Otherwise compilation fails and the error names that allowlist and `--incompatible_stop_exporting_language_modules=false`. Zel never reads that flag and applies no repo allowlist, so `cc_common` is visible from every `.bzl`.
  - Zel's bzlmod evaluator only lists both names in a static name universe. A use with no global becomes a non-executable rule callable when strict names are off, and a runtime "not defined" error when `ZEL_STRICT_BZLMOD` is set, rather than `None` or the flag/repo guard.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CcCompilationContextApi.java`
- Bazel lines: 178
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_cc/0.2.17/cc/private/cc_info.bzl`
- Verdict: partial
- Differences:
  - `type()` of a Bazel compilation context is the provider name `CcCompilationContextInfo`. Zel returns `ProviderInstance`. `str` prints the full `bzl%CcCompilationContextInfo` key, and fields are a `BTreeMap`, so the text is sorted by name rather than provider declaration order. A missing field reads `field '<name>' not found`.
  - `direct_headers`, `direct_public_headers`, `direct_private_headers`, and `direct_textual_headers` are immutable lists. Zel stores the header sequences as tuples, and `freeze` of the concatenated direct headers stays a tuple, so an empty context has `()` rather than `[]`.
  - Bazel moves `.inc` files out of modular public and private headers into textual headers, drops duplicate artifacts (first occurrence kept), and unions exported dependencies' modular public, modular private, and textual headers into those lists. Zel keeps the given sequences, including duplicates, and does not merge exported headers into the direct lists.
  - `_header_info` is equal only to the same identity token. A separate module without a main module fails. `merged_deps` are folded into the header sets and are not a field. Zel is a struct compared by fields, keeps `deps` and `merged_deps` unmerged, adds `_zel_native`, and accepts a separate module with no main module.
  - Non-sequence header arguments fail Bazel’s sequence-of-`File` cast. Zel accepts only `list`, `tuple`, or `None` (`None` becomes an empty tuple) and does not require each element to be a `File`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CcCompilationOutputsApi.java`
- Bazel lines: 71
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Starlark `CcCompilationOutputs` value with struct fields `objects` and `pic_objects` (file sequences), `_header_tokens`, `_module_files`, `_dwo_files`, `_pic_dwo_files`, `_gcno_files`, and `_pic_gcno_files` (file sequences), `_lto_compilation_context` (object), or a `temps` method that takes the Starlark thread and returns a depset.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CcDebugInfoContextApi.java`
- Bazel lines: 43
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_cc/0.2.17/cc/private/cc_info.bzl`
- Verdict: partial
- Differences:
  - `CcDebugContextInfo` stores `files` and `pic_files` with no depset check. The Java accessors are declared to return `Depset`.
  - The provider is callable and builds an instance from those fields. This interface defines no constructor and no defaults.
  - Unknown attributes follow the provider field map. The Java type is a `StarlarkValue` whose `files` and `pic_files` are struct-field methods.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CcInfoApi.java`
- Bazel lines: 135
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `CcInfo(...)` is keyword-only, with `compilation_context`, `linking_context`, `debug_context`, and `cc_native_library_info` each defaulting to `None` and accepted only as the declared context type or `None`. Zel only predeclares the name and, on call, stores the supplied keywords with no defaults, no type checks, and no error for unexpected names; positional arguments are dropped.
  - Struct fields `compilation_context`, `linking_context`, `_debug_context`, and `_legacy_transitive_native_libraries` (a depset; the last two may raise `EvalException`) are never filled. A call result only has the exact keywords passed, so a missing field errors (`field '…' not found`) instead of returning `None` or a depset, and `debug_context` / `cc_native_library_info` are not exposed under those private field names.
  - Keywords are kept in a `BTreeMap`, so field iteration is sorted by name rather than the declared field order.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CcLinkingContextApi.java`
- Bazel lines: 46
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Starlark builtin `LinkingContext` is not registered. Struct field `linker_inputs` returns a `Depset`. There is no Zel value of that type, so the field is absent.
  - `extra_link_time_libraries` is a method, not a struct field. It is passed the Starlark thread and declared to throw `EvalException`. The default body throws `UnsupportedOperationException`. Zel has no such method.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CcLinkingOutputsApi.java`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Starlark `CcLinkingOutputs` value exposes nullable `library_to_link` (`LibraryToLink` or null) or nullable `executable` (`File` or null) as struct fields.
  - `all_lto_artifacts` is not a method that receives the Starlark thread, returns a sequence, and can fail with an evaluation error.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CcModuleApi.java`
- Bazel lines: 2113
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/natives.rs`
- Verdict: partial
- Differences:
  - `compile`, `link`, `configure_features`, `is_enabled`, `create_lto_compilation_context`, `create_compilation_outputs`, `merge_compilation_outputs`, `create_compile_variables`, `create_link_variables`, `create_library_to_link`, `create_linker_input`, `create_linking_context`, `merge_cc_infos`, `create_compilation_context`, `create_module_map`, `create_cc_toolchain_config_info`, `create_linking_context_from_compilation_outputs`, `merge_compilation_contexts`, `create_extra_link_time_library`, and the native-module `CcToolchainInfo` field are absent. Java still exposes them; the interface defaults throw `UnsupportedOperationException`.
  - `do_not_use_tools_cpp_compiler_present` is a struct field whose value is `None`. Zel exposes a function that returns `False`, so the field itself is truthy.
  - `check_experimental_cc_shared_library` is always `True` (Java reads `--experimental_cc_shared_library`, default `False`). `incompatible_disable_objc_library_transition` is always `False` (Java default `True`). `add_go_exec_groups_to_binary_rules` is always `False` and ignores the flag.
  - Java rejects these natives unless the caller is `_builtins` `cc_common.bzl`, `cc_common_bazel.bzl`, or `common/cc/compile`, `link`, or `toolchain_config`, and the parameters are keyword-only. Zel does not check the caller and also accepts them positionally.
  - `legacy_cc_flags_make_variable_do_not_use` fails as unimplemented instead of returning the toolchain’s legacy cc-flags make variable.
  - `get_memory_inefficient_command_line`, `get_execution_requirements`, and `get_tool_requirement_for_action` return a tuple. Java returns an immutable list. Duplicate execution requirements are kept; Java’s set drops them.
  - `get_tool_for_action` and the requirement methods require the action config to be enabled. Java resolves any action config on the toolchain, including a disabled one. Relative tool paths are string-joined with no `.`/`..` normalization, and a drive-letter path is treated as relative. Java rejects an empty `path`, rejects setting both `path` and `tool`, and joins with `PathFragment` (`getSafePathString` of an empty path is `.`).
  - Action-config flag sets are expanded even when their `actions` list does not name that action. Java’s expander still requires the action name.
  - `get_environment_variables` returns a mutable dict and overwrites a duplicate key. Java returns a frozen dict and fails when two entries share a key.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CcStarlarkApiProviderApi.java`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Starlark struct with the legacy C++ provider fields `transitive_headers` and `libs` as non-null depsets of files, or `link_flags`, `defines`, `system_include_directories`, `include_directories`, `quote_include_directories`, and `compile_flags` as non-null lists of strings.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CcToolchainConfigInfoApi.java`
- Bazel lines: 40
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel declares a Starlark provider type `CcToolchainConfigInfo` that is a struct, plus a nested `Provider` type. This interface defines no fields, methods, or constructor body. Zel does not define that provider or constructor. The name appears only as a bzlmod static-resolution identifier and is not bound as a provider value.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CcToolchainProviderApi.java`
- Bazel lines: 218
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No native `CcToolchainInfo` whose calls and struct fields (`needs_pic_for_dynamic_libraries`, `static_runtime_lib`, and `dynamic_runtime_lib` with named-only `feature_configuration`; `built_in_include_directories`, `all_files`, `sysroot`, `compiler`, `libc`, `cpu`, `target_gnu_system_name`, `objcopy_executable`, `compiler_executable`, `preprocessor_executable`, `nm_executable`, `objdump_executable`, `ar_executable`, `strip_executable`, `ld_executable`, `gcov_executable`) throw `UnsupportedOperationException` (`Native CcToolchainInfo API no longer exists, use Starlark provider instead.`).

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CcToolchainVariablesApi.java`
- Bazel lines: 27
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/cc_features.rs`
- Verdict: partial
- Differences:
  - `type()` is `Variables` in Bazel and `host_object` in Zel (`Host` kind `cc_toolchain_variables` is not mapped in `type_name`).
  - `str`/`repr` is `<unknown object com.google.devtools.build.lib.rules.cpp.CcToolchainVariables$MapVariables>` (default `StarlarkValue.repr`; this type adds no override) versus `<cc_toolchain_variables {registry index}>`.
  - Attribute errors name the type `Variables` in Bazel and `cc_toolchain_variables` in Zel. Calling the value fails as `'Variables' object is not callable` versus `'host_object' object is not callable`.
  - `for` fails as `type 'Variables' is not iterable` versus `type 'host_object' is not iterable`. `in` fails as `unsupported binary operation: … in Variables` versus `type 'cc_toolchain_variables' is not iterable`. Indexing fails as `type 'Variables' has no operator [](…)` versus `type 'cc_toolchain_variables' is not indexable`. Ordered comparison fails as `unsupported comparison: Variables <=> Variables` versus `unsupported comparison: host_object <=> host_object`.
  - Starlark `==` uses `MapVariables.equals`: same bindings, parent by reference. Zel compares host kind and registry id, so two separately built values with the same bindings are unequal and hash differently. Both are truthy and `dir()` is empty.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CppConfigurationApi.java`
- Bazel lines: 189
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - `do_not_use_macos_set_install_name` is missing. Bazel is a struct field that always returns true; reading it in zel is an unknown-attribute error.
  - `fdo_instrument` and `cs_fdo_instrument` always return None. Bazel returns the `--fdo_instrument` / `--cs_fdo_instrument` string, or null when unset.
  - `disable_nocopts` is always true and ignores `--incompatible_disable_nocopts`.
  - `objc_should_strip_binary` is `objc_enable_binary_stripping` and strip-binaries (`--strip=always`, or `sometimes` in fastbuild). Bazel is `objc_enable_binary_stripping` and compilation mode `opt`.
  - `linkopts` is only the `--linkopt` list. With `--experimental_omitfp`, Bazel also appends `-Wl,--eh-frame-hdr`. The `copts` omit-frame-pointer prefix matches.
  - `copts` (after that prefix), `cxxopts`, `conlyopts`, `objccopts`, `linkopts`, and `strip_opts` come from splitting a comma-joined string and dropping empty segments, so a value that contains a comma or is empty is not one list entry.
  - `fission_active_for_current_compilation_mode` turns on for any comma token `yes` and off for any token `no`, compared case-sensitively to the mode. Bazel enables every mode only when the whole `--fission` value is `yes`, clears them only when it is `no`, and matches modes case-insensitively. `should_strip_binaries` likewise compares `--strip` case-sensitively to `always` / `sometimes`.
  - `grte_top` is `Label::parse_canonical`, or None on failure. Bazel maps `default` to null, rejects values that do not start with `//`, and rewrites the target name to `everything`. `custom_malloc` is None for any non-canonical label, including relative labels Bazel’s label converter accepts.
  - Thread-taking methods accept any arguments and skip the private Starlarkification allowlist and the expanded-API allowlist (`force_pic`, `generate_llvm_lcov`, `fdo_instrument`), so calls that throw `EvalException` in Bazel still return a value.
  - Fragment attribute order is `BTreeMap` key order, not the API declaration order.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/CppModuleMapApi.java`
- Bazel lines: 40
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No `CcModuleMap` value. `name` and `file` are not Starlark methods (`structField` is false) that return a string and a `File` or fail with an evaluation error.
  - Module maps that zel does handle are rules_cc provider fields read as attributes (`name`, `file`), so a call is not the getter and a missing field is an attribute error.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/DebugPackageInfoApi.java`
- Bazel lines: 109
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_cc/0.2.17/cc/private/debug_package_info.bzl`
- Verdict: partial
- Differences:
  - `target_label` and `unstripped_file` are required constructor parameters. Zel’s `provider(fields=...)` and the predeclared schemaless `DebugPackageInfo` both accept a call that omits either field and do not create it.
  - `stripped_file` and `dwp_file` default to None and stay present (`allowReturnNones`). Zel stores only the keyword arguments that were passed, so an omitted field reads as a missing field (`field '…' not found`) rather than None, and `hasattr` is false.
  - `stripped_file` and `dwp_file` accept only a File or None. Zel does not apply that type check.
  - Every constructor parameter is keyword-only. The predeclared provider ignores positional arguments and keeps unknown field names. The loaded rules_cc provider rejects both.
  - `target_label` and `unstripped_file` cannot be None. Zel stores None when the caller passes it.
  - Instance field order is sorted `BTreeMap` order (`dwp_file`, `stripped_file`, `target_label`, `unstripped_file`), not the interface declaration order (`target_label`, `stripped_file`, `unstripped_file`, `dwp_file`).

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/FeatureConfigurationApi.java`
- Bazel lines: 26
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/cc_features.rs`
- Verdict: partial
- Differences:
  - `type()` is `FeatureConfiguration` (`@StarlarkBuiltin`). Zel `Value::Host` kind `FeatureConfiguration` reports `host_object`.
  - The interface does not override `StarlarkValue.isImmutable()`, so the value is unhashable (`unhashable type: 'FeatureConfiguration'`). Zel treats every `Host` value as hashable, keyed by `(kind, id)`.
  - `repr` stays the `StarlarkValue` default, `<unknown object com.google.devtools.build.lib.rules.cpp.FeatureConfigurationForStarlark>`. Zel prints `<FeatureConfiguration {registry index}>`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/LibraryToLinkApi.java`
- Bazel lines: 125
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not expose a Starlark `LibraryToLink` value whose `objects`, `pic_objects`, `lto_bitcode_files`, and `pic_lto_bitcode_files` fields are sequences that may be None.
  - Zel does not expose the nullable artifact fields `static_library`, `pic_static_library`, `dynamic_library`, `resolved_symlink_dynamic_library`, `interface_library`, and `resolved_symlink_interface_library`.
  - Zel does not expose `alwayslink` as a boolean that cannot be None.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/LinkerInputApi.java`
- Bazel lines: 60
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Rust `LinkerInput` value implements the four struct fields. `owner` is a `Label` and may raise `EvalException`; `user_link_flags` is a sequence of strings; `libraries` is a sequence selected with Starlark semantics (a depset of `LibraryToLink`, or a deprecated list); `additional_inputs` is a sequence of files.
  - `/workspace/zel-cursor/third_party/bcr/rules_cc/0.2.17/cc/private/link/create_linker_input.bzl` only builds a Starlark `LinkerInputInfo`. Its `libraries` and `additional_inputs` are always frozen lists, with no semantics or depset branch, and `owner` is the stored value with no `EvalException`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/cpp/LtoCompilationContextApi.java`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel exposes an undocumented Starlark builtin type `LtoCompilationContext` with no fields or methods (opaque `StarlarkValue`). Zel never registers that type name, so `type()` cannot return `LtoCompilationContext`.
  - Zel’s `compile.bzl` keeps `lto_compilation_context` as a plain dict and loads `create_lto_compilation_context` from `lto_compilation_context.bzl`, which is not in the Zel tree. The vendored rules_cc provider is `LtoCompilationContextInfo`, a different type with `lto_bitcode_inputs`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/java/JavaAnnotationProcessingApi.java`
- Bazel lines: 92
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/private/java_info.bzl`
- Verdict: partial
- Differences:
  - `type()` is `ProviderInstance`, and `dir()` lists no attributes. The Java builtin is `java_annotation_processing` and always exposes `enabled`, `class_jar`, `source_jar`, `transitive_class_jars`, `transitive_source_jars`, `processor_classpath`, and `processor_classnames`.
  - The provider schema only rejects unknown names. Those fields are optional and untyped, so `enabled` need not be bool, `class_jar` and `source_jar` need not be a File or None, the three jar sets need not be depsets, and `processor_classnames` need not be a list of strings.
  - `processor_classnames` is the caller's list, and freezing a provider does not freeze its fields, so later mutation changes the stored value. The Java getter returns an `ImmutableList`, copied into a Starlark list frozen with the configured target.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/java/JavaCommonApi.java`
- Bazel lines: 710
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - `_check_java_toolchain_is_declared_on_rule` always returns `None`. Bazel errors when the rule did not declare the Java toolchain.
  - `_google_legacy_api_enabled`, `_incompatible_java_info_merge_runtime_module_flags`, and `incompatible_disable_non_executable_java_binary` are fixed `false` (`zel-builtins` `natives.rs`) and do not follow the semantics flags.
  - `tokenize_javacopts` is not a native. Bazel tokenizes the list in order and, on a tokenization failure, keeps that string unchanged.
  - `expand_java_opts` builds `$(location)` maps from `srcs`, `deps`, `data`, `tools`, and `resources` for both path modes. Bazel uses `srcs`, outputs, `deps`, `implementation_deps`, and `tools`, and includes `data` only when `exec_paths` is false; a `deps`/`tools` entry contributes its executable when it has one, otherwise its files-to-build. A bad tokenization is an attribute error there and a failed call here. Splitting treats every Unicode whitespace character (including Latin-1 NBSP and newlines) as a separator; Bazel splits only on space and tab. Inside double quotes, a backslash is always dropped; Bazel keeps it unless the next UTF-16 unit is `\` or `"`.
  - `collect_native_deps_dirs` keeps a directory when the exec-path basename ends in `.so`, `.dylib`, or `.dll`, or contains `.so.`. Bazel also accepts `.pyd`, `.wasm`, `.tgt`, `.vpi`, and versioned `.dylib` names matching `^.+\.((so)|(dylib))(\.\d\w*)+$`, and it skips interface libraries including `.dll.a` instead of failing. Directories come from `root_relative_path`, not the runfiles path.
  - `target_kind` returns that configured target's rule class. Bazel unwraps a merged target and an alias (`getActual()`) first, and returns `""` when the value is not a configured target.
  - `create_header_compilation_action` drops `compile_time_java_deps`, so the annotation-processing turbine action never applies reduced-classpath `.jdeps`. `strict_deps_mode` `DEFAULT` is not rewritten to `ERROR`, and an unknown mode does not fail.
  - `create_compilation_action` ignores `enable_jspecify` (no JSpecify implicit classpath, processor, or trailing javacopts), hardcodes `--experimental_fix_deps_tool=add_dep`, and does not run turbine annotation processing, Jacoco coverage, per-package toolchain data, or local bytecode optimization. `strict_deps_mode` has the same `DEFAULT` / unknown-mode gap. Javac options are shell-tokenized with the same Unicode-whitespace and quoted-backslash rules as above.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/java/JavaCompilationInfoProviderApi.java`
- Bazel lines: 57
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/private/java_info.bzl`
- Verdict: partial
- Differences:
  - `javac_options`, `runtime_classpath`, and `compilation_classpath` are always depsets, and `boot_classpath` is always an `ImmutableList` (list order). Zel’s `JavaCompilationInfo` stores the given values with no type check, so a depset in `boot_classpath` keeps depset order instead of list order.
  - Those four struct fields are always present and are not noneable. Zel allows a provider with fields omitted (read fails with `field '…' not found`; `hasattr` is false) or set to `None`, which is returned as `None`.
  - Provider fields sit in a `BTreeMap`, so `str()` prints them in sorted key order (`boot_classpath`, `compilation_classpath`, `javac_options`, `runtime_classpath`) rather than declaration order.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/java/JavaConfigurationApi.java`
- Bazel lines: 165
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - `default_javac_flags` is a comma-split of the raw `javacopt` string. Bazel shell-tokenizes each option (quotes and spaces), and on tokenization failure keeps the original argument.
  - `default_javac_flags_depset` is a `default`-order depset of those comma-split strings. Bazel returns a `preorder` (`NAIVE_LINK_ORDER`) depset whose only element is the shell-escaped, space-joined token string (empty set stays `preorder`).
  - `default_jvm_opts` comma-splits `jvmopt`. Bazel returns each flag value intact, including values that contain commas.
  - `plugins` is a list of those comma-split strings. Bazel returns `Label`s (canonical label parsing and label equality).
  - `generate_java_deps` is only `--java_deps` (default true). Bazel is `java_deps || experimental_java_classpath != OFF`, so `--java_deps=false` with the default classpath `bazel` is still true.
  - `multi_release_deploy_jars` is always true and ignores `--incompatible_multi_release_deploy_jars`.
  - `auto_create_java_test_deploy_jars` is always false and ignores `--experimental_java_test_auto_create_deploy_jar`.
  - These fields are absent (Starlark attribute error): `add_test_support_to_compile_deps` (default true), `bytecode_optimizer_mnemonic` (default `Proguard`), `split_bytecode_optimization_pass` (default false), `bytecode_optimization_pass_actions` (default int 1), `enforce_proguard_file_extension` (default false).
  - `use_ijars`, `use_header_compilation`, `use_header_compilation_direct_deps`, `reduce_java_classpath`, `enforce_explicit_java_test_deps`, `generate_java_deps`, `disallow_java_import_exports`, and `auto_create_java_test_deploy_jars` skip the builtins allowlist and accept any arguments. Bazel raises `EvalException` outside that allowlist and rejects unexpected arguments.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/java/JavaInfoApi.java`
- Bazel lines: 181
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/private/java_info.bzl`
- Verdict: partial
- Differences:
  - `cc_link_params_info` exists only when `experimental_google_legacy_api` is on, and the interface default throws `UnsupportedOperationException`. Zel hardcodes `java_common_internal_do_not_use._google_legacy_api_enabled` to false, so the usual constructor never sets the field and a read fails with "field not found". `to_implicit_exportable` always sets it to `CcInfo()` even with the flag off.
  - Omitted schema fields stay absent. Bazel fills them with `None`. That covers `cc_link_params_info` on the normal constructor and `_is_binary` except on the binary path. `_is_binary` is not a field of this Java API.
  - `module_flags_info` does not pick up `runtime_deps` when `incompatible_java_info_merge_runtime_module_flags` is enabled, because that native is also hardcoded false.
  - List fields (`source_jars`, `runtime_output_jars`, `_constraints`, and inherited `java_outputs`) stay mutable. Provider instances are not frozen, so those sequences can be updated after construction.
  - `dir()` on a provider instance is empty, so `add_constraints` and `make_non_strict` rebuild the provider from that and drop every field this API defines.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/java/JavaModuleFlagsProviderApi.java`
- Bazel lines: 31
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/private/java_info.bzl`
- Verdict: partial
- Differences:
  - Bazel exposes an undocumented builtin type `JavaModuleFlagsProvider`. Zel stores the same `add_exports` and `add_opens` fields on the private provider `_ModuleFlagsInfo`, so `type()` and provider identity differ.
  - The native fields always return a `Depset` of `String`. The Starlark provider does not check that those fields are string depsets.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/java/JavaOutputApi.java`
- Bazel lines: 127
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/private/java_info.bzl`
- Verdict: partial
- Differences:
  - `class_jar` is not `allowReturnNones`, so a null return fails the Starlark read. `_JavaOutputInfo.class_jar` may be `None` and reads as `None`.
  - `source_jars` is a nullable `Depset`, and the accessor is called with `StarlarkSemantics` (`useStarlarkSemantics = true`). Zel stores an untyped provider field and never consults `incompatible_depset_for_java_output_source_jars`. The rules_java constructor passes a default-order `depset`, which matches the current no-op flag (default true, always a stable-order depset). A list or other value is returned unchanged in Zel and cannot be returned by this native field.
  - Omitted provider keys fail attribute access (`field '…' not found`). This API always exposes every field, using `None` for the nullable ones (`ijar`, `compile_jar`, `header_compilation_jar`, `compile_jdeps`, `generated_class_jar`, `generated_source_jar`, `native_headers_jar`, `manifest_proto`, `jdeps`, `source_jar`).

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/java/JavaRuleOutputJarsProviderApi.java`
- Bazel lines: 70
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/private/java_info.bzl`
- Verdict: partial
- Differences:
  - `JavaInfo` merge stores `outputs.jdeps` and `outputs.native_headers` as `None` even when `jars` contains exactly one non-null `jdeps` or `native_headers_jar`. The Bazel getters return that single file, and return none only when the count is not exactly one.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/java/JavaToolchainStarlarkApiProviderApi.java`
- Bazel lines: 92
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/common/rules/java_toolchain.bzl`
- Verdict: partial
- Differences:
  - `ijar` and `java_runtime` are non-null on the Java API (`allowReturnNones` is unset). The rule stores `None` when those attributes are unset. `jacocorunner` and `proguard_allowlister` still allow `None`.
  - Provider fields are a `BTreeMap`, so `str`/`repr` walks names in sorted order and prints the full `<bzl>%JavaToolchainInfo` key. Bazel prints `JavaToolchainInfo(...)` in `provider()` fields-dict order.
  - `dir(info)` is empty (`ProviderInstance` is not handled). Bazel lists the schema fields that were set. `info + other` is unsupported; Bazel merges schema slots and errors when a field is set on both.
  - A missing field fails as `field '<name>' not found`. Bazel reports `'JavaToolchainInfo' value has no field or method '<name>'`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/objc/AppleBootstrap.java`
- Bazel lines: 56
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - Bazel predeclares `apple_common` as `Starlark.NONE`. Zel inserts an empty struct, then if it is still empty replaces it with `{apple_toolchain: host function}` that returns an empty struct, so a `.bzl` file can call `apple_common.apple_toolchain()` without a successful builtins export.
  - Bazel allows that binding when `--incompatible_stop_exporting_language_modules` is false (default) or the compiling `.bzl` matches the allowlist: repo name matches `.*\b<repo>\b.*` via `String.matches` (empty repo is only the main repo) and the package is a path-segment prefix of `test_starlark` or `tools/osx`, or any package in `_builtins`, `apple_support`, `bazel_tools`, `build_bazel_rules_apple`, `build_bazel_rules_swift`, `io_bazel_rules_go`, `local_config_cc`, `rules_apple`, `rules_cc`, `rules_go`, `rules_ios`, `rules_swift`, `stardoc`, or `tulsi`. The repo check runs only when client data is a `BazelCompileContext`; otherwise only the flag side of the OR applies. Denial reports those package ids and tells the user to set the flag false. Zel never consults the flag or the allowlist.
  - With builtins injection on (Bazel default), that guarded `None` is replaced by the unguarded `exported_toplevels` value, so the allowlist no longer applies. Zel also overwrites the global from `exports.bzl`. With injection disabled or failed, Bazel keeps guarded `None`; Zel leaves the `apple_toolchain` stub and does not treat an empty `--experimental_builtins_bzl_path` as disabling injection.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/objc/AppleCommonApi.java`
- Bazel lines: 153
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/natives.rs`
- Verdict: partial
- Differences:
  - `dotted_version` keeps any string whose first character is an ASCII digit, returns `None` for a missing argument or `None`, and returns an existing dotted-version unchanged. Java requires each component to match `\d+([a-z0-9]*?)?(\d+)?` (case-insensitive), rejects an empty string, empty components, punctuation, a leading minus, and any component integer outside signed 32-bit range, and fails the call.
  - Ordering treats the non-digit tail as one string and the leading digits as `u64` (`unwrap_or(0)` past `u64::MAX`). Java splits each component into a signed 32-bit head, an alpha sequence, and a signed 32-bit tail (a missing tail is 0), so `1.2beta2 < 1.2beta10` and `1.2x == 1.2x0`. The string tail reverses those (`beta10 < beta2`), and `compare_to` returns only `-1`, `0`, or `1` while a Java alpha `String.compareTo` can return a larger code-unit delta.
  - A trailing component matching `[a-z]\w*` is dropped before Java comparison (`10.10.simulator.internal` equals `10.10`). Those components stay in the Rust value, so the comparison is unequal.
  - `==` and dict hashing use the raw string. Java equality follows `compareTo`, so `1.2 == 1.2.0`, `0.1 == 0.01`, and `1.2x == 1.2x0` are the same value and the same dict key. In Rust `==` is false for those pairs even when both `<=` and `>=` are true.
  - `type()` is `host_object` rather than `DottedVersion`, and the Starlark field `to_string` is absent (`str()` and a `version` attribute expose the original text). A keyword call `dotted_version(version=...)` on the native ignores kwargs.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/platform/ConstraintCollectionApi.java`
- Bazel lines: 83
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Starlark `ConstraintCollection`. Platform constraints are a dict of setting label to value label on `PlatformInfo`, so `constraint_settings`, `get`, `has`, and `has_constraint_value` are not callable.
  - `get` returns a `ConstraintValueInfo` or `None`. Nothing returns that provider or `None` for a setting.
  - `has` is true when the `ConstraintSettingInfo` is set on this collection or a parent, or the setting has a default. The nearby `platform_has_value` checks a constraint-value label (explicit match, or the default only when unset), which is `has_constraint_value` membership, not `has`.
  - `constraint_settings` is only the settings this collection directly references. Parents are flattened into the same dict, so that direct-only sequence does not exist.
  - All four methods are enabled only with `EXPERIMENTAL_PLATFORMS_API`. There is no such gate.
  - As a `StarlarkIndexable`, index and `in` require a `ConstraintSettingInfo` key and reject other types. Dict lookup is by label and does not apply that error or the default-value fallback.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/platform/ConstraintSettingInfoApi.java`
- Bazel lines: 57
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/config.rs`
- Verdict: partial
- Differences:
  - `has_default_constraint_value` is always a Starlark struct field and is true exactly when a default constraint value is set. Zel’s `ConstraintSettingInfo` never has that field, so access fails.
  - `label` and `default_constraint_value` are struct fields only when `--experimental_platforms_api` is on (default off); otherwise Starlark access errors as experimental. Zel always exposes both and does not consult the flag.
  - `default_constraint_value` is a `ConstraintValueInfo` or `None`. Zel stores a `Label` or `None`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/platform/ConstraintValueInfoApi.java`
- Bazel lines: 52
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/config.rs`
- Verdict: partial
- Differences:
  - Bazel struct fields `constraint` (`ConstraintSettingInfo`) and `label` (`Label`) exist only when `--experimental_platforms_api` is true (`enableOnlyWithFlag = "-experimental_platforms_api"`, default false). With the flag off they are omitted from the Starlark method map, so attribute access, `getattr`, and `hasattr` fail with `'ConstraintValueInfo' value has no field or method '…'`. Zel’s `constraint_value` always stores both fields, and field lookup never reads `experimental_platforms_api` (the flag is recorded and left unused). With the flag on, both sides return the setting provider and the value’s own label.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/platform/ExecGroupCollectionApi.java`
- Bazel lines: 48
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_host.rs`
- Verdict: partial
- Differences:
  - The indexed exec-group value also has `exec_platform` (`String`, or `None` when unset). The Java type only has `toolchains`, so that access fails.
  - `toolchains` is a bare indexable map, not a `ToolchainContext`. It has no `toolchain_types` field. A missing toolchain type is an error. Bazel returns `None` when an optional toolchain did not resolve. A group with no toolchain context still indexes an empty map. Bazel fails with “Toolchains are not valid in this context”.
  - Index keys are a `Label`, a string passed through `norm_tc_key`, or a struct whose `name` is one of those. Bazel accepts a `Label`, a `ToolchainTypeInfo`, or a string parsed as a label in the evaluating `.bzl`, and rejects other key types. Non-string `in` tests return false. Bazel’s `containsKey` throws while casting the key.
  - The exec-group name `"default"` is a normal key when it is in the rule’s `exec_groups` dict. Bazel’s collection excludes that name from both indexing and membership.
  - Toolchain-type names in the missing-type error come from a `BTreeMap` (sorted). Bazel lists the resolved type labels in `ImmutableSet` iteration order.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/platform/IncompatiblePlatformProviderApi.java`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel registers an empty Starlark builtin type `IncompatiblePlatformProvider` (provider category, no fields or methods) so Starlark and cquery can recognize targets incompatible with the target platform. Zel has no such type or provider value.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/platform/PlatformBootstrap.java`
- Bazel lines: 33
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - Bazel `ImmutableMap.Builder.put("platform_common", platformCommon)` rejects a null value with `NullPointerException` and a duplicate key with `IllegalArgumentException`. Zel `BTreeMap::insert` in `install_analysis_modules` always overwrites, including the empty struct first installed for that name by `ANALYSIS_ONLY_GLOBALS`.
  - Bazel binds the injected `platform_common` module (the five real provider constructors). Zel binds a plain struct of `Value::Provider` tokens with the same names (`TemplateVariableInfo`, `ToolchainInfo`, `PlatformInfo`, `ConstraintSettingInfo`, `ConstraintValueInfo`). Calls store arbitrary kwargs and skip native provider construction; a missing attribute reports `'struct' value has no field or method …` and lists fields in `BTreeMap` order, and `repr` is `struct(...)` in that sorted order.
  - Package `.bzl` loading installs the symbol. In the bzlmod evaluator it is only a static-universe name (`BZL_BUILTIN_NAMES` in `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`) and is not inserted into globals, so a use becomes a `RuleCallable` named `platform_common` unless `ZEL_STRICT_BZLMOD` is set. Bazel registers this bootstrap in the shared `.bzl` environment.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/platform/PlatformCommonApi.java`
- Bazel lines: 72
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - Bazel `platform_common` is a builtin module (`type` is `platform_common`, `repr` is `<unknown object com.google.devtools.build.lib.rules.platform.PlatformCommon>`). Zel installs a plain struct (`type` is `struct`, `repr` prints the five fields in sorted order). A missing field is reported as `'platform_common' value has no field or method ...` in Bazel and as `'struct' value has no field or method ...` plus an available-attribute list in Zel.
  - All five provider values print as `<function Name>` in Bazel and `<provider Name>` in Zel. `type(platform_common.PlatformInfo)`, `ConstraintSettingInfo`, and `ConstraintValueInfo` are the anonymous Java class names (`…PlatformInfo$1`, `…ConstraintSettingInfo$1`, `…ConstraintValueInfo$1`). Zel reports `Provider` for every field. `ToolchainInfo` and `TemplateVariableInfo` are `Provider` on both sides.
  - `PlatformInfo`, `ConstraintSettingInfo`, and `ConstraintValueInfo` have no self-call, so calling them fails in Bazel. Zel treats them as constructors, keeps arbitrary keyword fields, and drops positional arguments.
  - `TemplateVariableInfo` takes one positional or named `vars` argument: `None` becomes an empty dict, otherwise it must be `dict<string, string>`, and the result exposes that map as `variables`. Zel ignores a positional dict, so `TemplateVariableInfo({'FOO': 'bar'})` is empty. A named `vars` is stored as `vars` with no dict or string check, and no `variables` field is created.
  - `ToolchainInfo` accepts only keyword arguments and rewrites private keys (`$` or `:` prefix) to `_`. Zel stores keyword names unchanged and silently discards positional arguments.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/platform/PlatformConfigurationApi.java`
- Bazel lines: 44
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - `platforms` is absent. Bazel returns a one-element list of the computed target platform (`ImmutableList.of(targetPlatform)`).
  - With `--platforms` unset, Bazel `platform` is the host platform (`@bazel_tools//tools:host_platform` by default, or `--host_platform`). Zel uses `cfg.platforms.first()` and, when that vector is empty, substitutes `@platforms//host:host`.
  - With `--host_platform` unset or empty, Bazel still returns `@bazel_tools//tools:host_platform` (empty input is converted to that default). Zel reads only an explicitly stored option string and otherwise substitutes `@platforms//host:host`.
  - A `host_platform` or `platform` string that `Label::parse_canonical` rejects becomes `@platforms//host:host`. Bazel resolves a host-platform label (including a non-absolute one) or fails option parsing.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/platform/PlatformInfoApi.java`
- Bazel lines: 57
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/config.rs`
- Verdict: partial
- Differences:
  - `label` and `constraints` are struct fields enabled only by `--experimental_platforms_api` (semantics key `-experimental_platforms_api`, default off). Access otherwise fails as an experimental API. Zel always returns both fields from the provider map. The flag exists and defaults to false in `/workspace/zel-cursor/crates/zel-starlark/src/semantics.rs`, but attribute access in `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs` never checks it.
  - `constraints` is a `ConstraintCollection`: index and `get` take a `ConstraintSettingInfo` and return a `ConstraintValueInfo` (parent chain, then the setting’s default, else `None` on index). `constraint_settings`, `has`, and `has_constraint_value` are part of that value. Zel stores a `BTreeMap` dict of setting label to value label (parent values copied in at platform build, keys ordered by canonical label). A missing setting is a dict key error, defaults are not inserted, and those collection methods are absent. Default fallback exists only in internal `platform_has_value` in `/workspace/zel-cursor/crates/zel-rules/src/toolchain.rs`.
  - The same provider also exposes `exec_properties`. This API’s Starlark surface is only `label` and `constraints`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/platform/ToolchainContextApi.java`
- Bazel lines: 45
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_host.rs`
- Verdict: partial
- Differences:
  - `toolchain_types` is missing. Any attribute access on the toolchains object fails, so callers never get the sequence of resolved toolchain-type labels.
  - An unresolved optional type stored as `None` is returned on index. A missing key, including an empty map, always errors with "was requested but only types […] are configured". Bazel uses a not-valid context whose index fails with "Toolchains are not valid in this context", whose membership test is false, and whose `toolchain_types` is empty.
  - String keys are not parsed with the evaluating thread's label converter. Zel only rewrites a single leading `@repo//` to `@@repo//`, and only the default map treats a `//pkg:type` string as a suffix of a canonical key. Relative labels and repo mapping do not resolve. An unparsable string becomes a missing-key error rather than a label-parse error. Exec-group maps do not apply the suffix match.
  - Index accepts a struct `name` (`config_common.toolchain_type`) and rejects a `ToolchainTypeInfo` provider (`type_label`). Bazel accepts a `Label`, a `ToolchainTypeInfo` (`type_label`), or a string, and rejects the toolchain-type requirement struct. Membership uses the raw string, ignores struct keys (returns false), and does not raise on a bad type. Bazel uses the same key transform and raises.
  - Aspect `ctx.rule.toolchains[type]` is not a provider collection from aspects applied to the base target's toolchains. Zel's `ctx.rule` has no `toolchains` field. Aspect `ctx.toolchains` is only the aspect's own `ToolchainInfo`/`None` map, and the default aspect resolver reports that toolchain resolution is unavailable.
  - With `--incompatible_auto_exec_groups`, Bazel `ctx.toolchains` is the union of the automatic exec groups' types. Zel keeps only the rule-level map and does not read that flag. The missing-key type list is sorted canonical strings (`BTreeMap`) rather than resolved-label set order.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/platform/ToolchainInfoApi.java`
- Bazel lines: 53
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`
- Verdict: partial
- Differences:
  - Bazel rejects positional arguments (`ToolchainInfo() got unexpected positional argument`). Zel’s `Value::Provider` call drops them and still builds the instance from keywords.
  - The constructor rewrites field names that start with `$` or `:` to `_` plus the rest (`Attribute.getStarlarkName`). Zel keeps the keyword, so `ToolchainInfo(**{"$x": 1})` is field `_x` in Bazel and `$x` in Zel. A pair that collapses to one name (`$x` and `_x`) fails the Bazel map build and stays two fields in Zel.
  - `type(instance)` is `ToolchainInfo`. Zel reports `ProviderInstance`.
  - `dir(instance)` lists the field names. Zel’s `dir` has no `ProviderInstance` arm and returns an empty list.
  - A missing field fails as `'ToolchainInfo' value has no field or method '…'` plus a sorted available-attributes list. Zel fails with `field '…' not found` and does not list fields.
  - Bazel instances are not subscriptable. Zel allows `instance["field"]`.
  - `repr` is `struct(k = v, …)` in Java UTF-16 key order. Zel prints `ToolchainInfo(k = v, …)` in UTF-8/code-point order, which diverges when a field name is outside the BMP.
  - The provider symbol prints as `<function ToolchainInfo>` in Bazel and `<provider ToolchainInfo>` in Zel.
  - Bazel instances are not Starlark-hashable (`isImmutable` stays false), so they cannot be dict keys. Zel accepts a `ProviderInstance` as a dict key when its fields are hashable.
  - Zel predeclares a callable global `ToolchainInfo` in `.bzl` files. Bazel exposes the constructor only as `platform_common.ToolchainInfo`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/repository/RepositoryBootstrap.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`
- Verdict: partial
- Differences:
  - Bazel `addMethods` predeclares four `.bzl` callables in every `.bzl` environment: `repository_rule`, `module_extension`, `tag_class`, and `__do_not_use_fail_with_incompatible_use_cc_configure_from_rules_cc`. Zel installs only the first three, and only inside `Rt::new_context`. The undocumented fail builtin is absent, so a call is an undefined name. In Bazel it is a no-op unless `--incompatible_use_cc_configure_from_rules_cc` is set, in which case evaluation fails.
  - The package loader binds `repository_rule`, `module_extension`, and `tag_class` to empty structs. A `.bzl` is evaluated with the real callables only when its source contains `repository_rule(`, `module_extension(`, `tag_class(`, or `tools/build_defs/repo`. Otherwise a call fails as `'struct' object is not callable`, including an alias that is invoked without those substrings in the file.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/repository/RepositoryModuleApi.java`
- Bazel lines: 297
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`
- Verdict: partial
- Differences:
  - `__do_not_use_fail_with_incompatible_use_cc_configure_from_rules_cc` is a `.bzl` global: no-op when `--incompatible_use_cc_configure_from_rules_cc` is off, and a hard failure with the rules_cc migration error when it is on. Zel never registers the symbol, so the call is an unknown name at either flag value.
  - `facts_version` is a Starlark int narrowed with signed 32-bit `toInt`, then rejected if negative. Zel uses `to_u64().unwrap_or(0) as u32`: non-integers and negatives become 0, values above `u32::MAX` wrap, and `2^31 .. 2^32-1` is accepted and later compared as `i64`.
  - `local`, `configure`, `os_dependent`, and `arch_dependent` must be bools. Zel uses `is_truthy()`, so `None`, non-zero ints, and non-empty strings are accepted and coerced.
  - `environ` must be a sequence of strings. `repository_rule` stores an `ImmutableSet` (duplicates dropped). Zel `to_string_list()` turns a non-list into `[]`, drops non-strings, accepts labels and depsets, and keeps duplicates.
  - `attrs` (`dict` or `None`) and `tag_classes` (`dict` of `tag_class`, default `{}`) are type-checked, and attribute names are validated (empty name fails; a function default must be private). Zel treats a non-dict as empty, stringifies non-string keys, and does not apply those name checks.
  - Only `implementation` is positional on `repository_rule` and `module_extension`; `tag_class`'s `attrs` is positional; every other parameter is keyword-only, and an unknown keyword or a non-string `doc` fails. Zel reads `attrs` only from keywords (so `tag_class({...})` keeps `{}`), ignores extra positionals and unknown keywords, and drops `doc`.
  - `remotable` is present only with `--experimental_repo_remote_exec`. With that flag, Java records it and adds an `exec_properties` string-dict attribute. Zel always reports the parameter unavailable, even when the flag is set.
  - Zel injects a hidden `repo_mapping` attribute and then strips that name from `repository_ctx.attr` and from call validation, so a user-declared `repo_mapping` cannot be passed or read. Java does not add that attribute.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/test/AnalysisFailureApi.java`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Starlark `AnalysisFailure` value with struct fields `label` (`Label` of the failing target) and `message` (string form of the analysis-phase error).
  - `/workspace/zel-cursor/crates/zel-rules/src/failure.rs` is console diagnostics (`events`, `abort_suffix`), not this object.
  - `AnalysisFailureInfo` is only an inert predeclared name in `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`, so callers cannot read `.label` or `.message` on a propagated failure.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/test/AnalysisFailureInfoApi.java`
- Bazel lines: 61
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - Java exposes one struct field, `causes`, as a `Depset` of `AnalysisFailure`. Zel only binds the name `AnalysisFailureInfo` as a schemaless `Value::Provider` with no `causes` field and no depset.
  - Java's provider type has no self-call constructor, so Starlark cannot build instances. Zel's `Value::Provider` call accepts any keyword arguments and stores them as instance fields.
  - Java re-propagates the provider under `--allow_analysis_failure`: a failing target with no failing deps gets one `AnalysisFailure`; otherwise `causes` is the union of dependency causes, and aspect and rule instances are merged. Zel records the flag (default false) and never constructs or merges this provider.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/test/AnalysisTestResultInfoApi.java`
- Bazel lines: 80
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`
- Verdict: partial
- Differences:
  - The constructor is the generic `Value::Provider` arm (`AnalysisTestResultInfo` is only registered as that value in `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`). `success` and `message` are required, positional or named, and typed `bool` and `string` at the call. Zel keeps only keyword arguments (positional arguments are bound solely for `ExecutionInfo`), so `AnalysisTestResultInfo(True, "m")` yields an instance with neither field, and a positional value plus the same name as a keyword does not fail as a duplicate argument.
  - Missing parameters, values that are not `bool`/`string` (including `None`), and unknown keywords fail at the Bazel call. Zel stores any keywords, including extras and `None`. A `bool`/`string` check runs later only when an `analysis_test` rule returns the provider (`/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_driver.rs`); other rules keep the unchecked fields.
  - `type(instance)` is `AnalysisTestResultInfo`. Zel reports `ProviderInstance`. `repr` of the provider is `<function AnalysisTestResultInfo>`; Zel prints `<provider AnalysisTestResultInfo>`. `repr` of an instance is `<instance of provider AnalysisTestResultInfo>`; Zel prints `AnalysisTestResultInfo(...)` with fields in `BTreeMap` order (`message` before `success`).
  - `dir(instance)` is the sorted field names `message` and `success`. Zel's `dir` has no provider-instance arm and returns `[]`.
  - String indexing and `in` on an instance fail in Bazel (not a structure or iterable). Zel indexes and tests membership by field name.
  - Two instances with the same fields compare unequal and are unhashable (`Info` uses identity and is not immutable). Zel equates and hashes them by provider name and field map.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/test/CoverageCommonApi.java`
- Bazel lines: 144
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_host.rs`
- Verdict: partial
- Differences:
  - Only `ctx` is positional in Bazel. Zel accepts all nine parameters positionally, in the order `ctx`, `source_attributes`, `dependency_attributes`, `extensions`, `metadata_files`, `baseline_coverage_files`, `coverage_support_files`, `coverage_environment`, `reported_to_actual_sources`, and never reads `ctx`, so a missing or foreign context is accepted.
  - `None` is accepted as an empty list or dict for `source_attributes`, `dependency_attributes`, `metadata_files`, `coverage_support_files`, and `coverage_environment`. Bazel rejects those (`metadata_files` is a sequence of `File` only; the environment must be a dict). `coverage_support_files` type errors are discarded, and a list of depsets is not flattened.
  - `extensions=[]` keeps every source file. Bazel adds none. A token `js` is matched as a case-sensitive `.js` suffix of the exec path. Bazel uses the raw suffix (`endsWith("js")`), case-insensitive on Windows.
  - With coverage enabled, every file on the named attributes is instrumented. Bazel also applies `--instrumentation_filter`, `--instrument_test_targets`, and `--experimental_collect_code_coverage_for_generated_files`.
  - `baseline_coverage_files` and `reported_to_actual_sources` are accepted and ignored: no baseline-coverage action is registered, and neither value is type-checked or stored. Non-empty support files, environment, or reported sources do not hit the builtins allowlist.
  - The rule's `coverage_support_files` are appended after dependency files, and path-string dedup keeps the dependency copy. Bazel puts the rule's files first in a stable-order depset. `coverage_environment` is a sorted `BTreeMap` (rule keys still overwrite). Bazel keeps first-insertion order with last-write wins. The result exposes `coverage_environment` and `coverage_support_files` as fields rather than `get_coverage_environment()` and `get_coverage_support_files()`.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/test/CoverageConfigurationApi.java`
- Bazel lines: 51
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - With coverage collection enabled, `configuration_field(fragment = "coverage", name = "output_generator")` always resolves to `@@bazel_tools//tools/test:lcov_merger`. Bazel returns the `--coverage_output_generator` label (default `@bazel_tools//tools/test:lcov_merger`).
  - `ctx.fragments.coverage` has no `output_generator` field (`Label` or `None`). The live fragment in `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_host.rs` and `fragment()` only expose boolean `collect_code_coverage`, so reading `output_generator` fails.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/test/ExecutionInfoApi.java`
- Bazel lines: 85
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`
- Verdict: partial
- Differences:
  - A call that omits an argument leaves that field absent, so `ExecutionInfo()` has neither field and `ExecutionInfo(exec_group=...)` has no `requirements`. Bazel supplies `requirements={}` and `exec_group="test"` on every instance.
  - Extra positional arguments are dropped, a parameter given both positionally and by keyword keeps the keyword, and unknown keywords are stored as fields. Bazel rejects all three.
  - `requirements` is not required to be a dict of strings and `exec_group` is not required to be a string. Non-string requirement entries are kept, then skipped when test execution requirements are collected.
  - The requirements dict is stored by reference, and constructing the provider does not freeze it, so later mutation of the caller's dict changes the field. Bazel copies the map into an immutable `dict<string, string>` at construction.
  - Provider fields are a `BTreeMap`, so repr and field iteration order is `exec_group`, then `requirements`. Bazel exposes them in declaration order: `requirements`, then `exec_group`.
  - `exec_group` is never read when building the test action. Bazel uses `getExecGroup()` to choose that action's exec group, and uses `"test"` when the provider is absent.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/test/InstrumentedFilesInfoApi.java`
- Bazel lines: 74
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/starlark_rule_host.rs`
- Verdict: partial
- Differences:
  - `instrumented_files` and `metadata_files` are depset struct fields on both sides.
  - Coverage data is the zero-argument methods `get_coverage_environment()` and `get_coverage_support_files()`, not fields. The environment method returns a fresh dict copy of the map, so mutating it does not change the provider. Both methods raise `file '<label>' cannot use private API` unless the caller is builtins or on the default internal allowlist.
  - Zel stores that data as the fields `coverage_environment` and `coverage_support_files`. Calling the Java method names fails with no such method, and there is no allowlist. The environment dict is the provider's own map, filled from a `BTreeMap`, so keys iterate in sorted order rather than first-insertion order, and in-place updates stay on the provider.
  - The Java provider symbol has no Starlark constructor. Zel's `InstrumentedFilesInfo(...)` is callable and a returned instance is accepted; omitted file fields become empty depsets, and a missing or non-dict `coverage_environment` becomes an empty map.
  - `dir()` on the Java value lists the two fields and the two methods. Zel's `dir()` on a provider instance is empty. Indexing and `in` succeed for Zel field names and fail on the Java value.
  - Java `==` and `repr` use only the two struct-field depsets (depset equality is nested-set contents) and ignore coverage data, printing `struct(...)`. Zel `==` and `repr` include every field, and depset `==` is by identity, so content-equal providers compare unequal.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/test/TestingBootstrap.java`
- Bazel lines: 53
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - `testing` is only `ExecutionInfo` and `TestEnvironment`. Bazel also binds `analysis_test`, which must be called from a BUILD thread, requires `name` to fully match `[A-Za-z_][A-Za-z0-9_]*`, rejects `attr_values["name"]`, defines a test rule with `analysis_test=True`, exports it as `name + "_test"`, and instantiates that one target. The attribute is absent in Zel.
  - `testing.TestEnvironment(environment, inherited_environment=[])` returns `RunEnvironmentInfo` (string-to-string dict, string sequence, `shouldErrorOnNonExecutableRule=false`). Zel binds a generic provider named `TestEnvironment`, so the result is not `RunEnvironmentInfo`. Positional arguments are dropped (only `ExecutionInfo` maps them), and there is no string cast or default empty `inherited_environment`.
  - `AnalysisTestResultInfo(success, message)` requires a bool and a string and accepts them positionally or by name. Zel stores only keyword fields with no type check, so a positional call yields an instance with neither field.
  - `AnalysisFailureInfo` and `InstrumentedFilesInfo` are non-callable provider keys. Zel treats both as callable providers that accept arbitrary fields. `AnalysisFailureInfo.causes` is produced by analysis, not by a Starlark constructor.
  - `coverage_common.instrumented_files_info` is bound, but `extensions=[]` keeps every source file. Bazel uses `FileTypeSet.NO_FILE` and keeps none. Bazel also applies `--instrumentation_filter`, `--instrument_test_targets`, and generated-file coverage, and turns `baseline_coverage_files` into baseline coverage artifacts. Zel ignores that argument. Non-empty `coverage_support_files`, `coverage_environment`, or `reported_to_actual_sources` is allowlisted in Bazel and type-checked (`depset` of `File`, or a sequence of `depset`/`File`). Zel skips the allowlist and drops a bad support-files value. The result exposes `coverage_environment` and `coverage_support_files` as fields. Bazel exposes allowlisted methods `get_coverage_environment` and `get_coverage_support_files`.
  - Package `.bzl` loading predeclares all five names. Module-extension evaluation (`BZL_BUILTIN_NAMES` in `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`) predeclares `testing`, `coverage_common`, and `InstrumentedFilesInfo` only, so `AnalysisFailureInfo` and `AnalysisTestResultInfo` fail static resolution there.

### `src/main/java/com/google/devtools/build/lib/starlarkbuildapi/test/TestingModuleApi.java`
- Bazel lines: 148
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - `testing.analysis_test` is absent. Bazel, only on a BUILD thread, checks `name` against `[A-Za-z_][A-Za-z0-9_]*`, rejects `name` inside `attr_values`, builds a rule with `test=True`, `analysis_test=True`, and `executable=False` (defaults `attrs={}`, `fragments=[]`, `toolchains=[]`, `attr_values={}`), exports it as `{name}_test`, and instantiates one target. Zel only treats `analysis_test` as a flag on an explicit `rule()` call and does not create that target.
  - `testing.TestEnvironment(environment, inherited_environment=[])` returns `RunEnvironmentInfo` with `shouldErrorOnNonExecutableRule=false`: `environment` is a required dict of string to string, `inherited_environment` defaults to `[]` and must be a sequence of strings, and a non-executable, non-test rule warns instead of erroring. Zel exposes a separate `TestEnvironment` provider, drops positional arguments, does not default or type-check those parameters, and analysis applies only `RunEnvironmentInfo`, so the variables are ignored and the warning is not produced.
  - `testing.ExecutionInfo(requirements={}, exec_group="test")` requires a string-to-string `requirements` dict and a string `exec_group`, and the test action runs in that exec group (`"test"` when omitted). Zel maps only the first two positional arguments, accepts any types and extra keywords, keeps a keyword when the same parameter is also positional, and does not fill those defaults. Non-string requirement entries are skipped when applied, and `exec_group` is never read, so a non-`"test"` group is ignored.

### `src/main/java/com/google/devtools/build/lib/starlarkdebug/module/StarlarkDebuggerModule.java`
- Bazel lines: 131
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No module lifecycle attaches a Starlark debugger only when `debugStarlark` is set, and only exposes `StarlarkDebuggerOptions` for the `build` command.
  - No server startup that waits for a client, installs the debugger, and reports `Error while setting up the debug server: ` on I/O or interrupt failure.
  - No reset-analysis path that deletes `FILE` Skyframe nodes whose path is in the breakpoint set, blocks the build on a latch until breakpoints arrive or the server closes, and clears the debugger after the command and on shutdown or crash.

### `src/main/java/com/google/devtools/build/lib/starlarkdebug/proto/starlark_debugging.proto`
- Bazel lines: 337
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Starlark debug wire protocol: no `DebugRequest`/`DebugEvent` payloads (breakpoints, continue, evaluate, frames, start, pause, children), no `Stepping` (`NONE`/`INTO`/`OVER`/`OUT`) or `PauseReason` values, and no breakpoint, frame, scope, location, or value model (`thread_id` 0 continues or pauses all threads, one breakpoint per line, conditional expressions, unset column). Only the `--experimental_skylark_debug*` flags exist, with no server that blocks execution until `StartDebuggingRequest`.

### `src/main/java/com/google/devtools/build/lib/starlarkdebug/server/ConditionalBreakpointException.java`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel uses this checked exception as the sole signal that a conditional breakpoint expression failed, carrying only the evaluator message (`EvalException.getMessageWithStack()`, or `getMessage()` for a syntax error or interruption). Zel has no Starlark debug server and no error that pauses the thread as `CONDITIONAL_BREAKPOINT_ERROR` with that message.

### `src/main/java/com/google/devtools/build/lib/starlarkdebug/server/DebugEventHelper.java`
- Bazel lines: 187
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel code builds the Starlark debug events and responses this helper emits: error (default sequence number 0), set-breakpoints, continue-execution, evaluate, list-frames, start-debugging, pause-thread, get-children, thread-paused, and thread-continued.
  - No counterpart copies a source location (line, column, path), or returns null when the location is null, or builds a frame from the function name, scopes, and location.
  - No counterpart builds scopes as local then global, drops local names from module globals, and emits only the global scope when there are no locals.
  - No counterpart maps stepping INTO, OUT, OVER, and NONE, or rejects an unrecognized stepping value.

### `src/main/java/com/google/devtools/build/lib/starlarkdebug/server/DebugRequestException.java`
- Bazel lines: 26
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Checked exception that stores only a message string. Debug-request handlers throw it, and the server maps `getMessage()` to a debug error event. Zel has no Starlark debug-server request path or equivalent error type (only debugger option strings in flag tables).

### `src/main/java/com/google/devtools/build/lib/starlarkdebug/server/DebugServerTransport.java`
- Bazel lines: 134
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never opens a Starlark debug socket, blocks until one client connects, or exchanges length-delimited `DebugRequest`/`DebugEvent` messages. Missing with that are the progress and success events, returning null on EOF or any read `IOException` (logging a parse error and posting a debug error event only while the connection is still open), verbose request and event logs, logging a send failure without failing the caller, and closing or treating as closed both the client socket and the listening server socket.

### `src/main/java/com/google/devtools/build/lib/starlarkdebug/server/DebuggerSerialization.java`
- Bazel lines: 185
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Starlark debugger value serialization. Bazel builds a debug `Value` with label, `Starlark.type`, raw string or `Starlark.repr` description, `hasChildren`, and a registered object id (or `0` when there are no children). Children come from non-empty maps, map entries labeled `key`/`value`, non-empty iterables and arrays as `[n]`, debug attributes, naturally sorted `Structure` fields (null values skipped; `EvalException` and `IllegalArgumentException` swallowed), and `Starlark.dir`/`getattr` on other `StarlarkValue`s under default semantics (`IllegalArgumentException`, `EvalException`, and `InterruptedException` swallowed). `StarlarkInt` is childless; other values are treated as childless.

### `src/main/java/com/google/devtools/build/lib/starlarkdebug/server/StarlarkDebugServer.java`
- Bazel lines: 274
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never binds a debug `ServerSocket` (backlog 1), waits for one client, or runs a daemon listener that reads `DebugRequest`s until null, posts a `DebugEvent` only when the handler returns one, logs the full stack on any throwable only while the transport is still open, then always logs shutdown and closes.
  - No close path that optionally logs a verbose debug event, closes the transport, runs `onClose`, logs an I/O stack trace, and always resumes every paused thread so the build can finish. `before` never pauses the current thread while the transport is open.
  - No request dispatch: `START_DEBUGGING` does not call `beforeDebuggingStart` with breakpoint paths then resume all threads; thread id 0 on continue or pause does not mean all threads (a specific continue also applies stepping); list-frames, set-breakpoints, evaluate, and get-children are absent. `DebugRequestException` is not turned into an error event with only the message. `PAYLOAD_NOT_SET` builds and drops a "No request payload found" event, then, like any other unset payload, returns "Unhandled request type". `DebugCallback` defaults (`beforeDebuggingStart`, `maybeBlockBeforeStart`, `onClose`) are absent.

### `src/main/java/com/google/devtools/build/lib/starlarkdebug/server/ThreadHandler.java`
- Bazel lines: 427
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Starlark debug-server thread controller. Missing with that are the initial pause-all until debugging starts, asynchronous pause-all and pause-only-if-stepping (already-paused versus unknown-thread errors), location breakpoints keyed with the column cleared and a later duplicate location replacing the earlier one, resume-all clearing stepping and continuing with no step, resume of a stepping thread clearing the step without an error while an unknown thread fails as `Unknown thread %s: cannot resume.`, pause checks skipped during a debugger eval, pause priority of global pause then initializing then pause-request then a line breakpoint (empty condition always hits; a failing condition pauses as a conditional-breakpoint error carrying the evaluator message) then stepping, blocking the paused thread until continue, reversed frame lists, child lookup only while paused, and evaluating statements in the innermost module as `<debug eval>` (expression value, or null when the last statement is not an expression).

### `src/main/java/com/google/devtools/build/lib/starlarkdebug/server/ThreadObjectMap.java`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no per-paused-thread id-to-value map. Bazel assigns a fresh signed 64-bit id on every `registerValue` (first id is 1; `AtomicLong.getAndIncrement` wraps from `Long.MAX_VALUE` to `Long.MIN_VALUE`), keeps every id for the same object, rejects a null value (`ConcurrentHashMap.put` throws), and returns null from `getValue` when the id is absent. Only the Starlark debugger option strings exist in flag tables.

### `src/main/java/com/google/devtools/build/lib/starlarkdocextract/AttributeInfoExtractor.java`
- Bazel lines: 168
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never builds a Stardoc `AttributeInfo`: it does not record the public name (internal-to-Unicode), type, mandatory flag, optional doc string, `nonconfigurable` when the attribute is not configurable, `natively_defined` when it is not Starlark-defined, or provider-name groups from required Starlark providers.
  - Bazel sets `default_value` only when the attribute is not mandatory, using a Starlark repr without the label constructor, and substitutes `<unrepresentable value>` on `InvalidStarlarkValueException`. Allowed values are appended the same way only when the set is an `AllowedValueSet`. Zel does neither.
  - Bazel emits caller-supplied implicit attribute infos first (map iteration order), then skips any attribute whose private name is already in that map, and documents an attribute only when it is Starlark-defined or native extraction is enabled, it is documented, and its public name is public. Zel has no such filter or ordering.
  - Bazel maps types to `AttributeType`: `INTEGER` and `TRISTATE` to `INT`; `LABEL`, `NODEP_LABEL`, `GENQUERY_SCOPE_TYPE`, and `DORMANT_LABEL` to `LABEL`; `STRING` and `STRING_NO_INTERN` to `NAME` when the public name is exactly `name`, otherwise `STRING`; `STRING_LIST`, `STRING_SET`, and `LICENSE` to `STRING_LIST`; `INTEGER_LIST` to `INT_LIST`; the four label-list types to `LABEL_LIST`; plus `BOOLEAN`, `LABEL_KEYED_STRING_DICT`, `STRING_DICT`, `STRING_LIST_DICT`, `LABEL_LIST_DICT`, `LABEL_DICT_UNARY`, `OUTPUT`, and `OUTPUT_LIST`, with everything else `UNKNOWN`. Zel does not perform this mapping. `starlark_doc_extract` is only a rule schema with implicit `%{name}.binaryproto` and `%{name}.textproto` outputs and is analyzed without writing those protos.

### `src/main/java/com/google/devtools/build/lib/starlarkdocextract/ExtractionException.java`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel error prefixes a failure with the Starlark module filename from `BazelModuleContext` (`in <filename>: <message>`). When that context is absent, Java returns the message unchanged, including null. When the context exists and the message is null, Java substitutes `cause.getMessage()`, then renders a null message as empty. Zel never applies those fallbacks.
  - `starlark_doc_extract` only declares implicit proto outputs and produces no files. It does not extract module documentation or raise this failure.

### `src/main/java/com/google/devtools/build/lib/starlarkdocextract/ExtractorContext.java`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No extractor settings object: required label renderer, provider-key to first qualified-name map (default empty, null rejected), and `extractNativelyDefinedAttrs` defaulting to false.
  - No public-name test: non-empty and `Character.isAlphabetic` on the first UTF-16 code unit (leading `_`, `$`, digits, and unpaired surrogates are private).
  - No documented provider name: map lookup by provider key, otherwise `provider.toString()` (key name or legacy struct id).
  - `starlark_doc_extract` is only a rule-class stub with `%{name}.binaryproto` / `%{name}.textproto` implicit outputs; analysis emits no files and never applies these rules.

### `src/main/java/com/google/devtools/build/lib/starlarkdocextract/LabelRenderer.java`
- Bazel lines: 117
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No doc-extract label renderer. `render` uses `getShorthandDisplayForm` (or `getDisplayForm` when shorthand is false) with the main-repo `RepositoryMapping`. If `mainRepoName` is set and the string does not already start with `@`, it prefixes `@<mainRepoName>`. Shorthand `//:<mainRepoName>` is rewritten to `@<mainRepoName>`. Zel never does that rewrite. `render_main_repo_name` is only an unused `starlark_doc_extract` attribute (default false) in `/workspace/zel-cursor/crates/zel-packages/src/rule_defs_gen.rs`.
  - `repr` prints each embedded label as `Label(<quoted getDisplayForm>)`, keeping the `:name` part. `reprWithoutLabelConstructor` prints the shorthand form as a quoted Starlark string with no `Label()` wrapper. Zel has neither printer: `Value::repr` always emits `Label` of the unambiguous canonical form (`@@` stripped only when the remainder starts with `//`), and label `Display` prints that canonical form with no constructor and no shorthand elision.
  - With an empty mapping and no main-repo name (`LabelRenderer.DEFAULT`), non-main repos stay canonical (`@@name//pkg:name`) and the main repo stays repo-relative. Apparent names (`@protobuf//...`) come from `RepositoryMapping.getInverse`. The nearest print helper, `label_shorthand_display` in `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`, takes the first `BTreeMap` apparent name whose canonical value matches, so the winner follows sorted apparent-name order rather than Bazel’s inverse mapping.

### `src/main/java/com/google/devtools/build/lib/starlarkdocextract/ModuleInfoExtractor.java`
- Bazel lines: 661
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No walk of a loaded module’s public globals and nested struct fields that builds `ModuleInfo` (module docstring and rendered file label, then rules, macros, providers, functions, aspects, repository rules, module extensions, and other symbols that have doc comments).
  - Unused or conflicting doc comments are not rejected (pluralized line list) unless `allowUnusedDocComments` is set, and symbols that carry their own `doc` argument are not rejected when a separate doc comment is also present.
  - Provider qualified names are not collected in a first pass over every public provider (first name wins, independent of the wanted-name filter) for later rule and attribute cross-references.
  - Implicit macro attributes (`name`, `visibility`) and the implicit repository-rule `name` attribute, including their fixed docstrings and flags, are not emitted. Aspect `attr_aspects` lists, provider schemas and `init`, and module-extension tag classes are not extracted.

### `src/main/java/com/google/devtools/build/lib/starlarkdocextract/ProviderNameGroupExtractor.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel code builds a Stardoc `ProviderNameGroup`. Bazel walks the provider set in iteration order and emits parallel `provider_name` and `origin_key` entries: the documented name (first qualified export name, otherwise `provider.toString()`), an origin name of `provider.toString()`, `file` set to the label-rendered defining module for a Starlark provider key, `"<native>"` for a builtin provider key, and `file` left unset for any other key (legacy struct providers). Both the documented name and the origin name and rendered label pass through `internalToUnicode`.

### `src/main/java/com/google/devtools/build/lib/starlarkdocextract/RuleInfoExtractor.java`
- Bazel lines: 110
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never builds a Stardoc `RuleInfo`. It does not record the qualified rule name, the origin key (`<native>` for native rules, the rendered Starlark extension label, or the definition-environment label for an unexported repository rule), an optional docstring, `test` for a test rule class, `executable` when `$is_executable` is a boolean attribute, the implicit mandatory `name` attribute, documentable attributes, or advertised provider groups. `starlark_doc_extract` is only a rule schema plus a no-file analysis stub.

### `src/main/java/com/google/devtools/build/lib/starlarkdocextract/StarlarkFunctionInfoExtractor.java`
- Bazel lines: 221
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel builds a `StarlarkFunctionInfo` from the scoped function name (`internalToUnicode`), the parsed docstring, every parameter, and an origin key. Zel never extracts function documentation into that proto. `starlark_doc_extract` is only a rule-class stub that emits no proto, and `UserFunctionDef` keeps name, params, body, location, module, and closure with no documentation string.
  - A null function docstring leaves doc, return, and deprecation unset and still emits parameters. A non-null docstring is parsed. Any parse error throws `ExtractionException` whose message is `Unable to generate documentation for function <name> (defined at <location>) due to malformed docstring`, then one ` <location> line <n>: <message>` line per error with newlines in the parse message replaced by spaces. The function name in that message is not passed through `internalToUnicode`. Zel has no docstring parser and no such failure.
  - On success the doc string is the summary, then `\n\n` only when both summary and long description are non-empty, then the long description, then `internalToUnicode` (including the empty string). Non-empty `Returns` and `Deprecated` sections become return and deprecation docs the same way. Empty sections stay unset. Parameter docs are stored by docstring parameter name, last duplicate wins, and names that do not match a real parameter are dropped.
  - Parameters follow `getParameterNames()` order. `*args` is at `numOrdinary + numKeywordOnly` when present and `**kwargs` is the last name when present. Those two get roles varargs and kwargs, `mandatory` false, no default, and docs looked up as `*` + name and `**` + name. Other parameters are ordinary when the index is below `numOrdinary`, otherwise keyword-only. A null `getDefaultValue` sets `mandatory` true and omits the default. A non-null default sets `mandatory` false and `default_value` to `internalToUnicode(labelRenderer.repr(default))`. A missing doc leaves the doc field unset. Index invariants that fail raise `IllegalStateException`. Zel binds the same parameter shapes only for calls and does not render roles, mandatory flags, or label defaults.
  - The origin name is the first module global whose value equals the function, encoded with `internalToUnicode`. Nested functions and lambdas that are not globals leave the name unset, and globals nested in structs, dicts, or lists are not searched. If `BazelModuleContext` is present, the file is `internalToUnicode(labelRenderer.render(label))`. Otherwise the file stays unset. Zel does not build that origin key.

### `src/main/java/net/starlark/java/annot/Param.java`
- Bazel lines: 121
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no shared `@Param` metadata type, so the annotation contract is absent: required `name`, `doc` default `""`, `documented` default `true`, `defaultValue` default `""` (empty means mandatory; the expression `"unbound"` is the sentinel), `allowedTypes` default `{}` (any value assignable to the parameter class), `named` default `false`, `positional` default `true`, and `enableOnlyWithFlag` / `disableWithFlag` default `""` (at most one non-empty, and only with a non-empty default). Builtin arguments are bound ad hoc at call sites.

### `src/main/java/net/starlark/java/annot/ParamType.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no runtime annotation or type carrying a required parameter class plus `generic1` defaulting to `Object`, which Bazel keeps for documentation and for `TypeChecker.fromAnnotation` when the class has one type parameter.

### `src/main/java/net/starlark/java/annot/StarlarkAnnotations.java`
- Bazel lines: 200
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel code walks a class and interface hierarchy to choose the most specific `@StarlarkBuiltin` or a signature-assignable `@StarlarkMethod` (superclass before interfaces, null when absent, `IllegalArgumentException` when two annotated ancestors are incomparable, `IllegalStateException` for an incompatible or duplicate annotated overload). Zel type names and methods are fixed on each value variant.

### `src/main/java/net/starlark/java/annot/StarlarkBuiltin.java`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel type carries this annotation’s metadata (`name`, `doc` default `""`, `documented` default `true`, `category` default `""`) or the inherited/overridden builtin-type lookup described here. `Value::type_name` in `/workspace/zel-cursor/crates/zel-starlark/src/types/value.rs` only hardcodes `type(x)` strings, and `/workspace/zel-cursor/crates/zel-packages/src/builtins.rs` mirrors `StarlarkBuiltinsFunction`, not this annotation.

### `src/main/java/net/starlark/java/annot/StarlarkMethod.java`
- Bazel lines: 206
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Rust annotation or method descriptor carries this member schema and defaults (`name`; `doc` `""`; `documented` true; `structField` false; `parameters` empty; `extraPositionals` and `extraKeywords` as a `Param` with `name` `""`; `selfCall`, `allowReturnNones`, `useStarlarkThread`, and `useStarlarkSemantics` false; `enableOnlyWithFlag` and `disableWithFlag` `""`), nor the call rules those flags encode (null returns become `None` only when allowed, thread and semantics parameters, flag gating, single `selfCall`). Argument binding elsewhere covers specific call sites, not this annotation.

### `src/main/java/net/starlark/java/annot/processor/StarlarkMethodProcessor.java`
- Bazel lines: 525
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no compile-time checker for `@StarlarkMethod` or `@StarlarkBuiltin` declarations, so the processor's errors are not enforced: methods must be public and non-static; names must be non-empty and unique per class; `documented` requires a non-empty `doc`; `structField` rejects thread, extra positional or keyword, self-call, and `Param` annotations; only one `selfCall` per class; flag names need a `+` or `-` prefix and only one of enable or disable may be set; `@Nullable` must match `allowReturnNones`; final return types must be Starlark-legal; parameter order, defaults, documentation, `Integer` rejection, `allowedTypes`, generic wildcards, and special `extraPositionals` (`Tuple`), `extraKeywords` (`Dict<String, Object>`), `StarlarkThread`, and `StarlarkSemantics` parameters must match the annotation.

### `src/main/java/net/starlark/java/cmd/Main.java`
- Bazel lines: 231
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel binary or crate entry point implements this standalone interpreter: `-c` / `-cpuprofile` / `--` flag parsing, single-file execution, the `>> ` / `.. ` REPL (blank line ends a chunk; trailing `if`/`def`/`for` stays open), `FileOptions` with toplevel rebinding and global load bindings, print-handler stdout, syntax and eval errors on stderr, read errors, the usage line, CPU profiling start/stop, or exits `0`/`1` (`System.exit`). `zel-starlark` is a library evaluator only; `examples/syntax_errors.rs` only prints parse errors and does not match this command.

### `src/main/java/net/starlark/java/eval/BuiltinFunction.java`
- Bazel lines: 520
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/ctxargs.rs`
- Verdict: partial
- Differences:
  - Keyword binding walks a `BTreeMap` (sorted by name). Java applies names in call order, and `**kwargs` in dict insertion order, so the first reported unexpected keyword, positional-only keyword, or duplicate can differ. `symlink(target=…, link_name=…)` reports `link_name` in Zel and `target` in Java.
  - Surplus positionals are rejected before keywords. `which("ls", 1, bogus=True)` is `which() accepts no more than 1 positional argument but got 2` in Zel. Java reports `which() got unexpected keyword argument 'bogus'` first.
  - `download` / `download_and_extract` `url` accepts any `java.lang.Iterable` (dict, set, and range, as well as list and tuple). Zel allows only a string, list, or tuple, so a dict or set of URL strings fails with `want 'string or Iterable'` instead of being iterated.
  - `is_dev_dependency` and `tag_sort_key` require a `bazel_module_tag`. The port types `tag` as any, so a bad value does not get `got value of type '…', want 'bazel_module_tag'` (the later errors are `expects a tag` and `expects a tag object`).
  - `watch_tree` is given a keyword-only `exclude` parameter that the Java method does not declare, so `exclude=` is an unexpected keyword in Java and is accepted by the port.
  - `f(name=…, **{"name": …})` is `got multiple values for argument 'name'` in Java. The port only sees the already merged map, so the call keeps the `**` value.
  - “did you mean” uses Java UTF-16 `length`/`charAt` and locale `toLowerCase`, versus Rust Unicode scalars and `to_lowercase`, so a non-ASCII keyword can get a different suggestion or a different edit-distance cutoff.

### `src/main/java/net/starlark/java/eval/CallUtils.java`
- Bazel lines: 196
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no semantics-and-class method registry. Java rewrites `String` to `StringModule`, skips synthetic methods, and drops methods whose `enableOnlyWithFlag` or `disableWithFlag` does not match the current semantics. Zel method sets are fixed per type, so those flags never add or remove a method.
  - A second `selfCall` method fails with `IllegalArgumentException` and the text `Class %s has two selfCall methods defined`. `getSelfCallMethod` returns null when none exists. Zel has no self-call lookup and never raises that error.
  - Two annotated methods with the same Starlark name abort descriptor construction (`ImmutableMap.buildOrThrow`). Zel does not reject duplicate annotated names.
  - The method map is inserted in Java method-name order. Zel has no equivalent ordered descriptor map.

### `src/main/java/net/starlark/java/eval/CpuProfiler.java`
- Bazel lines: 471
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Starlark CPU profiler in the Zel crates: no SIGPROF/interval-timer sampling, OS-tid to thread map, tick counter, or gzip pprof writer (`start`/`stop` errors, period in microseconds, sample value `ticks * period`, reversed stacks, string/function/location ids). `--starlark_cpu_profile` is only a flag string in `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`; `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` is the JSON trace profiler, and `/workspace/zel-cursor/crates/zel-starlark` has no profile hooks.

### `src/main/java/net/starlark/java/eval/Debug.java`
- Bazel lines: 176
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Starlark debugger hook. Missing: a global `setDebugger` that atomically replaces the hook and `close()`s the previous non-null debugger (`null` disables); `stepControl` (`NONE` returns null; `INTO` pauses on the next statement; `OVER` pauses when stack depth is ≤ the depth at the call; `OUT` returns null at depth 0 and otherwise pauses when depth is strictly smaller; any other stepping throws `IllegalArgumentException` with text `Unsupported stepping type: ` plus the stepping value); `getCallStack` (outermost frame first); `setThreadHook` (`onPushFirst` / `onPopLast`, plain replace with no close); and the `Debugger.before`, `ValueWithDebugAttributes`, `DebugAttribute`, and `Frame` inspection API.

### `src/main/java/net/starlark/java/eval/Dict.java`
- Bazel lines: 834
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/dict.rs`
- Verdict: partial
- Differences:
  - `popitem` removes the first insertion-order pair and, on an empty dict, raises `popitem: empty dictionary` before the frozen check. Zel `Vec::pop` removes the last pair, raises `popitem(): dictionary is empty`, and a frozen empty dict raises `trying to mutate a frozen dict value` instead.
  - A missing `pop` key raises `KeyError: <repr>` after a hashability check, including when a default is supplied. Zel raises `key <repr> not found in dictionary` and, for an unhashable key, returns the default or that key-not-found error.
  - `get` and `k in d` reject unhashable keys (`unhashable type: '…'`). Zel `get` returns the default (`None` if omitted) and `in` is false.
  - `pop` and `setdefault` take `default` by keyword. Zel reads only the second positional argument, so `pop(k, default=v)` ignores `v` and `setdefault(k, default=v)` inserts `None`.
  - `dict`/`update` accept any iterable of pairs (dict, list, tuple, set, range) and report `in <fn>, got <type>, want iterable`, `dictionary update sequence element #N is not iterable (<type>)`, and `item #N has length L, but exactly two elements are required`. Zel `dict()` accepts only dict, list, and tuple (`cannot convert <type> to dict`); `update` also walks sets, ranges, and strings (strings yield characters) and uses `got value of type '<type>', want 'iterable'` or `dictionary item must be a 2-element sequence`.
  - `d |= other` merges into the same dict and fails if `d` is frozen. Zel `|=` rebinds the name to a new unfrozen dict from `union`, so aliases of the original dict stay unchanged and a frozen left-hand side still succeeds.
  - `for` and comprehensions both hold the dict's iterator lock (`dict value is temporarily immutable due to active for-loop iteration`). Zel increments `iter_count` only for `for` statements, so a comprehension can mutate the dict it iterates.
  - After the enclosing function returns or the module finishes, Bazel freezes that dict. Zel leaves ordinary dicts mutable. `freeze()` also recursively freezes every stored key and value; Bazel `unsafeShallowFreeze` only marks that dict immutable.
  - `repr` of a cyclic dict prints `...` for the repeated value. Zel walks entries with no cycle break.
  - `ImmutableKeyTrackingDict` records keys read via `get`/`containsKey`/`keySet`/`entrySet` for `ctx.var` required-fragment tracking. Zel dicts do not record accessed keys.
  - NaN is a legal Bazel dict key (`NaN` equals `NaN`). Zel treats NaN as unhashable (`unhashable type: 'float'`).

### `src/main/java/net/starlark/java/eval/Eval.java`
- Bazel lines: 958
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`
- Verdict: partial
- Differences:
  - Java counts every statement and expression against `stepLimit` and raises `Starlark computation cancelled: too many steps`, and calls `checkInterrupt` on function entry, each `for` iteration, and each call. Zel never reads `max_computation_steps` and has no interrupt check.
  - At top level, outside `if`/`for`, Java runs `postAssignHook` and `StarlarkFunction.export` after assignments and `def`. Zel only stores the value.
  - Augmented assignment evaluates the target before the right-hand side in Java (`object`, key, or field, then the right-hand side). Zel evaluates the right-hand side first for every assignment.
  - Java updates `list += list`, `dict |= map`, and `set |= &= ^= -=` in place. Zel builds a new list, dict, or set, so other aliases do not see the update. `x.f = ...` and `x.f += ...` call `setField` in Java; Zel always errors with `cannot assign to attribute`.
  - Sequence assignment in Java rejects strings and other non-sequences with `got '<type>' in sequence assignment (want N-element sequence)` and length mismatches with `too few/many values to unpack (got A, want B)`. Zel treats a string as a sequence of Unicode scalars and reports `cannot unpack A values into B variables` or `got value of type '...', want 'iterable'`.
  - Java `load` requires a thread loader (`load statements may not be executed in this thread`), errors with `module '<name>' not found` when the loader returns null, and appends `SpellChecker.didYouMean` to `file '<mod>' does not contain symbol '<name>'`. Without a load hook, Zel inserts a `RuleCallable` and continues; with a hook, the missing-symbol error has no did-you-mean suffix.
  - An unbound resolved name is an error in Java: `<SCOPE> variable '<name>' is referenced before assignment.` Zel looks up by name through the enclosing locals map, so an unassigned local can see an enclosing binding. In strict mode the text is `name '<name>' is not defined`; otherwise the name becomes a `RuleCallable`. `break`/`continue` that escape a function body are ignored by `execFunctionBody` in Java and are `flow statement outside loop` in Zel.
  - `for` and comprehensions in Java use `Starlark.toIterable` (`type '<t>' is not iterable`; strings and depsets are not iterable) and hold the iterator lock for the whole loop. Zel `for` reports `iteration over depset is forbidden` and otherwise iterates strings as Unicode scalars; comprehensions do not take the iterator lock, and they snapshot and restore `globals`, discarding global assignments made inside the comprehension.
  - Java indexes and slices strings by UTF-16 `length`/`charAt` and requires a signed 32-bit int (`got <value> for string index/slice step/start index, want value in signed 32-bit range` or `want int`). Zel indexes and slices by Unicode scalars and `i64`. A non-`i64` slice bound is treated as omitted, so a non-int step becomes 1. A non-indexable value is `type '<t>' has no operator [](<k>)` in Java and `type '<t>' is not indexable with '<k>'` in Zel. Slicing a `range` stays a `range` in Java (step clamped to 32-bit); Zel materializes a list. List assignment uses the same 32-bit index rule and `can only assign an element in a dictionary or a list` in Java; Zel uses `i64` and `type '<t>' does not support item assignment` / `list index out of range`.
  - Java evaluates the callee, then positionals, keywords, `*args`, and `**kwargs`, and walks `**kwargs` in dict insertion order. Zel evaluates arguments before a non-method callee, and `kw_call_order` appends `**` keys in `BTreeMap` key order, so the bound `**kwargs` dict and unexpected-keyword errors follow sorted order.
  - Java evaluates parameter defaults when the `def` or `lambda` is created, and typechecks them when `EXPERIMENTAL_STARLARK_TYPE_CHECKING` is on. Zel evaluates `def` defaults at definition, but evaluates `lambda` defaults at the call and does not typecheck them. An evaluated `isinstance` expression is `isinstance() is not yet supported` in Java; Zel has no such expression and treats `isinstance` as a normal call.

### `src/main/java/net/starlark/java/eval/EvalException.java`
- Bazel lines: 250
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/eval/thread.rs`
- Verdict: partial
- Differences:
  - Java keeps the message, optional cause, `includeStackTrace`, and call stack as separate fields: `getMessage()` is only the message (a null cause message falls back to `cause.toString()`), a null message throws, and `toString` adds a stack only when `includeStackTrace` is set and a stack was attached. Zel’s `StarlarkError` is one `String` with no cause, no suppress-stack flag, and no null message.
  - Java fills the stack once (`withCallStack` ignores a later stack and rejects an empty one; `ensureStack` copies the thread stack or throws `IllegalStateException("empty callstack")`). Zel never stores a stack on the error; `fail()` formats frames immediately into the string, and other errors stay a bare message or `file:line:col: message` instead of growing a traceback on unwind.
  - Java’s formatted text is `Traceback (most recent call last):` plus frames, then `Error: ` plus the message, or `Error in <builtin>: ` with that innermost `BUILTIN` frame omitted and no location before `Traceback`. A zero column is omitted, and a zero line omits both line and column. `format_stack_trace_at` always prefixes `{file}:{line}:{column}: `, always prints line and column (including 0), and does not rewrite a builtin leaf; the unused `format_stack_trace` uses two spaces, drops the column and source line, and returns an empty string when there are no frames.
  - Java prints each recorded frame’s own name and program counter, outermost first. Zel inserts a synthetic `<toplevel>` from the first frame when its file is non-empty, then reports each frame at the next frame’s location (or the failure location for the last), and still emits `<toplevel>` at that location when the frame list is empty.
  - The default Java reader reads the file as strict UTF-8, splits only on `\n`, trims code points `<= U+0020`, and yields no line on I/O or decode failure; a global synchronized supplier can replace it. Zel checks a thread-local in-memory source map first, then `read_to_string` (strict UTF-8), splits with `lines()` (also lone `\r`), and trims Unicode `White_Space`. Line and column are signed 32-bit in Java (`line - 1` wraps at `Integer.MIN_VALUE`) and `usize` in Zel.

### `src/main/java/net/starlark/java/eval/EvalUtils.java`
- Bazel lines: 537
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`
- Verdict: partial
- Differences:
  - String indexing and slicing use Unicode scalars (`chars()`). Java uses UTF-16 code units (`length` / `charAt`), so a non-BMP character is one index in Zel and two in Bazel, and a Java index can return a lone surrogate.
  - Indexes, slice bounds, repeat counts, and shift counts must fit in a signed 32-bit int (`toInt`: "got … for …, want value in signed 32-bit range"). Zel uses `i64`: a non-`i64` index is a bare "index out of range"; a non-`i64` slice bound or step is treated as omitted (step defaults to 1); a non-`i64` repeat count becomes 0; shift counts up to `i64` are accepted (right shift of `2^31` succeeds; left shift fails only at `>= 512` with "shift count too large").
  - Repeat of a non-positive count is `""` or an empty list in both, but Java also errors "excessive repeat (…)" when the UTF-16 length times the count exceeds `Integer.MAX_VALUE` (strings) or `2^30` elements (lists). Zel repeats any non-negative `i64` with no size cap.
  - Int/float `+`, `-`, `*`, `/`, `//`, and `%` call `toFiniteDouble` and error "int too large to convert to float". Zel's `to_f64` overflows to infinity.
  - Real division by zero is "floating-point division by zero" for both int and float. Zel errors "division by zero" only for int/int; float and mixed `/` yield IEEE infinity.
  - Java `%` supports int/float, float/int, and float/float, and zero is "floating-point modulo by zero". Zel implements only float/float ("integer modulo by zero"); mixed int/float modulo is an unsupported binary operation.
  - String `%` with a non-tuple right-hand side is one positional argument (no `%(key)` and no width or precision). Zel treats a dict as a name map (`%(key)s`, dropping non-string keys) and accepts width and precision.
  - Ordered int/float comparison is exact (`compareIntAndDouble`, NaN greater than every int). Zel truncates the float with `as i64` first, so `1 < 1.9` is false.
  - Set `|`, `&`, `^`, and `-` run only when `experimental_enable_starlark_set` is on. Zel always performs them and does not read the flag.
  - `in` / `not in` on a non-container is "unsupported binary operation: <type> in <type>" after the `HasBinary` fallback. Zel reports "type '<type>' is not iterable". For a non-string left operand of `in <string>`, Java quotes the type (`not 'int'`); Zel does not (`not int`).
  - Unsupported unary `+` and `-` are "unsupported unary operation: +<type>" and "-<type>". Zel reports "unsupported unary +: <type>" and "unsupported unary -: <type>".
  - Item assignment on a non-list, non-dict is "can only assign an element in a dictionary or a list, not in a '<type>'"; a bad list index is "index out of range (index is N, but sequence has M elements)" or "got <value> for list index, want int". Zel reports "type '<type>' does not support item assignment", "list index out of range", and "list index must be integer, not <type>". A non-int string index is "got <type> for string index, want int" versus "type 'string' is not indexable with '<type>'", and other failed indexes are "type '%s' has no operator [](%s)" versus "type '<type>' is not indexable with '<type>'".
  - Field assignment calls `Structure.setField` (default "<type> value does not support field assignment"). Zel rejects every attribute assignment with "cannot assign to attribute".
  - After the built-in cases, Java tries `HasBinary` on the left operand and then the right, so same-provider `+` merges and a shared field is "cannot add struct instances with common field '<name>'". Zel merges only `Value::Struct` ("cannot add structs: field '<name>' appears in both operands"); `ProviderInstance +` is unsupported.
  - `addIterator` / `removeIterator` freeze the iterable for both `for` loops and comprehensions. Zel increments `iter_count` only for `for` over list, dict, and set, so a list mutated inside a comprehension stays mutable.

### `src/main/java/net/starlark/java/eval/FlagGuardedValue.java`
- Bazel lines: 87
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel factory rejects a flag identifier whose first character is not `+` or `-` with `IllegalArgumentException` text `flag needs [+-] prefix: %s`.
  - No zel guard exposes a value only when `StarlarkSemantics.isFeatureEnabledBasedOnTogglingFlags(flag, "")` is true, and otherwise reports `{name} is experimental and thus unavailable with the current flags. It may be enabled by setting --{flag without its first character}`.
  - No zel guard exposes a value only when `isFeatureEnabledBasedOnTogglingFlags("", flag)` is true, and otherwise reports `{name} is deprecated and will be removed soon. It may be temporarily re-enabled by setting --{flag without its first character}=false`.

### `src/main/java/net/starlark/java/eval/FormatParser.java`
- Bazel lines: 294
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/eval/format.rs`
- Verdict: partial
- Differences:
  - Field scan is UTF-16 code units, and any character except `.`, `[`, `]`, and `,` is a legal name character (non-ASCII and lone surrogates included). `str_format` scans Unicode scalars and rejects anything that is not ASCII alphanumeric, `_`, `(`, `)`, or `-`, so names such as `{名前}`, `{0:s}`, `{+1}`, and `{ foo}` are kwargs in Bazel and hard errors in zel.
  - A field of only digits and `-` is `Integer.parseInt` (signed 32-bit). Out-of-range values (`2147483648`, `-2147483649`) fall through `NumberFormatException` and become named lookups. Zel parses `isize` (64-bit here), so those strings are positional indexes, and it also accepts underscores (`{1_0}` is index 10; Bazel looks up the name `1_0`).
  - A negative index is registered as manual numbering before the range check, so `{}` followed by `{-1}` raises `Cannot mix manual and automatic numbering of positional fields`. Zel rejects `idx < 0` before the mix check and raises `No replacement found for index -1`.
  - Bazel checks each field character as it scans, so `{.` is `Invalid character '.' inside replacement field` and `{.{` is the same illegal-`.` error. Zel only watches for a nested `{` or a missing `}` first, so `{.` is `unclosed '{' in format string` and `{.{` is `Nested replacement fields are not supported`. A legal unclosed `{` is `Found '{' without matching '}'` in Bazel and `unclosed '{' in format string` in zel.
  - Replacement values go through `Printer.str(value, semantics)`. Zel uses `Value`'s `Display` and ignores semantics: `+inf` versus `inf` (and Java `%.17g` digit cleanup versus `format_float`), main-repo labels follow `incompatible_unambiguous_label_stringification` (`//pkg:name` when false; Bazel’s default is on) while zel always prints the unambiguous `@@` form, and cyclic values render as `...` in Bazel with no cycle guard on the Rust path.

### `src/main/java/net/starlark/java/eval/GuardedValue.java`
- Bazel lines: 49
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no guard that wraps a predeclared object, decides accessibility from `StarlarkSemantics` plus a nullable client-data object, returns the underlying object only when that check passes, and supplies the access-denied error string.

### `src/main/java/net/starlark/java/eval/HasBinary.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Rust trait lets a value implement `binaryOp` and return null to decline. After built-in cases, Bazel calls the left operand with `thisLeft` true, then the right operand with the operands reversed only if the left is not `HasBinary` or returns null, and throws `EvalException` only for a real failure. Zel’s `eval_binop` and `Value::add` / `Value::bitor` are closed matches that error immediately with `unsupported binary operation: …` and never hand the operator to the other operand.

### `src/main/java/net/starlark/java/eval/ImmutableSingletonStarlarkList.java`
- Bazel lines: 78
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/list.rs`
- Verdict: partial
- Differences:
  - `get(int)` takes a signed 32-bit index and throws `IndexOutOfBoundsException` (`index (i) must not be negative` or `index (i) must be less than size (1)`) unless `i == 0`. Zel indexes the `Vec` with `i64` (`BigInt::to_i64` in `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`): a value that does not fit in `i64` yields `index out of range`, and any other miss yields `index out of range (index is {i}, but sequence has {len} elements)`.
  - `contains(null)` returns false and does not call `equals`. Zel membership is `elements.contains` on `Value` (`PartialEq`) and has no null.
  - `toArray(T[])` with `a.length < 1` returns a new `Object[]` unchecked-cast to `T[]` (runtime component type `Object`, not `a`'s class). A longer array stores the element at index 0 and null-fills the tail. Zel has no `toArray`; the element is `elements[0]`.
  - This type is permanently immutable (`ImmutableStarlarkList`: frozen mutability, and `updateIteratorCount` does not lock it). `StarlarkList::new` sets `frozen: false`, so a one-element list accepts mutation until `freeze()`, and a nonzero `iter_count` reports `list value is temporarily immutable due to active for-loop iteration`.

### `src/main/java/net/starlark/java/eval/ImmutableStarlarkList.java`
- Bazel lines: 130
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/list.rs`
- Verdict: partial
- Differences:
  - This type is permanently immutable (`isImmutable` true, `Mutability.IMMUTABLE`). `addElement`, `addElementAt`, `addElements`, `removeElementAt`, `setElementAt`, and `clearElements` only call `Starlark.checkMutable` and always fail with `trying to mutate a frozen list value`. Zel `StarlarkList::new` starts `frozen: false`, so `append`, `extend`, `insert`, `clear`, `pop`, `remove`, and index assignment succeed until `freeze()`.
  - `unsafeShallowFreeze` always throws `IllegalArgumentException` (`cannot call unsafeShallowFreeze() on an object whose Mutability is already frozen`) and does not freeze children. Zel `freeze()` deep-freezes elements on the first call and is a no-op if already frozen.
  - `updateIteratorCount` ignores its delta and always returns false, so an immutable list never takes the active-iterator error. Zel increments `iter_count` on every list, including frozen ones; once `frozen` is set, `check_mutable` still reports the frozen error first.
  - `get(int)` uses a signed 32-bit index and `Preconditions.checkElementIndex`: a negative index or `i >= length` throws `IndexOutOfBoundsException` (`index (i) must not be negative` / `index (i) must be less than size (n)`), with no end-relative wrap. Zel indexing uses `i64` (`BigInt` values that do not fit `i64` become `index out of range`), counts negatives from the end, and uses `index out of range (index is N, but sequence has M elements)`. `size()` is a Java `int`; Zel uses `usize`.
  - `contains(null)` is false and does not scan. Zel has no null; `in` uses `PartialEq`, so `None` matches a `None` element.
  - Because these mutators check mutability only after the parent has validated the index or search, a frozen list differs from Zel, which checks `frozen` first: `pop` on an empty or out-of-range frozen list is Java `index out of range (index is N, but sequence has M elements)` versus Zel `trying to mutate a frozen list value` (Zel’s `pop from empty list` / `pop index out of range` only if not frozen); `remove` of a missing element is Java `item <repr> not found in list` versus that frozen error (Zel’s not-found text is `Debug` `item {item:?} not found in list`, not Starlark repr).
  - `toArray()` copies, and `toArray(T[])` copies into the caller array, null-filling the unused tail when it is longer, or allocates a new array of the destination component type when it is shorter. Zel exposes the `Vec<Value>` directly, with no null padding.

### `src/main/java/net/starlark/java/eval/JNI.java`
- Bazel lines: 30
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `JNI.load()` calls `System.loadLibrary("cpu_profiler")` and swallows `UnsatisfiedLinkError` so a missing native library is reported only when a profiler native method runs. Zel’s Starlark evaluator (`/workspace/zel-cursor/crates/zel-starlark`) never loads `cpu_profiler`, and `--starlark_cpu_profile` is only a flag declaration in `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`.

### `src/main/java/net/starlark/java/eval/LazyImmutableStarlarkList.java`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel type defers a supplier until the first `size`, `get`, or `elems` call, invokes it at most once under double-checked locking, and then drops the supplier. Lists in `zel-cursor/crates/zel-starlark/src/types/list.rs` are eager `Vec`s.
  - `unsafeOptimizeMemoryLayout` is absent: a forced list is not rewritten to `StarlarkList.wrap(Mutability.IMMUTABLE, elems)` sharing the element array, and an unforced list is not returned unchanged so the supplier stays deferred.
  - Java `get(int)` throws Guava `IndexOutOfBoundsException` from `Preconditions.checkElementIndex` for a negative index or an index `>= length`. Zel has no matching lazy-list accessor.

### `src/main/java/net/starlark/java/eval/MethodDescriptor.java`
- Bazel lines: 434
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel type stores Starlark method metadata and invokes it: return mapping (`null` to `None`, error on `null`, Java `int` to `StarlarkInt`, `fromJava`), reflection exception unwrapping, `*args`/`**kwargs`/`self`/`thread`/`semantics` flags, struct-field calls, parameter index lookup, or the experimental/deprecated method flag errors. Builtin calls are direct Rust functions, not this descriptor.

### `src/main/java/net/starlark/java/eval/MethodLibrary.java`
- Bazel lines: 1025
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/eval/builtins/universal.rs`
- Verdict: partial
- Differences:
  - Keyword parameters are not bound by name. `int()`, `float()`, `str()`, `repr()`, and `bool()` with no positional argument return `0`, `0.0`, `""`, `""`, and `False`; Bazel requires the argument (`float()` and `bool()` are the exceptions and match). Extra positionals are ignored. `enumerate(list=...)`, `dict(pairs=...)`, `len(x=...)`, and `sorted`'s positional `key` do not follow Bazel binding.
  - `min`, `max`, and `sorted` ignore `key`. `sorted` treats an ordering error as equality and still returns a list. Int/float ordering truncates the float with `as i64` (saturating), so `max(3, 3.7)` stays `3`. Bazel compares exactly and errors on incomparable elements (`expected at least one item` versus `min() arg is an empty sequence`).
  - Strings are sequences of Unicode scalars in `list`, `tuple`, `sorted`, `reversed`, `min`, `max`, `all`, `any`, `enumerate`, `set`, and `zip`. Bazel strings are not iterable. `len` is `String.length` (UTF-16 code units) versus `chars().count()`. `hash` wraps in `i32` like `String.hashCode`, but over scalars, so a non-BMP string such as `"😀"` has length 1 and a different hash. String order follows scalar order, which diverges from UTF-16 code-unit order outside the BMP.
  - `range` and `enumerate` use signed 32-bit `toInt` in Bazel (length must fit, and `i + start` wraps as a Java `int`). Zel uses `i64`, maps a non-int or an integer outside `i64` to `0`, and accepts ranges whose length exceeds `2^31-1`.
  - `int` allows a base on a non-string, treats a non-int base as 10, trims whitespace, and accepts base 0 with a leading `0` (`"010"` → 10). Bazel rejects those (`can't convert non-string with explicit base`, `cannot infer base when string begins with a 0`). Float-to-int is a saturating `i64` cast, so `int(1e20)` is `i64::MAX` rather than truncation toward zero of the full magnitude.
  - `float` trims and then uses Rust `f64` parsing, so `infinity`, mixed-case NaN/Inf, whitespace, and overflow (`1e309`) become non-finite floats. Bazel rejects bare `infinity`, rejects that whitespace, and reports `floating-point number too large`. An int that does not fit in a finite float becomes `inf` instead of `int too large to convert to float`.
  - `dict` accepts only a dict, list, or tuple of pairs. A set, range, or other iterable errors with `cannot convert <type> to dict`. Bazel iterates any iterable; pair-length errors differ (`dictionary item must be a 2-element sequence` versus `in dict, item #N has length ...`).
  - `set()` is always defined. Bazel raises `Use of set() requires --experimental_enable_starlark_set` when that semantics flag is off.
  - `len` of a struct returns its field count. Bazel errors `struct is not iterable`.
  - `getattr`, `hasattr`, and `dir` use fixed method lists. `getattr` on a dict reads keys and does not return methods (`keys`, `get`, …). `hasattr` is false for host objects even when `dir` lists their fields. Struct `dir` is field names only. Bazel exposes annotated methods and includes a “did you mean” error.
  - `fail` and `print` are implemented in `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`. `fail` ignores `stack_trace`, always prefixes a stack and `Error in fail:`, and writes `attribute <attr>:` with a fixed space instead of inserting `sep`. Both format with `Display`. `print` special-cases labels and `Facts`, turns a non-string `sep` into `" "`, and does not append the `print_test_marker` suffix.

### `src/main/java/net/starlark/java/eval/Module.java`
- Bazel lines: 347
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Predeclared names stay in a separate map and are filtered by `GuardedValue` with the module's semantics and client data. A disabled guard is left in place and `resolve` throws that guard's access error. Zel stores one `BTreeMap` (`ModuleEnv.globals` / `EvalContext.globals`) and inserts predeclared names (`json`, `proto`, `rule`, `attr`, and the other `.bzl` builtins) into it, with no guard. A name the file also assigns is still the predeclared value until that assignment runs. Bazel treats it as a null global slot and reports `global variable 'NAME' is referenced before assignment.` Zel reports `name 'NAME' is not defined` only when the key is absent.
  - `getGlobals()` walks `LinkedHashMap` insertion order and drops null slots. Zel globals are sorted by name. A missing `load` symbol in Bazel is `file '%s' does not contain symbol '%s'` plus ` (did you mean '…'?)` over that insertion order (equal edit distance keeps the first). Zel reports `file '{label}' does not contain symbol '{name}'` with no suggestion, and `{label}` may be the canonical form. Its export map also drops `_` names and includes predeclared entries, so `load` can see names `getGlobal` would not return.
  - `resolve` builds a `HashSet` of global, predeclared, and universe names. An unknown name is `name '%s' is not defined`, and a spelling-suggestion tie follows that hash-set order. Zel's static check walks a `HashSet` drain (process-random), then predeclared keys, then a fixed universe list, so the same-distance suggestion can differ. Runtime `strict_names` errors are `{loc}: name '{name}' is not defined` with no suggestion. With `strict_names` false (the `EvalContext` default), an unknown name becomes a rule callable.
  - `resolveType` fails with `global symbol 'NAME' cannot be used as a type` (or `predeclared` / `universal`). Zel does not resolve module names as types.
  - The first non-null module docstring is stored after `Starlark.trimDocString` and later executions do not replace it. `ModuleEnv` does not store a docstring.
  - `toString()` is `<module ?>` when client data is null and `<module {clientData}>` otherwise. Zel's debug text is `ModuleEnv(..)`. Function `repr` uses only `label_string: Option<String>` (`<function name from LABEL>` or `<function name>`), not an arbitrary client object.
  - `ofInnermostEnclosingStarlarkFunction(thread, depth)` returns null when there are not `depth + 1` Starlark frames and throws `IllegalArgumentException` if `depth` is negative. Zel has no depth walk from the module to an enclosing Starlark function.

### `src/main/java/net/starlark/java/eval/Mutability.java`
- Bazel lines: 281
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/value.rs`
- Verdict: partial
- Differences:
  - Java keeps one shared `Mutability` per evaluation; `freeze()` and `close()` drop its iterator map in constant time so every value that points at it becomes immutable, including values not reachable from a root. Zel has no such object: `Value::freeze` sets a per-value `frozen` flag and recursively walks only lists, dicts, sets, tuples, and structs (`/workspace/zel-cursor/crates/zel-starlark/src/types/list.rs`, `dict.rs`, `set.rs`). Provider fields and other values from the same evaluation stay mutable, and loading a `.bzl` exports `ctx.globals` without freezing (`/workspace/zel-cursor/crates/zel-packages/src/loader.rs`). Zel also freezes function defaults at `def` time (`/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`), so a default list is immutable for the rest of that file; Java leaves it mutable until the thread’s `Mutability` is closed.
  - The default `updateIteratorCount` treats any positive delta as `+1` and any negative delta as `-1`, treats `0` as a query, returns whether the identity-keyed count is positive, and after `freeze()` is a no-op that returns false. Underflow throws `IllegalStateException` with text `zero value in this.iteratorCount`. The count is a signed 32-bit `int` (wraps). Zel stores `iter_count: usize` on list, dict, and set and only changes it by one around `for` statements; list and dict comprehensions never lock the iterable, so mutation during a comprehension succeeds. Underflow panics in debug or wraps to `usize::MAX` in release, which then stays temporarily immutable, and `for` still increments the count on an already frozen value.
  - There is no annotation, no `getAnnotation` (space-joined `toString` of the annotation objects), and no `toString` of the form `[annotation]` when unfrozen and `(annotation)` when frozen. The frozen `IMMUTABLE` singleton (`annotation` `IMMUTABLE`, `toString` `(IMMUTABLE)`) is absent.
  - `createAllowingShallowFreeze`, `allowsUnsafeShallowFreeze`, `unsafeShallowFreeze`, and `checkUnsafeShallowFreezePrecondition` are absent. The Java default `unsafeShallowFreeze` throws `UnsupportedOperationException` with no message. The precondition throws `IllegalArgumentException` `cannot call unsafeShallowFreeze() on an object whose Mutability is already frozen`, or `cannot call unsafeShallowFreeze() on a mutable object whose Mutability's allowsUnsafeShallowFreeze() == false`.

### `src/main/java/net/starlark/java/eval/MutableStarlarkList.java`
- Bazel lines: 228
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/list.rs`
- Verdict: partial
- Differences:
  - Growth uses a signed 32-bit sum and rejects `size + extra` when it overflows `int` or exceeds `1 << 30`, with `excessive capacity requested (%d + %d elements)`. Zel `append`, `extend`, and `insert` grow a `Vec` with no `2^30` cap and no that error.
  - Indexes passed to `get`, `addElementAt`, `setElementAt`, and `removeElementAt` are Java `int`s (`StarlarkInt.toInt`: `got <n> for <what>, want value in signed 32-bit range`). Zel `insert`, `pop`, `index`, reads, and item assignment take `i64`, so a value that fits in `i64` but not `int` is applied instead of rejected. An int that does not fit in `i64` is reported as `list index must be integer, not int` on assignment and as `index out of range` on read.
  - Immutability is a shared `Mutability`. A null mutability is stored as `Mutability.IMMUTABLE`, and freezing that object freezes every list that points at it without walking elements. `unsafeShallowFreeze` only retargets this list and throws `IllegalArgumentException` if it is already frozen (`cannot call unsafeShallowFreeze() on an object whose Mutability is already frozen`) or `allowsUnsafeShallowFreeze()` is false. Zel `new` always starts `frozen: false`; `freeze` is silent if already frozen, sets the flag, and recursively freezes elements. Ordinary evaluation never freezes lists, so a later `append` that Bazel rejects with `trying to mutate a frozen list value` succeeds.
  - `pop` defaults to `-1` and fails with `index out of range (index is %d, but sequence has %d elements)`, including `pop()` on an empty list (`index is -1`). Zel `pop(None)` drops the last element, an empty list is `pop from empty list`, and any other bad index is `pop index out of range`. A non-int `pop` argument is `got <type> for i, want int` in Bazel; Zel treats it as a missing index and pops the last element.
  - `index` and `remove` miss with `item %s not found in list` using Starlark `repr`. Zel uses `item {item:?} not found in list` (`Debug`, so a string prints as `String("a")`). A non-int `start` or `end` is `got <type> for start/end, want int` (or the signed 32-bit `toInt` error). Zel treats a non-`i64` as omitted, so `start` defaults to 0 and `end` to the length and the search still runs.
  - Item assignment uses `getSequenceIndex` (`index out of range (index is %d, but sequence has %d elements)` and `got <type> for list index, want int`). Zel reports `list index out of range` with no index or length, or `list index must be integer, not <type>`.
  - `extend` requires a Starlark iterable, so a string fails with `in call to extend(), parameter 'items' got value of type 'string', want 'iterable'`. Zel `value_to_sequence` splits the string with `chars()` (Unicode scalars) and appends those one-character strings. `for` locks the list and a `finally` block always drops the iterator count, including on error and in comprehensions. Zel bumps `iter_count` only for `for`, not comprehensions, so a comprehension body can `append` to the list being iterated, and an error inside `for` returns before the decrement, leaving the list `temporarily immutable due to active for-loop iteration`.

### `src/main/java/net/starlark/java/eval/ParamDescriptor.java`
- Bazel lines: 281
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no eval-time builtin parameter descriptor: no default-expression evaluation (`None`/`True`/`False`/`unbound`/`0`/`1`/`[]`/`()`/`" "` shortcuts, cached Starlark eval, bootstrap and `failed to evaluate default value` errors), no allowed-type phrase (`Object` means any type, `NativeComputedDefault` filtered, `a` / `a or b` / `a, b, or c`), and no flag gating (`isEnabled` plus experimental `--flag` vs deprecated `--flag=false` messages with the `[+-]` prefix stripped). Parameter checks are inlined at individual call sites.

### `src/main/java/net/starlark/java/eval/Printer.java`
- Bazel lines: 423
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/value.rs`
- Verdict: partial
- Differences:
  - Quoted strings walk UTF-16 code units in Java and emit `\xHH` for units below 32 other than `\n`, `\r`, and `\t`. Rust `repr` uses `str` Debug, which walks Unicode scalars, emits `\u{…}` instead of `\xHH`, and also escapes non-printable scalars above U+001F (DEL, C1 controls, grapheme extenders) that Java copies through. Lone surrogates can appear in a Java string and cannot appear in a Rust `str`.
  - A value already on the printer stack is rendered as `...`. Rust `repr` and `Display` recurse with no cycle marker.
  - Java `repr`/`str`/`%s` of a null reference is the text `null`. Rust has no null; Starlark `None` is `None` on both sides.
  - `%` formatting lives in `/workspace/zel-cursor/crates/zel-starlark/src/eval/format.rs`. For `%o`, `%x`, and `%X`, Java prints a signed 32-bit value as 32-bit two’s complement and a signed 64-bit value as 64-bit two’s complement (`%x` of -1 is `ffffffff`); only integers outside signed 64-bit get a minus sign plus magnitude. Rust always prints a minus sign plus magnitude (`-1`).
  - Integer conversions of a float use Java’s full-magnitude truncating conversion and reject non-finite values with `got +inf, want a finite number` (`-inf` and `nan` the same way). Rust casts with `as i64` (saturates past `i64`; NaN becomes 0) and does not error.
  - `%e`, `%E`, `%f`, `%F`, `%g`, and `%G` call `StarlarkFloat.format` (`%f`/`%F` six fraction digits, `%e`/`%E` scientific with six fraction digits and an uppercase `E`, `%g`/`%G` from `%.17g` with trailing zeros removed, non-finite `+inf`/`-inf`/`nan`). Rust prints `format_float` for every one of those (`inf` with no leading `+`, shortest digits, always a lowercase `e`).
  - Rust accepts width, `0` padding, precision, `%i`, and `%(key)s` maps. Java treats the next code unit as the conversion, so `%10d`, `%.3g`, `%i`, and `%(key)s` fail. A dict on the right of `%` is one positional value in Java (`"%s" % d` stringifies the dict) and a key map in Rust (`"%s" % d` reports too few arguments unless the pattern uses `%(key)`).
  - Java’s “not enough arguments” text appends `: ` and the repr of the argument tuple; Rust omits it. Java’s unsupported-conversion text includes the UTF-16 index and the repr of the pattern (`unsupported format character "." at index 1 in "%.3g"`); Rust is `unsupported format character "."` with no index or pattern. Incomplete-pattern messages both quote the pattern, using the string escaping above.

### `src/main/java/net/starlark/java/eval/RangeList.java`
- Bazel lines: 214
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/range.rs`
- Verdict: partial
- Differences:
  - Bazel stores start, stop, step, and length as signed 32-bit ints and rejects a length that does not fit with `len(range(...)) exceeds signed 32-bit range`. Zel uses i64 bounds and a usize length, so it accepts values outside int32 and lengths above 2^31-1; `stop - start + step - 1` can overflow i64. Membership uses wrapping 32-bit `(i - start) % step` in Bazel, so an element of a wide range can be reported absent; Zel uses i64 arithmetic.
  - Bazel equality is sequence equality: every empty range is equal, and length-1 ranges are equal when the first element matches, ignoring step and stop. Zel compares start, stop, step, and len, so `range(0, 0) == range(1, 1)` and `range(5, 6) == range(5, 100, 10)` are true in Bazel and false in Zel.
  - Slicing a range returns another range in Bazel. If the slice step times the range step does not fit in a signed 32-bit int, Bazel clamps that step to `Integer.MAX_VALUE` or `Integer.MIN_VALUE` and pulls stop to a one-element range. Zel copies the whole range with `to_vec` and returns a list.
  - A bad range index in Bazel is `index out of range (index is %d, but sequence has %d elements)`, and a non-int index is `got %s for sequence index, want int`. Zel reports `index out of range` and `type 'range' is not indexable with '<type>'`.
  - Bazel does not treat range as immutable, so using one as a dict or set key fails with `unhashable type: 'range'`. Zel treats range as hashable.

### `src/main/java/net/starlark/java/eval/RegularImmutableStarlarkList.java`
- Bazel lines: 42
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/list.rs`
- Verdict: partial
- Differences:
  - Instances are permanently frozen (`Mutability.IMMUTABLE`). `append`, `extend`, `clear`, item assignment, and an in-range `insert`, `pop`, or `remove` raise `trying to mutate a frozen list value` and leave the elements unchanged. `StarlarkList::new` sets `frozen: false`, so those operations mutate the list until `freeze()`.
  - On a missing `remove`, this type searches first and raises `item <Starlark repr> not found in list`. `pop` converts the index with signed 32-bit `StarlarkInt.toInt` (`got <value> for i, want value in signed 32-bit range`) and then raises `index out of range (index is %d, but sequence has %d elements)` before the frozen error. `insert` does the same 32-bit conversion (`for index`) and clamps with `toIndex` only to fail the frozen check afterward. Zel `check_mutable` runs first, so a frozen list always raises `trying to mutate a frozen list value`. On a mutable list, indexes are `i64`: `insert` clamps into `[0, len]`, `pop` raises `pop from empty list` or `pop index out of range`, and `remove`/`index` raise `item <Debug> not found in list`.
  - `EMPTY` is one shared frozen list (`StarlarkList.empty()` always returns that object). Zel builds a new mutable empty list each time (`frozen: false`), so each one is a distinct value and can be appended to.
  - The constructor throws `IllegalArgumentException` with no message unless the array’s runtime class is exactly `Object[]`, then stores that array and returns it from `elems()`, so a later write to the array changes the list. Null elements are allowed. Zel takes an owned `Vec<Value>` (no null, no array-class check) and does not alias the caller’s buffer.
  - `unsafeShallowFreeze` always throws `IllegalArgumentException` (`cannot call unsafeShallowFreeze() on an object whose Mutability is already frozen`) and does not freeze elements. Zel `freeze()` is a no-op when `frozen` is already set, and otherwise recursively freezes every element.

### `src/main/java/net/starlark/java/eval/RegularTuple.java`
- Bazel lines: 159
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/value.rs`
- Verdict: partial
- Differences:
  - `repeat` takes a signed 32-bit count via `toInt("repeat")` (`got <n> for repeat, want value in signed 32-bit range`) and rejects a product above `StarlarkList.MAX_ALLOC` (`1 << 30`) with `excessive repeat (<len> * <n> elements)`. Zel (`/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`) uses `to_i64().unwrap_or(0)`, so a count outside i64 becomes 0 and the result is `()`, and a larger in-range product is allocated with no `excessive repeat` error.
  - `checkHashable` walks the elements, so a tuple holding a list or dict fails as `unhashable type: 'list'` or `unhashable type: 'dict'`. Zel `Value::is_hashable` fails the tuple itself, and dict, set, and depset report `unhashable type: 'tuple'` or `depset elements must be hashable, got unhashable 'tuple'`.
  - `isImmutable` is true when every element is immutable, so a tuple of frozen lists is immutable and a strict depset accepts it (the mutable-element error is `depset elements must not be mutable values`). Zel treats any list element as unhashable, so that tuple is rejected after the lists are frozen.

### `src/main/java/net/starlark/java/eval/Sequence.java`
- Bazel lines: 157
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`
- Verdict: partial
- Differences:
  - Indexing requires a signed 32-bit int (`got {value} for sequence index, want value in signed 32-bit range`, or `got {type} for sequence index, want int`). Zel accepts any `i64` index; a non-int yields `type '{seq}' is not indexable with '{type}'`, and an int that does not fit in `i64` yields `index out of range` with no element count.
  - Out-of-range `range` indexing uses the same `index out of range (index is {n}, but sequence has {len} elements)` text as other sequences. Zel reports only `index out of range` for `range`.
  - Ordered comparison treats `equals` elements as equal, then compares int and float exactly. Zel orders list and tuple elements with float-to-`i64` truncation, so `[1] < [1.5]` is true in Bazel and false in Zel, and an int equal to a float outside `i64` can be `==` but not ordered equal.
  - String elements order as UTF-16 code units. Zel orders them as UTF-8 bytes, so non-BMP characters sort differently inside a sequence.
  - Slice start, stop, and step must be signed 32-bit ints (`got … for start index, want int` or `want value in signed 32-bit range`). Zel treats a non-int or non-`i64` start or stop as omitted and a non-int step as 1, and it accepts `i64` steps.
  - Slicing a `range` returns a `range`. Zel materializes a list.
  - `cast` reports `for {what}, got {type}, want sequence` and `at index {i} of {what}, got element of type {type}, want {elem}`; `noneableCast` maps `None` to an empty list. Zel has no shared cast: `join` says `join() requires sequence argument` and `expected string for sequence element, got '{repr}' of type {type}` and accepts a set, and `None` is an empty sequence at some call sites and an error at others.

### `src/main/java/net/starlark/java/eval/SingletonTuple.java`
- Bazel lines: 120
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/value.rs`
- Verdict: partial
- Differences:
  - Repeat (`*`) requires a signed 32-bit count (`got <n> for repeat, want value in signed 32-bit range`) and rejects `n > 2^30` with `excessive repeat (1 * <n> elements)`. Zel uses `to_i64().unwrap_or(0).max(0) as usize` with no cap, so a count that does not fit in i64 becomes 0 (empty tuple) and a count above `2^30` that fits in i64 allocates.
  - Indexing requires a signed 32-bit int (`sequence index`); a non-int is `got <type> for sequence index, want int`. Zel accepts any i64, reports `index out of range` when the int does not fit in i64, and `type 'tuple' is not indexable with '<type>'` for a non-int. In-range int32 errors use the same `index out of range (index is <i>, but sequence has 1 elements)` text.
  - Slice start, stop, and step must fit in a signed 32-bit int or Starlark errors (`start index`, `stop index`, `slice step`). Zel clamps i64 bounds and treats a missing i64 (omitted or too large) as the default start `0`, end `len`, or step `1`, so an oversized bound can return a slice instead of failing. A non-empty slice of a one-element tuple is still that element, and an empty slice is `()`.
  - `checkHashable` fails with the element’s error (`unhashable type: 'list'`). Zel rejects the whole tuple, so dict or set insertion reports `unhashable type: 'tuple'`. A one-element tuple of NaN is hashable in Bazel (float is immutable) and unhashable in Zel.
  - `isImmutable` is true when the single element is immutable, including a frozen list, so a strict depset accepts that tuple. Zel depsets require `is_hashable`, which is false for any tuple that contains a list, and the error is `depset elements must be hashable, got unhashable 'tuple'` rather than `depset elements must not be mutable values`.
  - Ordering is lexicographic on the one element, then length. Java string order is UTF-16 code units and Zel `str` order is Unicode scalar values, so one-element tuples of non-BMP strings can compare in opposite directions. Mixed int/float ordering uses Java’s exact int-vs-double compare, while Zel truncates the float to i64. Incomparable elements both raise `unsupported comparison: <type> <=> <type>`.

### `src/main/java/net/starlark/java/eval/Starlark.java`
- Bazel lines: 1239
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`
- Verdict: partial
- Differences:
  - String `len`, indexing, slicing, and ordering use Java UTF-16 code units (`String.length`, `charAt`, `String.compareTo`). Zel uses Unicode scalars (`chars()`) and UTF-8/code-point order, so a non-BMP character has length 2 and can be sliced into a lone surrogate in Bazel, length 1 in Zel, and sorts below many BMP characters in Bazel but above U+FFFF in Zel. `dir` sorts names with that same order.
  - Slice and index bounds go through `toInt`, which errors outside signed 32-bit (`got <n> for <what>, want value in signed 32-bit range`) and on non-ints (`got <type> for <what>, want int`). Zel uses `i64`; a non-int or an integer that does not fit in `i64` is treated as omitted, so the step becomes 1 and start/stop take their defaults. A slice whose length does not fit in a signed 32-bit int fails in Bazel (`len(...) exceeds signed 32-bit range`). A non-sliceable value is `invalid slice operand: <type>` in Bazel and `type '<type>' is not sliceable` in Zel.
  - Slicing a `range` stays a `range` (`RangeList.getSlice`; a step product that does not fit in 32 bits is clamped to a one-element range). Zel materializes a list, so `range(3)[::-1]` is `range(2, -1, -1)` in Bazel and `[2, 1, 0]` in Zel.
  - Int/float ordering is exact (`compareIntAndDouble`: NaN is greater than every int, and every finite int is less than `+inf`). Zel truncates the float with `as i64` (toward zero, saturating at `i64::MIN`/`MAX`), so `1 < 1.5` is false and an integer above `i64::MAX` compares greater than `+inf`. Zel also orders host `dotted_version` values; Bazel rejects them with `unsupported comparison: <type> <=> <type>`.
  - `len` of a value with no length is `<type> is not iterable` (`Starlark.len` returns -1). Zel says `type '<type>' has no len()`, and `len` of a struct is the field count.
  - `toIterable`/`toArray` (so `list`, `for`, and `f(*seq)`) accept only `StarlarkIterable`. Strings are not iterable (`type 'string' is not iterable`, `argument after * must be an iterable, not string`). Zel's `value_to_sequence` splits a string into one-character strings.
  - `getattr` resolves annotated methods before fields and on failure reports `'<type>' value has no field or method '<name>'` plus a `did you mean` suggestion. Zel's `getattr` does not return string, list, dict, or set methods (`type '<type>' has no attribute '<name>'`; a dict looks up a key and says `attribute "<name>" not found`). A missing struct field lists `Available attributes:` instead of a did-you-mean suffix.
  - `**kwargs` are applied in dict insertion order (`LinkedHashMap`). Zel merges them into a `BTreeMap` and rebuilds the `**kwargs` dict in sorted key order, so the bound dict and unexpected-keyword lists differ when the keys are not already sorted.
  - `repr`/`str` escape controls as `\xHH`, print float infinity as `+inf`, and render a cyclic structure as `...`. Zel uses Rust `Debug` escapes (`\u{...}`), prints `inf`, and has no cycle guard.
  - `checkHashable` accepts every float, including NaN, because floats are immutable, and depset elements use `isImmutable`, which allows a tuple of lists. Zel rejects NaN as unhashable and rejects a tuple of lists in a depset because it requires `is_hashable`.
  - `execFile` returns a trailing expression, or `None`, and stores a PEP-257 docstring (`trimDocString`, tabs expanded at width 8, then dedented). Zel's `eval_str` discards expression statements and does not trim a module docstring. `startCpuProfile`/`stopCpuProfile` have no counterpart. Zel also prefixes these eval errors with `file:line:col:`; Bazel's `errorf` text is unprefixed.

### `src/main/java/net/starlark/java/eval/StarlarkCallable.java`
- Bazel lines: 210
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`
- Verdict: partial
- Differences:
  - The default `call` throws `function <getName()> not implemented`. Zel reports `{loc}: unknown builtin '<name>'` for an unknown builtin, and `'{type}' object is not callable` for every other non-callable value.
  - The default argument processor rejects a repeated keyword with `<toString()> got multiple values for parameter '<name>'` (`rule(...)` for a rule, `macro(...)` for a macro, and the Java `Class@identity` string when `toString` is not overridden) after pushing a frame at `getLocation()`. With the default `<builtin>` location, the printed form is `Error in <getName()>: …`. `call_callable` reports that duplicate only for a user function, as `<name>() got multiple values for parameter '<name>'` at the `def` location. A rule, provider, builtin, or host call keeps the last `**kwargs` value and still runs.
  - Named arguments are stored in a `LinkedHashMap` (explicit keywords in source order, then the `**kwargs` dict’s iteration order) and passed to `call` as a `Dict` under the thread’s mutability, so a frozen thread rejects later mutation. Zel passes a `BTreeMap`, so the callee walks keywords in sorted key order and there is no freeze check on that map.
  - `getLocation()` defaults to `Location.BUILTIN` (`<builtin>`, line 0). A leaf frame at that location is dropped from the traceback and replaced by the prefix `Error in <getName()>:`. Zel’s default location is an empty file at `0:0`, and builtin, rule, provider, and host calls are left off the Starlark call stack, with errors prefixed by the call-site `file:line:col:`.

### `src/main/java/net/starlark/java/eval/StarlarkFloat.java`
- Bazel lines: 296
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/value.rs`
- Verdict: partial
- Differences:
  - `str`/`repr` (`format(v, 'g')` vs `format_float`): Java prints `+inf`, and `%.17g` keeps fixed form until the exponent is ≥ 17, so `1e16` is `10000000000000000.0` and the smallest subnormal is `4.9e-324`. Zel prints `inf`, switches to scientific at exponent ≥ 16 (`1e+16`), and prints that subnormal as `5e-324`.
  - `%e`/`%E`/`%f`/`%F`/`%G` go through `StarlarkFloat.format` (US printf: `%f` is six fractional digits, `%e` of `0.1` is `1.000000e-01`, `%G` uses `E`). Zel’s `%` formatter always emits `format_float` for those conversions, so they match `str` (`0.1`, `1e+20`).
  - Real division rejects a zero divisor, including ±0 and an int 0, with `floating-point division by zero`. Zel float `/` and mixed int/float `/` return an IEEE infinity or NaN and do not error; int/int zero uses `division by zero`.
  - Remainder errors with `floating-point modulo by zero` and adjusts a truncating remainder so the sign follows the divisor (`-4.0 % 2.0` is `-0.0`). Zel errors only for float/float, with `integer modulo by zero`, uses `a - floor(a/b)*b` (`+0.0` for that case), and rejects float/int and int/float as `unsupported binary operation`.
  - `int(float)` and `%d`/`%x`/`%o` use `finiteDoubleToIntExact` (exact truncating integer for every finite value; non-finite error `can't convert float nan to int` or `can't convert float +inf to int`). Zel casts with `as i64`, so `1e20` becomes `9223372036854775807`, and non-finite values raise `cannot convert float NaN or Inf to int`.
  - Float/float order matches (NaNs equal and greater than +inf; −0 equals +0). Int/float order is exact via `getMantissa`/`getShift`; Zel compares after `as i64`, so `1.5` equals `1` and magnitudes outside i64 compare as `i64::MIN` or `i64::MAX`.
  - NaN is a hashable dict/set key (all NaNs equal). Zel `is_hashable` is false for NaN, so insert fails with `unhashable type: 'float'`.

### `src/main/java/net/starlark/java/eval/StarlarkFunction.java`
- Bazel lines: 592
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`
- Verdict: partial
- Differences:
  - Recursion is keyed by the compiled function (`Resolver.Function`), so every closure from re-executing the same `def` or `lambda` is the same function. Zel keys by the `UserFunction` allocation, so a new closure from that source can call another without `function '<name>' called recursively`.
  - Recursion is checked only after argument binding, so a recursive call with bad arguments reports the binding error. Zel checks first and reports `function '<name>' called recursively`.
  - A single unexpected keyword is suffixed with `SpellChecker.didYouMean` (` (did you mean '<param>'?)`) against the non-residual parameter names. Zel emits `got unexpected keyword argument: <name>` with no suggestion.
  - A keyword for an already-bound parameter is rejected while the keyword is added, before the positional-count check (`got multiple values for parameter '<name>'`). Zel reports `accepts no more than N positional argument(s) but got M` first when both apply.
  - `**kwargs` keeps `LinkedHashMap` insertion order (explicit keywords, then `**dict` order), and unexpected names contributed only by `**dict` are listed in that order. Zel inserts from a `BTreeMap`, so the dict and those extra names are sorted by key.
  - `def` defaults stay mutable until the defining module is frozen. Zel freezes them when the `def` runs, so mutating that list, dict, or set (or another alias of it) in the same file fails with `trying to mutate a frozen ...`.
  - Lambda defaults are evaluated once, when the lambda is created. Zel keeps the expression and evaluates it on every call, so later assignments, side effects, and errors happen at the call.
  - With `experimental_starlark_type_checking`, call arguments and the return value are checked (`in call to <f>(), parameter '<p>' got value of type '<got>', want '<want>'` and `<f>(): returns value of type '<got>', declares '<want>'`). Zel does not perform those checks.
  - Binding and recursion errors are located at the `def` name, or at the `lambda` keyword. Zel adds 4 to the `def`/`lambda` token column, so a lambda is 4 columns past the keyword and a `def` not followed by exactly one space is offset from the name.

### `src/main/java/net/starlark/java/eval/StarlarkInt.java`
- Bazel lines: 800
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/bigint.rs`
- Verdict: partial
- Differences:
  - `parse` rejects surrounding whitespace and, when `base` is 0, a leading `0` with no `0b`/`0o`/`0x` prefix (`cannot infer base when string begins with a 0: …`). Zel `from_str_radix` / `from_str_auto` trim whitespace and accept `010` as decimal. Failures also differ: Java reports `invalid base %d (want 2 <= base <= 36)` and `invalid base-%d literal: %s` with `repr` of the original text; Zel reports `invalid radix: …`, `invalid digit '…' for radix …`, or `invalid integer literal`.
  - `ofFiniteDouble` truncates any finite float toward zero to the exact integer (including magnitudes above 2^63). Zel `int(float)` uses `f as i64`, which saturates at `i64::MIN`/`i64::MAX`. Non-finite values are `can't convert float %s to int` in Bazel and `cannot convert float NaN or Inf to int` in Zel.
  - `toFiniteDouble` fails with `int too large to convert to float` when the nearest double is non-finite. Zel `to_f64` returns ±inf and, for integers wider than 64 bits, can differ by an ulp from `BigInteger.doubleValue()` because limbs are rounded iteratively.
  - `compareIntAndDouble` is an exact three-way compare (all ints are less than `+inf` and greater than `-inf`; `1 < 1.5`). Zel orders an int against `BigInt::from_i64(f as i64)`, so fractional floats compare equal to their truncation and values outside the i64 range, including infinities, compare against `i64::MIN`/`i64::MAX`.
  - `intEqualsFloat` stays exact for every finite float. Zel’s equality path left-shifts the mantissa and treats a shift ≥ 512 as not equal, so an int that is exactly a float with magnitude ≥ 2^564 compares unequal.
  - Shift counts must fit in a signed 32-bit int (`got %s for shift count, want value in signed 32-bit range`) before the negative-count and left-shift `>= 512` checks. Zel accepts any `i64`; a count above `i32::MAX` is `shift count too large: N` for `<<` and is performed for `>>`. A count that does not fit in `i64` is `shift count too large` with no operand value.
  - `toInt` rejects anything outside the signed 32-bit range (`got %s for %s, want value in signed 32-bit range`), which `range` and sequence indexing use. Zel uses `i64` for those, and `range` turns a non-`i64` bound into 0 via `unwrap_or`.
  - `truncateToInt` keeps the low 32 bits (int-attribute concatenation can wrap negative). Zel keeps the full integer and has no 32-bit narrowing conversion.

### `src/main/java/net/starlark/java/eval/StarlarkIterable.java`
- Bazel lines: 24
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/eval/builtins/universal.rs`
- Verdict: partial
- Differences:
  - Zel `value_to_sequence` treats a string as an iterable of Unicode scalar strings (`chars()`), so `for`, comprehensions, `list`/`tuple`/`sorted`/`all`/`any`, and `f(*s)` walk it. Bazel `String` does not implement `StarlarkIterable`; those constructs fail (`type 'string' is not iterable'`, or `argument after * must be an iterable, not string`). Character walks go through `elems()`, which yields UTF-16 code units.
  - A non-iterable in `value_to_sequence` is `got value of type '<type>', want 'iterable'`. Bazel `Starlark.toIterable` (used by `for` and comprehensions) is `type '<type>' is not iterable`. Zel `for` rewrites the generic error to `<loc>: type '<type>' is not iterable`, and a depset is `iteration over depset is forbidden` (comprehensions and builtins still use the `want 'iterable'` text). `f(*x)` uses `argument after * must be an iterable, not <type>` only for int, bool, None, and functions; other rejects keep the `value_to_sequence` text. Bazel `*` always uses `argument after * must be an iterable, not <type>`.
  - Zel allows only list, tuple, set, range, dict keys, and string. Bazel is an open interface: `OutputGroupInfo` iterates group names, and other native `StarlarkIterable` values do the same. Zel `ProviderInstance`, `Struct`, and `Host` fail the iterable check.
  - Zel copies the whole sequence before walking it. Range materialization uses wrapping `i64` (`cur += step` in `to_vec`; `for` uses `start + i as i64 * step`) and a `usize` length. Bazel’s range iterator is lazy and yields signed 32-bit values; a length that does not fit in a signed 32-bit int is rejected when the range is built. `all`/`any` therefore allocate the full range in Zel before any short-circuit.
  - Zel `for` increments `iter_count` on list, dict, and set, but a `?` error in the body returns before the decrement, so the value stays temporarily immutable. Comprehensions never adjust `iter_count` and walk a snapshot, so mutation during a comprehension succeeds. Bazel `for` and comprehensions bracket `updateIteratorCount` in `finally`, and mutation fails with `temporarily immutable due to active for-loop iteration`.

### `src/main/java/net/starlark/java/eval/StarlarkList.java`
- Bazel lines: 530
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/list.rs`
- Verdict: partial
- Differences:
  - `repeat` (`*`) requires a signed 32-bit count (`got %s for repeat, want value in signed 32-bit range`) and rejects a product above `1<<30` with `excessive repeat (%d * %d elements)` (length, then count). A non-positive count yields an empty list. Zel uses `to_i64().unwrap_or(0).max(0)` and has no element cap: an int outside i64 becomes 0 repeats with no error, an int in `2^31 .. 2^63-1` repeats, and a huge product allocates.
  - `concat` (`+`) and growth from `append` / `extend` / `insert` reject a signed 32-bit size overflow or a size above `1<<30` with `excessive capacity requested (%d + %d elements)`. Zel concatenates and grows with no cap.
  - `index` defaults an unbound `start`/`end` to `0` and `len` after `toIndex` of a signed 32-bit int (a non-int fails with `got %s for start/end, want int`). A miss is `item %s not found in list` using Starlark `repr`. Zel treats a missing, `None`, or non-i64 bound as the default, accepts i64 indexes outside int32, and reports `item {item:?}` (`item String("a") not found in list`).
  - `remove` reports the same miss as `item %s not found in list` with Starlark `repr`. Zel uses `Debug` (`item String("a") not found in list`).
  - `insert` converts the index with `toInt("index")` and clamps with `toIndex`. Zel clamps an i64, so values outside int32 succeed, and a missing or non-int index fails with `insert() takes index argument`.
  - `pop` defaults to `-1`, then `toInt("i")` and `getSequenceIndex`. An empty list or any other out-of-range index is `index out of range (index is %d, but sequence has %d elements)`. Zel treats an omitted or non-i64 argument as “last element”; an empty list is `pop from empty list`, and any other bad index is `pop index out of range`.
  - The annotated parameters (`item`, `index`, `x`, `start`, `end`, `i`, `items`) accept keywords. Zel list methods read positional arguments only.
  - `extend` requires a Starlark iterable, so a string is rejected. Zel `value_to_sequence` also accepts a string and splits it into Unicode scalar strings.
  - `compareTo` treats equal elements as equal before ordering, so `[None] < [None, 1]` is a length comparison. Zel list ordering always calls `compare_values`, which errors with `unsupported comparison: NoneType <=> NoneType` on equal unorderable elements, and it orders int/float elements by truncating the float to i64, so `[1] < [1.5]` is false.
  - `repr` / `str` print a cyclic list as `[...]`. Zel list formatting recurses with no cycle elision.
  - `getSlice` builds a `RangeList` and errors with `len(range(...)) exceeds signed 32-bit range` when the slice length does not fit in a signed 32-bit int. Slice bounds and step must also be signed 32-bit ints (`got %s for start index/stop index/slice step, want int`). Zel `slice_sequence` has no length cap, accepts i64 bounds and steps, and treats a non-int bound as omitted.

### `src/main/java/net/starlark/java/eval/StarlarkSemantics.java`
- Bazel lines: 277
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/semantics.rs`
- Verdict: partial
- Differences:
  - `experimental_starlark_type_checking` is a boolean key (`-` prefix, default false). Zel stores a `String` defaulting to `"off"`.
  - `getBool` and `setBool` require a leading `+` or `-` (that prefix is the default: `+` true, `-` false) and throw `IllegalArgumentException` with no message otherwise; an empty name throws `StringIndexOutOfBoundsException`. `Key` rejects a `+`/`-` prefix the same way and rejects a null default with `NullPointerException`. Zel has fixed fields and none of these errors.
  - `get` and `getBool` treat a null map entry as the default. `getGeneric` looks up the bare name, then `+name`, then `-name`, and only then the caller-supplied default. Zel has no null-versus-absent map and no prefix fallback.
  - Only non-default entries are kept, in a `TreeMap`. `toString` prints `StarlarkSemantics{k=v, ...}` in that order with the `+`/`-` prefix stripped, so `DEFAULT` (and `build()` on an empty map, which returns that singleton) is `StarlarkSemantics{}`. Setting a value back to its default removes the entry and restores equality with `DEFAULT`. Zel compares the whole struct and does not render a sparse, prefix-sorted map.
  - `isFeatureEnabledBasedOnTogglingFlags` returns true when both names are empty, `getBool(enabling)` when only the enabling flag is set, and the negation of `getBool(disabling)` when only the disabling flag is set. Both non-empty throws `IllegalArgumentException` with message `at least one of 'enablingFlag' or 'disablingFlag' must be empty`. Zel does not implement this.
  - `internal_bazel_only_utf_8_byte_strings` defaults to false, so strings are UTF-16 code units unless the flag is set, in which case they are raw UTF-8 bytes. Zel’s field defaults to false and is never read, so strings stay Unicode scalars either way.
  - `allow_recursion` defaults to false in both and Zel honors it. `experimental_enable_starlark_set` (default true), `print_test_marker` (default false), and `force_starlark_stack_trace` (default false) match, but those fields are never read, so disabling `set()`, the print test marker, and forced `fail` stack traces does not take effect.

### `src/main/java/net/starlark/java/eval/StarlarkSet.java`
- Bazel lines: 897
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/set.rs`
- Verdict: partial
- Differences:
  - `pop` removes the last inserted element (`Vec::pop`) instead of the first in insertion order, and an empty set raises `pop(): set is empty` instead of `set is empty`.
  - `remove` does not require a hashable element. A missing element is reported with `Debug` formatting (`item String("x") not found in set`) instead of Starlark `repr` (`element "x" not found in set`), so an unhashable value fails with that not-found text instead of `unhashable type: '...'`.
  - `discard` does not require a hashable element, so discarding an unhashable value succeeds instead of raising `unhashable type: '...'`.
  - `intersection_update`, `difference_update`, and `symmetric_difference_update` are not callable (`'set' value has no field or method '...'`), even though `dir()` lists them. Bazel updates the receiver in place.
  - Augmented `|=`, `&=`, `-=`, and `^=` build a new set and rebind the name. Bazel mutates the left-hand set, so a frozen set or a set with an active iterator errors (`trying to mutate a frozen set value` or `set value is temporarily immutable due to active for-loop iteration`) and other references observe the update.
  - `union`, `intersection`, and `difference` with no arguments clone `frozen` and `iter_count`. A copy of a frozen set stays frozen, and a copy taken during iteration stays temporarily immutable. Bazel returns a new set with the thread mutability and a zero iterator count.
  - Collection arguments use `value_to_sequence`, so a string is accepted and split into Unicode scalar characters. Bazel accepts only a collection (list, tuple, set, range) or a dict's keys and rejects a string and other non-collections with `for <method> argument got value of type '<type>', want a collection of hashable elements`; zel reports `got value of type '<type>', want 'iterable'`.
  - One-argument methods (`add`, `remove`, `discard`, `issubset`, `issuperset`, `isdisjoint`, `symmetric_difference`) ignore extra positional arguments and do not accept the parameter name as a keyword (`element` or `other`). Bazel rejects extra positionals and accepts those keywords.

### `src/main/java/net/starlark/java/eval/StarlarkThread.java`
- Bazel lines: 671
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/eval/thread.rs`
- Verdict: partial
- Differences:
  - Recursion is keyed by the shared resolved function (`rfn`, same `def` or `lambda`), skipping the frame just pushed. Zel evaluation uses `push_frame_for`, which keys off the `UserFunction` allocation pointer, so distinct closures from one lambda (the `eta` and Y-combinator cases) are not recursive. `is_recursive_call` and `push_frame` instead compare function names, so every same-named function would count.
  - The recursion failure text in Bazel is `function '<name>' called recursively`, then the call stack is formatted with an `Error:` prefix. `push_frame` and `push_frame_for` return that same sentence, and the caller prefixes the definition location with its column shifted by 4, with no traceback.
  - Bazel counts execution steps from 0 with a default limit of `Long.MAX_VALUE` and fails with `Starlark computation cancelled: too many steps` once `++steps` reaches the limit. `incrementExecutedSteps` adds a wrapping `long`. The Zel thread has no step counter, and `max_computation_steps` defaults to 0 and is never read, so that cancellation does not happen.
  - `format_stack_trace_at` always prints `column N` and starts with `<location>: Traceback (most recent call last):`. Bazel’s call-stack formatting omits a zero column and a zero line, and the traceback itself has no location prefix. An empty Zel stack still emits a `<toplevel>` frame. Bazel’s stack is empty in that case.
  - Zel invents `<toplevel>` from the first frame’s push location when the file is non-empty, and reports each frame at the next frame’s location. Bazel records a real `<toplevel>` frame whose program counter moves during execution, and `setErrorLocation` keeps the first error site.
  - Traceback source lines in Zel use Unicode `trim`, split on `\r` as well as `\n`, and `read_to_string` drops the line on invalid UTF-8. Bazel’s reader splits only on `\n`, trims only UTF-16 code units `<= U+0020`, and still returns a line under the default UTF-8 decoder, which replaces bad bytes.
  - The default print handler writes `<caller location>: <msg>` to stderr, using `<builtin>` when fewer than two frames are active. Zel’s default print is `DEBUG: <file>:<line>:<column>: <msg>`.
  - With no loader, Bazel rejects `load` with `load statements may not be executed in this thread`, and a null module yields `module '<name>' not found`. With no load hook, Zel inserts placeholder rule callables and continues.
  - `checkInterrupt` defaults to on and throws `InterruptedException` after clearing the Java interrupt flag. Zel never polls an interrupt flag. `pop` on an empty stack throws; `pop_frame` returns `None`. Construction rejects a frozen mutability with `IllegalArgumentException`. `equals` and `hashCode` throw `UnsupportedOperationException`.

### `src/main/java/net/starlark/java/eval/StarlarkValue.java`
- Bazel lines: 123
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/value.rs`
- Verdict: partial
- Differences:
  - `checkHashable` throws `unhashable type: '<Starlark.type>'` unless `isImmutable()` is true. Zel `is_hashable` accepts `range` (Bazel `RangeList` keeps the default `isImmutable` of false) and every builtin function (Bazel `BuiltinFunction` does too), and rejects `depset` (Bazel `Depset.isImmutable` is true, so the default accepts it), NaN floats (Bazel floats are immutable and hashable), and unexported providers (Bazel `StarlarkProvider.isImmutable` is false until export).
  - Every `Host` value is hashable. Bazel leaves `repository_ctx`, `module_ctx`, `ModuleExtension`, and `TagClass` on the default (unhashable). `Facts` and `Args` override `checkHashable` to always throw `unhashable type: '...'`.
  - Struct and provider-instance hashability walks `is_hashable_deep`: nested lists and dicts count even while mutable, nested depsets count, and a nested set never does. The default `checkHashable` only tests `isImmutable`, so a frozen list, dict, or set makes the struct hashable and an unfrozen one does not. The list, dict, or set value itself stays unhashable.
  - Default `str` calls one-argument `repr` (`StarlarkSemantics.DEFAULT`), so `str` and `repr` are the same text and the semantics argument is ignored. Default `debugPrint` calls `str`. Zel `Display` and `repr` differ for provider instances: `Display` prints the full `provider_name`, `repr` keeps only the segment after the last `%`. `Value` has no separate `debugPrint`.
  - Default `repr` is `<unknown object ` plus `getClass().getName()` plus `>`. Zel prints that only for five hard-coded host kinds and opaque structs. Any other value that still uses the Java default is `<kind id>` or a host hook.
  - `getStarlarkType` defaults to `Types.ANY`. Zel values have no Starlark type.

### `src/main/java/net/starlark/java/eval/StringModule.java`
- Bazel lines: 1106
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/eval/builtins/string_methods.rs`
- Verdict: partial
- Differences:
  - Indexing, `slice`, `elems`, `find`/`rfind`/`index`/`rindex`/`count` offsets, and empty-pattern `replace` walk Java UTF-16 code units. Zel walks Unicode scalar values, so a non-BMP character counts as two elements in Bazel and one in Zel.
  - `lower` and `upper` use Guava ASCII case mapping. `capitalize` applies `Character.toUpperCase` to the first UTF-16 unit only and ASCII-lowers the rest. Zel uses Unicode `to_lowercase`/`to_uppercase`, which changes non-ASCII letters and can change length (`ß` to `SS`, `İ` to `i` plus a combining dot).
  - `title` treats a word as a run of `Character.isLetter` and maps case one UTF-16 unit at a time. Zel treats a letter after any alphanumeric as mid-word, so `"a1b".title()` is `"A1B"` in Bazel and `"A1b"` in Zel, and Unicode case conversion can expand.
  - `isalpha` is `[A-Za-z]` versus `char::is_alphabetic`. `isdigit` is `Character.isDigit` (Unicode decimal digits) versus ASCII `[0-9]`. `isalnum` is ASCII letters or Java digits versus Unicode alphanumeric, including non-decimal numbers. `islower`, `isupper`, and `istitle` treat only ASCII `A-Za-z` as cased; Zel treats every Unicode letter as cased, so `"é".islower()` is false in Bazel and true in Zel, and `"aÉ".islower()` is true in Bazel and false in Zel.
  - `isspace` follows `Character.isWhitespace` (true for U+001C–U+001F, false for U+00A0, U+0085, U+2007, U+202F, U+2028, U+2029). Zel uses Unicode `White_Space`, which is the reverse on those code points.
  - Default `strip`, `lstrip`, and `rstrip` remove Bazel's Latin-1 whitespace set (U+0009–U+000D, U+001C–U+001F, space, U+0085, U+00A0), or only the ASCII subset when `internal_bazel_only_utf_8_byte_strings` is set. Zel `trim` uses Unicode `White_Space`: it strips extra spaces such as U+1680, U+2000–U+200A, U+2028, U+2029, U+202F, U+205F, and U+3000, and it leaves U+001C–U+001F in place. There is no UTF-8 byte-string mode.
  - `split` and `rsplit` require a non-empty string separator. Zel whitespace-splits when `sep` is omitted or not a string, including `None`. On that path `split` ignores `maxsplit`, while `rsplit` applies it and rejoins the left side with single spaces.
  - `rsplit` uses `lastIndexOf(sep, end - 1)`, which does not keep the match inside the remaining span, so `"aaa".rsplit("aa")` throws `StringIndexOutOfBoundsException`. Zel returns `["a", ""]`.
  - `index` and `rindex` report `substring not found` in Bazel and `substring not found: {sub:?}` in Zel. A non-string in a `startswith`/`endswith` tuple is `at index %d of sub, got element of type %s, want string` in Bazel; Zel skips it and can return false.
  - `start`, `end`, `count`, and `maxsplit` must fit in a signed 32-bit `int` (`got …, want value in signed 32-bit range`). Zel accepts any `i64` and clamps indexes. A non-int, or an integer that does not fit in `i64`, is treated as the default (start `0`, end at the end, count/maxsplit unlimited). `find` and `count` results are a Java `int` versus a `BigInt`.
  - `join` accepts any Starlark iterable, so a dict contributes its keys. Zel accepts only a list, tuple, or set. A non-string element is `expected string for sequence element %d, got '%s' of type %s` in Bazel and `expected string for sequence element, got '{}' of type {}` in Zel, with no index.
  - `elems()` is a frozen list in Bazel and a mutable list in Zel.
  - `format` reports an unclosed `{` as `Found '{' without matching '}'` in Bazel and `unclosed '{' in format string` in Zel. Bazel rejects only `.`, `[`, `]`, and `,` inside a field and looks up any other text as a name. Zel also rejects characters outside `[A-Za-z0-9_()\-]`. A numeric field outside the signed 32-bit range, such as `{2147483648}`, fails `Integer.parseInt` and is looked up as a name. Zel parses it as a positional index.
  - Omitted `removeprefix`/`removesuffix` arguments, and omitted `replace` `old`/`new`, are required-argument errors in Bazel. Zel treats them as `""`, so a missing prefix or suffix returns the original string. `replace` also honors a keyword named `maxsplit`; Bazel’s limit is the positional parameter `count` (`named = false`), so that keyword is an unexpected-keyword error. `splitlines` requires a bool `keepends`; Zel uses truthiness, so `1` keeps line breaks.

### `src/main/java/net/starlark/java/eval/Structure.java`
- Bazel lines: 82
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/struct_value.rs`
- Verdict: partial
- Differences:
  - Default `setField` always fails with `"{Starlark.type(this)} value does not support field assignment"` (a struct is `struct value does not support field assignment`), with the dot location stored on the exception rather than in that sentence. Zel rejects every attribute assignment, including structs, with `"{location}: cannot assign to attribute"`.
  - `getValue` and the default `getValue(semantics, name)` return null when the field is absent (semantics are ignored) and throw only for other retrieval failures; a null `getErrorMessageForUnknownField` selects the interpreter default `'{type}' value has no field or method '{name}'` plus a single did-you-mean suffix. Zel `StructValue::get` always errors: `unknown attribute {name}` when `is_attr`, otherwise `'struct' value has no field or method '{name}'\nAvailable attributes: {comma-separated BTreeMap keys}`, with no null custom-message hook, no semantics filter, and no did-you-mean suffix.

### `src/main/java/net/starlark/java/eval/SymbolGenerator.java`
- Bazel lines: 180
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no owner-scoped symbol generator. Java `create` stores an owner and `generate` (synchronized) returns a `LocalSymbol` of `(owner, index)` starting at 0, with `index++` wrapping as a signed 32-bit `int`. A null owner throws `NullPointerException` with text `Null owner`. Locals compare equal only to other locals with `owner.equals` and the same index (same-instance short-circuit), and `hashCode` caches `1000003 ^ owner.hashCode()` then `* 1000003 ^ index` in 32-bit wrap, recomputing whenever that value is 0. `exportAs(name)` replaces the token with a `GlobalSymbol` equal by owner and name (`isGlobal` goes from false to true). `createTransient` uses a fresh owner object so its symbols never match another generator, and `CONSTANT_SYMBOL` is one such index-0 local for the process. Local `toString` is `LocalSymbol{owner=<owner>, index=<index>}`.

### `src/main/java/net/starlark/java/eval/Tuple.java`
- Bazel lines: 173
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/value.rs`
- Verdict: partial
- Differences:
  - `compareTo` ties elements when `equals` is true and only then orders them, so equal but unordered values (dicts, sets, functions) do not stop a comparison and a longer tuple sorts greater by length. Zel always orders and returns `unsupported comparison: <type> <=> <type>` for those elements even when they are equal.
  - String elements are ordered by Java UTF-16 code units. Zel orders them by UTF-8 bytes (Unicode scalar order), so a tuple can sort the other way when the first difference is a supplementary character (U+10000 versus U+E000).
  - `repeat` requires a signed 32-bit count (`got <n> for repeat, want value in signed 32-bit range`) and rejects a product above `1<<30` with `excessive repeat (<len> * <n> elements)`. Zel uses `to_i64().unwrap_or(0).max(0) as usize` with no cap: a count outside i64 becomes the empty tuple, and a count that fits in i64 but not in i32 allocates that many copies.
  - `toString` uses `Starlark.repr`, which prints a cycle as `...`. Zel tuple `repr` and `Display` recurse with no cycle guard.

### `src/main/java/net/starlark/java/eval/TypeChecker.java`
- Bazel lines: 237
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no static Starlark type lattice: no `isSubtypeOf` (ANY unification, OBJECT as top, untagged unions, same-length covariant tuples, covariant sequence/collection, invariant mapping keys with covariant values, invariant list/dict/set, supertype walk), no `isValueSubtypeOf`/`type` (StarlarkValue type or ANY, bool, str, otherwise `IllegalArgumentException("Expected a valid Starlark value.")`), and no `fromAnnotation`/`fromJava` mapping of `None`/`void`, `String`, `boolean`, `int`/`long`/`Integer`/`Long`/`StarlarkInt`/`BigInteger`, `double`/`Double`/`StarlarkFloat`, `StarlarkList`, `StarlarkIterable`, `Sequence`, and `Object`/`StarlarkValue` (other Java types become ANY). Runtime `Value::type_name` is not this checker.

### `src/main/java/net/starlark/java/eval/cpu_profiler_posix.cc`
- Bazel lines: 203
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No POSIX SIGPROF CPU-profiler implementation in the Zel crates: no `supported` always-true, `gettid` (`SYS_gettid` / `pthread_threadid_np`), nonblocking event pipe, SIGPROF handler writing a big-endian int32 thread id (EAGAIN/EWOULDBLOCK discard to stderr, other `write` errors abort), `startTimer` (`sigaction` with `SA_RESTART`, refuse and restore when a handler is already installed, `setitimer(ITIMER_PROF)`), or `stopTimer` (disarm the timer, `signal(SIGPROF, SIG_IGN)`). `--starlark_cpu_profile` is only a flag string in `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`; `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` is the JSON trace profiler, and `/workspace/zel-cursor/crates/zel-starlark` has no sampling hooks.

### `src/main/java/net/starlark/java/eval/cpu_profiler_unimpl.cc`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel Starlark CPU-profiler entry point returns unsupported (`CpuProfiler.supported` is JNI `false` on this stub).
  - No Zel counterpart aborts the process from `gettid`, `createPipe`, `startTimer` (period ignored), or `stopTimer`. `--starlark_cpu_profile` is only an unused flag-table string and an unused `StarlarkCpuProfileFileInitializationFailure` exit-code variant.

### `src/main/java/net/starlark/java/lib/json/Json.java`
- Bazel lines: 860
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/eval/modules/json.rs`
- Verdict: partial
- Differences:
  - Encode nesting fails in Rust once `depth > 500` (`nesting depth limit exceeded`). Java raises that text only from `StackOverflowError` (the suite nests on the order of 100000). Decode has the same Java mapping, then wraps it with an offset; Rust decode has no depth cap and can panic.
  - Finite floats: Java `StarlarkFloat.toString` is `%.17g` plus trailing-zero cleanup (scientific when the exponent is `< -4` or `>= 17`), so `0.1` encodes as `0.10000000000000001` and `1e16` as `10000000000000000.0`. Rust `format_float` is shortest round-trip and switches at exponent 16 (`0.1`, `1e+16`). Non-finite values still fail as `cannot encode non-finite float nan|inf|-inf`.
  - Encode walks Java UTF-16 code points and replaces an unpaired surrogate with U+FFFD. With UTF-8 byte strings, `"😹"[:1]` is the raw byte `0xF0` and is emitted as that byte. Rust iterates Unicode scalars, so those strings cannot exist and are not rewritten.
  - Dict keys are sorted in both, but Java `String` order is UTF-16 code units and Rust `&str` order is Unicode scalar order, so supplementary-plane keys can reorder.
  - Java encodes `Encodable.objectForEncoding` (a `File`/`Artifact` becomes `{"path","root","short_path"}`) and any `Structure` or `NativeInfo` from sorted `dir()` plus `getattr`. A `NativeInfo` callable is skipped only after its key and colon are written. Rust encodes only `Value::Struct` fields and returns `cannot encode <type> as JSON` for `ProviderInstance`, `File`, and other host objects.
  - Sets encode as insertion-ordered arrays in both. Java wraps a bad element as `at set index <i>: ...`; Rust returns the element error with no index. `range` arrays match.
  - Decode of `\uXXXX`: Java concatenates a run of UTF-16 units, then encodes with UTF-16, or with UTF-8 when `internal_bazel_only_utf_8_byte_strings` is set, and appends those bytes as Latin-1, so a non-ASCII escape is one character per UTF-8 byte (U+FFFD becomes the three bytes `EF BF BD`). Rust always yields scalars, combines only one high+low pair, maps any other surrogate to U+FFFD, and ignores that flag.
  - Java rejects an unescaped string character `<= 0x1F` (`invalid character '\xHH' in string literal`). Rust keeps it.
  - Java skips only space, tab, LF, and CR. Rust `char::is_whitespace` also skips VT, FF, NBSP, and other Unicode spaces, on both decode and indent.
  - Java rejects `-`, a trailing dot (`5.`), `.e` (`5.e1`), and a leading zero (`00`, `0123`, `-01`) as `invalid number: ...`. Rust accepts `5.` and `5.e1` as floats and leading-zero digit strings as integers (`00` → 0, `0123` → 123). Other bad numbers fail as `invalid float|integer in JSON '...'` plus the Rust parse error.
  - Every Java decode error is `at offset <i>, <msg>` with texts such as `unexpected character "f" after value`, `unexpected end of file`, `got "}", want ',' or ']'`, `after object key, got "1", want ':' `, `got int for object key, want string`, and `in object, got ..., want ',' or '}'`. Rust has no offset and uses `unexpected trailing data in JSON`, `unexpected end of JSON`, `expected ',' or ']' in JSON array`, `expected string key in JSON object`, and similar. Dict value errors use Rust `{:?}` (`\0`) rather than Starlark `repr` (`\x00`).
  - Java `default` may be positional or keyword, including `None`, and any decode error returns it. Rust honors only keyword `default`; a second positional is ignored, so invalid input still fails. Extra positionals and unknown keywords are rejected in Java and ignored in Rust.
  - `indent` in Java scans one value and stops at depth 0 (`indent("1 2")` is `1`, `indent(" 1 ")` is `1`), consumes four characters after `\u`, and fails with `unexpected end of file`, `unexpected character ...`, or `input is not valid JSON`. Rust scans the whole string, drops every Unicode space (`indent("1 2")` is `12`), treats `\u` as a one-character escape, saturates depth, and always succeeds (empty input is `""`; an unclosed `[` still emits a newline and indent). `prefix`/`indent` default to `""` and `"\t"` in both; a non-string keyword uses that default in Rust and type-errors in Java. `encode_indent` is encode-then-indent in both, so those encode and indent differences apply.

### `src/main/java/net/starlark/java/spelling/SpellChecker.java`
- Bazel lines: 110
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/spellcheck.rs`
- Verdict: partial
- Differences:
  - Edit distance, the length short-circuit, and the suggest cutoff `min(5, (length + 1) / 2)` use Java UTF-16 code units (`length` / `charAt`), while Rust counts Unicode scalars (`chars()`). A non-BMP character is two units in Java and one in Rust, so the distance and whether a candidate is accepted diverge: two emoji have cutoff 2 in Java and 1 in Rust, and one substituted emoji (Java distance 1) is suggested in Java and rejected in Rust; characters that differ in both surrogates are distance 2 in Java and 1 in Rust. Java also compares unpaired surrogates, which `&str` cannot hold.
  - `suggest` lowercases with `String.toLowerCase()`, which follows the JVM default locale. Rust `to_lowercase()` is locale-independent Unicode mapping. Under a Turkish default, Java maps `I` to `ı` and `İ` to `i`, so `suggest("I", ["ı"])` returns `ı` and `suggest("İ", ["i"])` returns `i`; Rust maps `I` to `i` and `İ` to `i` plus U+0307, so the first returns `i` and the second returns no suggestion. With `Locale.US`, final sigma and `İ` match Rust.
  - `editDistance` takes a signed 32-bit cutoff. A negative cutoff returns 0 when the strings are equal and -1 otherwise. Rust takes `usize`, so that cutoff cannot be passed. The Rust distance is returned with `as i32`, which wraps if the value exceeds `i32::MAX`; `suggest` only passes a cutoff of at most 5, so that cast does not affect suggestions.

### `src/main/java/net/starlark/java/syntax/Argument.java`
- Bazel lines: 124
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java keeps a start and end for every argument: the end is the value’s end offset, and the start is the value (positional), the identifier (`name=expr`), or the `*` / `**` token. Those offsets are signed 32-bit UTF-16 indexes. `Expr::Call` has one call location at `(`, keyword names are bare strings with no span, and a bad `*` / `**` is reported at the value’s column minus one (`star_at`), so `**` and whitespace before the value do not land on the star token. Eval columns count Unicode scalars.
  - A legal call is one ordered list, evaluated as positionals, then keywords, then `*`, then `**`. Rust stores `pos_args`, `kw_args`, and a single `var_args` / `kw_var_args`, and evaluates `*` before keyword values, so in `f(a=g(), *h())` the `*` expression runs before `g()`.
  - Each `*` and `**` is its own node, and `getName()` is null except on a keyword. Rust keeps one `Option` per star form, so a later `*` or `**` replaces the earlier expression, and only `kw_args` carry a name.

### `src/main/java/net/starlark/java/syntax/AssignmentStatement.java`
- Bazel lines: 121
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java keeps a nullable type annotation and the constructor throws `IllegalStateException` (`Can't have type annotation on complex LHS`, `Can't have augmented assignment with type annotation`) unless the LHS is an identifier and the operator is null. `Stmt::Assign` has no type; with type syntax enabled, `parser.rs` drops `name: Type` and still assigns, so the annotation is gone and those errors are not produced.
  - `getOperatorLocation` uses an `int` offset into Bazel’s Latin-1 `char[]` (one column per byte, a UTF-16 code unit). Rust `location` and `op_location` are taken after `String::from_utf8_lossy`, counting one column per Unicode scalar, so the `=` column used for assignment errors moves after non-ASCII input and invalid bytes collapse to U+FFFD.
  - A non-lvalue LHS is still an `AssignmentStatement`; the resolver then reports `cannot assign to '<lhs>'` at that expression. Rust refuses the target while building `AssignTarget` with `{statement start}: cannot assign to non-lvalue expression`. Augmented assignment to a list or tuple is `cannot perform augmented assignment on a list or tuple expression` at the operator in Java; the eval node reports `{statement start}: augmented assignment to tuple not allowed` (lists and tuples are both `AssignTarget::Tuple`). The diagnostic parser matches Bazel’s text and column only when type syntax is off.

### `src/main/java/net/starlark/java/syntax/BinaryOperatorExpression.java`
- Bazel lines: 96
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`, `in`, `not in`) are `Expr::Compare` with a list of clauses, evaluated as a short-circuit chain (the middle operand of `a < b < c` runs once). This node is always one operator and two operands, and Bazel rejects that chain with `Operator '<' is not associative with operator '<'. Use parens.`
  - `getOperatorLocation` maps a signed 32-bit UTF-16 code-unit offset through `FileLocations` (column = offset minus line start). `Expr::BinaryOp`'s `Location.column` is a `usize` advanced once per Unicode scalar, so a non-BMP character and a Latin-1 source byte that is not ASCII land in a different column.
  - `getStartOffset` and `getEndOffset` span from the left operand's start through the right operand's end. `Expr::location` for `BinaryOp` is only the operator token.

### `src/main/java/net/starlark/java/syntax/CallExpression.java`
- Bazel lines: 100
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java stores one source-ordered argument list and `getNumPositionalArguments` is a signed 32-bit count of `Argument.Positional` only (`n++` wraps past `Integer.MAX_VALUE`). `Expr::Call` splits arguments into `pos_args`, `kw_args`, and a single `var_args` / `kw_var_args` `Option`, so mixed order is dropped and a later `*` or `**` replaces the earlier one; the positional count is `pos_args.len()` (`usize`).
  - Java’s span starts at the callee’s UTF-16 code-unit offset and ends at `rparenOffset + 1` (signed 32-bit addition), and `getLparenLocation` is a separate materialized `(` location. `Expr::Call` stores only the `(` line and column (`usize`); `location()` reports that parenthesis, and the eval lexer advances the column once per Unicode scalar.
  - `toString` writes the callee, `(`, arguments joined by `", "`, and `)`, and `ListExpression.appendNodes` abbreviates once the buffer reaches 32 characters or the fifth element when more remain (`+N more`). `Expr` has no equivalent printer.
  - A null callee throws `NullPointerException` (`Preconditions.checkNotNull`), and a null `(` location is stored and returned. The Rust callee is a non-null `Box<Expr>`, and a missing `*` or `**` is `Option::None`.

### `src/main/java/net/starlark/java/syntax/CastExpression.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no `cast` syntax node. With type syntax enabled, Bazel parses `cast(type, value[,])` as this expression (span from `cast` through the character after `)`) and evaluates it by returning only `value`, with `toString` `cast(<type>, <value>)`. Zel's lexer and parsers treat `cast` as an ordinary identifier, so that text is a normal call (or an undefined-name error) and the type operand is not discarded at evaluation.

### `src/main/java/net/starlark/java/syntax/Comment.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No comment syntax node retains the text (leading `#`, no trailing newline), the start offset, or the end offset (`offset + text.length()`, UTF-16 code units). Both Zel lexers (`/workspace/zel-cursor/crates/zel-starlark/src/syntax/lexer.rs` and `scan_to_newline` in `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`) discard from `#` through the character before `\n`.
  - `hasDocCommentPrefix` (`startsWith("#:")`) and `getDocCommentText` (strip `"#: "` when present, otherwise `"#:"`, otherwise null) are absent. `bazel_syntax.rs` records `#:` doc comments as not ported.

### `src/main/java/net/starlark/java/syntax/Comprehension.java`
- Bazel lines: 165
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java stores signed 32-bit UTF-16 code-unit offsets: the comprehension span is `[lbracketOffset, rbracketOffset + 1)` (`+1` wraps), a `For` clause spans `[forOffset, iterable.end)`, an `If` clause spans `[ifOffset, condition.end)`, and a dict body is a `DictExpression.Entry` that keeps the `:` offset used as the key-error site. `Expr::ListComp` and `Expr::DictComp` keep only a start `Location` (line and column as `usize`, one column per Unicode scalar), and `CompClause` has no `for`/`if` token, end, or colon position.
  - `For.vars` is an expression, so a one-element list or tuple target such as `[a]` or `(a,)` is unpacked. `CompClause::For` stores `var_names: Vec<Pattern>`, and a lone `Pattern::Tuple` is flattened, so those targets become a single name and bind the element whole.

### `src/main/java/net/starlark/java/syntax/ConditionalExpression.java`
- Bazel lines: 57
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - `getStartOffset` and `getEndOffset` are signed 32-bit UTF-16 code-unit indexes spanning the then-expression start through the else-expression end. `Expr::IfExpr` stores only the `if` token’s `Location` (`parser.rs` takes `loc()` on that token): `line` and `column` are `usize`, the evaluator lexer counts one column per Unicode scalar, and there is no end offset. The node position is the `if` keyword, and a non-BMP character is one column rather than a surrogate pair.

### `src/main/java/net/starlark/java/syntax/DefStatement.java`
- Bazel lines: 107
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java records a UTF-16 char start offset at `def` and an end offset at the last body statement (the identifier’s end if the body is empty). `UserFunctionDef` stores only the `def` token’s line and a column counted in Unicode scalars, with no end offset and no empty-body fallback. A non-BMP scalar on that line is two Java columns and one Rust column; Bazel’s Latin-1 file buffer counts each byte as one column.
  - Java keeps type parameters and a nullable return type, and `toString` is `printDefSignature` plus `" ...\n"` (`def f[T](a: int) -> T: ...`). Rust stores neither, so those annotations are dropped; the eval parser accepts `-> Type` and discards it, and `def f[T]` is not recorded as type parameters.
  - Java attaches a nullable `Resolver.Function` located at the identifier. Rust does not. Duplicate-parameter errors are reported at the `def` token, and call-binding errors use that column plus 4, which misses the name when `def` and the name are not separated by a single space.

### `src/main/java/net/starlark/java/syntax/DictExpression.java`
- Bazel lines: 93
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java stores the dict span as `int` offsets into the Latin-1 `char[]` Bazel parses (`ParserInput.fromLatin1`: one column per source byte). `getStartOffset` is the `{` offset and `getEndOffset` is `rbraceOffset + 1` (signed 32-bit wrap). `Expr::Dict` stores only the `{` line and column as `usize` Unicode-scalar coordinates and has no exclusive end one past `}`.
  - `Entry.getColonLocation` maps that byte offset through `FileLocations` (1-based column from the line start in the Latin-1 buffer). Rust colon `Location`s come from the evaluator lexer, which advances the column once per Unicode scalar, so a non-ASCII byte before `{` or `:` produces a different column. Invalid UTF-8 still yields a Java node; the Rust node is only built from a valid UTF-8 `&str`.
  - `Entry` is its own node from the key start offset through the value end offset, and that node is the dict-comprehension body. Rust stores pairs in a `Vec` and uses `Expr::DictComp` with the `{` location only, so there is no entry span.

### `src/main/java/net/starlark/java/syntax/DocComments.java`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no doc-comment block type: it does not reject an empty list or a line without a `#:` prefix, and it does not keep the lines, start and end locations, newline-joined trimmed doc text, or a newline-joined raw-line string. `#` lines, including `#:`, are discarded by the lexer, and `bazel_syntax` leaves doc comments unported.

### `src/main/java/net/starlark/java/syntax/DotExpression.java`
- Bazel lines: 57
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java `getStartOffset` is the object's start and `getEndOffset` is the field identifier's exclusive end (`nameOffset + name.length()`, UTF-16 `int` offsets). `Expr::Attr` keeps a single `Location` taken from the `.` token, and `Expr::location()` returns that point, so the expression position is the dot and there is no end position.
  - `getDotLocation` maps `dotOffset` through `FileLocations`, whose column is a count of UTF-16 code units. The Rust location's column counts Unicode scalars (`col += 1` per `char`), so a non-BMP character earlier on the line changes the reported column.

### `src/main/java/net/starlark/java/syntax/Ellipsis.java`
- Bazel lines: 46
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No ellipsis expression node: Java stores a start offset, uses end offset `startOffset + 3`, kind `ELLIPSIS`, `toString`/`prettyPrint` `"..."`, and `NodeVisitor.visit`. Zel `ast::Expr` has no ellipsis variant; `...` is only a token that `parse_primary` always rejects with `ellipsis ('...') is not allowed outside type expressions` and does not record a 3-unit span, including in type expressions where Java builds the node without that error.

### `src/main/java/net/starlark/java/syntax/Expression.java`
- Bazel lines: 96
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - `Expression.parse` and `parse(input)` parse one expression from a complete input under `FileOptions` (`FileOptions.DEFAULT` when omitted: type syntax off, invalid type expressions rejected), skip leading doc-comment blocks and trailing newlines and doc comments, require EOF, and throw `SyntaxError.Exception` with every accumulated error. A stack overflow becomes `internal error: stack overflow while parsing Starlark expression <<source>>` plus the Java stack trace. Zel has no equivalent entry point: `Parser::parse_expr` reads one expression from an existing token stream, returns the first `StarlarkError`, and leaves any leftover tokens alone.
  - `parseTypeExpression` parses type syntax even when `allowTypeSyntax` is false: identifiers, `Ctor[args]` (`TYPE_APPLICATION`), `|` unions, list and dict type arguments, and `...` (`ELLIPSIS`). `tolerateInvalidTypeExpressions` switches that to an arbitrary expression except an unparenthesized tuple. Zel `Expr` has no type-application or ellipsis variant, so `list[int]` is an index and `...` is three `Dot` tokens. In `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`, type annotations always fail with `type annotations are disallowed`, and a value-position ellipsis fails with `ellipsis ('...') is not allowed outside type expressions` without producing an ellipsis node.
  - With type syntax enabled, `cast(type, value)` and `isinstance(value, type)` are `CAST` and `ISINSTANCE` nodes, and they are rejected when type syntax is off. Zel always parses both names as ordinary identifier calls.
  - Java expression kinds keep `INT_LITERAL`, `FLOAT_LITERAL`, and `STRING_LITERAL` as distinct nodes with source offsets; `True`, `False`, and `None` stay identifiers. Zel folds all five into `Expr::Literal`, and `Expr::location` for every literal is `Location::default()` (`0:0`, empty file).
  - `Expression.parse` rejects comparison chains (`a < b < c`) with `Operator '<' is not associative with operator '<'. Use parens.` and folds adjacent `"a" + "b"` into one string literal. Zel accepts the chain as one `Compare` and leaves the `+` as a binary operation of two literals.

### `src/main/java/net/starlark/java/syntax/ExpressionStatement.java`
- Bazel lines: 45
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java `getStartOffset` and `getEndOffset` are the child expression’s signed 32-bit UTF-16 code-unit offsets (start of the whole expression, exclusive end). `Stmt::Expr` only exposes `location()`, one `Location` (file, line, `usize` column) and no end position. For binary and comparison expressions that location is the operator; for calls it is `(`; for attribute, index, and slice it is `.` or `[`, so the statement position is not the leftmost subexpression.
  - A literal expression statement (`Expr::Literal`) reports `Location::default()` (empty file, line 0, column 0). Java returns the literal node’s real start and end offsets.
  - When a location is recorded, the column advances once per Unicode scalar. Java’s offsets and columns count UTF-16 code units, so a supplementary character advances the column by two.

### `src/main/java/net/starlark/java/syntax/FileLocations.java`
- Bazel lines: 122
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`
- Verdict: partial
- Differences:
  - In-range offsets use the same line table (a dummy slot, line 1 at offset 0, a new line only after each `\n`, 1-based column `offset - line_start + 1`, and offset `size` is a valid EOF position). Java indexes UTF-16 code units (`char[]`, `buffer.length`). Zel’s table is `src.as_bytes()` cast with `b as char`, so a decoded non-ASCII character is wider than `String.toCharArray()` / `ParserInput.fromUTF8` (é is 1 UTF-16 unit vs 2 UTF-8 bytes; a non-BMP scalar is 2 vs 4). That matches `ParserInput.fromLatin1` on those same UTF-8 bytes. Invalid UTF-8 cannot be passed as Zel’s `&str`; lossy replacement changes length and columns, while Java Latin-1 keeps each raw byte.
  - `getLineAt` throws `IllegalStateException` with text `Illegal position: <offset>` when `offset < 0` or `offset > size`. `Lexer::location` clamps the `isize` into `0..=size` and still returns a line and column (a negative offset becomes line 1 column 1; past the end becomes the EOF position).
  - Java line, column, offset, and the `computeLinestart` length are signed 32-bit. `column = offset - linestart[line] + 1` wraps at 2^31, and the `int` line-count (starting at 2, plus one per newline) can overflow so `new int[size]` throws `NegativeArraySizeException`. Zel stores line starts in `usize` and the offset in `isize`, and does not wrap at 2^31.

### `src/main/java/net/starlark/java/syntax/FileOptions.java`
- Bazel lines: 141
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Starlark `FileOptions` or builder exists. The eight dialect flags and defaults (`requireLoadStatementsFirst` true, the other seven false) are not a configurable options object. Nearby code only hardcodes fragments: `Resolver.allow_late_loads` defaults false and is then set from the filename, `EvalContext.allow_type_syntax` defaults false, and private `load` symbols are always rejected.

### `src/main/java/net/starlark/java/syntax/FloatLiteral.java`
- Bazel lines: 53
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Zel `Expr::Literal(Value::Float(f64))` stores only the numeric value. `getRaw()` source text is not kept, so distinct spellings of the same number are not recoverable.
  - `getStartOffset` is the token offset and `getEndOffset` is `tokenOffset + raw.length()` (UTF-16 code units, signed 32-bit add). `Expr::location()` returns `Location::default()` (empty file, line 0, column 0) for every literal, and no end offset is stored. `parse_primary` computes a line/column before consuming a float token and then discards it.

### `src/main/java/net/starlark/java/syntax/FlowStatement.java`
- Bazel lines: 56
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java stores a signed 32-bit UTF-16 start offset and an exclusive end of `offset + keyword.length()` (`break` 5, `continue` 8, `pass` 4), so the end sum wraps at 2^31; Rust `Stmt::Pass`/`Break`/`Continue` keep only a start `Location` whose column counts Unicode scalars (`usize`) and have no end offset, so a non-BMP character earlier on the line shifts the Java column by 2 and the Rust column by 1.
  - Java `toString` (and pretty-print) is the keyword plus a newline (`break\n`, `continue\n`, `pass\n`); the Rust statement has no such rendering.

### `src/main/java/net/starlark/java/syntax/ForStatement.java`
- Bazel lines: 82
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Loop variables are an `Expression` (identifier, index `a[b]`, attribute `c.d`, call, or a tuple/list of those), so a resolved loop assigns through `setIndex` / `setField`. `Stmt::For` stores `var_names: Vec<Pattern>` (`Name` or nested `Tuple` only); the parser rejects anything else with `Expected variable name`.
  - A one-element target `(a,)` stays a one-element tuple expression and is unpacked (length mismatch: `too few/many values to unpack (got %d, want %d)`; non-sequence: `got '%s' in sequence assignment (want %d-element sequence)`). The Rust parser flattens a lone `Pattern::Tuple`, so `(a,)` is stored as a bare name and the whole element is bound.
  - `getStartOffset` is the `for` token as a signed 32-bit UTF-16 code-unit index. `getEndOffset` is the last body statement’s end offset, or the iterable’s end offset when `body` is empty. `Stmt::For` keeps only a start `Location` whose line and column are `usize` and whose column advances one per Unicode scalar, with no end position.
  - `toString` is `for ` + vars + ` in ` + iterable + `: ...\n`. `Stmt` has no equivalent `Display`.
  - `vars` and `iterable` are rejected with `NullPointerException` when null; a null `body` is stored and fails later in `getBody` / `getEndOffset`. The Rust fields are non-null (`body` is a `Vec`, empty when there are no statements).

### `src/main/java/net/starlark/java/syntax/Identifier.java`
- Bazel lines: 139
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - `getEndOffset` is `nameOffset + name.length()` in UTF-16 code units, with signed 32-bit wrap. `Expr::Ident` stores only a start `Location` (`line`/`column` as `usize`). Bazel reads files as Latin-1 (`ParserInput.fromLatin1`), so each source byte is one UTF-16 column; the eval lexer counts Unicode scalars, so an identifier after a multibyte UTF-8 character is reported at a different column.
  - `isValid` is false for the empty string and for any UTF-16 code unit outside `[A-Za-z_]` (digits only when the index is greater than 0). A use of such a misparsed identifier is reported as `contains syntax errors`. Eval identifiers come only from the ASCII lexer, and an unknown name is `name '...' is not defined`. The diagnostic parser reports `contains syntax errors` for every error-expression slice, including text that `isValid` would accept.
  - The node holds a nullable `Resolver.Binding`. `setBinding` throws `IllegalStateException` if a binding is already set, and keyword-argument and field identifiers stay null after resolution. `Expr::Ident` is `(String, Location)` with no binding.
  - `boundIdentifiers` returns an insertion-ordered set of identifier nodes, walking nested lists and tuples and skipping index, dot, and other elements, so `x[0], y = ...` and `for x[0] in ...` bind only the identifiers. `pattern_names` only collects `Name` and `Tuple` patterns. An index or attribute inside a list or `for` target fails the parse (`cannot assign to non-lvalue expression` or `Expected variable name`). A wholly invalid Java LHS is `cannot assign to '%s'`. At top level, two identifiers with the same name in one target (`a, a = ...`) are both bound and the second reports `'a' redeclared at top level` (`allowToplevelRebinding` defaults to false). Those names are inserted into a `HashSet`, so the second binding is dropped and the redeclaration is not reported.
  - `isPrivate` is `name.startsWith("_")`. With `allowLoadPrivateSymbols` defaulting to false, `load` of that symbol is `symbol '%s' is private and cannot be imported`. The eval `load` parser accepts it. The diagnostic parser always emits `symbol '{orig}' is private and cannot be imported` and has no allow flag.

### `src/main/java/net/starlark/java/syntax/IfStatement.java`
- Bazel lines: 92
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java keeps one node per `if` or `elif` (`isElif` when the token is `ELIF`). Each `elif` is the sole statement of the previous node's else block, and only a final `else` suite is a plain else block. Zel `Stmt::If` is a single node: `branches[0]` is the `if`, later pairs are `elif`s in source order, and `else_body` is only the final else suite.
  - Java `getElseBlock()` is null when no else was attached, and a misparse can leave a non-null empty then/else list. `getEndOffset()` then returns `condition.getEndOffset()`; otherwise it is the end offset of the last statement of the else block if non-null, else of the then block. Zel `else_body` is a `Vec` that is empty when there is no else, and the node has no end offset or empty-body fallback.
  - Java `getStartOffset()` is the `if` or `elif` token's signed 32-bit UTF-16 code-unit index. Zel stores one `Location` (1-based line and column as `usize`) taken at the `if` keyword only, counted in Unicode scalars, with no per-`elif` start.

### `src/main/java/net/starlark/java/syntax/IndexExpression.java`
- Bazel lines: 67
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java stores a half-open UTF-16 span from the object's start offset through one past `]` (`getEndOffset` is `rbracketOffset + 1` in a signed 32-bit `int`, so the end wraps at `Integer.MAX_VALUE`) plus a separate `[` offset. Rust `Expr::Index` keeps only a line/column `Location` of the `[` token, counted in Unicode scalars (`usize`), so `location()` is the bracket rather than the object's start and a non-BMP character earlier on the line shifts the column relative to Java.

### `src/main/java/net/starlark/java/syntax/IntLiteral.java`
- Bazel lines: 102
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`
- Verdict: partial
- Differences:
  - Hex and octal values above `u128` stay the raw digits (`0x100000000000000000000000000000000` becomes `"100000000000000000000000000000000"`). Java `BigInteger` returns the decimal value (`340282366920938463463374607431768211456`). Decimal literals still match, because a token that does not start with `0` is already decimal text.
  - Java `Long.parseLong` and `BigInteger` accept a leading `+` or `-`, including after `0x` or `0o` (`"-1"` is `-1`, `"0x-10"` is `-16`, `"-9223372036854775808"` is `Long.MIN_VALUE`). Rust rejects `+` and `-` as non-digits (`invalid base-N integer literal`).
  - `Character.digit` accepts non-ASCII digits, so `"١"` and `"１２"` are `1` and `12`, and `"0xｆ"` is `15`. Rust `char::is_digit` is ASCII-only, so those strings are `invalid base-N integer literal`.

### `src/main/java/net/starlark/java/syntax/IsInstanceExpression.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - With type syntax enabled, Bazel lexes `isinstance` as its own keyword and this node is the only form: `isinstance(<test>, <type-expr> [,])`, spanning from the keyword through the character after `)`, printed as `isinstance(<value>, <type>)`. Resolution always fails with `isinstance() is not yet supported` after walking the value, then the type. Zel has no such keyword or node (`keyword()` never maps `isinstance`). The text is an identifier, so `isinstance(...)` is an ordinary call of any arity, including keywords and `*`/`**`. With `allow_type_syntax`, `eval_file_checked` skips `bazel_syntax::parse`, so that call is evaluated instead of rejected.

### `src/main/java/net/starlark/java/syntax/LambdaExpression.java`
- Bazel lines: 70
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - The Java node’s span is a signed 32-bit offset of the `lambda` token through `body.getEndOffset()`, mapped by `FileLocations` on a Latin-1 char buffer (`ParserInput.fromLatin1` for Bazel files), so columns count source bytes. `Expr::Lambda` stores only a start `Location` whose column advances once per Unicode scalar, and it has no end offset. A non-ASCII byte before `lambda` (UTF-8 `é` is two Bazel columns and one Zel column) moves the reported position, and bytes that are not valid UTF-8 never become this node.

### `src/main/java/net/starlark/java/syntax/Lexer.java`
- Bazel lines: 1056
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`
- Verdict: partial
- Differences:
  - `#:` comments become `DOC_COMMENT_BLOCK` or `DOC_COMMENT_TRAILING` in Java (a block also counts as a preceding newline, so EOF is not rewritten to `NEWLINE`). The port skips them like ordinary comments, so `def f() #: c` is reported at `newline` (`expected :`) instead of at the comment text `#: c`.
  - With `FileOptions.allowTypeSyntax` (default false), Java tokenizes `cast` and `isinstance` as keywords. The port always tokenizes them as identifiers.
  - With `FileOptions.stringLiteralsAreAsciiOnly` (default false), Java errors on string characters `>= 0x80` and on octal escapes `>= 0x80`. The port never emits those errors.
  - Integer literals wider than 128 bits with a `0x` or `0o` prefix: Java `IntLiteral.scan` yields a `BigInteger` whose decimal `toString()` is what syntax errors quote. `int_literal_scan` keeps the raw prefix-stripped digits (`0x1` followed by 32 zeros is shown as those hex digits, not the decimal value).
  - Subnormal float tokens in syntax errors use Java `Double.toString` (`4.9E-324`, `9.9E-324` for the values of `5e-324` and `1e-323`). `java_double_to_string` prints Rust’s scientific form (`5.0E-324`, `1.0E-323`).
  - `parse` turns each byte of a Rust `&str` into a char (Latin-1), matching Bazel `ParserInput.fromLatin1`. `Lexer` on `fromUTF8` or `fromString` scans UTF-16 code units, so a non-ASCII scalar is one BMP char or a surrogate pair, and ill-formed UTF-8 is `U+FFFD`, which changes tokens and columns.

### `src/main/java/net/starlark/java/syntax/ListExpression.java`
- Bazel lines: 107
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Bracketed nodes start at the `[` or `(` offset and end at `rbracketOffset + 1` (exclusive, one UTF-16 code unit past the closer). Unparenthesized tuples (`lbracketOffset < 0`) start at the first element and end at the last element's end, so a following comma is outside the node. `Expr::List` and `Expr::Tuple` keep only a start `Location`. The evaluator lexer advances the column once per Unicode scalar, and there is no exclusive end.
  - The constructor rejects an empty element list unless both delimiter offsets are `>= 0`, so an empty unparenthesized tuple cannot exist. `Expr::Tuple` is a `Vec` plus a start location and does not record delimiter presence or check that invariant.
  - `toString` always prints tuples as `(...)` and lists as `[...]`, adds a comma for a one-element tuple, and `appendNodes` drops the current element and writes `+%d more` when the buffer length is `>= 32` UTF-16 units or when element index 4 is reached and another element follows. That text is what assignment errors and call/type-argument debug strings use. Rust expressions have no equivalent rendering.
  - The evaluator parser (`/workspace/zel-cursor/crates/zel-starlark/src/syntax/parser.rs`, `parse_expr_or_tuple`) accepts a trailing comma on an unparenthesized tuple (`a,` and `a[1,]`), so those sources become a `Expr::Tuple`. Bazel reports `Trailing comma is allowed only in parenthesized tuples.` The diagnostic parser in `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs` does match that error.

### `src/main/java/net/starlark/java/syntax/LoadStatement.java`
- Bazel lines: 85
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - `getStartOffset` is the `load` keyword’s signed 32-bit UTF-16 code-unit index, and `getEndOffset` is the 32-bit sum `rparenOffset + 1` (the position just past `)`). Rust `LoadStatement` has no offsets; `Stmt::Load` keeps only the `load` token’s `Location` (`line` and `column` as `usize`). That column comes from `lexer.rs`, which advances once per Unicode scalar (`char_indices`). The diagnostic `LoadInfo` in `bazel_syntax.rs` instead maps each source byte to a Latin-1 column (`as_bytes()` with `b as char`) and records the label literal, not an exclusive end at `)`.
  - An unaliased binding is one shared `Identifier`, so the local and original names share one UTF-16 start (`token.start + 1` when the symbol is a string) and an end of `nameOffset + name.length()` (UTF-16 units). Rust stores two independent strings, in source order, with no binding offsets. `LoadInfo.symbols` keeps only the original name and its Latin-1 position, so the local alias has no position on the node.

### `src/main/java/net/starlark/java/syntax/Location.java`
- Bazel lines: 115
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/location.rs`
- Verdict: partial
- Differences:
  - `toString` prints `file`, then `:line` only when `line != 0`, then `:column` only when `column != 0` (so `line == 0` drops a non-zero column, and an empty file yields `""`, `:line`, or `:line:col`). Rust `Display` always prints `line:column` when `file` is empty and `file:line:column` otherwise, including zeros.
  - `fromFileLineColumn` throws `IllegalArgumentException` when `column != 0` and `line == 0`. Rust `Location::new` accepts that pair. There is no `fromFile` or `BUILTIN` (`"<builtin>"`, line 0, column 0); `Default` is an empty file with line 0 and column 0.
  - Java stores signed 32-bit `line` and `column` and orders with `String.compareTo` (UTF-16 code units) then `Long.compare(((long) line << 32) | column, …)`, so a negative column is sign-extended into the line bits. Rust stores `usize` and implements no `Ord`.
  - `hashCode` is the signed 32-bit mix `97 * file.hashCode() + 37 * line + column`. Rust `Location` is not `Hash`.
  - A null `file` throws `NullPointerException`. Rust `file` is a `String` (Unicode scalars); Java equality and ordering also accept unpaired UTF-16 surrogates.

### `src/main/java/net/starlark/java/syntax/Node.java`
- Bazel lines: 130
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java start and end offsets are signed 32-bit UTF-16 code-unit indexes into a shared `FileLocations`. `getStartLocation` and `getEndLocation` map those indexes to a 1-based column in UTF-16 code units, and the end location is the position immediately after the node. Zel `Expr::location` and `Stmt::location` return one stored `Location`. The lexer that fills it advances the column by one per Unicode scalar, so a non-BMP character shifts later columns by 1 in Zel and by 2 in Bazel. There is no exclusive end position.
  - `Expr::Literal` (int, float, string, bool, `None`) returns `Location::default()`: empty file, line 0, column 0. Java literals keep real start and end offsets. A null `FileLocations` is rejected when the node is built; Zel uses that zero default.
  - `getStartLocation` is the start of the leftmost token or subexpression (left operand, callee, object). Zel stores the operator or punctuation instead: binary and boolean operators, the `if` of a conditional expression, and `(` / `.` / `[` on calls, attributes, indexes, and slices.
  - `prettyPrint()` is canonical source via `NodePrinter`, and `toString()` defaults to that. Zel syntax nodes have no pretty-printer.

### `src/main/java/net/starlark/java/syntax/NodePrinter.java`
- Bazel lines: 590
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Starlark syntax-tree pretty printer. `Expr` and `Stmt` in `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs` are parsed and evaluated, but nothing reprints statements, suites, `def` signatures, parameters, calls, comprehensions, slices, loads, or string literals (including the `< 32` `\xNN` escapes).

### `src/main/java/net/starlark/java/syntax/NodeVisitor.java`
- Bazel lines: 319
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Rust syntax visitor walks the tree in lexical order with `skipNonSymbolIdentifiers` (keyword-argument names and dot fields omitted), rejects comments, visits only a load binding’s local name, skips null optional children, or provides final list/block walks. `resolver.rs` and `checks.rs` recurse for their own checks and do not implement this traversal.

### `src/main/java/net/starlark/java/syntax/Parameter.java`
- Bazel lines: 164
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java keeps a type expression on every parameter (`getType()`). `Mandatory`, `Star`, and `StarStar` end at that expression when it is present, and `NodeVisitor.visit(Parameter)` walks it (so `TypeResolver` can type the binding). `ParamDef` has no type; `parser.rs` parses `: Type` and discards it. With `allow_type_syntax`, the Latin-1 checker is skipped, so the annotation is neither rejected nor resolved.
  - A bare `*` is a `Parameter.Star` with a null identifier (`getName()` / `getIdentifier()` are null) whose start is the `*` token. The constructor rejects a type in that case (`id != null || type == null`), and `getEndOffset()` then calls `getIdentifier().getEndOffset()` (NPE). Rust stores no node and only sets `is_kw_only` on later parameters. `def f(*):` is a resolver error on that star in Java; the eval parser treats a star not followed by a comma as `*args` and errors with "Expected parameter name".
  - Only `Optional` has a default (`getDefaultValue()` is null on the other three; its end offset is the default expression, not the type). `ParamDef.default` is an `Option` on every parameter, and the eval parser accepts `*args = expr` and `**kwargs = expr`. Call binding never applies that default, because the `is_args` / `is_kwargs` arms run first. Java cannot represent it, so `=` is a syntax error.
  - Spans are Java `int` offsets into Latin-1 bytes (`ParserInput.fromLatin1` for BUILD files) or UTF-16 code units. Mandatory and optional parameters start at the name; `*` / `**` forms start at the star token. `ParamDef` has no offsets. The eval lexer counts one column per Unicode scalar, and `syntax/resolver.rs` reports `duplicate parameter` at the `def` location rather than at the parameter.

### `src/main/java/net/starlark/java/syntax/Parser.java`
- Bazel lines: 1715
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`
- Verdict: partial
- Differences:
  - `#:` is a real token (`DOC_COMMENT_BLOCK` / `DOC_COMMENT_TRAILING`), and those tokens are statement and expression-list terminators. Zel scans every `#` as a comment, so `x = #: oops` is reported at `'#: oops'` in Bazel and at `'newline'` in Zel.
  - Bazel treats a parenthesized name as an identifier (`f((x)=1)` is a keyword argument). Zel requires the argument to start with an identifier token and otherwise reports `keyword argument must have form name=expr`.
  - With type syntax left off, a type-alias tail is `parseTypeExpr` (identifier, `[…]`, `|` only). Zel parses it with `parse_test`, so `type X = a + b; y = 1` stops before `+` in Bazel (recovery then reports another syntax error at `;`) and consumes `a + b` in Zel (only the “type annotations are disallowed” error). There is no `FileOptions` switch: Bazel’s `allowTypeSyntax` path (`cast` / `isinstance`, annotations, `...` inside type expressions, duplicate type parameters) is always rejected.
  - `parseExpression` / `parseTypeExpression` parse one expression, require EOF, throw `SyntaxError.Exception`, and turn `StackOverflowError` into an `internal error: stack overflow…` syntax error. Zel only parses a whole file into an error list and does not catch stack overflow. Bazel also folds `"a" + "b"` into one string literal in the tree; Zel does not build that tree.

### `src/main/java/net/starlark/java/syntax/ParserInput.java`
- Bazel lines: 110
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`
- Verdict: partial
- Differences:
  - `fromLatin1` stores one UTF-16 char per input byte (`0xff & b`, U+0000–U+00FF). `parse` takes a `&str` that loaders built with `String::from_utf8_lossy` (`/workspace/zel-cursor/crates/zel-packages/src/loader.rs`, `/workspace/zel-cursor/crates/zel-skyframe/src/lib.rs`), then maps each UTF-8 byte of that string to a scalar. An invalid file byte becomes U+FFFD and is then scanned as the three bytes `EF BF BD`.
  - `fromUTF8` and `readFile` decode with `StandardCharsets.UTF_8` (malformed input replaced by U+FFFD) into UTF-16 code units, so a non-BMP character occupies two positions. The diagnostic buffer keeps one character per UTF-8 byte. The evaluator lexer (`/workspace/zel-cursor/crates/zel-starlark/src/syntax/lexer.rs`) advances one column per Unicode scalar.
  - `fromString` and `fromCharArray` keep Java UTF-16 code units, including unpaired surrogates. Rust source is a UTF-8 `&str` of Unicode scalars, which cannot contain an unpaired surrogate.
  - `fromLines` joins the arguments with `"\n"` and sets the file name to `""`. Callers pass one string and their own file name.
  - A null file throws `NullPointerException`. `fromCharArray` keeps a null content array. Rust file and source are always present `&str` values. `readFile` throws `IOException` on a read failure; the loaders return an error string.
  - `fromCharArray` stores the caller’s array, so a later write changes the input. `parse` copies the bytes into a new `Vec<char>`.

### `src/main/java/net/starlark/java/syntax/Program.java`
- Bazel lines: 162
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`
- Verdict: partial
- Differences:
  - `compileFile` copies the global name-to-doc-comment map (declaration order, only names that have a `#:` comment) and the `#:` comments not attached to any global, in file-comment order. Zel never collects `#:` comments, so both results are missing.
  - With `resolveTypeSyntax`, `compileFile` runs `TypeResolver.annotateFile` and fails the compile when a type is unknown, a constructor is misused, or a type expression is unsupported. Zel’s `allow_type_syntax` path skips that check and discards annotations and `type` aliases, so those files compile. The flag defaults off on both sides, and then both reject the syntax at parse time.
  - If the file already has scan or parse errors, `compileFile` still resolves it and throws one exception containing every parse error plus every resolution error. `eval_file_checked` returns at the first diagnostic-parse failure and adds structural resolver errors only for `.bzl` files when `static_names` is set, so other files drop resolution errors whenever a parse error exists.
  - A successful `compileFile` returns every top-level load’s decoded module string and that string literal’s location, in source order, duplicates included, and does not fetch modules. Zel keeps canonical labels in a `BTreeMap` keyed by the raw string, so a repeated module string keeps only the last canonical form. With `allow_type_syntax` the diagnostic parse is replaced by an empty result, the static load list is empty, and loads run later only if execution reaches them.
  - Bazel file text is Latin-1, so those load strings and columns are one unit per file byte. Zel reads the file with `String::from_utf8_lossy` first, so an invalid UTF-8 byte becomes U+FFFD and both the module string and its columns differ. The diagnostic scan then treats that lossy string’s UTF-8 bytes as Latin-1, while the lexer that executes `load` counts Unicode scalars, so a non-ASCII module string used for preload does not match the string used at execution.
  - `compileExpr` resolves one expression, fails with only that resolution error list, and returns empty loads, locations, and doc comments, without the type resolver. Zel has no expression compile; `eval_expr` evaluates immediately and does not produce that result or the same pre-eval resolution failure.

### `src/main/java/net/starlark/java/syntax/Resolver.java`
- Bazel lines: 1405
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/resolver.rs`
- Verdict: partial
- Differences:
  - Top-level rebinding is rejected for `.bzl` under the Java defaults (`allowToplevelRebinding` false, `loadBindsGlobally` false): a second assignment, function, or load of the same name reports `'%s' redeclared at top level` and `'%s' previously declared here`, and `load(..., "x")` followed by `x = 1` reports `conflicting global/file-local declaration of 'x'`. Zel never emits those. `resolve_module` instead keeps one file-wide set of load locals and fails with `load statement defines '{name}' more than once` at the load statement, including in BUILD files, where Java allows the second load (`allowToplevelRebinding` and `loadBindsGlobally` are true). A repeated local inside a single load is reported with Java's message by `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`.
  - Loads-first (`requireLoadStatementsFirst`, default true) records every late load and a second error on the first other statement, `\tfirst non-load statement appears here`. Zel returns on the first late load and omits that note. `pass` counts as that first statement in Java; `Stmt::Pass` does not, so `pass` then `load` is accepted in a `.bzl` file. The switch is `allow_late_loads = !file_name.ends_with(".bzl")`, so `.scl` and other non-`.bzl` files that use Java's default options may place loads later.
  - `x += …` on a function-local that has no earlier assignment in that body is a static error, `local variable '{name}' is referenced before assignment`, including when an enclosing function already assigned `x`. Java binds the name for the whole function in the first pass and leaves the failure to runtime. The list/tuple augmented-assignment error matches Java and is implemented in `bazel_syntax.rs`.
  - Undefined names are not static errors unless `strict_names` or `static_names` is set (both default false). Java `resolveFile` always reports `name '%s' is not defined`. When `/workspace/zel-cursor/crates/zel-starlark/src/syntax/checks.rs` does run, suggestions omit enclosing-block locals (Java's `getAllSymbols` includes them), then walk module names in `HashSet` order, the predeclared slice, and a fixed universe. Edit distance uses Unicode scalar length and `to_lowercase`; Java uses UTF-16 `String.length` / `charAt` and locale `toLowerCase`, so non-BMP characters and case folding change whether a hint is offered and which equal-distance candidate wins.
  - Top-level `if` and `for` are always errors in `resolveFile`. Zel adds those messages only for `.bzl` when `static_names` is set; otherwise they execute.
  - `resolve_module` does not enforce parameter order or call-argument order (required-after-optional, bare `*`, keyword after `*args`, and the other `resolveFunction` / `visit(CallExpression)` rules). Those messages exist only on the `bazel_syntax.rs` parse used by `eval_file_checked`. `eval_str` accepts programs Java rejects. That checked path also returns before `resolve_module` whenever any parse or undefined-name error exists, so a late load, a cross-load duplicate, or the augmented-assignment error is dropped; Java accumulates them with the other resolver errors.
  - An illegal assignment target fails the eval parse as `cannot assign to non-lvalue expression`. Java reports `cannot assign to '%s'` and continues resolving. Private load symbols are always rejected; Java's `allowLoadPrivateSymbols` permits them. With type syntax enabled, Java still reports `isinstance() is not yet supported`, `type alias statement not at top level`, and a second type annotation (`type annotation on '%s' may only appear at its first declaration`); Zel does not.

### `src/main/java/net/starlark/java/syntax/ReturnStatement.java`
- Bazel lines: 57
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java `getEndOffset` is the result expression’s exclusive end when `getResult()` is non-null, and `returnOffset + "return".length()` (6 UTF-16 code units, signed 32-bit `int` add) when the result is null. `Stmt::Return` stores only a start `Location` (`usize` line and column) and has no exclusive end.
  - `ReturnStatement.make(expr)` sets the statement start to `expr.getStartOffset()`, so the span is the expression. Zel lambda desugaring uses `Stmt::Return(Some(body), <lambda location>)`, locating the statement at the `lambda` keyword. A final top-level expression stays `Stmt::Expr` and evaluates as `ControlFlow::None`; Bazel replaces it with `ReturnStatement.make(expr)`.
  - The Java start is a UTF-16 code-unit offset; column is `offset - lineStart + 1`, so a non-BMP character counts as two. Zel advances the column once per Unicode scalar.

### `src/main/java/net/starlark/java/syntax/SliceExpression.java`
- Bazel lines: 82
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - Java’s slice span starts at the object (`getStartOffset`) and ends at the UTF-16 index immediately after `]` (`getEndOffset` is `rbracketOffset + 1`), with `[` stored separately for `getLbracketLocation`. `Expr::Slice` keeps a single `Location` captured at `[` in `/workspace/zel-cursor/crates/zel-starlark/src/syntax/parser.rs` before that token is consumed, and it has no end position, so the expression location is the bracket.
  - `getLbracketLocation` uses UTF-16 columns (`FileLocations`: one column per Java `char`, so a supplementary character counts as two). The `Location` on `Expr::Slice` comes from `/workspace/zel-cursor/crates/zel-starlark/src/syntax/lexer.rs`, which advances the column once per Unicode scalar.

### `src/main/java/net/starlark/java/syntax/StarlarkFile.java`
- Bazel lines: 156
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`
- Verdict: partial
- Differences:
  - `parse` always returns a file whose statement list survives errors; `errors()` is a live unmodifiable view the resolver appends to, and `ok()` tracks that list. `bazel_syntax::parse` returns diagnostics only. The tree `eval_str` actually runs (`parser.rs` `parse_module`) fails on the first error and drops the rest.
  - Offsets (`getEndOffset` = `locs.size()`, and columns) index the `ParserInput` `char[]`. Bazel feeds raw file bytes through `fromLatin1` (one char per byte, including ill-formed UTF-8). Rust `parse` scans `src.as_bytes()` as Latin-1, and loaders pass `String::from_utf8_lossy` first, so invalid UTF-8 becomes U+FFFD before that scan. `fromString` / `fromUTF8` positions are UTF-16 code units; the Rust buffer is UTF-8 bytes. The AST lexer then counts Unicode scalars.
  - `parse(input, options)` stores the dialect (`DEFAULT`: loads must be first, no toplevel rebinding, loads are not global, type syntax off, string literals not ASCII-only). Rust `parse` takes no options. Type syntax is always a diagnostic error unless `allow_type_syntax` skips this parser (`ParseOutput::empty()`). There is no ASCII-only string-literal mode.
  - Java `parse` records only scanner and parser errors; resolver errors are appended later in traversal order. Rust `parse` also returns structural resolver errors and stable-sorts them by offset, mapping a negative key to line/column `(0, 0)`.
  - `getResolvedFunction()` stays null until the resolver sets it. `ParseOutput` has no resolved-function slot. `setParseProfiler` is a global hook around the parse; Rust `parse` does not call one.

### `src/main/java/net/starlark/java/syntax/Statement.java`
- Bazel lines: 52
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - `Kind` includes `TYPE_ALIAS` and `VAR`. `Stmt` has neither, so a type alias and a bare `name: Type` annotation cannot be represented. `parser.rs` lowers both to `Stmt::Pass`, discarding the alias name, type parameters, definition, annotated identifier, type expression, and doc comments, so they are indistinguishable from `pass`.
  - `Kind.FLOW` is a single tag for `break`, `continue`, and `pass`. `Stmt` uses three variants (`Break`, `Continue`, `Pass`), so a dispatch on statement kind does not group them.

### `src/main/java/net/starlark/java/syntax/StringLiteral.java`
- Bazel lines: 98
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`
- Verdict: partial
- Differences:
  - No string-literal node. `parse_string_literal` drops `lexer.value` and returns `Ex::Other`. The evaluator stores `Expr::Literal(Value::String)` in `/workspace/zel-cursor/crates/zel-starlark/src/syntax/parser.rs`, and `Expr::location()` for `Literal` is `Location::default()` (line 0, column 0). Java `getValue`, `getStartOffset`, `getEndOffset` (stored exclusive token end), and `getLocation` (location of `startOffset`) are absent. There is no `unquote`: Java lexes with `FileOptions.DEFAULT` and throws `IllegalArgumentException` with the first lexer message, or `"invalid syntax"` unless the token is a `STRING` covering `[0, s.length())`.
  - Offsets and text units differ. Java stores signed 32-bit indexes into UTF-16 code units (unpaired surrogates are one unit each, and `unquote` can return them). `bazel_syntax::parse` copies `src.as_bytes()` into `Vec<char>` (each byte one Latin-1 scalar) and keeps token bounds as `isize`, so non-ASCII UTF-8 shifts string columns relative to Java and does not wrap at 2^31. The value that actually executes is re-lexed from Unicode scalars in `/workspace/zel-cursor/crates/zel-starlark/src/syntax/lexer.rs`.
  - That value lexer does not match the lexer `unquote` uses. `\x`, `\u`, and `\U` are decoded, other unknown escapes are kept as `\` plus the character with no error, and octal uses `char::from_u32` of the full value or U+FFFD. Java reports `invalid escape sequence: \…` (`unquote` throws) and, for octal `> 0xff`, errors and keeps `(char) (octal & 0xff)`. Raw strings do not consume the character after `\`: a quote can close the literal, and `\r\n` / `\r` / `\n` are not folded into `\` plus a single `\n`, so a file `bazel_syntax` accepts can evaluate to a different string. `b` / `br` prefixes become bytes tokens; Bazel treats `b` as an identifier, so `unquote` rejects the input.

### `src/main/java/net/starlark/java/syntax/SyntaxError.java`
- Bazel lines: 100
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`
- Verdict: partial
- Differences:
  - `toString` is `Location` plus `": "` plus the message, and `Location` drops a zero column (`file:line: message`) and a zero line (`file: message`). Zel always prints `file:line:column: message`, so those cases become `file:line:0: message` and `file:0:0: message`. Non-zero line and column match, including an empty file name (`:1:2: message`).
  - `SyntaxError.Exception` is absent. Java rejects an empty list with `IllegalArgumentException("no errors")`, keeps a snapshot of the list, and `getMessage()` is only the first error’s message, or `"{message} (+ {n} more)"` when there are more. Zel returns every error as its own location-prefixed string and allows an empty list.

### `src/main/java/net/starlark/java/syntax/TokenKind.java`
- Bazel lines: 129
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`
- Verdict: partial
- Differences:
  - `TK` has no `CAST` (`"cast"`), `ISINSTANCE` (`"isinstance"`), `DOC_COMMENT_BLOCK` (`"#:"`), or `DOC_COMMENT_TRAILING` (`"trailing #: "`), so those kinds are never emitted and those `toString` texts never appear.
  - `TK::Null` is an extra kind and `text()` returns `"null"`. Java keeps a null `TokenKind` reference, and `TokenKind.toString()` has no such value.

### `src/main/java/net/starlark/java/syntax/TypeAliasStatement.java`
- Bazel lines: 84
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/bazel_syntax.rs`
- Verdict: partial
- Differences:
  - No type-alias node is built. `parse_type_alias_tail` always reports `type annotations are disallowed` at the `type` keyword, then parses the identifier, optional `[` parameters `]`, `=`, and the definition and discards them. Java still returns a `TypeAliasStatement` (start offset = `type` keyword, end offset = `definition.getEndOffset()`), including when type syntax is allowed.
  - `toString` is absent. Java prints `type ` + name, then if parameters are non-empty `[` + names joined by `", "` (the shared `appendNodes` cut: drop the element that reaches 32 characters, or the 5th element when more remain, and append `+N more`) + ` = ...\n`. The definition is always elided.
  - The eval parser (`/workspace/zel-cursor/crates/zel-starlark/src/syntax/parser.rs`) turns only `type Name = expr` into `Stmt::Pass` and drops `expr` with no diagnostic. `type Name[P] = expr` is not treated as an alias, so the parameter list is never stored.

### `src/main/java/net/starlark/java/syntax/TypeApplication.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no syntax node for a type application (`constructor[arg, …]`): no constructor identifier, type-argument list, end offset at the closing bracket, or abbreviated `toString` of that form. Bracket suffixes are ordinary `Index`/`Slice` expressions, and type syntax is either rejected or parsed as a general expression.

### `src/main/java/net/starlark/java/syntax/TypeResolver.java`
- Bazel lines: 319
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/parser.rs`
- Verdict: partial
- Differences:
  - Bazel runs this pass only when `resolveTypeSyntax` is set (`experimental_starlark_type_checking` and type syntax both on) and then fails compilation with `SyntaxError.Exception` if any type error was recorded. Zel stores `experimental_starlark_type_checking` (default `"off"`) and never consults it; with `allow_type_syntax` the annotations are parsed and dropped, and an unknown or illegal type still compiles.
  - Identifiers resolve only through `Types.TYPE_UNIVERSE` (`None`, `bool`, `int`, `float`, `str`, and the constructors `list`, `dict`, `set`, `tuple`, `Collection`, `Sequence`, `Mapping`). A miss becomes `Types.ANY` plus `type '%s' is not defined`. A non-constructor applied to arguments yields `'%s' is not a type constructor, cannot be applied to '%s'`; an unknown constructor yields `type constructor '%s' is not defined`; a constructor with no arguments yields `expected type arguments after the type constructor '%s'`. `IllegalArgumentException` from the constructor (for example `list[] accepts exactly 1 argument but got 2`, `dict[] accepts exactly 2 arguments but got 1`) is reported and the type becomes `ANY`. Zel parses the annotation with `parse_expr` and discards it, so those cases produce no diagnostic and no type.
  - `|` is `Types.union`. Any other binary operator is `binary operator '%s' is not supported` and `ANY`. Anything else is `unexpected expression '%s'` or `expression '%s' is not a valid type.` and `ANY`. Zel treats `|` as bitwise or inside the discarded expression and does not build a union.
  - `def` stores a `Types.callable` on both the resolved function and the function identifier: names omit `*`/`**`, a missing annotation is `ANY`, positional-only count is always 0, positional count is the parameters before the first `*`, the mandatory set is parameters with no default (insertion order), a named `*args`/`**kwargs` with no annotation is `ANY`, a missing star is null, a bare `*` adds no varargs type, and a missing return type is `ANY`. Lambda callables always have return `ANY`, and every parameter binding is `ANY` or its annotation. A binding or function that already has a type throws `IllegalArgumentException`; a missing binding throws. Zel `ParamDef` keeps only the name, default, `*args`, `**kwargs`, and keyword-only flag, so annotated and unannotated parameters are the same and no callable is recorded.
  - An annotated assignment sets that binding and a later assignment with no annotation leaves it. A variable annotation (`x: int`) sets the binding to the evaluated type. Zel turns `name: Type` with no `=` into `Pass` (no binding) and `name: Type = value` into a plain assignment. `parse_lambda` treats the first `:` as the start of the body, so a lambda parameter annotation is not resolved.

### `src/main/java/net/starlark/java/syntax/UnaryOperatorExpression.java`
- Bazel lines: 63
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs`
- Verdict: partial
- Differences:
  - The start position is a signed 32-bit UTF-16 code-unit offset (`opOffset`). Zel stores only the operator token’s `Location`, whose column is a `usize` advanced once per Unicode scalar, so an operator after a non-BMP character is reported at a different column and the index does not wrap at 2^31.
  - The exclusive end is the operand’s end offset, so the node spans the operator through the operand. Zel keeps no end position, so that half-open span is absent.

### `src/main/java/net/starlark/java/syntax/VarStatement.java`
- Bazel lines: 76
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/parser.rs`
- Verdict: partial
- Differences:
  - A bare `name: Type` (no `=`) becomes `Stmt::Pass` at the identifier token. The identifier, type expression, and trailing `#:` doc comments are discarded. Java keeps all three (`docComments` may be null) and the span runs from the identifier’s start offset through the type expression’s end offset.
  - A non-identifier left-hand side is a syntax error in Java (`type annotations must have a single identifier on the left-hand side`) and becomes an error expression over that span. Rust only takes the annotation path for `Expr::Ident`, so other left-hand sides stay expression statements and that diagnostic is not produced.
  - Java builds this node only when type syntax is allowed (off by default). Otherwise the colon is an error (`type annotations are disallowed`), is not consumed, and the statement is just the left-hand expression. The Rust parser always consumes the annotation and succeeds, with no allow/disallow switch.
  - Java binds the identifier as a typed binding and, when type resolution is on, walks the type. `Stmt::Pass` binds nothing. With the default strict type grammar, a non-type expression is `expected a type`; Rust parses it with ordinary expression syntax (including operators the type grammar rejects) and then drops it.

### `src/main/java/net/starlark/java/types/StarlarkType.java`
- Bazel lines: 39
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No static type-lattice root, so the default `getSupertypes()` empty list is never produced. Runtime values under `/workspace/zel-cursor/crates/zel-starlark/src/types/` are unrelated.

### `src/main/java/net/starlark/java/types/Types.java`
- Bazel lines: 606
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Starlark static type lattice: no `Any`/`None`/`bool`/`int`/`float`/`str`/`object` singletons, callable signature formatting (`Callable[[...], R]` and `(params) -> R` with `/`, `*`, `*args`, `**kwargs`, and `[optional]`), union flattening (drop duplicates, collapse to `object`, reject an empty union), or `list`/`dict`/`set`/`tuple`/`Collection`/`Sequence`/`Mapping` constructors and their supertype edges. Annotation syntax is only rejected (`type annotations are disallowed`) or parsed and discarded; it never builds these types.

### `src/main/starlark/builtins_bzl/bazel/exports.bzl`
- Bazel lines: 60
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/builtins/bazel/exports.bzl`
- Verdict: partial
- Differences:
  - The vendored `.bzl` matches, but `apply_exported_rules` in `/workspace/zel-cursor/crates/zel-packages/src/loader.rs` installs `exported_rules` only on `native`. BUILD predeclared names stay the native classes from `rule_defs_gen.rs`. Bazel replaces those BUILD symbols: `cc_toolchain_suite` becomes a `filegroup` that keeps only `name`, and each name in `_REMOVED_RULES` fails with the buildifier message.
  - A BUILD `cc_toolchain_suite(...)` in Zel is still a `cc_toolchain_suite` target, and unknown attributes are rejected. Bazel drops every argument except `name` and creates a `filegroup`. `native.cc_toolchain_suite` in a `.bzl` follows the Bazel lambda.
  - `exported_toplevels` (`py_internal`, `java_common`, `cc_common`, `apple_common`) are inserted into BUILD globals. Bazel injects them only into `.bzl` environments.
  - After loading `exported_toplevels`, Zel adds a `CcToolchainInfo` provider field to `cc_common` when that field is absent. The struct this file exports does not define `CcToolchainInfo`.

### `src/main/starlark/builtins_bzl/common/builtin_exec_platforms.bzl`
- Bazel lines: 465
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/build_options.rs`
- Verdict: partial
- Differences:
  - `CppOptions` sets `compiler` from `host_compiler`. `BuildOptions::exec` never writes `compiler`, so a non-null `--host_compiler` stays only on `host_compiler` and the exec `compiler` remains the default null.
  - `copt` and `cxxopt` append `-g0` unless `py_internal.get_current_os_name()` is `windows` (`OS.getCurrent()`, including a `blaze.os` override). Zel appends `-g0` unless `cfg!(windows)` (the compile-time target family).

### `src/main/starlark/builtins_bzl/common/cc/cc_common_bazel.bzl`
- Bazel lines: 84
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/builtins/common/cc/cc_common_bazel.bzl`
- Verdict: partial
- Differences:
  - `check_private_api` always returns `None`, so the allowlists on `check_experimental_cc_shared_library`, `incompatible_disable_objc_library_transition`, `add_go_exec_groups_to_binary_rules`, `get_tool_requirement_for_action`, `implementation_deps_allowed_by_allowlist`, and `internal_DO_NOT_USE` never fail. Bazel raises `file '…' cannot use private API` unless the caller is `@_builtins` or matches the allowlist (`("", "third_party/bazel_rules/rules_cc")` and `("rules_cc", "")` for `internal_DO_NOT_USE`).
  - `_builtins.internal.cc_common` is callable from any `.bzl`. Bazel rejects callers other than `cc_common.bzl`, `cc_common_bazel.bzl`, and `common/cc/{compile,link,toolchain_config}` (`add_go_exec_groups_to_binary_rules` is the Java method that also skips that check).
  - Flag reads are constants: `check_experimental_cc_shared_library` is always `True` (Bazel `--experimental_cc_shared_library` defaults to false), `incompatible_disable_objc_library_transition` is always `False` (Bazel defaults to true), and `add_go_exec_groups_to_binary_rules` stays `False` when the flag is enabled.
  - `do_not_use_tools_cpp_compiler_present` is a function (truthy; a call returns `False`). Bazel’s struct field is the `void` method result, `None`.
  - `legacy_cc_flags_make_variable_do_not_use` errors as unimplemented. Bazel returns the toolchain provider’s `_legacy_cc_flags_make_variable` string.
  - `implementation_deps_allowed_by_allowlist` errors as unimplemented. This Bazel tree’s `CcModule` has no such method, so an allowlisted call fails there too, with a missing-method error after the allowlist check.
  - `get_execution_requirements`, `get_tool_requirement_for_action`, and `get_memory_inefficient_command_line` return tuples. Bazel returns immutable lists. Execution-requirement duplicates are kept; Bazel’s `ImmutableSet` drops them.
  - `get_tool_for_action` and the requirement helpers require the action config to be enabled. Bazel resolves any action config present on the toolchain, including a disabled one.
  - A relative tool `path` is `tools_directory + "/" + path` with no `PathFragment` normalization (`.` / `..`, and an empty directory becoming `.`). Absolute means `starts_with('/')`. Setting both `tool` and `path`, or neither, or an empty `path`, is accepted; Bazel requires exactly one nonempty path or a `tool` artifact and returns a workspace-root exec path unprefixed.
  - `get_environment_variables` overwrites duplicate keys and returns a mutable dict. Bazel’s `ImmutableMap` builder throws on a duplicate key and the dict is immutable. `expand_if_available` is applied only when it is a single string; a list of variables (Java `expandIfAllAvailable`) is ignored, so the entry is always emitted.
  - `internal_DO_NOT_USE()` field order is sorted (`BTreeMap`). Bazel exposes the Java `cc_internal` object in method-registration order.

### `src/main/starlark/builtins_bzl/common/cc/cc_helper.bzl`
- Bazel lines: 1181
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/builtins/common/cc/cc_helper.bzl`
- Verdict: partial
- Differences:
  - `_dll_hash_suffix` formats `hash(workspace_name + package)` with `"_%x"`. The 32-bit hash matches Java `String.hashCode` on BMP text, but Zel prints a negative hash as a signed magnitude (`my_pkg` → `-3f29c747`) while Bazel prints the unsigned 32-bit hex (`c0d638b9`). Windows DLL name suffixes differ whenever that hash is negative.
  - `hash()` walks Unicode scalars (`c as i32`). Bazel walks UTF-16 code units, so a non-BMP character in that same string changes the hash (`"a😀b"` is `4077187` in Zel and `57849694` in Bazel).
  - `_expand`, `_tokenize`, `_expand_nested_variable`, `_expand_single_make_variable`, `_replace_name`, and `_get_base_name` index, slice, and take `len` by Unicode scalar. Bazel uses UTF-16 code units. BMP text matches. A supplementary character changes indices, slices, and backslash-escape consumption in copts, linkopts, defines, includes, env, and output basenames.
  - `_create_save_feature_state_artifacts` writes `str(feature_configuration)`. Bazel writes `<FeatureConfiguration(ENABLED:…;REQUESTED:…)>`. Zel writes `<FeatureConfiguration N>`.

### `src/main/starlark/builtins_bzl/common/cc/link/create_extra_link_time_library.bzl`
- Bazel lines: 185
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/builtins/common/cc/link/create_extra_link_time_library.bzl`
- Verdict: partial
- Differences:
  - `constant_fields` and `depset_fields` are built from `kwargs.items()`. Bazel stores `**kwargs` in call-site order (`LinkedHashMap`). Zel fills that dict from a `BTreeMap`, so the lists are sorted by field name. Those lists are part of `_key`, so two libraries with the same `build_library_func` and the same fields passed in different keyword orders stay distinct in Bazel and merge in Zel. `merge_extra_link_time_libraries` then walks `constant_fields + depset_fields` in that order.
  - `build_libraries` copies user fields with `for key in dir(library)`. Bazel `dir` returns the provider field names. Zel `dir` has no `ProviderInstance` arm and returns `[]`, so `build_library_func` is called with an empty `**kwargs` and every user field on `ExtraLinkTimeLibraryInfo` is dropped.
  - `_cc_internal.check_toplevel` in Bazel raises `Passed function must be top-level functions.` when the function is not the same-named global of its module. In analysis, Zel’s `cc_native` returns `None` and accepts nested functions and closures. At package load, `zel-builtins` has no `check_toplevel` implementation and fails with `cc_internal.check_toplevel is not implemented yet`.

### `src/main/starlark/builtins_bzl/common/cc/link/lto_backends.bzl`
- Bazel lines: 516
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/builtins/common/cc/link/lto_backends.bzl`
- Verdict: partial
- Differences:
  - `cc_internal.create_lto_backend_action` and `create_lto_backend_action_template` are unimplemented (`/workspace/zel-cursor/crates/zel-builtins/src/cc_internal.rs`) and fail analysis. Bazel registers an `LtoBackendAction` (or a template that expands one action per tree-artifact file), expands the `lto-backend` command line, and appends `-fPIC` when `use_pic` is set.
  - `collect_per_file_lto_backend_opts` always returns `[]` (`/workspace/zel-cursor/crates/zel-analysis/src/cc_native.rs`). Bazel appends `--per_file_ltobackendopt` options whose regex filter matches the object file, so those flags never reach `user_compile_flags`.
  - `wrap_link_actions(actions, None, True)` returns the original actions factory. Bazel wraps it so later `declare_shareable_artifact` / `declare_shareable_directory` calls create shareable bin-directory artifacts (used for non-indexing backends and `shared.nonlto` / `shared.nonlto-obj`). Zel declares an ordinary derived artifact at the same root-relative path and drops shareable ownership.

### `src/main/starlark/builtins_bzl/common/exports.bzl`
- Bazel lines: 37
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/builtins/common/exports.bzl`
- Verdict: partial
- Differences:
  - `proto_common_do_not_use.INCOMPATIBLE_ENABLE_PROTO_TOOLCHAIN_RESOLUTION` is the live `--incompatible_enable_proto_toolchain_resolution` value in Bazel. Zel’s host always returns `true` and ignores a false flag (the default is `true` on both sides).

### `src/main/starlark/builtins_bzl/common/paths.bzl`
- Bazel lines: 350
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/builtins/common/paths.bzl`
- Verdict: partial
- Differences:
  - `is_absolute` uses `len(path) > 2 and path[1] == ":"`. Bazel `len` and `s[i]` count UTF-16 code units (`String.length` / `charAt`); Zel counts Unicode scalar values (`str::chars`). A path whose first scalar is outside the BMP, such as `"😀:/a"`, is absolute in Zel and not in Bazel, because Bazel’s index 1 is a surrogate. The same code-unit versus scalar indexing applies to `elems()`, `rfind`, `index`, and slices in this file; for well-formed text those other functions still return the same strings.

### `src/main/starlark/builtins_bzl/common/xcode/providers.bzl`
- Bazel lines: 173
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/builtins/common/xcode/providers.bzl`
- Verdict: partial
- Differences:
  - The `.bzl` text matches, but every version this provider returns is built by Zel’s `apple_common.dotted_version` (`/workspace/zel-cursor/crates/zel-builtins/src/natives.rs`, compared in `dotted_version_cmp`). `==` and hashing use the raw string, so `1.2` is not `1.2.0` and `10P99q` is not `10P99q0`. Java `DottedVersion.equals` follows `compareTo`, where trailing zero components and a missing second number do not count.
  - Ordering keeps the whole non-digit tail as one string. Java splits each component with `(?i)\d+([a-z0-9]*?)?(\d+)?` and compares that trailing integer as a signed 32-bit value, so `1.2beta12 > 1.2beta2` and `1.2beta == 1.2beta0`. Zel orders `1.2beta12 < 1.2beta2` and treats `1.2beta` and `1.2beta0` as unequal.
  - Java stops at a trailing descriptive component matching `(?i)[a-z]\w*`, so `10.10.simulator.internal` equals `10.10`. Zel keeps those components, and a non-empty tail sorts before a bare number.
  - Java `Integer.parseInt` rejects a component above `2^31-1` and rejects `1..2`, `2:3`, and `1.-1`. Zel parses with `u64` (`unwrap_or(0)` on overflow) and accepts those strings when the first character is a digit. `None` becomes `None` instead of the Java `String` type error, so init does not fail and the getters return `None`.
  - Returned objects have `.version` and no `.to_string`. Bazel’s Starlark field is `.to_string` (`compare_to` exists on both).
  - `availability.lower()` is Rust Unicode `to_lowercase` (a character can expand, as with `İ`). Bazel uses `Ascii.toLowerCase` (ASCII `A–Z` only). `LOCAL` / `REMOTE` / `BOTH` / `UNKNOWN` still match.
  - `xcode_version_flag.find("-")` and the slice that builds `requires-xcode-label:` count Unicode scalars. Bazel `String.indexOf` / `substring` count UTF-16 code units, so a non-BMP character before `-` changes the label.

### `src/main/starlark/docgen/cpp.bzl`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file republishes these C/C++ rules as `binary_rules` (`cc_binary`), `library_rules` (`cc_library`, `cc_import`, `cc_shared_library`, plus `cc_static_library` only when `hasattr(native, "cc_static_library")`), `test_rules` (`cc_test`), and `other_rules` (`fdo_prefetch_hints`, `fdo_profile`, `memprof_profile`, `propeller_optimize`, `cc_toolchain`).

### `src/main/starlark/docgen/java.bzl`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Build Encyclopedia entry point whose module docstring is the rule family `Java` and that re-exports `java_binary` (`binary_rules`), `java_import` and `java_library` (`library_rules`), `java_test` (`test_rules`), and `java_package_configuration`, `java_plugin`, `java_runtime`, `java_toolchain`, and `java_single_jar` (`other_rules`) from `@rules_java`. The only `java.bzl` files under `/workspace/zel-cursor` are the repository helper `java_import_external` (`/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/build_defs/repo/java.bzl` and its test fixture), and `java_single_jar` does not appear in the Zel crates.

### `src/main/starlark/docgen/objc.bzl`
- Bazel lines: 29
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Build Encyclopedia entry that loads `objc_library` and `objc_import` from `@cc_compatibility_proxy//:proxy.bzl` and exports them on `library_rules`, with empty `binary_rules`, `test_rules`, and `other_rules`.

### `src/main/starlark/docgen/proto.bzl`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Build Encyclopedia entry point that publishes the "Protocol Buffer" family as empty `binary_rules` and `test_rules`, `library_rules` (`proto_library`, `cc_proto_library`, `java_proto_library`, `java_lite_proto_library`, `py_proto_library`), and `other_rules` (`proto_toolchain`, `proto_lang_toolchain`).

### `src/main/starlark/docgen/python.bzl`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Nothing in Zel loads `py_binary`, `py_library`, `py_test`, and `py_runtime` and re-exports them as the Build Encyclopedia structs `binary_rules`, `library_rules`, `test_rules`, and `other_rules`.

### `src/main/starlark/docgen/sh.bzl`
- Bazel lines: 34
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Build Encyclopedia entry point publishes the `Shell` family or groups `sh_binary`, `sh_library`, and `sh_test` into `binary_rules`, `library_rules`, `test_rules`, and an empty `other_rules`. Zel loads those rules from vendored `@rules_shell` Starlark and does not register them as native rule classes.

### `src/main/starlark/tests/builtins_bzl/builtin_test_setup.sh`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel code copies `$TEST_SRCDIR/_main/$1` (or the first `RUNFILES_MANIFEST_FILE` hit, with the path truncated at `$1`) to `${2:-$1}`, exits 1 when that source is missing, skips the copy when the destination already exists, runs `chmod -R ugo+rwx`, renames every `*.builtin_test` by stripping that suffix, or appends `rules_testing`, `bazel_skylib`, `rules_python`, `platforms`, and `rules_shell` module deps.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/a_suffix.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines `int a_suffix()` returning `42` for the `cc_shared_library` test. Zel has no source that implements that function; the path appears only as a filename string in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/a_suffix.h`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file declares `a_suffix` or reproduces this cc_shared_library test header. The path is only named in a patch-corpus file list.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/bar.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file defines `int bar()` returning 42 or otherwise implements this `cc_shared_library` builtin-test translation unit. The path appears only as a filename in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/bar.h`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No file under `/workspace/zel-cursor` provides this test header’s declaration (`int bar();`). The path is only named in a patch-corpus file list, which does not implement it.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/bar2.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel source defines `bar2()` as a C++ function returning 42. The only hit is a path string in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/bar2.h`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no header or other source that declares `int bar2()` under this include guard. The path appears only as a filename string in a patch-corpus listing, which does not provide the declaration.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/bar3.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel source defines `bar3()` or otherwise reproduces this fixture (`int bar3() { return 42; }`). Other `bar3` hits are unrelated (a protobuf field, a runfiles mock path, and a path listing in a patch corpus).

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/bar3.h`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no counterpart that declares `int bar3()` under the `EXAMPLES_TEST_CC_SHARED_LIBRARY_BAR_3_H_` include guard. The path is only named in a patch-corpus file list, which does not provide the declaration.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/barX.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel source defines `barX` returning 42. The only mention is a path listing in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/barX.h`
- Bazel lines: 20
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No header or other source under `/workspace/zel-cursor` declares `int barX()`. The name appears only as a path string in a fixture listing, not as an implementation.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/baz.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file reimplements this `cc_shared_library` test fixture (`int baz()` returns 42 and includes `baz.h`). `third_party/bcr/apple_support/2.5.2/test/rpaths/baz.cc` is a separate fixture with the same function body.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/baz.h`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file provides this header’s `int baz();` declaration for the `cc_shared_library` builtin test.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/cc_shared_library_integration_test.sh`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file skips with exit 0 when the first argument is `non_linux`, then checks `libfoo_so.so` with `nm` (present `U _Z3barv`, `T _Z3bazv`, `T _Z3foov`, `t _Z3quxv`, `t _Z12indirect_depv`, `t _Z13indirect_dep2v`; absent `_Z13indirect_dep3v` and `_Z4bar3v`), requires `objdump` RUNPATH to contain `kittens`, runs `binary` and `cc_test` expecting dynamic `U _Z3foov` and stdout `hello 42`, and fails unless `readelf -d` `NEEDED` count equals `EXPECTED_NUM_LIBS`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/direct_so_file_cc_lib.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Rust counterpart defines `direct_so_file_cc_lib()` (the C function that returns 0). The path appears only as a filename string in a patch-corpus listing.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/direct_so_file_cc_lib.h`
- Bazel lines: 20
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file declares `direct_so_file_cc_lib` (the header’s only interface: an `int`-returning function). The name appears only as a path string in a Bazel file list under the patch-corpus fixture, not as a C or Rust implementation.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/direct_so_file_cc_lib2.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file defines `direct_so_file_cc_lib2()`, which returns `0`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/direct_so_file_cc_lib2.h`
- Bazel lines: 20
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file declares `direct_so_file_cc_lib2` or vendors this test header. The header’s only behavior is the C declaration `int direct_so_file_cc_lib2();` (defined in the sibling `.cc` to return 0).

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/failing_targets/a.cc`
- Bazel lines: 13
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel `a.cc` is an empty translation unit (license comments only, no symbols or statements) used as `srcs` of `cc_library` `a`. Nothing under `/workspace/zel-cursor` provides that source or the `failing_targets` graph that compiles it.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/failing_targets/b.cc`
- Bazel lines: 13
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file ports this empty translation unit, the sole source of `cc_library` `//src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/failing_targets:b` (linked by `b_so` and `b2_so`).

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/failing_targets/c.cc`
- Bazel lines: 13
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Comment-only translation unit (no symbols or executable code), used as `cc_library` `c` `srcs` in the failing-targets test. No Zel file under `/workspace/zel-cursor` copies this fixture or an equivalent empty `c.cc`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/failing_targets/empty.cc`
- Bazel lines: 13
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file reimplements this license-only, empty translation unit. `BUILD.builtin_test` does not list it as a source, and the only `empty.cc` files under `/workspace/zel-cursor` are unrelated (`third_party/bcr/rules_cc/0.2.17/cc/private/toolchain/empty.cc` defines `int main() {}`).

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/failing_targets/main.cc`
- Bazel lines: 13
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No file under `/workspace/zel-cursor` implements this translation unit. The Bazel file is only the Apache license header (no functions, I/O, or exit behavior) and is used as `srcs` of `two_dynamic_deps_same_export_in_binary`; that failing-targets fixture is not present in Zel.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/foo.cc`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file implements `foo()` calling `diff_pkg`, `bar`, `baz`, `qux`, and on `IS_LINUX` `direct_so_file_cc_lib` and `direct_so_file_cc_lib2`, then returning 42.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/foo.h`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file declares `int foo()` under the `EXAMPLES_TEST_CC_SHARED_LIBRARY_FOO_H_` include guard. The only `foo.h` under `/workspace/zel-cursor` is the empty third-party file `/workspace/zel-cursor/third_party/bcr/rules_cc/0.2.17/tests/simple_import/foo.h`, and the Bazel path appears only as an inventory line in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/indirect_dep.cc`
- Bazel lines: 15
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no translation unit that defines `indirect_dep()` and returns `0`. Starlark `link_indirect_deps` in the `cc_shared_library` rules is a different feature and does not provide this symbol.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/indirect_dep2.cc`
- Bazel lines: 15
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no source that defines `indirect_dep2()` (the C++ test fixture that returns `0`). `cc_shared_library.bzl` does not supply this symbol.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/indirect_dep3.cc`
- Bazel lines: 15
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no translation unit that defines `indirect_dep3` returning `0`. The only hit is a path listing in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/l.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file defines the linkstamp symbol `my_linkstamp` as the C string `"0"` that Bazel’s `qux` `cc_library` passes via `linkstamp = "l.cc"`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/main.cc`
- Bazel lines: 21
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel source implements this test program (`std::cout << "hello " << foo() << std::endl`, exit 0). The path appears only as a filename list entry in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/not_depended_on.cc`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file defines `foo()` that includes `bar.h`, calls `bar()`, and returns `42`. The path appears only in a Bazel file listing, not as a translation unit.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/python_test.py`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no test that resolves `FOO_SO` through runfiles, loads that shared library, and checks that the Windows export `?foo@@YAHXZ` or the Itanium export `_Z3foov` returns 42. Nearby `.dll` suffix checks only classify library names.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/quux.cc`
- Bazel lines: 14
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel source defines `quux()` returning 42. Matches are only the string `quux` in third-party fixtures and a vendored Bazel path list.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/qux.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file defines `int qux()` returning 42 or otherwise ports this `cc_shared_library` test fixture.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/qux.h`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No file under `/workspace/zel-cursor` declares `int qux()` or otherwise reproduces this `cc_shared_library` integration-test header.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/qux2.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines `int qux2()` returning `42`. No Zel file defines that symbol or any equivalent translation unit.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/qux2.h`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The header only declares `int qux2()`. No file under `/workspace/zel-cursor` declares or defines `qux2` or the `EXAMPLES_TEST_CC_SHARED_LIBRARY_QUX2_H_` guard. The path appears only as a listing in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/starlark_tests.bzl`
- Bazel lines: 431
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no reimplementation of these `cc_shared_library` analysis tests or fixture rules (`linking_order_test`, `additional_inputs_test`, `build_failure_test`, `paths_test` and its `__pkg__`/`__subpackages__` checks, `runfiles_test`, `interface_library_output_group_test`, `check_linking_action_lib_parameters_test`, `exports_test`, `pdb_test`, `wrapped_cc_lib`, `forwarding_cc_lib`, `forwarding_cc_lib_aspect`, `nocode_cc_lib`).

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test/testenv.sh`
- Bazel lines: 18
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file assigns `LDD_BINARY=ldd`, `RPATH=RUNPATH`, and `EXPECTED_NUM_LIBS=5` for the `cc_shared_library` integration test. The only `testenv.sh` (`/workspace/zel-cursor/crates/zel-bazel-tools/resources/tools/build_defs/pkg/testenv.sh`) is the pkg harness: it requires `TEST_SRCDIR` and `TEST_WORKSPACE` and sources `unittest.bash`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test2/bar.cc`
- Bazel lines: 13
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file reimplements this `cc_shared_library` test2 translation unit. The only `bar.cc` under `/workspace/zel-cursor` is the unrelated apple_support rpath fixture `/workspace/zel-cursor/third_party/bcr/apple_support/2.5.2/test/rpaths/bar.cc` (`int bar() { return 12; }`). The Bazel file is an empty translation unit (Apache license comments only), so there is no runtime behavior to match.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test2/bar.h`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No file under `/workspace/zel-cursor` provides this test header’s declaration (`int bar();`) or its include guard (`EXAMPLES_TEST_CC_SHARED2_LIBRARY_BAR_H_`). The path is only named in a patch-corpus file list, which does not implement it.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test3/bar.cc`
- Bazel lines: 13
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - License-only translation unit (no symbols) used as the sole `srcs` entry of `cc_library` `:bar`. No Zel file reproduces that empty unit. The path is only listed in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`. Other `bar` sources define a function (`third_party/bcr/rules_cc/0.2.17/examples/my_c_archive/bar.c` returns `-42`; `third_party/bcr/apple_support/2.5.2/test/rpaths/bar.cc` returns `12`).

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test3/bar.h`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No file under `/workspace/zel-cursor` declares `int bar()` for this `cc_shared_library` test3 header (`EXAMPLES_TEST_CC_SHARED_LIBRARY3_BAR_H_`). The path appears only in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`. Vendored `int bar()` definitions in `rules_cc` (`return -42`) and `apple_support` (`return 12`) are separate fixtures.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test3/diff_pkg.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel defines `int diff_pkg()` returning `42` for the `cc_shared_library` test. Zel has no source that implements that function; the path appears only as a filename string in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test3/diff_pkg.h`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No file under `/workspace/zel-cursor` declares `int diff_pkg()`. The path is only listed in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test4/bar.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel-cursor source implements this fixture. Bazel defines `int bar()` returning `42`; the only `bar.cc` under zel-cursor is an unrelated third-party rpath test that returns `12`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test4/bar.h`
- Bazel lines: 20
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No file under `/workspace/zel-cursor` declares `int bar();` for this `cc_shared_library` test4 header (include guard `EXAMPLES_TEST_CC_SHARED_LIBRARY4_BAR_H_`). The path is only listed in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`. Other `int bar()` declarations (`third_party/bcr/rules_cc/0.2.17/examples/my_c_archive/main.c`, `third_party/bcr/apple_support/2.5.2/test/rpaths/test.cc`) belong to unrelated fixtures.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test4/main.cc`
- Bazel lines: 21
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel `main` prints `hello ` plus `bar()` (`42`) and a newline, then returns `0`. No Zel source implements that program; the path is only listed in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test5/bar.cc`
- Bazel lines: 14
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel’s translation unit defines `int bar()` and returns `42` as the `cc_library` `//…/cc_shared_library/test5:bar` source linked into `bar_so`. No Zel source defines that symbol or reproduces this fixture. The path is only listed in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`. Nearby `bar` files are different fixtures (`third_party/bcr/apple_support/2.5.2/test/rpaths/bar.cc` returns `12`; `third_party/bcr/rules_cc/0.2.17/examples/my_c_archive/bar.c` returns `-42`).

### `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test5/main.cc`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel `main` writes `hello, world!` plus a newline to stdout and returns 0. No Zel source implements that program. The path appears only as a filename in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/bar.cc`
- Bazel lines: 20
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file implements this fixture: `bar()` returns `2 * foo()` and `unused()` returns `0`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/bar.h`
- Bazel lines: 20
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No file under `/workspace/zel-cursor` declares `int bar();` and `int unused();` behind `EXAMPLES_TEST_CC_STATIC_LIBRARY_BAR_H_`. The path is only named in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`, which does not implement the header.

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/cc_static_library_integration_test.sh`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file reimplements this check: find `libstatic.a`, run `nm -C`, require text symbols `T foo`, `T bar`, and `T unused`, and exit 1 if `lib_only` is present.

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/foo.cc`
- Bazel lines: 16
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file is the sole source of `cc_library` `//src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test:foo`, including `foo.h` and defining `foo()` so it returns 42 (`bar()` is `2 * foo()`, and `//…/test:test` requires `bar() == 84`).

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/foo.h`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file provides this header’s include-guarded `int foo();` declaration. The only `foo.h` under `/workspace/zel-cursor` is the empty third-party file `/workspace/zel-cursor/third_party/bcr/rules_cc/0.2.17/tests/simple_import/foo.h`, and this Bazel path appears only as an inventory line in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/lib_only.cc`
- Bazel lines: 15
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No file under `/workspace/zel-cursor` defines an exported `lib_only` that returns `-1` for the `cc_static_library` symbol test. The only hit is a path listing in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/mock_toolchain.bzl`
- Bazel lines: 158
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel rule or macro builds this test `CcToolchainConfigInfo`: optional `validate_static_library` action (`validate_static_library.sh`), enabled `cpp_link_static_library` tool `/usr/bin/my-ar`, `archiver_flags` (`MY_KEY=my_value`, `abc`, `/MY_OUT:%{output_execpath}` when `output_execpath` is available), `symbol_check` (`--check-symbols`, implying `validate_static_library` only when that attr is true), `static_library` artifact prefix `prefix` and extension `.lib`, plus the `mock_cc_toolchain` macro’s empty file, filegroups, `cc_toolchain` file sets, and `toolchain` registration (`provide_validate_static_library` default true).

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/nested/nested_bar.h`
- Bazel lines: 18
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file provides this `cc_import` header fixture: include guard `EXAMPLES_TEST_CC_STATIC_LIBRARY_BAR_H_` and the declarations `int bar();` and `int unused();` used with `includes = ["nested"]`.

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/nested/stripped_bar.h`
- Bazel lines: 20
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file provides this header’s include guard `EXAMPLES_TEST_CC_STATIC_LIBRARY_BAR_H_` or the `int bar()` and `int unused()` declarations. The path appears only as a filename in a patch-corpus listing.

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/starlark_tests.bzl`
- Bazel lines: 243
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel test checks `cc_static_library` analysis behavior: default output `prefix<name>.lib`, `CppTransitiveArchive` argv/env (`MY_KEY`, `PATH` overlay, `/usr/bin/my-ar`, `abc`, `.o` inputs, `--check-symbols`), ordered `linkdeps`/`linkopts` files (duplicate linkopts kept, trailing blank line), and `ValidateStaticLibrary` only when the toolchain provides it (`_validation` uses `short_path`).

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/test.cc`
- Bazel lines: 21
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel source implements this fixture: `main` returns 1 when `bar()` is not 84 and 0 otherwise. The path appears only as a filename in a Bazel file list, not as code.

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/test_static_import_includes.cc`
- Bazel lines: 19
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file ports this `cc_test` `test_static_import_includes` source. It includes `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/nested/nested_bar.h` (the header exposed by `cc_import` `static_import_includes` with `includes = ["nested"]`) and returns exit code 1 when `bar()` is not 84, otherwise 0.

### `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/test_static_import_strip_include_prefix.cc`
- Bazel lines: 21
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file compiles an include of `src/main/starlark/tests/builtins_bzl/cc/cc_static_library/test/nested/stripped_bar.h`, calls `bar()`, and exits 1 when the result is not 84 (exit 0 when it is). The path appears only as a listing in `/workspace/zel-cursor/crates/zel-bzlmod/tests/fixtures/patch_corpus/symlink_dir_escape/before/lnk/bazel_code_files.txt`.

### `src/main/starlark/tests/builtins_bzl/cc_builtin_tests.sh`
- Bazel lines: 199
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Zel file runs the cc builtins suite: it copies `src/conditions/BUILD`, adds `rules_cc` and protobuf, overrides module `test_repo` to `src/main/starlark/tests/builtins_bzl/cc/cc_shared_library/test2`, sets `--output_user_root=C:/tmp` on Windows, then requires `bazel test --define=is_bazel=true --test_output=streamed //src/main/starlark/tests/builtins_bzl/cc/...` to succeed.
  - No Zel file fails `cc_static_library` `//pkg:static` when `foo` is defined in both `direct1.cc` and transitive `indirect.cc`, then requires Windows logs `direct1.obj`, `indirect.obj`, and ` foo(`, Darwin `Duplicate symbols found in .*/pkg/libstatic.a:` plus `direct1.o: T foo()` and `indirect.o: T foo()`, and otherwise the same header plus `direct1.pic.o: T foo()` and `indirect.pic.o: T foo()`. A rebuild with `--features=-symbol_check` must succeed.
  - No Zel file fails the same graph when `direct1.cc` has an uninitialized `int foo` and `indirect.cc` has `int foo = 21`, requiring Windows ` foo`, Darwin `direct1.o: S _foo` and `indirect.o: D _foo`, and otherwise `direct1.pic.o: B foo` and `indirect.pic.o: D foo`, then succeeds with `--features=-symbol_check`.
  - No Zel file requires `cc_static_library` `//pkg:protobuf` with `deps = ["@com_google_protobuf//:protobuf"]` to build successfully (`|| fail`; a failed build fails the suite). Missing runfiles or setup scripts exit 1.

### `src/main/starlark/tests/builtins_bzl/objc/dotted_version.bzl`
- Bazel lines: 247
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/natives.rs`
- Verdict: partial
- Differences:
  - A component with an empty alpha sequence sorts before the same leading integer plus a suffix in the Starlark file (`"" < "beta"`, so `1.5` < `1.5alpha`). `compare` treats an empty suffix as greater, so `1.5` > `1.5alpha`.
  - The Starlark file parses a trailing integer after the alphanumeric run and compares it as an arbitrary-precision int, with a missing value equal to 0 (`1.5gamma5` < `1.5gamma29`, `1.5alpha` == `1.5alpha0`, `1.5beta02` == `1.5beta2`). Rust keeps the whole remainder as one string, which reverses `gamma5`/`gamma29` and makes the zero and leading-zero cases unequal.
  - A component whose first character is an ASCII letter, and every component after it, is dropped (`1.5.decription` == `1.5`, `1.5.decription.6.7` == `1.5`). Rust keeps those pieces as numeric 0 plus a non-empty suffix, so the shorter version sorts greater.
  - Construction fails in Starlark on an empty string, an empty component (`1..2`, `1.`), a non-alphanumeric middle (`1.2a-b`), or a component that neither starts with a digit nor is a letter-led stop. A letter-only string (`beta`) succeeds and compares equal to `0`. Rust rejects only an empty string or a string whose first character is not an ASCII digit, so `1..2` is accepted and `beta` errors. `None` or a missing argument returns `None` in Rust and fails in Starlark.
  - Component integers are unbounded Starlark ints. Rust parses with `u64` and maps overflow to 0, so a component above `u64::MAX` compares equal to 0.
  - `isdigit` is `CharMatcher.javaDigit()` (Unicode decimal digits). A component of non-ASCII digits then fails in `int()`, which accepts only ASCII. Rust accepts `1.١٢` and compares that component as numeric 0 plus the original suffix.

### `src/main/starlark/tests/builtins_bzl/objc/dotted_version_test.bzl`
- Bazel lines: 128
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/natives.rs`
- Verdict: partial
- Differences:
  - `1.5gamma5` compares less than `1.5gamma29` because the trailing values are integers 5 and 29. Zel’s `compare` (and the same split in `dotted_version_cmp` at `/workspace/zel-cursor/crates/zel-starlark/src/eval/builtins/universal.rs`) orders the suffixes as strings, and `"gamma5"` sorts after `"gamma29"`, so `compare_to` returns a positive value.
  - A component that starts with a letter ends the version: `1.5.decription` and `1.5.decription.6.7` both compare equal to `1.5`. Zel keeps that component as number 0 with a non-empty suffix and keeps any later components, so both compare less than `1.5`.
