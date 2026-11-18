# Bazel vs zel code differences: repositories bzlmod

Category rule: Bzlmod, repository rules, external dependency resolution, fetching, and repository cache behavior.

- Failure entries: 163
- Missing: 11
- Partial: 152
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ArchiveRepoSpecBuilder.java`
- Bazel lines: 114
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/registry.rs`
- Verdict: partial
- Differences:
  - Zel has no builder. The archive arm always writes the URL, integrity, strip, patch, and overlay attributes, then `type` only when `archive_type` is non-empty. A builder call that skips a setter omits that attribute. Zel cannot.
  - `HTTP_ARCHIVE` is `@@bazel_tools//tools/build_defs/repo:http.bzl`. Zel stores a single `@`. The lockfile id and rule load match after add-or-strip, but the stored `bzl_file` string differs.
  - `setRemotePatchStrip` stores a 32-bit `StarlarkInt`. Zel uses `as_i64().unwrap_or(0)`. JSON `null`, a bool, a string, or `1.5` fails Gson before the builder runs. Zel writes 0. Values outside `i32` but inside `i64` fail Gson and are kept in Zel. Integers outside `i64` become 0 in Zel and fail the Bazel parse.
  - Integrity strings, including `""`, are stored as given. Zel rejects trim-empty module, patch, and overlay integrity.
  - Attribute strings are Java UTF-16. A lone JSON surrogate stays a surrogate. Zel maps an unpaired surrogate to U+FFFD. Duplicate JSON keys collapse to one entry in Gson and are kept as separate pairs in Zel.
  - `setPatches` writes a list of labels. Bazel main never calls it. Zel's archive spec has no `patches` attribute.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/AttributeValues.java`
- Bazel lines: 40
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - Bazel stores the dict reference and returns it, with no copy. Zel snapshots into an owned vector, so later mutation of the kwargs dict is visible only on the Java side.
  - Values stay Starlark objects. `AttrVal` keeps only `i64` (any other int becomes a decimal string), stores tuples as lists, stores a label's canonical form and rebuilds a string, and turns every other value into a string.
  - AutoValue equality is map equality: insertion order does not matter, a Starlark int equals a numerically equal float, and NaNs compare equal. `AttrVal` equality is order-sensitive, type-strict, and uses `f64` equality (NaN ≠ NaN).
  - Nested dict keys stay Starlark values. Zel stringifies each key, so keys that share a display form collapse, and int `1` and float `1.0` are no longer the same key.
  - Keys missing from call order are appended from a `BTreeMap`, so they come out sorted. A Starlark dict iterates in insertion order.
  - `toString()` is the Starlark repr. Zel's rendering uses Rust debug (`inf` / `NaN` instead of `+inf` / `nan`) and escapes only a short set of characters.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/AttributeValuesAdapter.java`
- Bazel lines: 191
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - Bazel writes a label's unambiguous canonical form (`@@//pkg:name`) and parses it back. Zel stores `canonical_form()` (`//pkg:name`), so read-back is a plain string. Any `@@` string is accepted with no syntax check. The usages digest also quote-wraps labels.
  - Label dict keys are written without extra quotes and parsed back to labels. Zel keys are always strings, and `from_json` never rebuilds a label key, so `@@//foo:bar` is rewritten as a quoted string.
  - Bazel ints allow only signed 32-bit and throw otherwise. Gson truncates values that fit in a long. Zel keeps `i64`, stringifies bigger ints, and reads non-`i64` text as `f64`.
  - Bazel throws on floats. Zel emits Rust debug from `to_json` and Rust display from the usages digest.
  - Bazel applies internal-to-Unicode transcoding to values, dict keys, and labels after the `@@` and quote checks. Zel strings are Unicode scalars with no transcoding. A one-character `'` throws in Java. Zel returns `"'"`.
  - Duplicate JSON keys collapse to one entry in Gson and are kept as separate pairs in Zel. HTML escaping is off on both sides, and dict insertion order matches.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelDepGraphFunction.java`
- Bazel lines: 255
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs`
- Verdict: partial
- Differences:
  - A repo-relative extension label (`//…`, `:…`) is resolved in that module's canonical repo. `//conditions` and `//visibility` are forced to the main repo. `convert_label` looks up apparent repo `""`, which exists only for the root, and has no `conditions`/`visibility` rule. Innate `//:MODULE.bazel` is rewritten to `@@{canonical(repo_name)}//:MODULE.bazel`. Any other non-root repo-relative label fails with `no repo visible as '@'`.
  - `LabelConverter` rejects illegal labels. `convert_label` only checks that an apparent repo is in the mapping. Package, target, and repo syntax are not rechecked, so those strings become extension ids.
  - Two usages in one module that convert to the same extension id make the table builder throw. Zel keeps every usage. Unique-name assignment dedups the id, and root repo overrides for the same exported name keep the later entry.
  - `bazel_tools` and `platforms` always use the fixed canonical names. Two versions of either make the bimap throw on a duplicate key. Zel's ordered map overwrites that name, so the earlier version has no canonical repo.
  - `resolveRepoOverrides` always records the mapping lookup. A name absent from the resolved root mapping is a non-visible repository name. Zel inserts the override only when that apparent name is present, so the generated repo stays `{unique}+{exported}`.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelDepGraphValue.java`
- Bazel lines: 160
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs`
- Verdict: partial
- Differences:
  - On a valid graph, the ordered repo mapping matches: extension imports in usage order, then root `""` to `""`, the module's own repo name, then bazel deps. Duplicate apparent names throw in Java. Zel overwrites with the later entry.
  - A canonical name shared by two modules is rejected. Multiple versions of `platforms` or `bazel_tools` abort in Bazel. The ordered map keeps one module and drops the other. The inverse lookup is a linear first match.
  - A missing module, a missing extension unique name, or a dep with no canonical name throws in Java. Zel returns an empty mapping, skips that usage, or omits that dep.
  - The lookup map is a `BTreeMap` sorted by apparent name. Insertion order, which the inverse uses, exists only on the ordered vec.
  - An unknown apparent name becomes a non-visible repository name with a did-you-mean suffix. Zel returns a visible unvalidated name, and the empty-name lookup returns the context repo without reading the map, so `@` in a non-root module resolves to that module.
  - An override whose apparent name is absent from the root mapping is stored as that non-visible name in Bazel and omitted in Zel. Abridged modules are the full dep graph in BFS order. Zel does not store that list. `Resolution` is not a sky value and has no `BAZEL_DEP_GRAPH` key. Extension unique names and repo overrides are sorted maps, so iteration does not match insertion order.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelFetchAllFunction.java`
- Bazel lines: 112
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/session.rs`
- Verdict: partial
- Differences:
  - Bazel returns the repos whose fetch succeeded and whose `excludeFromVendoring` is false. `run_fetch` only prints success. Vendoring uses a separate, narrower filter.
  - With a vendor directory, Bazel drops a repo when the rule is local or configure, when `VENDOR.bazel` ignores or pins it, and for every repo override. Zel skips only the name `bazel_tools`, ignore and pin entries, and a narrower local-repo check. A `repository_rule(local=True)` under any other name, a configure rule, and an apparent-name override are vendored by Zel and omitted by Bazel.
  - A pinned repo missing from the vendor directory fails the Bazel fetch. Zel skips that name and the vendor command still succeeds.
  - Ignored repos are still fetched into the external cache in Bazel, then left off the returned list. Zel does not fetch them.
  - `fetch --configure` keeps a found repo whose rule is configure, and a failed repo definition fails the node. Zel returns false when the `.bzl` path contains `bazel_tools` or when loading that file fails, so those repos are skipped and the command can still succeed. Configure is stored with Rust truthiness rather than a Starlark boolean.
  - A repository-directory failure is omitted and this function still returns. A Zel fetch error aborts the command. Bazel requests every extension and every repo in one batch. Zel stops at the first sequential error, so later repos are not fetched.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelFetchAllValue.java`
- Bazel lines: 79
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/session.rs`
- Verdict: partial
- Differences:
  - No sky value or interned `BAZEL_FETCH_ALL` key. This type is a hermetic value whose only field is an ordered repository-name list, keyed by a boolean (configure-only versus all). `bazel fetch` ignores the list. `bazel vendor` copies exactly that list. Zel rebuilds its own name vector.
  - With `--vendor_dir` set, the stored list is fetch successes whose `excludeFromVendoring` is false: not `local()`, not `configure()`, and not ignored or pinned. Zel drops the literal name `bazel_tools`, ignore and pin, and local repos whose `.bzl` path contains `bazel_tools`. Configure repos and any other `local=True` rule stay in zel's vendored set. `bazel_tools` is always omitted by zel even when this list would keep it.
  - With no vendor directory, a successful fetch-all value has an empty list. Zel never materializes that empty value.
  - `key(true)` limits what gets fetched before the list is built. Zel's configure flag exists only inside `run_fetch`. A failed bzl eval is treated as not-configure. A missing repo definition fails the sky function, so this value is not produced.
  - The producer appends names by iterating a set of sky keys in hash order. Zel's vector follows insertion order. Equality and which repo is copied first can differ.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelLockFileFunction.java`
- Bazel lines: 185
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/lockfile.rs`
- Verdict: partial
- Differences:
  - A missing workspace lockfile is an empty lockfile on both sides. Any other read failure (permissions, a directory, invalid UTF-8) is a `BAD_LOCKFILE` error in Bazel and an empty lockfile in Zel.
  - Bazel treats the text as version 28 only when `"lockFileVersion":` is followed immediately by optional Java whitespace and ASCII digits, then `Integer.parseInt`. A legal space before the colon is an unsupported version and the body is never parsed. A digit string above `Integer.MAX_VALUE` is a parse failure except for the hidden lockfile, which becomes empty. Zel allows Unicode whitespace around the colon, parses an `i64`, and a value other than 28 is an empty lockfile or the "not supported" error.
  - Bazel parses with lenient Gson (comments, single quotes, unquoted names, `NaN`/`Infinity`). Zel accepts only strict JSON.
  - After a version match, a null `repoRuleId` is an empty lockfile in Bazel (`update`/`refresh`) or the "not supported" error. Zel always returns a parse error. Omitted hash, yanked-version, and facts maps fail Bazel's non-null build and are empty maps in Zel.
  - Registry hashes in Bazel are 64 ASCII hex digits (SHA-256 only, no trim). Zel trims, accepts SRI first, then SHA-256 hex, and also accepts URL-safe base64 and missing padding.
  - `repoRuleId` splits on the first `%` in Bazel and the last `%` in Zel. A string with no `%` is kept in Bazel and rejected in Zel. Yanked-version keys split on every `@` in Bazel. Zel uses `split_once`, so a later `@` is part of the version.
  - Repo attributes that are JSON numbers become a Java `int` (fractional parts and bits above 32 are truncated). Zel keeps an `i64` when it fits, otherwise an `f64`. `factsVersions` entries that are not Java ints fail the Bazel file. Zel drops non-`i64` entries and keeps values above `Integer.MAX_VALUE`.
  - Unpaired `\u` surrogates become `?` after Bazel's internal conversion. Zel combines a high surrogate with the next escape even when it is not a low surrogate, and uses U+FFFD otherwise. Duplicate object keys: Gson keeps the last value. Zel's second version check keeps the first. Map iteration is JSON order in Bazel and sorted order in Zel.
  - Merge-conflict hint text looks for `<<<<<<<`, `=======`, `|||||||`, or `>>>>>>>` in Bazel. Zel scans only for `<<<<<<<` or `>>>>>>>`. Hidden-lockfile parse failures become an empty lockfile on both sides.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelLockFileModule.java`
- Bazel lines: 357
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`
- Verdict: partial
- Differences:
  - Bazel drops old factor entries once a new result exists unless the os/arch dependence is unchanged. Zel keeps every old factor map for a still-used extension and only overwrites the current factor string, so changing `os_dependent` or `arch_dependent` leaves stale entries.
  - The hidden lockfile stores only reproducible extension results, empty yanked versions, no registry hashes, and facts taken from the previous hidden file. Zel copies remote registry hashes into the hidden file and writes no facts there.
  - Bazel keeps a facts entry only when the extension still has a usage and the value is not empty, and keeps a facts version only when that entry exists and the version is non-zero. Zel copies the workspace maps through unchanged, so removed extensions and empty facts leave the old entry in place.
  - Bazel merges every done extension still in the usage table, including warm-server results this command did not run, and skips the update when package loading did not sync. Zel merges only extensions evaluated in this session.
  - Bazel writes a file only when the new value is not equal to the old one, and logs I/O errors without failing the command. Zel rewrites the workspace file whenever the UTF-8 text differs, skips creating a missing empty workspace file, always rewrites the hidden file, and either drops the workspace I/O error or reports it as a warning.
  - Extension and facts maps are ordered by label, extension name, then isolation key. Zel sorts the serialized key strings, so `//a/b:a.bzl` sorts before `//a:b.bzl`, and isolated versions sort as text. Yanked versions stay in dep-graph insertion order in Bazel and are sorted by name and version in Zel. Java string order is UTF-16. Rust map order is UTF-8 bytes.
  - `facts_version` is a Java `Integer`. Zel uses `u32`, so a negative becomes 0 and a value above `u32::MAX` is truncated.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelLockFileValue.java`
- Bazel lines: 180
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/lockfile.rs`
- Verdict: partial
- Differences:
  - The empty value matches: version 28 and empty registry hashes, yanked versions, module extensions, facts, and facts versions.
  - Registry hashes are an optional checksum in Java. Equality includes the algorithm and the digest, so SHA-256 and BLAKE3 with the same 32-byte digest are different. Zel stores hex only, so those two compare equal.
  - Facts versions are Java `int`. Zel stores `i64`, so a version outside ±2³¹−1 can be held in Rust and cannot be stored in this Java field.
  - Module-extension maps are keyed by extension id and eval factors. Zel keys both levels with strings, so distinct ids that share a printed form collide, and os and arch are not separate fields.
  - Facts are a typed value in Java and raw JSON in Zel, so Starlark int, float, and `None` do not match JSON number and null under equality.
  - Both maps compare equal independent of order. Iteration does not: Guava maps are insertion-ordered and `BTreeMap` is sorted. Java string order is UTF-16. Rust order is by scalar values, so non-BMP keys sort differently.
  - `KEY` and `HIDDEN_KEY` are two identity-distinct sky keys. `Lockfile` has no sky key. Workspace and hidden lockfiles are two values in the runtime.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelModTidyFunction.java`
- Bazel lines: 114
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/tidy.rs`
- Verdict: partial
- Differences:
  - Bazel resolves `@buildozer_binary//:buildozer.exe` and returns that path on the tidy value. A missing package lookup restarts Skyframe. A package that does not exist becomes `IllegalStateException`. Zel never looks up buildozer and edits `use_repo` in process.
  - The sky value also carries the root module file plus every include. Zel has no such set, so included module files with no `use_repo` edit are left untouched. Bazel later runs buildozer `format` on each path.
  - Root-module extensions are requested with an eval key that skips import and inject checks and does not post the fixup warning. Zel skips the import check, but still logs the warning, and `run_tidy` logs it again. `mod tidy` does not print that warning in Bazel.
  - An external-deps exception is stored, posted as an error with no location, and the function still returns both fixups and errors. Zel keeps one error string per extension, writes the successful edits, then returns exit 2 and skips the info lines.
  - Innate `use_repo_rule` extensions used by the root are evaluated. Zel skips innate ids, so a failing innate extension fails Bazel tidy and is ignored by zel.
  - Any missing skyframe dep returns null and drops errors collected in that attempt. Zel has no restart. Resolution failures abort before tidy.
  - A successful value that is not a single-extension value is dropped with no error. Zel treats every successful eval as an extension result.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelModTidyValue.java`
- Bazel lines: 60
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/tidy.rs`
- Verdict: partial
- Differences:
  - Zel has no sky value and no `BAZEL_MOD_TIDY` key. `run_tidy` keeps fixups and errors, writes the files, and returns. Nothing is retained for a later step.
  - The buildozer path is absent, and buildozer is never run. Bazel feeds that binary ISO-8859-1 stdin, one line per module file, including files with no fixups, and treats exit 3 as success. Zel rewrites `use_repo` itself, so format-only includes stay untouched. Code points above U+00FF stay UTF-8. Bazel's ISO-8859-1 encode replaces them with `?`. A non-UTF-8 `MODULE.bazel` fails in Zel.
  - Module file paths are not part of the tidy result. Changed files are applied from a `BTreeMap` (sorted path order). Bazel appends commands in fixup-list order and walks the set in insertion order. The Java set collapses duplicate paths. The Rust vec does not.
  - Errors stay on a successful value. After the edits, Bazel reports each one and then fails with a count. Zel joins the raw strings, returns exit code 2, and prefixes a single `ERROR:`. If any error is present, Zel returns before the warning and info lines, so those messages are dropped.
  - Repo names in each Zel command come from a `BTreeSet`, so they are sorted. Bazel stores the command strings already built, in list order.
  - Zel skips innate ids before recording a fixup. The Bazel value includes a fixup for every root extension usage whose eval value has one, innate usages included.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelModuleInspectorFunction.java`
- Bazel lines: 214
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/inspector.rs`
- Verdict: partial
- Differences:
  - The happy-path augmented graph matches: each unpruned module is marked loaded, original and resolved children are linked, a used parent is recorded as a dependant only when its key is in the resolved graph, and the reason follows the override on the resolved module name. Version equality is normalized-string equality on both sides.
  - Bazel builds the augmented map in a hash map and groups the modules index from that iteration order. Zel stores the dep graph in a `BTreeMap` and sorts each index entry by module key. That changes the order of versions in "available versions" errors and the order `<module>` expands to.
  - A missing original-dep entry throws in Java. Zel substitutes the resolved key, so the edge stays original, no unused dep is recorded, and the requested version never becomes its own node. A resolved key that is not in the unpruned graph throws in Java. Zel still inserts a node and sets `loaded`.
  - Deps are a bimap. Two repo names that resolve to one module key throw. Zel keeps both names. The all-deps view then collapses them and the last insert wins.
  - Extension failures are recorded, posted immediately, and left on the inspector value. A null extension value restarts the sky function. Zel only drops an extension when generated repos are absent. Strings are stored off to the side, deduped, and printed only after `run_mod` succeeds, so identical messages change the failure count and a later argument error hides the extension errors Bazel already emitted.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelModuleInspectorValue.java`
- Bazel lines: 237
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/inspector.rs`
- Verdict: partial
- Differences:
  - `errors` is an immutable list of `ExternalDepsException` on the sky value. `Inspector` has no errors field. A failed extension is a string on the runtime provider and is left out of `extension_repos`, so the value itself cannot report which extensions failed.
  - Extension repos are one multimap of unique internal names in insertion order. Zel stores a vec of pairs, which can repeat an id and a name. `repos_of` flattens matches into a `BTreeSet`, so names come out sorted and de-duplicated.
  - Canonical names are a map of module key to repository name. Zel stores a vec of bare name strings in resolution order. `canonical_name` returns the first pair, so a repeated key does not overwrite the way a map does. `RepositoryName.toString()` is `@@` plus that name.
  - The dep graph iteration order is explicitly non-deterministic. Zel's `dep_graph` is a `BTreeMap` ordered by module key, which matches the lexicographic comparator for legal names. Anything that walks the map without sorting sees a different node order.
  - The modules index maps a name to a set of keys. Zel maps to a vec sorted by module key. The same keys are present. The order of versions for one name is not the set's insertion order.
  - Deps and unused deps are bimaps. A repeated repo name or module key throws. Zel stores vecs, overwrites the same repo name, and allows two names to share one key. On a repeated key, Rust keeps the later name.
  - Dependants are sets. Zel uses vecs. Both builders drop a duplicate key and keep insertion order. `isUsed` agrees: the root key or a non-empty dependant set.
  - Resolution-reason labels match. Zel also carries extension usages and unique names. In Bazel those tables live on the dep-graph value, not on this sky value. There is no inspection sky key. The graph is built on demand.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelModuleResolutionFunction.java`
- Bazel lines: 397
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs`
- Verdict: partial
- Differences:
  - Check order differs, so the reported failure differs when more than one check fails. Bazel runs the direct-dep check, then Bazel compatibility, then yanked versions. Zel runs yanked versions, then direct deps, then compatibility.
  - Yanked status is not lockfile-stable on http(s) registries. Under update, Bazel reuses a stored yank reason and treats a module whose `source.json` hash is already in the lockfile as not yanked. Error mode does the same and otherwise skips the network. Zel always fetches `metadata.json` with checksums off, so a later yank fails a previously green build and a cleared yank disappears.
  - A registry I/O failure while reading yanked metadata fails open in Bazel. Zel turns that error into a resolution failure. An unparsable version key is the same fail-open warning in Bazel and a hard error in Zel. When several selected modules are yanked, Bazel reports the first offender in resolved-graph order for lockfile hits, then hash order for downloads. Zel reports resolved-graph order.
  - `all` allows every yanked module on both sides. Bazel strips `+build` metadata and rejects bad names and versions. Zel compares the raw `name@version` string to the key's display. `foo@1.2.3+build` allows `foo@1.2.3` in Bazel and does not match in Zel. `Foo@1.0,all` errors in Bazel and is allow-all in Zel. A bad token exits 37 in Zel and is worded as the flag even when it came from the environment.
  - Compatibility uses Bazel's version rules: a `-pre` suffix sorts strictly below that release, any other suffix is ignored, and `==` is equality. Zel keeps every digit run, so a prerelease compares the other way and `==` and `~>` are ignored (the function returns true). An empty version info skips the check. Zel's default version is `9.3.2`.
  - These failures are `VERSION_RESOLUTION_ERROR` (exit 48) in Bazel. Zel reports those resolve errors as exit 37. The final dep graph stores only the registry repo spec. Zel copies a non-registry override's spec onto the module. A discovered root repo name missing from the resolved root deps throws in Java. Zel emits the direct-dep message with `got null`.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelModuleResolutionValue.java`
- Bazel lines: 67
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs`
- Verdict: partial
- Differences:
  - There is no sky value and no singleton sky key for `BAZEL_MODULE_RESOLUTION`. AutoValue equality on the four fields is what Skyframe uses to skip dependents. `Resolution` is not `PartialEq`, so that change check does not exist.
  - The resolved and unpruned graphs are insertion-ordered maps (BFS for the pruned graph, discovery order for the unpruned graph). Zel's ordered map keeps the same insertion order, and both unpruned graphs are the full discovery graph after dep-key rewrite. The Rust maps are public and mutable after `resolve` returns.
  - `Resolution::empty` puts only the root in `dep_graph` and leaves `unpruned_dep_graph` empty. A Bazel resolution value always has the root in both graphs.
  - Registry file hashes are a `BTreeMap` sorted by URL. Bazel's map is insertion order. Digests match: both store sha256 of the bytes, and a missing file is empty. Bazel's lockfile re-sorts, so lockfile key order matches. In-memory iteration does not: vendor reports the first conflicting registry URL from insertion order.
  - Bazel stores a checksum (algorithm plus hash). Zel stores only the lowercase sha256 hex. The algorithm is not recoverable from the Rust map.
  - Selected yanked versions are a `BTreeMap` ordered by module name, then semver. Bazel's map keeps builder order: BFS of the resolved graph, then hash order for versions fetched from a registry. The lockfile writes that map without sorting, so two or more allowed yanked modules change key order. Values are the same yanked-reason strings, and modules with no registry are omitted in both.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/CompiledModuleFile.java`
- Bazel lines: 196
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/checks.rs`
- Verdict: partial
- Differences:
  - Valid top-level `include` calls are only shape-checked. Bazel records the decoded label, the literal `dev_dependency`, and the call start, and compiles every non-ignored include before execution. Zel reads the file only when the builtin runs, so `fail("x")` then a missing include fails on the missing include in Bazel and on `fail` in Zel.
  - `"a" + "b"` is one string literal before Bazel's checker, so `include("//:a" + ".MODULE.bazel")` is legal. Zel keeps a binary op and requires a string literal, so that call is rejected.
  - A bare `include = …` stops the check in both. Any other binding is an error in Bazel because the identifier is visited, and is ignored in Zel: tuple and list targets and comprehension `for` targets are not walked. `[0 for include in [1]]` is a syntax error in Bazel and is accepted in Zel.
  - On an assignment that is not a bare `include` name, Bazel visits the right-hand side first. Zel visits the index or attribute target first, so the two "called directly" errors come out in opposite orders.
  - Bazel checks UTF-8 under the enforcement flag (default warning) and always scans Latin-1 bytes. Invalid UTF-8 warns, or in error mode fails the read. Zel counts Unicode scalars. Module reads use lossy UTF-8, so invalid bytes become U+FFFD with no warning, and non-ASCII labels, string values, and columns differ from Latin-1.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/DelegateTypeAdapterFactory.java`
- Bazel lines: 147
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/json.rs`
- Verdict: partial
- Differences:
  - The adapter matches only an exact raw class (`ImmutableMap`, sorted map, bimap, `Dict`, list, set) that is a parameterized type. Zel has no such dispatch. A runtime sorted map stored in a declared map field still writes in the sorted map's iteration order and reads back as a plain insertion-order map.
  - Map and dict adapters preserve iteration order. Zel lockfile objects are `BTreeMap`s, so a round-trip re-sorts. Yanked versions are emitted in dep-graph order plus hash order. Zel orders by `(name, Version)`.
  - Extension ids and facts are sorted by label (repository, package, target), then extension name, then isolation key. Zel sorts the id string. `//a:b/c` sorts before `//a/b:a` in Bazel and after it as a string, because `/` is before `:`.
  - ASCII string keys that agree still diverge for non-BMP keys. Java `compareTo` orders a supplementary character by UTF-16 surrogates. Rust orders by UTF-8 bytes.
  - Reading a declared sorted map rebuilds a natural-order `TreeMap`. A custom comparator does not survive a round trip. Keys that are not `Comparable` throw. Zel always re-sorts with the key's `Ord`.
  - Gson throws on a duplicate key whose previous value was non-null. Zel keeps every duplicate pair, then the last value wins on insert. If a key adapter emits an array or object, Gson writes `[key, value]` pairs. Zel lockfile maps are always JSON objects of string keys.
  - Bimap reads throw when two keys share a value. Zel does not represent that bimap. The usages digest always emits `"proxies":[]`.
  - Sets write in insertion order and keep the first of any duplicate. Zel stores dep lists as vecs and preserves JSON duplicates, dropping null and non-strings. Gson throws on a null element. A null collection is JSON `null` in Bazel. Zel omits an absent dep list.
  - Dict reads reject Java null, double, and nested maps through Starlark checks. Lockfile facts and attribute dicts bypass this factory. Zel turns JSON null, numbers, arrays, and objects into Starlark `None`, int/float, list, and dict.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/Discovery.java`
- Bazel lines: 258
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs`
- Verdict: partial
- Differences:
  - The finished graph matches for the same root module and overrides. Both do multi-round breadth-first discovery, rewrite dep and nodep versions the same way, fetch a nodep only when that module name was already present, and drop nodep edges whose keys are still absent. Dedup order matches. Skyframe's null restart has no Rust equivalent and does not change a completed graph.
  - Dependency-chain text is gated differently. Bazel appends the chain only for `BAD_MODULE` and `MODULE_NOT_FOUND`, and rewrites the failure code to `BAD_MODULE`. Zel adds that prefix for every load error except a few registry and repository-path strings. A missing override `MODULE.bazel` gets the chain in Bazel and not in Zel. A lockfile checksum miss stays unchained in Bazel. Zel's missing-checksum message does not match the filter, so Zel adds the chain.
  - `bazel_tools` is not loaded from an override. Bazel treats a user non-registry override as the module to fetch. Zel always evaluates the embedded `bazel_tools` module file before the override branch, so a local-path or command override for `bazel_tools` does not change that node's deps or extensions.
  - Zel ends discovery with a check that fails when `--inject_module` names a module already present via original deps. This Bazel commit has no `--inject_module`, and discovery does not perform that check.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ExternalDepsException.java`
- Bazel lines: 77
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/session.rs`
- Verdict: partial
- Differences:
  - The Java type stores the specific `ExternalDeps.Code` on the failure detail. Zel keeps only an `i32`. Mapping that integer back picks the first schema code whose exit matches, so every 48 becomes `MODULE_NOT_FOUND`. `BAD_MODULE`, version resolution, bad lockfile, and extension errors are indistinguishable. Exit 37 is hardcoded to `CRASH_UNKNOWN`, not `EXTERNAL_DEPS_UNKNOWN`.
  - Session open classifies the resolve message with substring checks. Registry I/O is 32 only when main-mapping errors are set, otherwise 37. Lockfile read failures and many mapping errors are 48. Java would still exit 48 for bad module, version resolution, and bad lockfile, and 32 for registry access.
  - Cause factories keep the throwable and append `cause.getMessage()`. A null message makes protobuf `setMessage` throw. Zel has no cause field, so there is no `Caused by` chain.
  - The call-stack factory drops a builtin leaf frame and prefixes `Error in <name>:`, or prints a traceback. Source lines come from a UTF-8 file read. An empty stack throws. Zel's closest formatter always prints a `<toplevel>` frame and prefixes `{file}:{line}:{col}:`.
  - `String.format` is locale-sensitive (`%n` is the platform newline, a null `%s` becomes the text `null`). Zel builds messages with compile-time formatting.
  - On `mod`, `fetch`, and `vendor`, Zel maps 32 to a non-infrastructure exit named `ERROR_ACCESSING_REGISTRY`. Java's exit 32 is `REMOTE_ENVIRONMENTAL_ERROR` with infrastructure failure true, so the name and priority differ when more than one failure is compared. The failure detail stores only the first `ERROR:` line, not the full multi-line message.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/Facts.java`
- Bazel lines: 185
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs`
- Verdict: partial
- Differences:
  - String-key indexing, `in`, and `get` (default `None`) match, as do the opaque repr, `print` of the dict, tuple values becoming lists, and rejection of iteration, `len`, `keys`/`items`, and item assignment.
  - Bazel allows only `None`, bool, int, float, string, list, tuple, and dict. A set fails the build. Zel turns a struct into an object and every other value into a string of its display form, so a set is stored and read back as that string.
  - Nested non-string keys fail in Bazel. Zel uses the display form as the JSON key, so an int key `1` is kept as `"1"`. A non-dict facts argument fails the cast in Bazel. Zel stores that JSON value, and later indexing fails with `invalid facts`.
  - Bazel sorts every dict and freezes lists and dicts, with a depth limit of 7. Zel keeps insertion order, has no depth limit, and builds ordinary mutable lists and dicts. Ints that do not fit in `i64` become `0`. A float is stored with Rust display, so `1.0` is read back as the int `1`.
  - A missing key returns Java `null`, which `print` shows as `null`. Zel raises a key error. A non-string index is the same null in Bazel and a type error in Zel. Extra positional or keyword arguments are rejected by the Bazel method and ignored by Zel.
  - Bazel raises `unhashable type: 'Facts'`. Zel treats every host value as hashable, so a facts value can be a dict key.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/FactsAdapter.java`
- Bazel lines: 68
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/json.rs`
- Verdict: partial
- Differences:
  - A non-finite float fails in Bazel (`json.encode` throws, wrapped as `IllegalStateException`). Zel emits `inf`, `-inf`, and `NaN` into the lockfile.
  - Integer-valued floats change type in Zel. Rust prints `1.0` as `1` and `-0.0` as `-0`, so the next parse returns an int. Bazel's encode text always has a decimal point or an exponent, and decode stays a float.
  - Integers outside `i64` are full decimal in Bazel and `0` on the Zel write path. On read, Zel parses them with `f64`.
  - Object key order differs. Bazel encodes dicts in UTF-16 lexicographic order. Zel keeps Starlark insertion order. A supplementary-plane key and a BMP key also sort differently.
  - Zel stringifies non-string dict keys and turns structs into objects and every other value into a JSON string. Bazel's `json.encode` fails those.
  - Bazel rejects a non-dict root, nesting deeper than 7, and Starlark-invalid numbers (`01`, `1.`, `5.e1`). Zel stores the JSON unchanged. `01` becomes int `1`, and a list or deep tree is accepted.
  - Duplicate object keys collapse to one entry in Gson. Zel's parser appends every pair. `\u` escapes are decoded under Bazel UTF-8 byte-string semantics. Gson also rewrites U+2028 and U+2029 before that decode. Zel decodes `\u` to a Unicode scalar (or U+FFFD).
  - Values returned to Starlark are immutable, key-sorted dicts in Bazel. Zel builds ordinary unlocked dicts in file order, so later mutation of a fact value succeeds only in Zel. Pretty-printing matches for the shared shape.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/GitRepoSpecBuilder.java`
- Bazel lines: 135
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/registry.rs`
- Verdict: partial
- Differences:
  - `setInitSubmodules` writes both `init_submodules` and `recursive_init_submodules` to the same boolean, including the default `false`. Zel copies only a JSON boolean `init_submodules` and never sets `recursive_init_submodules`. `init_submodules: true` clones recursively in Bazel and one level deep in Zel.
  - Java boolean and int setters always store the value. A missing `init_submodules` or `verbose` becomes explicit `false`, and `patch_strip` becomes `0`. Zel omits the bool attrs when the key is absent or not a JSON bool. `remote_patch_strip` is still defaulted to `0`. Rule defaults make omitted `false`/`0` fetch the same, but the attribute map and lockfile differ.
  - String setters drop `null` and `""`. Zel keeps `""`, so empty `remote`, `commit`, `tag`, `shallow_since`, and `strip_prefix` are absent in Bazel and present in Zel. A null or missing `remote` still builds a spec in Bazel. Zel returns an error and does not build a spec.
  - Empty `remote_patches` is omitted. Zel always inserts `remote_patches`, including `{}`. A blank patch integrity is stored by the builder. Zel errors.
  - `setRemoteModuleFile` drops an empty URL list and an empty integrity string. Zel always sets one URL and integrity `""` when no module-file hash is recorded.
  - Zel also copies `add_prefix` onto `git_repository`. Gson ignores that field, and the rule has no such attribute, so a source.json `add_prefix` is dropped in Bazel and is an unknown repo-rule attribute in Zel.
  - Zel rejects remotes whose scheme is not `http`, `https`, `file`, `ssh`, or `git`, commits that are not 7–64 hex digits, and tags that start with `-`. The builder stores those strings with no checks.
  - The rule id is `@@bazel_tools//…`. Zel stores a single `@` and only adds the second `@` in `rule_id`, so the lockfile id matches and the in-memory bzl string does not. Insertion order of attributes differs. A non-integral JSON `patch_strip` fails Gson and becomes `0` in Zel.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/GsonTypeAdapterUtil.java`
- Bazel lines: 438
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/lockfile.rs`
- Verdict: partial
- Differences:
  - ASCII lockfile shape matches: 2-space pretty JSON plus a trailing newline, HTML escaping off, registry hashes as lowercase SHA-256 hex or `"not found"`, digests as padded standard Base64, extension ids, versions, module keys, and eval factors.
  - Registry-hash reads are wider in Zel. Java accepts only a 64-hex SHA-256 or the exact marker `"not found"`. Zel also accepts SRI and trimmed hex, then rewrites them to hex. Digest bytes reject missing padding and URL-safe alphabet in Java. Zel accepts both.
  - Module-key and isolation-key reads keep only the second `@` or `+` segment in Java, so a proxy name `a+b` round-trips as `a`. Zel rejects `foo@1.0@x` and keeps lockfile extension ids as raw strings, so the full usage name is preserved.
  - `RepoRuleId` splits on the first `%`. Zel splits on the last `%`. Label attribute values are the unambiguous canonical form (`@@//pkg:name` for the main repo). Zel stores `canonical_form()` (`//pkg:name`). Integers outside signed 32-bit throw in Java. Zel writes an i64, and a bigger int becomes a JSON string.
  - Facts run Starlark `json.encode` (keys sorted by UTF-16 order), reject nesting deeper than 7, and keep full integer text. Zel stores insertion order, has no depth cap, and maps ints with `to_i64().unwrap_or(0)`.
  - Map order differs where Java does not hand Gson a string-sorted map. Zel sorts yanked versions by module key. Bazel keeps discovery insertion order. Extension and facts maps are sorted by extension id, not by the formatted key string.
  - Non-ASCII strings go through internal-to-Unicode, treating the internal string as Latin-1 bytes. Zel strings are already Unicode, so non-UTF-8 bytes cannot survive the way they do in Bazel.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/IndexRegistry.java`
- Bazel lines: 658
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/registry.rs`
- Verdict: partial
- Differences:
  - Bazel skips `metadata.json` under `--lockfile_mode=update` when `source.json` is already hashed and the selected version is not in the previously selected yanked versions, reuses the lockfile yank reason when it is, and returns no yanked versions under `--lockfile_mode=error` without a fetch. Refresh always refetches. Zel always calls `yanked_versions`. A version yanked after the lockfile was written fails an update or error build in Zel and still succeeds in Bazel.
  - A previously absent lockfile entry throws with a refresh hint. Zel returns not found and the caller always reports `{url}: not found`.
  - Vendored registry files are served from `<vendor>/_registries/<host>/<path>` when the checksum is known. Zel never consults a vendor directory.
  - Zel remembers bytes, 404s, and errors for the process. Bazel refetches when there is no checksum, including `file://` registries and `metadata.json`. A local registry edit stays invisible in Zel after the first read.
  - HTTP fetch uses the client env and a checksum. Zel uses curl with no client env, netrc, or URL rewriter. HTTP 410 is not found in Zel and an I/O failure in Bazel.
  - `local_path` is path-fragment-normalized. `module_base_path=/hello/foo` and `path=../bar` become `/hello/bar` in Bazel and stay with `..` in Zel. On Windows, drive-letter paths are absolute in Bazel and joined onto the registry directory in Zel.
  - Omitted git `init_submodules` and `verbose` are still set to false, and init also sets `recursive_init_submodules`. Zel sets init only when the JSON boolean is present and never sets the recursive flag. Empty strings are omitted by Bazel and kept by Zel. `add_prefix` is ignored by Gson and forwarded by Zel. Zel also rejects schemes, commits, and tags before the repo rule runs.
  - A missing remote-module-file hash throws in Bazel. Zel writes an empty integrity. Whitespace-only integrity is accepted by Bazel and rejected by Zel. A malformed mirror URL throws. Zel does not parse mirrors as URIs. A URL fragment is dropped by `URI` and kept in Zel's string split.
  - Bad yanked-version JSON warns and fails open in Bazel, with Gson's message. An unparsable yanked version is a hard error in Zel. Two keys that parse as the same version throw in Bazel. Zel keeps one. A non-string yank reason fails Gson and becomes `""` in Zel.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/InnateRunnableExtension.java`
- Bazel lines: 217
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs`
- Verdict: partial
- Differences:
  - The extension name is split on the first space in Zel and the last space in Bazel, because the label may contain spaces and the rule name may not. `//foo:my file.bzl` plus `http_archive` loads the wrong file in Zel.
  - A rule name starting with `_` is rejected before the `.bzl` is loaded, with no spellcheck. Zel loads first and accepts a `_name` binding if it is in the module globals.
  - A global that exists but is not a repository rule has a type in the Bazel message. Zel uses the same "does not export" text as a missing name and may append a suggestion. Spellcheck candidates are sorted in Zel. Bazel keeps module definition order.
  - More than one module on the same innate id is `BAD_MODULE`. Zel concatenates every usage. Tags whose name is not `repo` are skipped. Bazel instantiates every tag.
  - Some `.bzl` load failures are swallowed and the tags are still emitted. Bazel always fails the extension. The label must be absolute. Bazel parses it with the using module's repo mapping. Zel also allows `//external`, which Bazel rejects.
  - `deprecation` is dropped as a legacy builtin. Zel treats it as an unknown attribute. Bazel type-checks defaults and fails if a default label is not visible. Zel only visibility-checks attributes present on the call.
  - Duplicate tag names throw when the map is built. Zel keeps both specs, and lookup uses the first. The stored spec is the raw tag attributes, including `name`. `@bazel_tools` is rewritten with `starts_with`, which also matches `@bazel_tools_extra`. Bazel's spec is the instantiated rule: explicit non-`None` values only, labels already rewritten, and the rule id taken from the file that called `repository_rule()`, not the re-export file.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/InterimModule.java`
- Bazel lines: 269
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - Compatibility level and the dep-spec maximum are absent on the Rust struct. Module-file evaluation always stores `0` and `-1`, and selection then treats every dep as a single compatibility level, so version selection for graphs Bazel builds does not change. Dep rewriting leaves original deps unchanged. An unset repo name defaults to the module name on both sides.
  - A second `flag_alias` for the same native name throws when the map is built. Zel pushes another pair and later keeps the first key while overwriting the value. Bazel stores the label only after normalizing it in the module repo. Zel stores the raw string. A relative `:foo` becomes `@<module-repo>//:foo` in Bazel and `@@<canonical>:foo` in Zel, which is not a valid canonical label.
  - Additional patches are a Starlark list of labels parsed as main-repo labels. Zel writes the override's raw strings. On fetch, `//…` and `:…` are resolved in the module repo, so a root single-version override patch is taken from the dependency repo. Patch commands and patch args match. A null repo spec with a non-empty patch set would NPE in Java. That call is unreachable. Zel skips the write when the spec is missing.
  - The registry is a `Registry` instance, null for the root and for non-registry overrides. Zel stores an optional URL with the same cases. Yanked-version checks reuse the lockfile under `USE_AND_UPDATE` and return no yanked versions under `ENFORCE` without fetching `metadata.json`. Zel always fetches yanked versions with the checksum disabled, so a changed or unreachable metadata file fails or reports a yank where Bazel still accepts the lockfile.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/LocalPathRepoSpecs.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - `local_path_override` matches: both store the path string unchanged as the only attribute. The `.bzl` identity is a parsed canonical label in Bazel and the apparent string with one `@` in Zel. Outputs that go through `rule_id` match. A comparison against the raw `@@bazel_tools//…` form does not.
  - `--override_module` and `--inject_module` do not feed the same string. Bazel substitutes every `%workspace%`, resolves a relative path against the working directory, drops the override when the path is empty, and normalizes `.` and `..`. Zel only treats a `%workspace%` prefix, joins any other relative path to the workspace root, turns an empty path into that workspace path, and does not collapse `..`.
  - Registry `local_path` specs differ the same way. Bazel treats `C:/…` as absolute on Windows and normalizes before create. Zel treats absolute as `starts_with('/')` and does not normalize. A missing `module_base_path` is `Optional.get()` in Bazel and an error in Zel.
  - Non-ASCII paths stay an internal Latin-1 string, and lockfile serialization runs internal-to-Unicode. Zel stores UTF-8. ASCII paths match. A byte at or above 128, or a non-UTF-8 path, does not.
  - Built-in modules are recorded as `local_repository` with no attributes when the user did not override them. `create` always sets `path`. Bazel's built-in override is a non-registry override with a null spec, and inspecting it throws. The attribute map is an immutable dict that rejects a null path. Zel uses a vec that later code can extend.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/LockFileModuleExtension.java`
- Bazel lines: 81
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/lockfile.rs`
- Verdict: partial
- Differences:
  - Reproducibility matches: no metadata yields false. Digests are standard padded base64. The Java getters return the live byte array, so writing into it changes the stored digest. Zel's vec changes only through a mutable binding.
  - Recorded inputs are parsed values. A string that does not parse becomes a parse failure, which is always outdated. Zel keeps the raw string. Only `ENV`, `FILE`, and `DIRENTS` can invalidate the entry, so `DIRTREE` and `REPO_MAPPING` never do. A string with no space is an empty value, which can still match, instead of a forced refetch.
  - A missing or non-array `recordedInputs` becomes `[]`. Gson fails the lockfile read in that case. Zel's base64 decoder accepts unpadded and URL-safe text. Java's decoder rejects it.
  - Generated repo specs collapse duplicate JSON names to the last value. Zel keeps every duplicate. Factor text is parsed and canonicalized. Zel keys the inner map by the raw factor string and replaces only that exact string, so a reordered `os`/`arch` key misses and a change between `general` and a platform-specific entry leaves the old keys.
  - JSON `moduleExtensionMetadata: null` becomes an object in Zel. Bazel's optional reads null as empty. Both report not reproducible, and Zel rewrites the null as an object. An unknown `useAllRepos` string is kept. Bazel's enum adapter rejects the lockfile.
  - Reuse of a reproducible entry does not compare lockfile facts with the hidden lockfile. Bazel does that when the entry is reproducible, so an edited facts block can still be reused in Zel.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/LockfileModuleExtensionMetadata.java`
- Bazel lines: 287
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/tidy.rs`
- Verdict: partial
- Differences:
  - Explicit `NO`, `REGULAR`, and `DEV` fixups match: the same Starlark errors, the same sorted warning sections, and the same add and remove repo sets. The lock mapping drops facts and omits the lock entry for the default value, which matches `of` against the default metadata.
  - With both explicit sets null, Bazel returns no fixup. Zel treats that case, and a missing metadata object during `mod tidy`, as "expected imports are the current imports that the extension generated", so it warns and deletes `use_repo` names the extension did not generate.
  - Bazel reports direct-dep names that were not generated only inside the fixup, and only when the root module uses the extension. Zel runs that check before the root-usage test, so a non-root usage fails the extension eval.
  - A lockfile object with exactly one of the two dep arrays throws. Zel returns no fixup. Any `useAllRepos` string other than `REGULAR` or `DEV` follows the explicit-list path. Bazel only stores the enum.
  - The warning's "defined in" label is the apparent bzl file. Zel passes the canonical extension id. The event is a warning at the first proxy. Zel logs `WARNING: <location>:`.
  - An empty proxy name becomes the buildozer token `_unnamed_usage`. Zel applies that token by searching the source for a `use_repo` whose first argument is that literal, so `use_repo(use_extension(...), ...)` is left unchanged. Add also errors when that proxy has no existing `use_repo` call. The Bazel command is what buildozer uses to insert one.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/Module.java`
- Bazel lines: 123
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - On a complete canonical-name map the bazel-deps entries match: the root inserts the empty name to main, a non-empty repo name maps to the module's own canonical name, then each dep apparent name maps to that dep's canonical name. An empty repo name skips the self entry in both.
  - Java returns a repository mapping whose context repo is that owner. A missing canonical-name entry throws. Zel returns a hash map only: a missing owner becomes `""`, and a dep whose key is absent is omitted. Duplicate apparent names make Java fail. The hash map keeps the last write.
  - Java looks up `""` in the entries, so `@//pkg` is the main repo only from the root. Zel's empty-name lookup always returns the owner, so `@//pkg` in a non-root module resolves to that module. Any other unknown apparent name becomes a visible repository name of that string. Java returns a non-visible name and fails the extension-label check.
  - Deps are an immutable map that rejects duplicate keys. Zel stores a vec. Successful module files reject a repeated repo name in both. Flag aliases throw on a second alias of the same name. Zel keeps a vec and lets the later label replace the earlier one.
  - `getRepoSpec()` stays null for non-registry overrides. Zel sets the module spec to that override spec (except `bazel_tools`, which stays absent in both). Zel's resolved module still carries interim-only compatibility, original deps, nodep deps, and registry URL. Bazel drops those in `toModule`.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleBase.java`
- Bazel lines: 80
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - Name, version, and key are independent in both. The root key is an empty name and an empty version while `module()` name and version may be non-empty. A non-registry override forces an empty key version and leaves the `module()` version as written. An omitted repo name defaults to the module name. Platform, toolchain, and extension-usage lists stay in source order. Dev-dependency entries are omitted when dev deps are ignored, which is always for non-root modules.
  - Flag aliases are an immutable map. A second alias with the same name fails when the map is built. Zel stores a vec and keeps the later value, so the same module file fails in Bazel and resolves in Zel.
  - Alias labels are normalized as labels in the module repo: `//pkg` becomes `//pkg:pkg`, a bare name becomes `//:name`, and an invalid label is an error. Zel keeps the raw string and only rewrites `@`, `@@`, and `//` prefixes, so `//pkg`, `:tgt`, and a bare name do not become a parsed canonical label.
  - With `--experimental_single_package_toolchain_binding`, Bazel rejects any `register_toolchains` pattern containing `...` before it is stored. Zel lists the flag but does not check it.
  - Bazel returns unmodifiable lists and maps. Zel's fields are public and mutable. After `bazel_tools` is evaluated, Zel clears its execution-platform registrations. The current `MODULE.bazel` has none, so that clear does not change today's graph.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleExtension.java`
- Bazel lines: 83
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`
- Verdict: partial
- Differences:
  - `doc` is not stored. Bazel keeps a trimmed optional string for documentation extraction. Zel never reads the `doc` argument.
  - Extra positional arguments and unknown keywords are ignored. A non-dict `tag_classes` becomes an empty list. Non-string dict keys are stringified. `environ` drops non-strings. `os_dependent` and `arch_dependent` follow Starlark truthiness. A `facts_version` that is not a non-negative signed 32-bit int becomes `0`, and a value that fits in `u64` but not `u32` is truncated. Bazel rejects those.
  - `module_extension()` is not limited to `.bzl` initialization. Bazel takes the defining file from the init thread and errors otherwise. That label is the file being initialized, including when the call is inside a helper. Zel stores a bzl file only when the host value is a direct top-level global, and also stores an export name the record does not have.
  - The Starlark value is hashable. Bazel treats this type as unhashable. Calling it reports a different message, and attribute access reports a different missing-member message.
  - Tag classes stay insertion-ordered, and Zel drops tag-class docs. Evaluation rebuilds `module.tags` through a `BTreeMap`, so field order is alphabetical. Declared `environ` is sorted before inputs are recorded. Bazel keeps declaration order.
  - Lockfile factors are not `OS.toString()` plus `os.arch`. Zel uses only `osx`, `windows`, and `linux`, and only `aarch64` or `x86_64`. Bazel uses the JVM `os.arch` string, which is `amd64` on a typical x86_64 JDK. Location omits a zero column in Bazel.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleExtensionContext.java`
- Bazel lines: 321
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs`
- Verdict: partial
- Differences:
  - The module list and the root non-dev flag match. The working directory is always removed. Extension execution is local. Watching outside the workspace is rejected for an explicit watch and skipped for `auto`. Declared environ is recorded before eval, sorted by name.
  - `is_isolated` is not gated. Bazel hides it unless the experimental flag is on. Zel always returns whether an isolation key is set.
  - `tag_sort_key` returns the same pair, but its type, repr, and print differ, and the value compares equal to a handwritten tuple.
  - `is_dev_dependency` accepts any struct. A missing field is false. Bazel requires a type-checked tag.
  - `extension_metadata` only reads keyword arguments, so positional calls are treated as both dep lists omitted, and unknown keywords are ignored. `reproducible` uses Rust truthiness. Dep lists drop non-strings and turn labels into their canonical form. Bazel fails the cast.
  - Facts have no depth cap. Non-JSON values are stringified. Non-string dict keys are stringified. Keys are not sorted. Ints that do not fit in `i64` become `0`. The metadata object is then split on `|`, so a fact string containing `|` is parsed as the wrong metadata.
  - A non-string facts index is a type error rather than a missing key. Facts are hashable. Facts are taken only from the workspace lockfile when the version matches. Bazel uses the hidden lockfile when the workspace file has no facts entry for that extension.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleExtensionEvalFactors.java`
- Bazel lines: 98
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs`
- Verdict: partial
- Differences:
  - The string shape matches for the three dependency combinations: `general`, `os:<os>`, `arch:<arch>`, or `os:<os>,arch:<arch>`. Zel never parses, so a lockfile key in the other order, with a repeated prefix, or with extra comma segments is not rewritten. Bazel parse keeps the last `os:`/`arch:` segment and emits the canonical form. Lookup is exact string equality, so those keys do not hit.
  - Host tokens differ. Bazel os is `linux`, `osx`, `windows`, `freebsd`, `openbsd`, or `unknown`, with `blaze.os` overriding `os.name`. Arch is runtime `os.arch` (`amd64` on Linux and Windows x86_64). Zel uses compile-time cfg: os is only `osx`, `windows`, or else `linux`. Arch is `aarch64` or else `x86_64`. An os- and arch-dependent extension on Linux x86_64 is `os:linux,arch:amd64` in Bazel and `os:linux,arch:x86_64` in Zel, so the lockfile entry misses. macOS aarch64 matches.
  - `hasSameDependenciesAs` is true when both sides have the same empty or non-empty os and arch, even if the names differ. Bazel keeps other factor entries of that shape and drops entries whose shape changed. Zel copies every factor entry for a live extension and only replaces the exact factor string, so a stale `general` stays beside the new key.
  - Compare order is the canonical string, and that order is the lockfile factor-key order. Zel orders the raw key. Canonical ASCII keys sort the same. A non-BMP os or arch string can diverge because Java compares UTF-16 code units and Rust compares UTF-8 bytes.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleExtensionEvalStarlarkThreadContext.java`
- Bazel lines: 189
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`
- Verdict: partial
- Differences:
  - Name checks match at the call, except a rejected name is sanitized in Bazel and printed raw in Zel, so a control character changes the error text. Duplicate apparent names fail in both places, and call order is preserved.
  - Bazel only snapshots the call. Unknown attributes, type conversion, and label parsing run after the implementation returns. Zel validates inside the call, so a bad attribute aborts the implementation. Later side effects and further repo rules still run in Bazel and do not run in Zel.
  - Label strings are converted only after the implementation, with the `.bzl` repo mapping, then every generated name, then root overrides. A string `@sibling//:t` therefore sees repos created later in the same implementation. Zel stores the original string and resolves it at fetch with an empty package. Sibling names that the defining module did not import become unknown repos, and a relative label is `//:file` instead of `//<bzl-package>:file`. Generated names are applied after overrides, so they overwrite overrides.
  - A `Label(...)` value keeps the mapping from the `Label()` call on both sides. A label Bazel rejects during extension eval can be locked by Zel and fail only when the repo is fetched. Instantiate errors are reported on the call stack after the repo-rule frame is removed. Zel attributes the failure to the repo-rule call.
  - Deep clone copies dicts and iterables and rejects anything else. Zel stringifies any other value, turns an int that does not fit `i64` into a decimal string, and accepts float. Dict keys stay typed in call order in Bazel. Zel kwargs are a `BTreeMap`, and dict keys are stringified.
  - `native.existing_rule` returns none only on the extension-eval thread in Bazel and for every `.bzl` this runtime loads in Zel. Label `print` uses the first inserted apparent name in Bazel and the lexicographically first name in Zel.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleExtensionEvaluationProgress.java`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - This is a fetch-progress event for one module-extension evaluation. The resource id is `module extension ` plus the extension id. `ongoing` stores the caller's message (`starting`). `finished` stores `finished.` and is posted only after a successful eval. The UI keys the row on the resource id, appends seconds only after more than 3 seconds, and drops the row when finished, so `finished.` is stored and never shown. A failed eval never posts finished, so the starting row stays until the command ends.
  - Zel never posts that in-progress event and never posts a finished event. Nothing in bzlmod calls the download bar for a module extension. Generated repos, errors, and the lockfile are unchanged.
  - `module_ctx.report_progress` reads the status and returns `None`. Bazel posts that string on the same resource id, which replaces the visible status. Zel drops the status.
  - Zel's download bar stores a URL and a byte count, not a status string, and always appends seconds, including 0. Bazel orders rows by start time. Zel's map orders by URL first.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleExtensionId.java`
- Bazel lines: 93
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs`
- Verdict: partial
- Differences:
  - Innate matches: both are true when the extension name contains a space. The printed id matches for the form Zel stores: unambiguous canonical label, extension name, and an optional isolation key. The lockfile key drops a leading `@@` only when the remainder starts with `//`, so the main repo becomes `//pkg:name%ext`. That differs from `toString()` on the main repo.
  - Isolation-key parsing is absent. Java splits on every `+` and keeps only the first two pieces. Fewer than two pieces throws. The module half keeps only the first two `@` pieces. Zel's module-key parse uses `split_once`, so a second `@` stays inside the version and fails version parse.
  - Lexicographic order is only partly reproduced. Empty isolation sorts first, then module name, version, and usage name. Package comparison differs: Java compares the package path as one string, so `//foo-bar:a` sorts before `//foo/bar:a`. Segment order sorts `foo/bar` first. The derived string order also sorts `@@//a/b:a` before `@@//a:b/c`, while label order sorts the shorter package first.
  - Equality differs when two strings are the same label. Java equates labels by repo, package, and target. Zel equates the stored strings. The comparator strips every leading `@`, so sorted unique ids collapse spellings that equality keeps distinct.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleExtensionMetadata.java`
- Bazel lines: 180
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`
- Verdict: partial
- Differences:
  - `None`, `"all"`, and empty-list checks, the three use-all values, both-or-neither, duplicate and cross-list errors, and user repo-name rules match. Invalid names with control characters differ only in the message.
  - Bazel rejects a non-sequence and a non-string element. Zel turns a non-sequence into an empty list, drops non-strings, and accepts a tuple, a depset, or a select of strings. A label is rewritten to its canonical form and then repo-name-checked.
  - Facts validation requires a string-keyed dict, a fixed set of types, sorted keys, and at most 7 levels. Zel does not fail: non-string keys are stringified, structs become objects, other values become strings, key order stays insertion order, and depth is unlimited. An overflowing int becomes `0`. A whole-number float is read back as an int. `inf` and `NaN` become text.
  - Omitted facts are an empty dict before create. Zel stores `None` and encodes `-`. Explicit `facts=None` is a type error in Bazel and JSON null in Zel. `reproducible` is a Java boolean. Zel uses truthiness, so `1` is true. Positional arguments and unknown keywords are rejected by Bazel and ignored by Zel.
  - The value is split on `|`. A fact string containing `|` splits the payload and facts are dropped with no error. Bazel keeps those facts.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleExtensionRepoMappingEntriesFunction.java`
- Bazel lines: 82
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`
- Verdict: partial
- Differences:
  - Bazel waits on the dep graph and the validated extension value and restarts until both exist. Zel builds a map immediately from usages and only appends evaluated repo names after results are cached, so a lookup that has not evaluated the extension omits generated repos that no `use_repo` imported.
  - Both start from the module that owns the extension `.bzl`. If that repository is not a module, Bazel dereferences a null key. Zel uses the first usage module's mapping.
  - Sibling visibility in Bazel is every generated internal name. Zel writes each `use_repo` exported name from every usage, then each cached generated name.
  - Bazel applies repo overrides last, so the overridden name resolves to the root module's overriding repo. Zel writes overrides first and later import and generated writes replace them, so `override_repo` and `inject_repo` lose to the generated name.
  - Bazel records an override even when the overriding apparent name is absent from the root mapping. Zel inserts it only when that name is present. The value also stores the defining module name and version. Zel's mapping is only apparent-to-canonical strings, and package `module_name` and `module_version` are none.
  - An unknown apparent name becomes a non-visible repo in Bazel and a real unvalidated canonical name in Zel. The empty name looks up the main repo in Bazel's entries and returns the mapping owner in Zel, so `@//…` inside an extension repo resolves to that extension repo. A canonical name that matches no extension is not found in Bazel. Zel's final branch returns the root module mapping.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleExtensionRepoMappingEntriesValue.java`
- Bazel lines: 82
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`
- Verdict: partial
- Differences:
  - Bazel keeps one interned sky value per extension: the same map and a precomputed hash for every repo that extension generated. Zel rebuilds a vec per canonical repo and stores it with no shared hash.
  - Entry precedence differs. Bazel order is the host module's full mapping, then every generated repo, then root overrides. A generated name hides a host bazel dep, and an override hides the generated repo. Zel writes overrides first, then every `use_repo` export, then generated names, so the extension repo wins and the override is dropped.
  - Bazel waits on the extension value, so the map always contains every generated repo. Zel adds non-imported generated repos only when results are already filled, and it also inserts export names the extension never generated.
  - The module key is the module that owns the extension `.bzl`, and Bazel stamps that name and version onto packages in the generated repos. Zel does not store that key and returns none for both. For an innate extension whose `.bzl` repo is the main repo, Zel uses the first matching usage's module mapping instead of the `.bzl` owner.
  - A canonical name that is neither a module nor `{unique}+…` is not found in Bazel. Zel returns the root module mapping. When the host is the root, Bazel's entries include the empty name mapped to main, so `@` is the main repo. Zel's empty-name lookup returns the owner, which these callers set to the extension repo itself.
  - A duplicate internal name makes the bimap inverse throw. Zel keeps the last write. Override keys are iterated in sorted order, so the first apparent name for a canonical repo can differ.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleExtensionUsage.java`
- Bazel lines: 219
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - Proxy imports are a bimap. A repeated local name or exported name is rejected. Zel stores a vec, which keeps both, and the inverse lookup is a first-match scan. Parser-built usages still reject a repeated exported name, so normal `use_repo` data matches. The value itself does not.
  - Repo overrides are copied from a hash map, so entry order is hash-bucket order. Zel uses a sorted map. With two or more overrides, walking the map changes order, and the usages digest copies that sorted order, so the digest bytes differ. A duplicate key overwrites in the sorted map. The Java builder throws.
  - Trim-for-evaluation returns a new usage with tag locations forced to builtin, proxies replaced by an empty list, and repo overrides replaced by an empty map. Zel never builds that value. The digest writer hardcodes those empties, so the hash input matches, while in-memory proxies, overrides, and tag locations stay intact.
  - Locations are file, line, and a UTF-16 column. A zero column is omitted from `toString`. Zel always prints `file:line:column`, and columns advance once per Unicode scalar. A non-ASCII byte shifts every later column. Splitting the location on `:` also fails when the file path contains `:`.
  - The containing module-file path is a path fragment, so `.` and `..` are normalized. Zel stores the raw string, so `foo/../MODULE.bazel` and `MODULE.bazel` compare equal in Java and stay distinct in Rust.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFile.java`
- Bazel lines: 32
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/module_file.rs`
- Verdict: partial
- Differences:
  - Bazel keeps the file as opaque bytes and later parses them as Latin-1: each byte becomes U+0000–U+00FF. Invalid UTF-8 is left as those characters unless the UTF-8 enforcement flag is warning or error. Zel never keeps the bytes. A non-UTF-8 root `MODULE.bazel` is evaluated as an empty file. Registry content and embedded `bazel_tools` substitute U+FFFD and continue. A byte `0xFF` is `ÿ` in Bazel and U+FFFD in Zel.
  - Non-registry override locations differ. Bazel stores the canonical label, such as `@@name+//:MODULE.bazel`. Zel stores the filesystem path, and that string is what errors show. Registry locations are both URL strings and line up after Zel trims a trailing slash.
  - Equality compares the bytes and the location, and a null content or location throws. The shared byte array means a caller who mutates it after create changes later reads. Rust strings are an owned copy and cannot be null. Current call sites pass a freshly read array and do not mutate it.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileFunction.java`
- Bazel lines: 1013
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs`
- Verdict: partial
- Differences:
  - A missing root `MODULE.bazel` writes the Bzlmod migration comment file and warns. Zel evaluates an empty file with no warning and no created file.
  - Single-version override patches are applied to the registry module-file bytes before evaluation, so they change the resolved deps. Only main-repo patch labels are applied. Zel never patches that text. It only copies patches onto the repo spec, so resolution still sees the unpatched file and a bad patch fails later.
  - A registry `include()` fails before evaluation. Zel fails at the call with a different message. Dev includes are skipped in both cases.
  - Include paths are parsed as labels, require a real package, and read that label's file. Zel's root reader only checks a `BUILD` file when the string contains `:`, and a colon-less label is read as a path instead of the package's file. Non-registry includes skip the package check, and `Path::join` drops the repo directory. Nested includes join onto the parent path, so error locations differ.
  - Empty `bazel_dep` versions fail before any registry access. Zel searches registries first, so a registry I/O error wins, and a module found at an empty version is accepted.
  - Registry modules set `print` to a no-op. Zel writes `DEBUG:` lines to stderr. `bazel_tools` is a normal non-registry override, so a root override replaces its module file. Zel always evaluates the embedded file and then clears its execution-platform registrations.
  - `--inject_repository` appends repo tags on the existing innate usage in place. Zel removes that usage and pushes it at the end, which changes extension-usage order. Invalid UTF-8 is evaluated as U+FFFD for registry files. A non-UTF-8 root file is treated as missing.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java`
- Bazel lines: 1213
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/module_file.rs`
- Verdict: partial
- Differences:
  - An omitted or empty version is parsed to the empty version and stored. Zel leaves the key version in place, so a non-root `module(name = "foo")` keeps the registry version and later passes the "different version" check that Bazel fails. Compatibility is checked before version parse in Zel, so a bad version plus a bad compatibility list fails with a different error.
  - `bazel_dep` accepts a dep whose name equals the current module. Zel errors that a module cannot depend on itself. `register_toolchains` rejects `...` when the single-package flag is set. Zel has no such check.
  - Label normalization rewrites every extension label to the apparent form with one `@`, so a root `//:e.bzl` and `@@<repo>//:e.bzl` share one usage. Zel leaves a root repo-relative label as `//pkg:name` and keeps a `@@` prefix, so those calls become distinct extensions. `//conditions` and `//visibility` stay on the main repo in Bazel and follow the module repo in Zel. Extension names are ASCII identifiers. Zel treats Unicode alphanumeric as valid.
  - Export keeps the first global binding. Zel walks the globals after evaluation and the last binding overwrites the proxy name, so an alias changes the isolation key. `use_repo` kwargs are recorded in source order. Zel stores them from a sorted map. `inject_repo` uses the key as the name inside the extension. Zel swaps that pair when the repo need not already exist.
  - Include requires the target basename to end in `.MODULE.bazel` and not start with `.`. Zel treats the whole post-colon string as the name, so a hidden path and a second colon differ. Overrides are dropped when dev deps are ignored, before the duplicate check. Zel always stores them and always errors on a second override.
  - Single-version patches are parsed as main-repo labels. Zel skips that check for non-root modules, rejects every `@repo//…` patch, and stores the raw string. `git_override` forwards a `name` attribute. Zel rejects `name`. `flag_alias` normalizes the label and rejects a duplicate native name. Zel stores the raw string and appends duplicates. Integer attributes that do not fit in `i64` become the default instead of failing.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileValue.java`
- Bazel lines: 105
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/module_file.rs`
- Verdict: partial
- Differences:
  - Zel keeps the evaluated interim module and, after resolution, the dep graph. It does not keep a per-file key or a separate root versus non-root value. The root name may differ from the empty root key, and a non-registry module may declare a version because the key version is empty. Both of those match.
  - Registry file hashes are per file, and the root implementation always returns an empty map. Zel folds every registry module-file fetch into one sorted map that later also receives `source.json` hashes, which Bazel stores on the repo-spec value. Digests match. Sorted iteration versus discovery insertion order changes lockfile key order once more than one URL is recorded.
  - Root overrides have the same precedence. Bazel replaces a command override in place. Zel removes and then pushes, so a command override moves to the end. The builtin entry is not a null spec. It is a `local_repository` with no attributes.
  - The non-registry canonical-name maps are not stored. Bazel maps every non-registry override, including one that is not a direct dependency. Zel only has canonical names for modules that remain in the pruned dep graph. A non-registry module reachable only by a nodep edge cannot be resolved as `name+`.
  - Module file paths are a set of workspace-relative fragments. Zel stores a vec of absolute paths. The same label included twice stays once in the Bazel set and is appended again in the Zel vector. Dev-dependency includes are omitted on both sides when dev dependencies are ignored.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleKey.java`
- Bazel lines: 142
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - Display matches: root is `<root>` and `root module`. Any other key is `name@_` when the version is empty and `name@version` otherwise. `name@` and `name@_` are the same key because an empty version parses as empty. `bazel_tools` and `platforms` keep those bare repo names. Root's canonical name is empty. A normal key formats as `name+version` or `name+`.
  - `getCanonicalRepoName(true)` throws when a non-root, non-well-known key has an empty version. Zel still formats the version, and an empty version displays as `""`, so the result is `name+`.
  - Repository-name creation still validates the character set. A module name outside that set throws. Zel returns the formatted string with no validation.
  - `fromString` splits on every `@` and uses only the first two fields. Zel uses `split_once`, so later `@` stays in the version token. `foo@1.0@x` parses in Java and is a version error in Rust. `@@` parses as the root key in Java and errors in Rust. A string with no `@` throws an index exception in Java and returns an invalid-key error in Rust.
  - Lexicographic order is name, then version, which matches derived order for ASCII and other BMP names. A supplementary-character name and a name with a code point in U+E000–U+FFFF can sort in opposite orders.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleThreadContext.java`
- Bazel lines: 446
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/module_file.rs`
- Verdict: partial
- Differences:
  - Overrides return immediately when dev deps are ignored, so they are discarded and a second override is not an error. Zel always stores the override and always errors on a duplicate.
  - A built-in dep is skipped when the module key name equals that built-in. The root key name is empty, so `module(name = "bazel_tools")` still attempts the dep and then fails because the name is already the current module name. A different repo name keeps a real `bazel_tools` dep. Zel skips when the `module()` name equals the built-in, so both of those root files succeed and do not depend on `bazel_tools`.
  - Built-in overrides are added for the root only. For `bazel_tools` that value is the null-spec sentinel. Zel inserts `local_repository` with an empty attribute list, so a reader sees an empty path rather than the sentinel.
  - A missing overriding repo appends a spellcheck suggestion. Zel emits the same sentence with no suggestion. An imported repo that was injected is reported on the `use_repo` call stack. Zel builds the traceback at the override location. A repo that is both overriding and overridden walks a hash map, so which pair is chosen can differ. Zel also errors when the overriding repo name equals the module's own repo name. Bazel records that pair and continues.
  - Include sets the current file to the label's workspace-relative path and restores it afterward. Zel keeps only the target name when the current file contains no slash, and otherwise joins the label onto the includer's directory. A missing include is an internal error because the file must already be compiled. Zel evaluates it through the include reader.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/NonRegistryOverride.java`
- Bazel lines: 54
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - Bazel's `bazel_tools` override is a null spec. Any read throws. Zel cannot store a missing spec. When the root has no override it inserts `local_repository` with an empty attribute list and later drops that spec only because the module name is `bazel_tools`. If the root module itself is named `bazel_tools`, both fail, but the messages differ.
  - The bootstrap rule set is the three `@@bazel_tools` http, git, and local rules. Callers load those files with a bootstrap key and a null main-repo mapping, so label print stays canonical. Zel always sets the root mapping and has no bootstrap load key, so `print` of a label inside those rules uses the root apparent name.
  - Display rewrites `patches` entries that start with `@@//` and renders a label attribute as a quoted string. Bazel's record string keeps the raw Starlark dict. A Starlark int that does not fit in `i64` is a quoted string in Zel and an unquoted integer in Bazel.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/Registry.java`
- Bazel lines: 73
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/registry.rs`
- Verdict: partial
- Differences:
  - `getUrl` keeps the configured URI string, including a trailing slash. Zel trims trailing slashes, so lockfile keys and error text that use the raw URL do not match a slash-terminated registry.
  - `getModuleFile` returns raw bytes plus a location. Zel returns a lossy UTF-8 string. Illegal bytes fail the module read under UTF-8 enforcement in Bazel and become U+FFFD in Zel. A missing file is `None` rather than a not-found exception. The caller rebuilds `{url}: not found` and walks the next registry.
  - `getRepoSpec` throws if the module-file hash entry is missing or empty. Zel uses an empty integrity string and still returns a spec. A non-hex string becomes the SHA-256 of empty bytes instead of failing.
  - Yanked-version I/O fails open in Bazel, including a bad version and a non-string reason. Zel fails the whole resolution on a version-parse error. A non-string reason becomes `""` and the version is yanked with an empty reason. Malformed JSON is a warning plus none on both sides.
  - The lockfile short-circuit is absent. Bazel skips the network when the lockfile already records the selected version. Zel always fetches `metadata.json`. A version yanked after the lockfile was written fails Zel and still builds in Bazel.
  - A registry sky value is never equal to the previous one, so dependents rerun. Zel keeps a process-wide cache of the first result, so a file that appears or changes after the first fetch stays missing or stale.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RegistryFactory.java`
- Bazel lines: 41
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/registry.rs`
- Verdict: partial
- Differences:
  - Scheme checks match for lowercase `http`, `https`, and `file`. `file://` forces hash mode ignore even under error mode. Off, update, and error map to ignore, use-and-update, and enforce.
  - `java.net.URI` lowercases the scheme, so `HTTP://` and `FILE://` are accepted. Zel keeps the raw scheme and rejects them.
  - Refresh is use-immutable-and-update: re-fetch previously absent files, and always revalidate yanked versions. Zel drops lockfile entries whose hash is absent, then uses use-and-update. A still-missing file is fetched again, but yank handling does not follow the refresh rule.
  - Previously selected yanked versions let update and error reuse the lockfile yank reason without reading mutable metadata. Zel never passes that map. Resolve always fetches yanked versions, so a yank that appears after the lockfile was written changes the result, including under error mode.
  - A vendor directory serves known-checksum registry reads from the vendor tree. Zel writes the vendor tree during vendor but never reads it back.
  - Mirrors are a per-registry set, parsed as URIs at creation. Zel copies the raw flag strings onto every registry, with no per-registry split and no URI check, so archive URL lists differ.
  - The production factory attaches the non-strict repo env. Zel's registry does not take that map. HTTP fetches inherit the process environment.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RegistryFactoryImpl.java`
- Bazel lines: 84
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/registry.rs`
- Verdict: partial
- Differences:
  - Scheme and path checks match `java.net.URI` for the cases in the factory test, including mixed-case schemes. `https://` and `file://` are rejected by Java as a missing authority. Zel treats an empty authority as a hierarchical URI, so `https://` succeeds and `file://` fails later with a different message. A single-slash `file:/tmp/reg` is a file registry in Bazel. Zel rejects every `file:` URL that does not start with `file://`.
  - Refresh maps to use-immutable-and-update. Zel has no immutable mode. It drops lockfile entries whose hash is absent and then uses use-and-update. Previously selected yanked versions are never stored. Bazel serves yanked reasons from the lockfile and, in enforce mode, reports none-yanked without reading metadata. Zel always fetches metadata, including under error mode.
  - File registries ignore lockfile hashes on both sides. Each mirror string is parsed as a URI, and URI equality collapses case variants of scheme and host. Zel stores the raw strings. A vendor directory serves checksummed reads. Zel writes those files during vendor but never reads them. Registry downloads still get the full client environment. Zel's curl inherits the process environment.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RegistryFileDownloadEvent.java`
- Bazel lines: 51
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/registry.rs`
- Verdict: partial
- Differences:
  - A normal fetch matches: present bytes become lowercase SHA-256 hex, and a missing file becomes none, which the lockfile writes as `"not found"`. Duplicate URLs are last-write-wins. Sorted versus encounter order does not change the lockfile, because Bazel sorts before write.
  - A repository-cache hit does not hash the bytes it returns. It stores the lockfile hash and returns the cached file. Java always checksums the bytes. A cache file whose contents are not that digest is still recorded as the expected hash in Zel. Bazel would record the digest of those bytes and fail the fetch on mismatch.
  - A thrown fetch still posts an event with an empty checksum, because the post is in `finally`. Zel inserts nothing when fetch returns an error. A caller that collected posts after a caught I/O error would see a "not found" URI in Bazel and no entry in Zel.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RegistryFunction.java`
- Bazel lines: 101
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs`
- Verdict: partial
- Differences:
  - `%workspace%` is not expanded on registry URLs. Bazel substitutes every occurrence before URI parsing. Zel validates the raw string, and `%wo` is a malformed escape, so that URL is rejected.
  - Mirrors are looked up by the unsubstituted registry URL, and each mirror is parsed as a URI. A bad mirror fails registry creation. Zel passes the raw flag strings into every registry.
  - Lockfile yanked versions are not inputs. In update and error, Bazel reuses the selected yanked versions and treats a locked `source.json` with no yank entry as not yanked. Zel always fetches metadata.
  - A vendor directory is not passed into registry fetches. Refresh drops the cached registry node at most hourly. Zel drops recorded 404s, but the process-wide URL cache keeps the first HTTP body, so mutable registry bytes stay stale past one hour.
  - Hash modes otherwise match, and `file://` registries ignore lockfile hashes on both sides. On `mod`, an invalid URL exits 37 in Zel and 48 in Bazel. Build, fetch, and vendor exit 48.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RegistryKey.java`
- Bazel lines: 47
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs`
- Verdict: partial
- Differences:
  - The same URL string maps to one registry inside a single resolve. Bazel's interned key is the graph-wide node id for a non-hermetic registry function, shared by module-file, repo-spec, and yanked-version lookups. Zel rebuilds that map on every resolve and has no sky-key interner.
  - A null URL throws. An empty string and any non-URL stay distinct keys and fail later. Zel rejects an invalid URL and does not insert it.
  - A literal `%workspace%` is a legal key component. The registry function expands it before URI parsing. Zel parses the raw map key, and `%` not followed by two hex digits is a malformed escape.
  - Interning is identity for the Java key. Zel dedupes only with string equality for that context. A Java string with an unpaired surrogate can be a key and cannot be a Rust string.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RegistryOverride.java`
- Bazel lines: 29
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - A non-empty string replaces the default registry list with that single URL. `""` keeps the defaults. Only the single-version and multiple-version overrides carry it.
  - Bazel throws the "no version" error when the key version is empty before it reads the registry. Zel opens the override registry first and emits that message only when the fetch finds nothing. An empty version plus a bad registry URL fails as an invalid URL. An empty version plus a registry that actually has that module file can succeed in Zel and always fails in Bazel. The same split applies when the override registry string is empty.
  - Both are empty only for `""`, so the sentinel matches. One URL versus a one-element list queries the same registry.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RegularRunnableExtension.java`
- Bazel lines: 404
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs`
- Verdict: partial
- Differences:
  - `--incompatible_require_repo_extension_metadata` is never applied. Bazel rejects a `None` return when the mode is `all`, or `root` and the extension label's repository is the main repo. Zel accepts `None` and continues.
  - Eval factors differ on real hosts. Bazel stores `OS.toString()` and the raw `os.arch` (`amd64` on HotSpot x86_64). Zel uses compile-time cfg: anything other than macOS or Windows becomes `linux`, and any arch other than `aarch64` becomes `x86_64`. Lockfile keys such as `arch:amd64` and `os:freebsd` do not match.
  - `facts_version` is a Java int that Bazel rejects when negative or outside the signed 32-bit range. Zel stores `to_u64().unwrap_or(0) as u32`, so negatives become 0 and values from `2^31` through `2^64-1` are kept or truncated.
  - A label in `//external` is rejected by Bazel. Zel only checks the file suffix. The "did you mean" set is sorted before the first equal edit-distance wins. Bazel walks module globals in definition order. Length counts UTF-16 units versus Unicode scalars.
  - Evaluation failures are not the same string. Bazel reports the Starlark stack as an event and throws once. Zel appends a message that drops the real repository, and load failures repeat the inner reason. `module.tags` field order follows hash order in Bazel and sorted tag-class name in Zel.
  - Downloads do not see the non-strict repo env. Zel's curl inherits the process environment. A failed delete of the module context directory fails the extension in Bazel and is ignored in Zel. A load cycle makes the transitive digest empty in Zel instead of a failed load.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RepoRuleId.java`
- Bazel lines: 60
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`
- Verdict: partial
- Differences:
  - There is no rule-id value. The defining `.bzl` label and the rule name are two strings. Record equality is label equality, so one `@` and two `@` are the same id. Zel compares the raw strings, so those two forms are different specs even though the printed id matches.
  - `toString()` is the unambiguous canonical form plus `%` plus the rule name. A repo-relative `//pkg:file.bzl` is left unchanged, so the id is missing the `@@`. Override display strips one `@` and then adds `@@`, so a stored `@@repo…` becomes three `@`s. Single-`@` labels such as `@bazel_tools//…` still match.
  - The rule name is fixed on the first non-indented top-level assignment. Zel fills an empty name by scanning globals in sorted order, so the alphabetically first global wins. An assignment nested under `if` or `for` stays unexported in Bazel and is exported in Zel. Printing the rule value uses the id. Zel's host object prints an index with no label or name.
  - Lockfile parse splits on the first `%` in Bazel and the last `%` in Zel. A string with no `%` becomes a null label in Bazel and is rejected in Zel. `@@bazel_tools//` is rewritten back to one `@` on read.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RepoSpec.java`
- Bazel lines: 29
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - The lockfile object is still `{repoRuleId, attributes}`, and string escaping for `@@…` and quoted strings matches. Java keeps a parsed label plus a rule name. Equality is label equality, so one `@` and two `@` are the same spec. Zel stores two strings, so those spellings compare unequal. A main-repo bzl is locked as `//pkg:file.bzl%rule` instead of `@@//pkg:file.bzl%rule`. Reading rewrites only the `@@bazel_tools//` prefix back to one `@`.
  - Java splits the id on the first `%`. No `%` yields a null label. Zel splits on the last `%` and returns none when the id is missing or has no `%`. A target name that contains `%` binds different label and rule pieces.
  - A null id serializes as JSON null. Zel has no null id. A missing attributes key returns none. A non-object becomes an empty list. Java expects an object and throws on null.
  - Attributes are a dict: one value per key, and equality ignores insertion order. Zel attributes are a vec, so order and duplicate keys change equality. Integers are signed 32-bit. A value outside that range throws. Zel uses `i64`, turns a non-i64 number into `f64`, and stores floats. Java rejects floats.
  - Label attributes are written in unambiguous canonical form and require an `@@` prefix. Zel stores `canonical_form()`, which is `//pkg:name` for the main repo, so Java would read that string back as a string. Non-ASCII strings go through Latin-1-as-UTF-8. Zel writes Unicode. Java does not put `name` on the spec. Zel keeps it until the lock entry drops it, then reinserts the repo map key on read.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RepoSpecFunction.java`
- Bazel lines: 86
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/registry.rs`
- Verdict: partial
- Differences:
  - I/O failures use the same prefix and exit 32. Bazel stores the failure as transient, so Skyframe can rerun the fetch in the same build. Zel fails resolve on the first error and does not retry. A null cause message is concatenated as the literal `: null`.
  - A missing module-file checksum throws inside the callee, so this function does not apply the registry error. Zel writes an empty integrity and still returns a spec. A JSON `type` of null throws in the Java switch. Zel treats a missing or null type as archive.
  - A relative local path with no registry metadata throws from `Optional.get()` in Java. Zel returns a "non local registry" error, then the wrapper turns that into the exit-32 registry error. Path fragments normalize the local path. Zel joins raw strings, so the path attribute and the error text differ, including on Windows file URLs.
  - Archive integrity that is only whitespace is kept by Gson. Zel treats it as missing. An empty patch integrity is stored on the Java spec and fails in Zel. Git `init_submodules: true` also sets the recursive flag. Zel sets only `init_submodules`. Absent booleans are still emitted as false in Java and omitted in Zel. Zel also rejects schemes, commits, and tags, and copies `add_prefix`.
  - Mirrored archive URLs drop URI fragments in Java. Zel leaves `#…` on the path. A mirror string that is not a URI fails in Java and is still concatenated in Zel. On success, Bazel's hash map is only this call's events in post order. Zel merges grabs into a sorted map, and a cached `bazel_registry.json` is not recorded again.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RepoSpecKey.java`
- Bazel lines: 55
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs`
- Verdict: partial
- Differences:
  - For a registry module, both sides fetch `source.json` with that module's key and its registry URL. Modules whose registry is null are dropped before the key is built. Zel skips the fetch when the URL is absent.
  - A null registry throws. Zel's missing registry is the none branch and does not throw. An empty URL string is kept on both sides.
  - Equality is the module key plus the URL, then interning makes equal keys one instance. Zel has no interned key. One resolve visits each module key once, so the spec for that graph matches. Bazel can reuse the node when both components match. Zel fetches on every resolve.
  - The URL stored in the key is `uri.toString()`, which lowercases the host and keeps a trailing slash. Zel trims trailing slashes and leaves the host as written. The request still matches. The key strings differ for a trailing slash or a mixed-case host.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RepoSpecValue.java`
- Bazel lines: 39
- Zel counterpart: `none`
- Verdict: partial
- Differences:
  - Bazel keeps one immutable sky value per registry module: a non-null repo spec plus only the registry-file checksums posted while that spec was fetched. Zel stores the spec on the module and folds those URLs into one mutable map that already holds discovery hashes. A missing spec is none or an error string instead of a transient sky restart.
  - A null spec or a null map is rejected. Zel's spec is optional. It is none for the root and for `bazel_tools`, and it also holds non-registry override specs. Bazel never builds this value for a null registry.
  - Checksums carry an algorithm plus a hash. Zel stores a lowercase hex string or none. For this producer both sides are SHA-256, and both keep the last write for a repeated URL. Sorted iteration versus download order can change a later dump of the merged map. File URLs remain in the resolution map and are dropped only when the lockfile is written.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RootModuleFileFixup.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/tidy.rs`
- Verdict: partial
- Differences:
  - The success sentences match: updated `use_repo` calls, with the isolated proxy name when an isolation key is present. The label half differs. Java stores the apparent form with one leading `@` removed, and a root label is rewritten to `@<module repo name>//…`. Zel keeps a leading `@@` and leaves a root label as `//pkg:name`.
  - Commands are a multimap of workspace-relative paths to one buildozer line. Zel holds absolute paths plus a structured command. An include whose file is not in the workspace root is joined onto that file's directory, so the recorded path differs.
  - Zel never runs buildozer. It rewrites `use_repo` itself and only then maps an empty proxy to `_unnamed_usage`. Java appends a buildozer `format` per module file, so comment layout differs even when the repo set matches.
  - The warning is an event at the first proxy. Tidy does not print it. Zel prints `WARNING:` before the info line, so tidy emits the warning Bazel suppresses. A zero column is omitted in Java and always printed in Zel. With no proxies, Java throws before the record exists. Zel uses an empty location.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RunnableExtension.java`
- Bazel lines: 66
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs`
- Verdict: partial
- Differences:
  - Bazel exposes one value the caller can query for eval factors, the transitive digest, and the facts version before `run`. Zel has no shared trait. Regular evaluation can skip the implementation. Innate evaluation always builds repo specs from tags.
  - An innate extension returns an empty recorded-input list and reproducible metadata, with empty eval factors and facts version 0, so it is a lockfile entry. Zel leaves metadata unset and keeps only non-innate ids, so innate repos stay out of the lockfile and are rebuilt on every evaluation.
  - Eval factors use `OS.toString()` and raw `os.arch` (`amd64` on Linux x86_64). Zel maps every non-macOS non-Windows OS to `linux` and every non-`aarch64` arch to `x86_64`.
  - Facts version is a Java int. The caller uses hidden-lockfile facts when the workspace has no entry. Zel stores a `u32` and compares it only with workspace facts, so hidden-only facts never reach the implementation. A version outside signed 32-bit range fails in Bazel. Zel truncates or stores 0, and the extension still runs.
  - `run` returns null when Skyframe must restart. Zel returns a result and caches a success. A `.bzl` that is not loaded yet restarts in Bazel and fails the evaluation in Zel. A missing `extension_metadata` is rejected when the flag requires it. Zel leaves metadata unset and does not read that flag.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/Selection.java`
- Bazel lines: 480
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs`
- Verdict: partial
- Differences:
  - With every module at compatibility level 0 and every dep at max compatibility `-1`, results match: one highest version per name, the multiple-version ceiling, missing and reachable disallowed versions rejected, unreachable disallowed versions kept only in the unpruned graph, and the same BFS prune. An empty version sorts above every other version on both sides.
  - Selection groups are name, compatibility level, and target allowed version. Zel groups only by name and ceiling. Two compatibility levels of one module stay two candidates in Bazel and error if both remain reachable, including when the second edge is only a nodep. Zel upgrades every dependent to the single numeric maximum and succeeds.
  - `max_compatibility_level` tries a cartesian product, lowest compatibility level first, and the first valid graph wins. Zel has one rewrite and returns on the first walk error, so a test that keeps `ccc@1.1` in Bazel keeps `ccc@2.0` in Zel.
  - Multiple-version allow-lists do not snap across compatibility levels. Zel ceilings over every allowed version of that name, rewrites the edge, and succeeds. The same cross-level snap rewrites unpruned edges Bazel leaves unchanged.
  - A second reachable module with the same name and no multiple-version override is rejected, including the root recorded with a null dependent. Zel never checks names, so that graph is kept. A missing old-graph node throws in Java and returns a missing-module error in Zel.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/SingleExtensionEvalFunction.java`
- Bazel lines: 512
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs`
- Verdict: partial
- Differences:
  - Innate extensions still go through the lockfile. Zel only builds repo specs from `repo` tags and never reads or writes lockfile facts or digests. In error mode, a changed innate extension does not produce the outdated-lockfile error, and update does not refresh its entry.
  - Facts prefer the workspace lockfile when the version matches, and fall back to the hidden lockfile only when the workspace map has no entry. Zel reads only workspace facts. A hidden-only record at version 0 becomes empty facts.
  - Reproducible extensions whose workspace facts differ from the hidden lockfile are invalidated, and error mode fails when new facts differ. Zel has neither check, so a stale facts object is reused when digests match, and a real facts change in error mode succeeds.
  - Lockfile info is recorded only for update and refresh. Zel records used and facts for every mode except off, including error, so an error-mode evaluation can rewrite the lockfile.
  - A lockfile hit still generates the fixup but does not print the warning. Zel returns the lock result immediately, so a stable incorrect `use_repo` warns only on the run that executes the extension. On a fresh run Zel prints the warning inside eval, including tidy.
  - This function does not reject missing imports. The later function does, and it caches the eval separately. Zel fails the eval itself, does not cache that failure, and does not treat overrides as generated repos. A raw eval caches first, and a later validating call skips the import check.
  - Recorded-input checks include repo mapping and return "restart" when sky values are missing. Zel ignores repo-mapping inputs, hashes external files immediately, and always scans every check. Eval-factor keys use `os.arch` (`amd64`) versus compile-time `x86_64`, so a Bazel lock entry is a miss.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/SingleExtensionFunction.java`
- Bazel lines: 108
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs`
- Verdict: partial
- Differences:
  - Import failure text matches for a non-isolated extension, except the location is the first proxy of the usage, not the proxy that owns the import. An isolated extension omits the `%module+usage` suffix.
  - A name present in the repo-override map is a legal import even when the extension did not generate it. Zel treats every missing import as an error, including a `use_repo` of a root `inject_repo` name. When that missing name is also overridden, Bazel reports the override error and Zel reports the import error.
  - `inject_repo` of a generated repo, and `override_repo` of a missing repo, are hard errors. Zel never runs that check, so those module files succeed.
  - The fixup warning is emitted for every regular evaluation, and tidy prints it again. Bazel posts it only from this function, so the eval-only key used by `mod tidy` does not.
  - Results are filled only after a successful import check, but a cache hit returns before that check. An earlier raw eval makes a later validating call skip the import check. Bazel keeps the unvalidated value on the eval key and still runs this function. A failed check also drops the eval, so the extension runs again.
  - Spellcheck lowercases with the default locale and UTF-16 units. Zel uses Unicode scalar lowercase. Non-ASCII repo names can get a different suggestion.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/SingleExtensionUsagesFunction.java`
- Bazel lines: 66
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/resolve.rs` (`usages_of`, `repo_mapping`) and `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs`
- Verdict: partial
- Differences:
  - The usages row and the abridged-module filter match when the graph is consistent: both keep only modules that use this extension, in dep-graph order. A name clash inside one module's mapping keeps the Bazel-module dep over an extension import. The lockfile digest drops repo mappings, and Zel hardcodes `"repoMappings":{}`.
  - A missing dep graph returns null so Skyframe can restart. Zel only runs after a finished resolution, so there is no null restart and no per-extension value that change-prunes other extensions.
  - An extension id that is not in the graph makes `getExtensionUniqueNames().get(id)` null, and AutoValue throws before evaluation starts. Zel substitutes `""` and still evaluates the extension with an empty unique name and an empty modules list.
  - An overriding apparent name that is absent from the root mapping is still recorded as a non-visible repository whose name is that apparent name, and that string is written into the usages digest. Zel skips the entry, so the extension repo stays `{unique}+{exported}` and the digest records `""`.
  - Override-map order differs. Bazel stores the usage's overrides in a `HashMap` and returns that row in hash iteration order. Zel stores a `BTreeMap` sorted by exported name. Two or more overrides therefore produce a different `usagesDigest` whenever hash order is not lexicographic. A single-key lookup is the same.
  - An unknown apparent name in the stored full mapping returns a non-visible repo plus a did-you-mean suffix. Zel misses and substitutes `[unknown repo '…' requested from @@…]`, or the raw name for `override_repository`, `bazel_tools`, and `platforms`. Tag label attributes canonicalize to different repos.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/SingleExtensionUsagesValue.java`
- Bazel lines: 132
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/usages_digest.rs`
- Verdict: partial
- Differences:
  - Trimmed lockfile JSON matches on the parts the 9.3.2 fixture covers: proxies, per-usage repo overrides, and tag locations are dropped (the location is the builtin `{"file":"<builtin>","line":0,"column":0}`), `repoMappings` is `{}`, an absent isolation key is omitted, a present key is `module+"+"+proxy` (`<root>` for the root), and the SHA-256 is over UTF-16LE code units.
  - Label tag attributes change the digest. Bazel writes `Label.getUnambiguousCanonicalForm()` with no extra quotes (`@@//pkg:name`, `@@repo//pkg:name`). Zel stores `Label::canonical_form()` (`//pkg:name` for the main repo) and then wraps any `@@…` value in quotes. `AttrVal::to_json` does not wrap labels; the digest path does.
  - Repo-override object order changes the digest. Bazel keeps insertion order from the `override_repo` and `inject_repo` calls. Zel's `repo_overrides` is a `BTreeMap`, so those keys are emitted in sorted order.
  - An overriding repo missing from the root mapping is `""` in Zel. Bazel still yields a non-visible `RepositoryName` whose `getName()` is the apparent name, and that string is what Gson writes.
  - Integers outside Java `int` change the outcome. Bazel throws while building the digest. Zel writes a JSON number for any `i64`, and a larger `BigInt` becomes a JSON string.
  - A float, or any other Starlark value that is not `None`, bool, int, string, label, dict, or iterable, makes Bazel throw from `AttributeValuesAdapter`. Zel emits Rust `f64` display for a float and stringifies anything else.
  - The interned SkyKey and equality of the untrimmed value (proxy imports, locations, per-usage `mustExist`, and `repoMappings`) have no Zel object. Lockfile reuse follows only the trimmed digest, which is the same check Bazel uses in `SingleExtensionEvalFunction`.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/SingleExtensionValue.java`
- Bazel lines: 149
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`
- Verdict: partial
- Differences:
  - Bazel stores one sky value with generated repo specs, a canonical-name bimap, an optional lockfile entry, an optional fixup, facts, and a facts version. Zel keeps generated specs and metadata on the result and puts the lockfile entry, facts, and facts version on the lockfile instead.
  - The eval key is a separate non-hermetic node, so a bad `use_repo` still caches the evaluation. Zel uses one map: a raw eval fills it, and a later validating call returns that hit without import checks. A failed validating call is not inserted, so the extension runs again.
  - Generated specs are a map. A duplicate internal name fails the build. Zel stores a vec, which can hold two specs with the same exported name. Lookups take the first.
  - The canonical name is `unique+internal` for every generated repo, with no override applied, and the name is still validated. Zel uses the same string but does not validate. The extension-repo helper returns the override target when a root override exists, which the Bazel bimap does not. A shorter unique name that is a prefix of another can claim the repo.
  - Lockfile info is present for update and refresh and empty for off and error. Zel records an entry whenever the mode is not off, including error. The fixup warning is emitted only from the validated node. Zel logs it inside evaluation and does not store the fixup.
  - The validated node rejects an import that was not generated unless that name is a repo override, and it rejects a bad inject or override. Zel only checks imports, still fails when the name exists only as an override, and formats the main-repo id with `@@//`. Facts version is a Java int. Zel stores a `u32`, so a negative becomes 0 and a value above `u32::MAX` wraps.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/SingleVersionOverride.java`
- Bazel lines: 68
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - Patches are a list of labels in Java and a list of strings in Rust. Java prints the canonical form, so the main repo is `//pkg:name`. Zel keeps the raw string and only rewrites a leading `@@` when the rest starts with `//`. `@//:p.patch`, `:p.patch`, and `//pkg` stay as written, so override equality and the root-override error text diverge. Those strings are also what later patch application copies onto the repo spec.
  - Patch strip is a Java int. Zel's field is an `i64`, so values outside the signed 32-bit range can be stored. Bazel rejects those before create. A Starlark int that does not fit in `i64` becomes 0 instead of failing.
  - A null version, registry, patches, or patch commands throws. Rust has no null for these fields. Java's lists are immutable. The Rust fields are public vecs. Mutating them changes equality and the patches applied later.
  - Version, including empty (not pinned), registry (empty means no registry override), patch-command order, and an in-range patch strip, including negatives, match.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/StarlarkBazelModule.java`
- Bazel lines: 191
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs` (`eval_regular`) and `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs`
- Verdict: partial
- Differences:
  - The four fields (`name`, normalized `version`, `tags`, `is_root`), one list per tag class, tag order, and the unknown-tag-class sentence with a spelling hint match. `module_ctx.is_dev_dependency` and `module_ctx.tag_sort_key` still order tags by `(module index, tag index)`.
  - `type()` is `bazel_module`, `bazel_module_tags`, and `bazel_module_tag`. Zel's `type_name` is `"struct"` for all three. The module's `repr` still matches. Bazel `tags` prints as an unknown object. Zel prints a real `struct(...)` of the tag-class lists.
  - A missing tag class reads `unknown tag class <field>`. Zel's struct lookup is `'struct' value has no field or method '<field>'` plus the available attributes. `print` and `fail` use `'<class>' tag at <location>` in Bazel, while `str` and `repr` stay the unknown-object text. Zel uses one `struct(...)` for all of those, and that struct also exposes `_dev_dependency` and `_sort_key`, which are not Starlark fields in Bazel. An unknown attribute is `unknown attribute <field>` plus a spelling hint, not the generic struct error.
  - `module_ctx.tag_sort_key` is a `sort_key`: `repr` is `<sort_key>` and `print` is `<sort_key module=N tag=M>`. Zel returns a tuple `(N, M)`. Pairwise order matches.
  - Tag-class lists are frozen. Appending raises `trying to mutate a frozen list value`. Zel's lists start unfrozen, so the same append succeeds. Dict-typed attributes are likewise frozen in Bazel and mutable in Zel. Bazel does not hash these objects. Zel hashes the struct by its fields, so the module can be a dict key.
  - A label whose repo is not visible fails before the extension runs, with `no repository visible as '@<repo>'`. Zel keeps a label whose repo is `[unknown repo '<repo>' requested from @@<canonical>]`. `//conditions:...` and `//visibility:...` are forced into the main repo. Zel resolves them in the using module's repo. Package-relative `foo:bar` is `invalid label 'foo:bar'`. Zel rewrites a value that does not start with `@`, `/`, or `:` to `:<value>`, so `foo:bar` becomes a target named `foo:bar`.
  - `tags` and `visibility` are stripped before validation, as for a repo rule. Bazel reports them as unknown attributes. A tag attribute literally named `tags` is treated as missing during that check.
  - `allow_empty = False` is not enforced in `AttributeUtils.typeCheckAttrValues`. Zel rejects an empty list. An allowed-values failure in Bazel is `the value for attribute '<attr>' has to be one of 'a' or 'b' instead of 'c'`. Zel leaves the repo-rule wording because that string does not match the `in call to '...' repo rule` rewrite.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/Tag.java`
- Bazel lines: 78
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/model.rs`
- Verdict: partial
- Differences:
  - Both keep the tag name, raw keyword attributes, the proxy dev-dependency flag, and the creation site, and both leave schema checks until extension evaluation. For an explicit `tag(name = "a")`, the hashed JSON matches: the dev flag stays, and the location is the builtin zero location.
  - Keyword order differs. Bazel stores a dict in insertion order. Zel appends `**dict` keys from a sorted map, so `tag(**{"b": 1, "a": 2})` hashes in sorted key order.
  - Integers that do not fit in `i64` become strings. Values in `[2^31, 2^63)` get a JSON number and a digest in Zel, and abort the hash in Bazel because `toInt` throws outside signed 32-bit.
  - A float keyword is stored and written into the usages JSON. The Java attribute adapter has no float case and throws on that same hash.
  - Location is one `file:line:column` string that always prints the column. Bazel keeps a triple and omits a zero column. Tag type-check tracebacks split that string on `:`, so a module path that contains `:` gets the wrong file, line, and column.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/TagClass.java`
- Bazel lines: 51
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs` (`tag_class`, `attrs_from_kw`)
- Verdict: partial
- Differences:
  - `create` indexes attributes by the public name (`$` and `:` names become `_…`) and throws when two attributes share that name, for example `$foo` and `_foo`. Zel stores the raw dict key and keeps both entries. Later lookup uses the first match.
  - The record keeps `Optional<String> doc`, which is part of equality and what Stardoc reads. The `tag_class` arm never reads `doc`. The record holds a full `Attribute` list. Zel keeps only `kind`, `default`, `mandatory`, `values`, and `allow_empty`, so per-attribute docs and the rest of the schema are gone.
  - `Dict.cast` rejects a non-dict and a non-`Descriptor` value. A missing or non-dict `attrs` becomes an empty schema in Zel. A non-struct value fails with `attrs must be created with attr.*()`. The `attrs` parameter is positional in Bazel. The `tag_class` arm reads only keywords, so a positional dict is dropped. Extra keywords are rejected by Bazel's parameter binding and ignored here. `doc` is keyword-only in Bazel.
  - `type()` is `tag_class`, `repr` is the unknown-object text, the value is truthy, and it is unhashable. Equality is structural over the attribute list, `doc`, and the index. Zel's `repr` matches and the value is truthy, but `type()` is `TagClass`, every host value is hashable, and `==` compares the allocation index, so two `tag_class()` calls with the same attrs and doc compare equal in Bazel and unequal in Zel. Ordering fails on both sides, with those two type names in the error.
  - Attribute-index key order is insertion order and is the only set of tag fields. Zel keeps that vec order, then `build_attr_struct` puts fields in a `BTreeMap`. The tag struct also gains `_dev_dependency` and `_sort_key`, which are not tag-class attributes in Bazel.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/TypeCheckedTag.java`
- Bazel lines: 166
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs` and `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs` (no `TypeCheckedTag` type)
- Verdict: partial
- Differences:
  - `type(tag)` is `bazel_module_tag`. Zel builds the tag with `StructValue::new_attr`, so `type(tag)` is `RepoDefinition`. `str`, `repr`, `print`, and `fail` all use that opaque repr. Bazel `print` and `fail` use `debugPrint` (`'install' tag at file:line:col`), which is what `fail("Conflict between", tag1, "and", tag2)` depends on. `str` and `repr` stay the unknown-object text.
  - `dir(tag)` includes `_dev_dependency` and `_sort_key`. Those overwrite a real attribute of either name. Bazel's field set is only the tag-class attributes. An unknown name is `unknown attribute <name>` with no spelling hint. Bazel adds `didYouMean` over the attribute names.
  - `tag_sort_key` returns a tuple `(module_idx, tag_idx)`. `module_idx` is the index among modules that use this extension, not the full-graph BFS index stored in `SortKey`. Pairwise order matches when dep-graph order matches that BFS order, but `print` and `repr` show different numbers, `type()` differs, and `<` against a non-tuple succeeds in Zel and raises `unsupported comparison: sort_key <=> …` in Bazel. Zel's `sorted` ignores `key`, so `sorted(tags, key=lambda tag: module_ctx.tag_sort_key(tag))` does not order by the key.
  - `is_dev_dependency` reads `_dev_dependency` with Starlark truthiness and returns `False` for any struct that lacks the field. Bazel accepts only a `bazel_module_tag` and returns the stored boolean.
  - Validation is the repo-rule checker, rewritten to `in '<tag>' tag, …` only when the message starts with `in call to '<tag>' repo rule with name '`. That matches a plain unknown attribute, a missing mandatory attribute, and a generic scalar type error. It does not match bool (`one of [False, True, 0, 1]` versus `value of type 'boolean'`), allowed values (Bazel's `has to be one of` versus an unrewritten repo-rule sentence), or `int` outside signed 32-bit (`Type.INTEGER` rejects; Zel keeps any `i64`).
  - Undeclared `name`, `tags`, and `visibility` are accepted and omitted from the struct. Bazel reports them as unknown attributes. A declared `tags` or `visibility` is removed before validation, so a mandatory one is reported missing even when it was passed, and a bad type is still copied into the struct. A declared `repo_mapping` is deleted from the schema, so providing it is an unknown-attribute error.
  - A label whose apparent repo is not in the module repo mapping becomes `[unknown repo '…' requested from @@…]`. `TypeCheckedTag.create` fails with `no repository visible as '@…'`. Assignment fails on both sides, with `bazel_module_tag value does not support field assignment` versus `cannot assign to attribute`.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/VendorFileFunction.java`
- Bazel lines: 196
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/vendor.rs` (`VendorFile::load`, `load_or_init`, `parse_at`)
- Verdict: partial
- Differences:
  - A missing `VENDOR.bazel` is created with the same ASCII header and yields empty ignore and pin sets only on the vendor command, and only after a non-`bazel_tools` repo is considered. Bazel creates it on every evaluation. Fetch uses `load`, which does not create the file.
  - A failed create is ignored and the call still succeeds. Bazel fails transiently with `error creating VENDOR.bazel file`. Fetch turns any read or parse error into an empty file, so a bad `VENDOR.bazel` does not fail the build. Bazel fails the `VendorFileValue` node.
  - Invalid UTF-8: Bazel's default `--incompatible_enforce_starlark_utf8=warning` warns and parses the bytes as Latin-1. Zel `read_to_string` fails. In error mode Bazel reports a persistent `error reading VENDOR.bazel file` and drops the UTF-8 detail.
  - Even for valid UTF-8, Bazel builds Starlark strings from raw bytes and then rejects a repo name outside `[A-Za-z0-9_.+-]`, including `.` and `..`. Zel keeps the decoded Unicode string and never checks the charset, so `ignore("@@café+")` or `ignore("@@foo bar")` succeeds there and fails in Bazel. ASCII names that match that pattern agree, including stripping `@@` and treating `@@` as the empty main-repo name.
  - The vendor environment is only `ignore` and `pin` plus the Starlark universe. Zel also predeclares `json` and `proto`, and its universe includes `chr` and `ord`, which this Bazel method library does not. Those names evaluate in Zel and are undefined in Bazel.
  - Parser failures are close: each `syntax error at …` is reported, then `error parsing VENDOR.bazel file`. Bazel's event text is `ERROR <file>:<line>:<col>: …`. Zel prints `ERROR: …`. `load`, `def`, `for`, `if`, `*args`, and `**kwargs` errors are not classified as syntax, so Zel keeps only the first message and drops SyntaxError's `(+ N more)`.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/VendorFileValue.java`
- Bazel lines: 40
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/vendor.rs`
- Verdict: partial
- Differences:
  - Both hold an ignored set and a pinned set of bare canonical names. Membership matches, and a repo listed in both sets is allowed in both.
  - Bazel is a sky value with one hermetic key. Zel has no skyframe node. Fetch loads the file on every repo, so a rewrite during a run is visible to later fetches. Bazel keeps one snapshot until the node is invalidated.
  - List equality is order- and duplicate-sensitive. Set equality is not. Reversing two ignore names, or listing one name twice, changes the sky value and does not change the Rust value. Membership is unchanged.
  - Bazel stores repository names that already passed validation. Zel stores the raw string after `@@` and never validates. `ignore("@@..")` or `ignore("@@foo/bar")` never becomes a vendor-file value. Zel records those strings and later membership hits them.
  - A null list throws. Rust has no null list. Empty inputs match.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/VendorManager.java`
- Bazel lines: 236
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/session.rs` (`run_vendor`) and `/workspace/zel-cursor/crates/zel-bzlmod/src/vendor.rs` (`replant_symlinks`)
- Verdict: partial
- Differences:
  - Bazel skips a repo when `external/<repo>` is a symlink whose target is `vendor/<repo>`, or when the vendor marker's UTF-8 text equals the external marker (or, when that marker is absent, the cache entry's recorded inputs). Zel always copies over the vendor directory and never reads `.recorded_inputs`. On a repo-contents-cache hit Zel rewrites the external marker to `"{fingerprint}\n"`, and that short text is what vendor stores. Bazel stores the full recorded-inputs marker, so later staleness checks see different inputs.
  - For a repo that is not served from the contents cache, Bazel moves the marker aside, renames the tree (on failure it chmod's the source to `0755` and leaves the emptied source directory), replants symlinks, then puts the marker back. It then deletes the external path (a symlink is removed, the cache target is not) and symlinks it at the vendor dir. The external marker is gone. Zel copies into a temp dir, renames that onto the vendor dir, and writes the vendor marker only when `read_to_string` succeeds. Invalid UTF-8 makes Bazel throw and abort that repo. Zel leaves the previous vendor marker in place and still swaps the tree, and it leaves the external marker on disk. `std::fs::copy` does not keep mtime. Bazel's copy keeps attributes, so mtime-based freshness of vendored files diverges. Failures while creating the external symlink or writing the marker are ignored in Zel.
  - Both plant `<vendor>/bazel-external` and `<external>/_main` and rewrite absolute symlinks into the main repo, the same repo, or another external repo to the same relative shape. Zel refreshes `bazel-external` at the start of every vendor command. Bazel plants it only inside a repo that is actually copied, so an output-base change with every repo already symlinked leaves a stale `bazel-external`. Zel drops a directory it cannot list and continues. Bazel fails the repo. On non-Unix, Zel removes the old symlink and does not compile the replacement `symlink`, so those links are deleted.
  - Registry files are `<vendor>/_registries/<host>/<path>`. Bazel lowercases the host, drops userinfo, port, query, and fragment, strips one leading slash, then URL-decodes. That decoder turns `+` into a space and percent-decodes again, so both `a+b` and `a%2Bb` become `a b`. Zel joins `_registries` with the raw substring after `://`, so host case, port, userinfo, query, and `%XX` stay in the path. Two URLs that differ only by port or host case collide in Bazel and not in Zel.
  - Bazel writes the bytes it was given and creates parent directories, throwing if that fails. A second vendor leaves an existing file. Zel re-fetches and overwrites. A registry that changed between resolution and vendor is stored unverified, and a download or write error is skipped while the command still succeeds. `file:` URLs and entries with no checksum are skipped on both sides.
  - Bazel's offline read hashes the vendored bytes and throws `Checksum was %s but wanted %s`, formatted in the checksum's own form (SRI or hex). Zel's registry fetch never opens `_registries`, so vendor mode still uses the network or the repository cache, and a checksum mismatch there is only the hex failure path.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/YankedVersionsFunction.java`
- Bazel lines: 63
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/registry.rs` (`yanked_versions`), called from `resolve.rs`
- Verdict: partial
- Differences:
  - A missing or blank `modules/<name>/metadata.json` is fail-open in both, and the yanked check is skipped. `metadata.json` is fetched without a checksum in both, so lockfile hash enforcement does not apply to it.
  - Transport and other I/O failures are fail-open only in Bazel. `compute` catches every `IOException`, warns `Could not read metadata file for module %s from registry %s: %s`, and returns empty, so a yanked module is still allowed. Zel propagates the fetch error and resolution fails.
  - An unparsable yanked-version key is fail-open only in Bazel: the parse error is wrapped, caught, and turned into the same warning plus an empty result. Zel returns an error and resolution fails. One bad key drops the whole yanked map in Bazel and aborts resolution in Zel.
  - A non-string `yanked_versions` value makes Gson refuse the string map, so Bazel warns and fail-opens the entire file. Zel uses an empty reason and keeps the entry, so the selected version is treated as yanked unless that version is allowed.
  - If `yanked_versions` is present but not a JSON object, Gson fails the file and Bazel warns and fail-opens. Zel ignores the field and returns an empty map (known "nothing yanked", no warning). The selected version is allowed either way. Strict JSON that both parsers reject allows the module, but Zel's stored warning already starts with `WARNING:` and the resolver prefixes `WARNING:` again.
  - Version keys that normalize to the same version (`1.0+aaa` and `1.0+bbb`) make `ImmutableMap.Builder.buildOrThrow()` throw. That is not an `IOException`, so the SkyFunction crashes. Zel's `BTreeMap` keeps the last reason and enforces that yanked version.
  - A Skyframe restart when the registry value is missing, the `BZLMOD` profile span, and `DownloadManager` injection have no Zel equivalent. They do not change the yanked map when the registry is already available.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/YankedVersionsUtil.java`
- Bazel lines: 114
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/session.rs`
- Verdict: partial
- Differences:
  - A well-formed comma-separated `name@version` list, the exact token `all`, and the union of the flag with the environment variable all allow the same yanked modules. Bazel stores parsed module keys. Zel keeps trimmed raw strings and sets allow-all when any token is `all`.
  - Bazel does not trim. A padded token and a whitespace-only entry fail. Zel trims each piece and drops blanks, so those inputs are accepted.
  - Bazel returns as soon as it sees `all`, and it reads the environment variable before the flag. Tokens after `all` are ignored. Zel validates every token, so `all` does not suppress a later bad token.
  - Bazel rejects a module name that is not a Bazel module name, and a version that does not parse, including `name@_`. Zel only requires an `@`. `Foo@1.0` and `foo@1..0` are kept as strings that never equal a resolved key, so the command continues and the yanked-version check fails later.
  - Version parse drops `+build`, so `foo@1.0+build` allows yanked `foo@1.0`. Zel compares the raw string to the display form, so that token does not allow `foo@1.0`. An environment error names the variable and the whole value. Zel always reports the flag and one trimmed token, including for environment input. Bazel exits 48. Zel exits 37.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/YankedVersionsValue.java`
- Bazel lines: 66
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/registry.rs`
- Verdict: partial
- Differences:
  - The three-way payload matches for normal metadata: missing, blank, or unparseable metadata is not yanked. A parsed object with no yanked versions is an empty map. A present entry is a yank reason. Resolution only looks up the selected version.
  - Modules with no registry are stored as none-yanked in Bazel and skipped in Zel. Both leave the module unyanked.
  - There is no interned key of module name and registry URL. Bazel computes one module-wide map per registry. Zel re-reads metadata once per selected version, so a file that changes mid-resolve can yank one version and not another.
  - A bad version string fails open in Bazel: the parse becomes an I/O error, and the function stores empty and warns. Zel returns an error and aborts resolution.
  - A non-string yank reason fails Gson's string map and becomes not yanked. Zel stores `""` and treats that version as yanked with an empty reason. Two JSON keys that normalize to the same version throw in Bazel. The sorted map keeps one entry.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/modcommand/ExtensionArg.java`
- Bazel lines: 149
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/args.rs` and `/workspace/zel-cursor/crates/zel-options/src/converters_ext.rs`
- Verdict: partial
- Differences:
  - The converter split matches: the first `/` separates the module argument, the first `%` must be at or after that slash, the label is `[slash, percent)`, and the extension name is the remainder, including an empty name. `toString` is `module + label + "%" + name`. Both always build a non-isolated id.
  - Resolution still requires exactly one module key and passes unused modules through without a warning. On the happy path `//pkg:file.bzl` becomes `@@{canonical}//pkg:file.bzl` (an empty root canonical name yields `@@//pkg:file.bzl`), and `//pkg` still defaults the target to the last package segment.
  - Java resolves the label with an empty repo mapping and the module's canonical repo, and stores a `Label`. Zel only accepts a string that starts with `//`, checks that the target is non-empty, and concatenates `@@{repo}` with that string. It does not apply Bazel's package or target rules.
  - `//foo/...` and `//foo/...:all` throw in Java because the package contains `...`. Zel returns `@@repo//foo/...:...` and `@@repo//foo/...:all`. `//pkg:foo/.` is rewritten to target `foo` in Java and stays `foo/.` in Zel. `//foo//bar`, `///pkg:x`, `//pkg:a:b`, `//foo/./bar`, package names containing `\` or non-ASCII, and targets Java rejects become a successful Zel id.
  - Packages exactly named `conditions` or `visibility` are forced into the main repo even when the module's canonical name is not empty. Zel always prefixes the module repo, so `<module>//visibility:public.bzl%ext` does not name the same extension.
  - A stored label that does not start with `//` is also different. `@@other//pkg:file.bzl` is a canonical label in Java and ignores the module repo. `@foo//pkg:file.bzl` becomes a non-visible repo because the mapping is empty. Zel rejects both. The CLI converter cannot produce these, because the label slice always starts at the first `/`.
  - If the module key has no canonical-name entry, Java throws inside `RepositoryMapping.create`. Zel uses `""` and emits a main-repo label `@@//…`.
  - When several versions match, both throw the same "exactly one module version" message, but Java prints the set in dep-graph encounter order and Zel prints sorted `ModuleKey` order, so the `Choose one of:` list can differ.
  - An empty comma-separated list returns an empty list in Java (the documented "disable the filter" value). `ExtensionArg::parse_list("")` returns `Empty values are not allowed as part of this comma-separated list of options`. The options checker matches Java for `""`, and `ModOptions` turns an all-empty raw value into an empty vec before `parse_list`, so `--extension_filter=` still disables the filter. A direct `parse_list("")` does not. `","` and `"a,,b"` fail in both with that same empty-element message.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/modcommand/GraphvizOutputFormatter.java`
- Bazel lines: 166
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/graphviz.rs`
- Verdict: partial
- Differences:
  - The usual DOT text matches: header, breadth-first walk over key-sorted children, root label, diamond and dotted targets, dashed indirect edges, `label=MVS|SVO|MVO|NRO` only when verbose, usage edges, and repo clusters, including leaving `subgraph` unclosed.
  - A key missing from the dep graph or the result map throws before any line is printed, so Bazel emits nothing. Zel skips that node and still prints a `digraph`, dropping the node and anything reached only through it.
  - Extension edges and clusters are sorted by bare repo name, then the package as one path string, then the target name. Zel orders packages as `/`-separated segments. `@@//foo-bar:e.bzl%ext` is before `@@//foo/bar:e.bzl%ext` in Bazel (`'-' < '/'`) and after it in Zel, so subgraphs and edges swap.
  - The same label tie-break, and any other sorted string that contains a supplementary-plane character, uses Java UTF-16 code units versus Rust byte order. `foo` plus U+1F600 sorts before `foo` plus U+E000 in Bazel and after it in Zel. Valid module and repo names are ASCII, so this shows up on label package or target names.
  - A missing parent, dep, or resolution reason throws while formatting a direct edge. Zel turns that into an empty label, so the edge is still printed as `[ ]`.
  - Body newlines are `\n` on both sides. The final newline is `System.lineSeparator()` (`\r\n` on Windows). Zel always appends `\n`.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/modcommand/JsonOutputFormatter.java`
- Bazel lines: 169
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/json.rs`
- Verdict: partial
- Differences:
  - The depth-first walk matches: cycles, then indirect edges, then direct edges; `unexpanded` cuts the node off; `<root>` keys; `dependencies`, `indirectDependencies`, and `cycles`; extension modes `hidden`, `usages`, `repos`, and `all`; the first visit of an extension id is expanded and later visits set `unexpanded`. `--verbose` writes `unused` plus `resolvedVersion` or `originalVersion`, and both sides store the changed version string in `resolutionReason`. A null apparent repo name is omitted. Repo lists are sorted strings. On UTF-8 Linux the pretty printer matches Gson: two-space indent, `": "`, empty `[]` and `{}`, and a single trailing newline.
  - `extensionUsages` order differs. Java sorts packages with one string compare. Zel splits the package on `/` and compares segments. Packages `foo-bar` and `foo/bar` (same for `.` and `+`, which sort before `/`) come out in opposite orders.
  - Gson still writes U+2028 and U+2029 as `\u2028` and `\u2029`. Zel emits the raw characters. Other controls and `<`, `>`, `&`, `=`, and `'` match.
  - `--charset=ascii` builds the writer as US-ASCII, so non-ASCII in the JSON is replaced with `?`. Zel's JSON is always Unicode. `charset` only changes text-tree glyphs.
  - The record separator is `System.lineSeparator()` (`\r\n` on Windows). Zel always appends `\n`. Interior newlines stay `\n` on both sides.
  - A missing dep-graph or result entry throws in Java. Zel fills an empty name and version and drops a missing apparent name.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/modcommand/ModExecutor.java`
- Bazel lines: 792
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/executor.rs`
- Verdict: partial
- Differences:
  - With `--charset=ascii`, Bazel wraps the stream in a US-ASCII writer, so any unmappable character in graph, path, explain, or `show_extension` text is written as `?`. Zel keeps the original Unicode and only swaps the tree-drawing glyphs.
  - If a path search records a target and never inserts `<root>` (a `from` module dropped by the builtin filter), Bazel throws on the missing root. Zel inserts an empty `<root>` and `path` and `all_paths` print that graph. A `from` key absent from the dep graph is skipped. Bazel throws on `depGraph.get`.
  - `show_extension` order compares package paths segment by segment. Bazel sorts with repository name, then the package path as one string, then the target. `@@r//a-b:e.bzl` and `@@r//a/b:e.bzl` come out in opposite orders.
  - `tagToFunctionArgs` prints `Starlark.repr`. Zel's attribute repr changes the tag text: a label is a quoted string where Bazel prints `Label("…")`; a non-finite float is `inf` or `NaN` where Bazel prints `+inf` or `nan`; scientific form is `1e20` or `1e-7` where Bazel prints `1e+20` or `1e-07`; a string control character below 32 other than newline, tab, or carriage return stays raw where Bazel prints `\xHH`; a tuple is printed as a list where Bazel prints `(…)`; an int that does not fit in `i64` is stored as a string and printed quoted.
  - With an empty proxy list, Bazel throws on `getProxies().getFirst()`. Zel prints `## Usage in <module> from ` with an empty location.
  - Module-key order uses UTF-16 code units in Java and Unicode scalar values in Rust, so a name with a supplementary-plane character can sort differently. Ordinary BMP names match the lexicographic comparator.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/modcommand/ModOptions.java`
- Bazel lines: 307
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/mod.rs`
- Verdict: partial
- Differences:
  - Flag defaults match: `from=<root>`, `verbose=false`, `include_unused=false`, `extension_filter` unset, `extension_info=hidden`, `base_module=<root>`, `extension_usages` empty, `depth=-1`, `cycles=true`, `include_builtin=false`, `charset=utf8`, `output=text`, `all_repos=false`, `all_visible_repos=false`. Enum parsing is ASCII case-insensitive. An empty `--extension_filter` is "all extensions"; unset is no filter. `depth < 1` becomes 1 for `explain`, 2 for `deps`, and `Integer.MAX_VALUE` or `i32::MAX` otherwise. Subcommand matching and `isGraph` are the same. `--depth` accepts `0x` and leading-zero octal. Booleans accept `true`, `1`, `yes`, `t`, `y`, and the `--no` form.
  - `--charset=ascii` wraps every formatter that shares the executor's writer (`text`, `json`, `graph`, `show_extension`, and text or jsonproto `show_repo`) in a US-ASCII writer. A non-ASCII character fails that write, and the writer swallows the `IOException`. Zel only swaps the box-drawing strings in the text formatter and still emits Unicode for every format.
  - `--from`, `--base_module`, and `--extension_usages` reject numeric version segments that do not fit in `i64`. Bazel accepts unsigned segments through `18446744073709551615`.
  - `@@.` and `@@..` are rejected on both sides, but the message differs. Bazel says `repo names are not allowed to be '.'` or `'..'`. Zel's options parser reports the character-class error and never reaches the parser that has the Bazel wording.
  - `getDefaultOptions` is a test object: `depth=Integer.MAX_VALUE`, `cycles=false`, and `baseModule` and `extensionUsages` left Java-null. Zel's default is the annotation defaults (`depth=-1`, `cycles=true`, `base_module="<root>"`, `extension_usages=[]`). That does not change a parsed `mod` command line.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/modcommand/ModuleArg.java`
- Bazel lines: 420
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/args.rs`
- Verdict: partial
- Differences:
  - The four forms (`<root>`, `<module>@<version>` or `_`, `<module>`, `@name`, `@@name`), unused-module checks, and the success-path messages match.
  - Positional version errors append the cause. Zel yields `error parsing version: 1..2: identifier is empty` and `error parsing version: <n>: numeric version segment is too large: <n>`. Bazel's message is only `error parsing version: <n>`. The options converter's version check matches Bazel's shorter text, so `--from` and `--base_module` differ from a bare argument.
  - A numeric segment in `2^63 .. 2^64-1` is a valid Bazel unsigned long and a valid positional `ModuleArg::parse` (`u64`). The flag converter uses `i64`, so the same version is rejected on a flag and accepted as a positional argument.
  - `@@.` and `@@..` on a flag get the charset error. Bazel and positional validation say `repo names are not allowed to be '.'` or `'..'`.
  - `@` and `@@` failures that contain C0 or DEL characters are escaped by Bazel (`\r` to `\\r`, other controls to `<?>`). Both Zel parsers keep the raw characters.
  - `ModuleArg::parse_list("")` errors (`split(',')` yields one empty piece). The comma-separated converter turns that single empty string into an empty list. `ModOptions` drops an all-empty `--from` or `--extension_usages` before `parse_list`, so that flag path matches. A direct `parse_list("")` does not.
  - "Available versions" and the resolved key list follow sorted `ModuleKey` order. Bazel's set keeps dep-graph encounter order, so the error text and any consumer that does not re-sort differ when more than one version exists. The set of keys is the same.
  - If two modules share a canonical repo name, Bazel `findAny()` follows hash iteration and Zel takes the first sorted key.
  - A modules-index key missing from the dep graph makes Bazel throw on `isUsed()`. Zel treats it as unused, so it is omitted from "available versions" and can change the unused-versus-missing error.
  - Resolving repo names with no canonical name returns `""`. Bazel throws on a null map value.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/modcommand/OutputFormatters.java`
- Bazel lines: 170
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/executor.rs`
- Verdict: partial
- Differences:
  - On a complete dep graph, verbose labels match. `<root>` and an original edge produce no explanation. A replaced edge records the other version, with the empty version printed as `_`. Minimal-version selection sets the requester list to the original dependants and joins it in stored order with `", "`. Any other reason uses the lowercased enum name. The text is `(to|was <version>, cause <reason>)`, with `to` when the child is unused.
  - `getFormatter` throws for `streamed_proto` and `streamed_jsonproto`, and a null format throws `Output format cannot be null.` Zel maps every format other than JSON and Graph to the text formatter, so those two values would print a text tree. Graph subcommands reject them first in both trees, so a normal `mod deps`, `graph`, `path`, `all_paths`, or `explain` invocation does not reach this branch.
  - A missing parent, child, repo name, dep-reason entry, or replacement module throws in Bazel, including when the unused-inclusive dep map has no entry or `resolutionReason` is null. Zel returns none, and the verbose annotation is omitted while the rest of the node is still printed.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/modcommand/RepoOutputFormatter.java`
- Bazel lines: 169
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/repo.rs`
- Verdict: partial
- Differences:
  - Text for a found repo and for a builtin or override matches the `## <key>:` / `load(...)` / `name` / `_original_name` skeleton, and proto output still drops overrides and uses the same `@` / `@@` / module-key split. Attribute rendering and proto attribute messages do not match.
  - Text attribute values go through `Starlark.repr`. A label prints as `Label("//pkg:t")` or `Label("@@repo//pkg:t")`. Zel quotes the label string (`"//pkg:t"`), so label and label-list attributes differ. `select({...})` has no attribute form.
  - Other controls below U+0020 are `\xHH` in Bazel. Zel only escapes `"`, `\`, newline, carriage return, and tab, so the other controls are raw characters.
  - Text does not convert internal bytes to Unicode. Bazel keeps attribute strings as Latin-1 raw bytes, so UTF-8 `ä` is the two characters U+00C3 U+00A4 and the UTF-8 writer emits `Ã¤`. Zel prints the Unicode scalar `ä`. With `--charset=ascii`, Bazel replaces non-ASCII with `?`. Zel always returns a Unicode string.
  - Starlark float repr is `%.17g` with a forced decimal or exponent (`1.0`, `nan`, `+inf`). Zel uses Rust debug formatting. Integers outside signed 32-bit make Bazel's proto path throw. Zel stores `i64` and writes that varint into the `int32` field.
  - Proto walks every rule attribute in declaration order, fills the default when the value was omitted, and sets `explicitlySpecified` from the explicit map. Zel walks only the explicit attributes, always sets `explicitlySpecified` true, and never emits defaults. A null value still gets a typed attribute with no value field. Zel skips `None` entirely.
  - Bazel sets the real discriminator (`LABEL`, `LABEL_LIST`, `INTEGER_LIST`, `STRING_LIST_DICT`, `LABEL_DICT_UNARY`, `TRISTATE`, `SELECTOR_LIST`, `LICENSE`, and so on). Booleans also set `string_value` to `"true"` or `"false"` and `int_value` to 1 or 0. Tri-state also sets int and string. `nodep` is false for string and label dep types and true for `NODEP_LABEL` and `NODEP_LABEL_LIST`. Zel types strings and labels as `STRING`, every list as `STRING_LIST`, and every dict as `STRING_DICT`. It sets only `boolean_value`, always sets `nodep` false, and turns non-string list or dict entries into `""`. There is no `double_value` on the query attribute, so a float attribute panics.
  - Label strings in proto are `//pkg:t` in the main repo and `@@repo//pkg:t` otherwise, after the Latin-1 conversion. The bzl label is the unambiguous canonical form (`@@//pkg:file.bzl`, or `@@[unknown repo '…']//…` when the repo is not visible). Zel only rewrites a leading `@`, `@@`, or `//`.
  - An unknown output format throws. Zel has no dispatcher; the caller rejects unknown formats earlier. Delimited proto swallows `IOException`. JSON uses `System.lineSeparator()`. Zel appends `\n` and returns the bytes with the same varint length prefix.

### `src/main/java/com/google/devtools/build/lib/bazel/bzlmod/modcommand/TextOutputFormatter.java`
- Bazel lines: 259
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/text.rs`
- Verdict: partial
- Differences:
  - On a complete graph the tree matches: ASCII and UTF-8 glyphs, ancestor indents versus the current edge, `<root> (name@_ )` when the version is empty, `name@_` keys, `(cycle) `, `(*) `, `# `, `(unused) ` only when not verbose, verbose `(was|to <version>, cause …)` text, extension lines (`$id`, `...` only for a repeat under `ALL`), used repos then indirect unused repos, and a depth-first walk that treats a node already on the path as a cycle and does not expand it.
  - A missing result node throws before any line is printed. Zel prints `<root>` or `name@version` and returns without children.
  - A missing root module throws. Zel prints `<root> (@_)` and, when not verbose, `(unused)`. A later missing module is the same split: Bazel throws, and Zel treats it as unused and still walks children.
  - Extension siblings are ordered by repository name, then the package path as one string, then the target name. Zel compares package segments split on `/`. `@@r//foo/bar:e.bzl` and `@@r//foo-bar:e.bzl` come out in opposite orders, so the text tree order changes.
  - Bazel writes the buffered newline-separated text through the executor's writer (UTF-8, or US-ASCII when `--charset=ascii`, which replaces characters the ASCII encoder cannot map) and `println` adds `line.separator`. Zel appends the Unicode string and a final `\n` only.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/AttributeUtils.java`
- Bazel lines: 162
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs` (`validate_repo_rule_call`, `check_repo_rule_labels`)
- Verdict: partial
- Differences:
  - Bazel returns one converted Starlark value per schema slot, with defaults filled in and `None` omitted. Zel's validator only returns unit. A repository-rule spec keeps the raw kwargs, including `name`. Bazel's instantiate stores only explicitly passed, already converted values.
  - `None` is skipped on both sides, then the default is used. Bazel's default for an omitted label is Java null. Zel uses none for a label and `""`, `0`, `false`, `[]`, or `{}` for the scalar, list, and dict kinds.
  - Unknown-attribute text matches, including a spelling hint. Bazel suggests against the attribute index only. Zel also suggests `name`. The tie-break follows candidate order, and the max edit distance uses UTF-16 length versus Unicode scalar count, so a non-ASCII name can change whether a hint is offered.
  - Bazel reports the first bad kwargs entry in call order. Zel reports unknown attributes in call order only when every order key survived filtering. If the call passed `tags` or `visibility`, it falls through to sorted-map order. Type errors always walk that sorted map, so an alphabetically earlier attribute wins over an earlier keyword.
  - Bazel drops `name`, `tags`, `deprecation`, and `visibility` before this function. Zel drops only `tags` and `visibility` and special-cases `name`. A passed `deprecation` is an unknown attribute in Zel.
  - Boolean failures that are not `False`, `True`, `0`, or `1` say `expected one of [False, True, 0, 1]`. Zel still says `expected value of type 'boolean'`. Bazel rejects integers outside signed 32-bit. Zel's int is `i64`, so `2**31` is accepted. A value that does not fit in `i64` is stored as a string and fails as a string, not as a range error.
  - List and dict context differs. Bazel says `element {i} of attribute '…'` and type-checks both dict sides. Zel says `list element` with no index, does not check dict keys, and for a bad string-list-dict value reports `string` rather than `list(string)`. An int key `1` becomes `"1"`, so the type error disappears and it can overwrite the string key `"1"`.
  - Bazel accepts any iterable, including a depset, as a list. Zel accepts lists and tuples and turns any other value into a string, which then fails the list check. Label-keyed dict keys that canonicalize to one label are a conversion error in Bazel. Zel does not detect that.
  - Allowed values run on the converted native value for every type, before the mandatory-attribute pass and in kwargs order. Zel checks `values` only for strings, only after every type error, and with the repo-rule wording. For tags, only the `in call to '…' repo rule` prefix is rewritten, so this sentence stays in the repo-rule wording. Zel also rejects `allow_empty = False`. This class never reads that flag.
  - Mandatory text matches for tags and for a repo rule whose `name` is a string. If `name` is missing, Bazel omits the name clause. Zel still prints `with name ''`.
  - Non-visible labels are checked after conversion, including defaults, in schema order. Zel does not run the label check for tag classes. Extension `repository_rule` calls pass no owner, which disables the check. `use_repo_rule` checks only raw strings that start with a single `@`, only on supplied values, and only for a subset of label kinds. The message keeps the source text, and attribute order is sorted. An unknown apparent repo becomes a successful `[unknown repo '…']` result instead of failing inside type-checking. Relative labels such as `foo/bar:quux` fail Bazel's package-context parse. Zel rewrites a bare string to `:{s}` and can accept a colon in the target. `output` is not constrained to the current package.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/DigestWriter.java`
- Bazel lines: 225
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs` (`predeclared_input_hash`, marker read and write)
- Verdict: partial
- Differences:
  - The predeclared hash is not the same value. Bazel mixes marker version 7, the stable language fingerprint, `Label.toString()` of the `.bzl` (the main repo is `//pkg:file.bzl`), the rule name, the transitive `.bzl` digest, the definition name, one JSON object of the type-checked attribute dict (defaults included, `None` as JSON null, labels as `@@` strings, dict insertion order), `os.name` and `os.arch` lowercased with the root locale, declared environment values, then load-time repo-mapping table cells. Zel writes version 8, a hardcoded flag fingerprint, always `@@repo//pkg:file`, Starlark repr of attributes sorted in a map with `name` and `None` dropped, a Rust OS and arch table, then mappings re-lexed from `load()` statements.
  - On Windows, Java `os.name` is `windows 10` or `windows 11`. Zel always records `windows`. Any OS other than Linux, macOS, and Windows is recorded as `linux`. `os.arch` matches for the common `amd64`, `aarch64`, and macOS `x86_64` cases only.
  - Bazel returns null from the hash computation when the Skyframe view is missing so the caller restarts. Zel hashes an empty env list if the `.bzl` cannot be evaluated, and an unreadable `.bzl` becomes `sha256("native")` (or a stored fallback) instead of the loaded transitive digest.
  - The marker stores the hash plus every recorded input (environment, file, directory entries, directory tree, repo mapping), ISO-8859-1, one line each. Zel drops the rule-body recordings. The marker is only `{hash}\n` in UTF-8. A later fetch is reused whenever that hash matches, so watched files, directory listings, env reads, and repo mappings from the rule body never invalidate the repo.
  - A bad line rejects the whole marker (same outcome as a hash mismatch). Zel ignores lines that are not `ENV:`, `FILE:`, or `DIRENTS:`, so a Bazel marker's directory-tree or repo-mapping changes stay current. A byte at or above 0x80 is invalid UTF-8, so reading the marker fails and Zel refetches.
  - External `@@repo//…` paths are statted only if that repo directory already exists. Bazel requests the file through Skyframe and can fetch that repo first. Directory-entry names use lossy UTF-8, so non-UTF-8 entries collapse to U+FFFD. Bazel I/O failures become a transient repository exception. Zel turns a marker read error into "not current" and ignores remove errors. The reason string is discarded. Only a boolean remains.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/RepoDefinition.java`
- Bazel lines: 67
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/struct_value.rs` and `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs` (`build_attr_struct`)
- Verdict: partial
- Differences:
  - `rctx.attr.name` is the canonical repository name on both sides. Bazel returns the record's name before the attribute map. Zel stores that same canonical string as a normal `name` field.
  - Explicit non-`None` attributes are returned as already type-checked Starlark values. A missing key or an explicit `None` falls through to the attribute default. Zel does the same, otherwise `""`, `0`, `False`, `[]`, `{}`, or `None` for labels.
  - `dir(rctx.attr)` is the rule's attribute names plus `name`, sorted. `repr`, `str`, and `type` match: the unknown-object text and `RepoDefinition`.
  - Unknown fields use `unknown attribute <field>`. Bazel then appends a spelling hint. Zel stops at that sentence.
  - Assigning a field fails in Bazel with `RepoDefinition value does not support field assignment`. Zel rejects every attribute assignment with `cannot assign to attribute`.
  - List and dict attribute values are frozen, so `rctx.attr.srcs.append(...)` raises `trying to mutate a frozen list value`. Zel's lists and dicts stay mutable, so the same call updates the list.
  - Equality and dict-key hashing use the Java record: rule, explicit attribute dict, canonical name, and original name. An attribute set explicitly to its default is a different record from one where that attribute was omitted, even though reading it returns the same thing. Zel compares and hashes the materialized field map, so those two compare equal, and the original name is not part of the value.
  - `rctx.attr + struct(...)` is `unsupported binary operation: RepoDefinition + …` in Bazel. Zel merges the two structs when field names do not overlap.
  - A user attribute named `repo_mapping` is a normal field in Bazel. Zel always removes `repo_mapping` after building the struct. With `--experimental_repo_remote_exec`, Bazel also adds `exec_properties` (default `{}`) on every repo rule. Zel never adds that field.
  - A computed or late-bound label default throws when converted to Starlark. Zel turns a function default into a string and serves that string.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/RepoDefinitionFunction.java`
- Bazel lines: 332
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/mod.rs` (`repo_definition`)
- Verdict: partial
- Differences:
  - Bazel resolves in order: canonical `--override_repository`, then `@bazel_tools` to the embedded tools tree, then a non-registry override, then apparent-name overrides, then the module repo spec, then extension repos. Zel walks a sorted map and returns the first entry that is either the canonical name or any root-mapping apparent name for it, so an apparent key that sorts first beats a canonical override. Two apparent names that map to the same canonical repo throw in Bazel. Zel keeps the lexicographically first path.
  - An empty `--override_repository=name=` removes that override in Bazel. Zel stores it as the workspace directory. Relative paths are resolved against the client working directory in Bazel and against the workspace root in Zel. Bazel replaces the substring `%workspace%`. Zel only accepts it as a prefix.
  - For a non-registry override, Bazel applies an apparent CLI override only when that module is a direct root dep, and only under that dep's apparent name. Otherwise it returns the override's repo spec and never consults later apparent overrides. Zel applies every root-mapping alias first, including indirect modules and `override_repo` imports.
  - Bazel loads the `.bzl` and type-checks attributes, drops `name`, `tags`, `deprecation`, `visibility`, and `None`, and converts label strings with the main-repo mapping. A missing symbol is `repository rule %s does not exist (no such symbol in that file)`. A wrong type names the actual type. Zel returns the stored spec unchanged. Patch labels are pre-rewritten to an `@@//…` main-repo label and do not use that mapping. Fetch checks the rule later with `'…' is not a repository rule exported by …`.
  - `@bazel_tools` is an override of the embedded tools directory unless a canonical override already matched. Zel's show-repo path uses the install base only when one is set, otherwise `external/bazel_tools`. Fetch does not treat it as an override: it materializes a tree, writes `BUILD.bazel`, and deletes `BUILD`.
  - An apparent override that is not visible from the main repo is a persistent error, with a spelling suggestion, and it says to use `--inject_repository`. Zel emits that sentence with no suggestion, only for `build`, `test`, `run`, and `coverage`, only when the main mapping is non-empty, and it also skips names containing `~`. `fetch` and `mod` ignore an unknown apparent name.
  - Extension lookup in Bazel is the first insertion-order unique name whose prefix matches, then an exact internal-name hit. The first prefix that does not own that canonical name is not-found even if a later extension does. An extension eval failure fails the function. Zel scans extensions in sorted id order and keeps going until the stripped remainder is a generated repo name, so a later extension can still match. Eval errors become none unless another extension matches. The spec is then taken from usage order, which can differ from that sorted map, so the spec and the original name can come from different extensions when more than one prefix matches.
  - An unknown repo is not-found (fetch reports `Repository '@@name' is not defined`). Zel returns none (`In repo argument …: no such repo`). The empty canonical name is none immediately in Zel. Bazel still runs the lookup and typically ends at not-found, but a canonical override whose key is empty would win.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/RepoDefinitionValue.java`
- Bazel lines: 96
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/repo.rs`
- Verdict: partial
- Differences:
  - Bazel has a third successful value, `NotFound`. Fetch turns it into `Repository '<name>' is not defined`, with the name from `getNameWithAt()` (`@@name`, or `@@[unknown repo '…' requested from …]`). `mod show_repo` prints nothing for it. Zel has only found and override. A missing or empty canonical name returns none. An explicit `show_repo` argument then errors with `In repo argument {key}: no such repo`, and `--all_repos` drops the name.
  - The value is not comparable, so Skyframe always treats it as changed. A recomputed not-found, the same override path, or a found value whose record compares equal is still a change. That includes a `.bzl` repo mapping closed over by the callable, which record equality does not see. Zel derives equality and has no hermetic repo-definition node, so that forced dirty node does not exist. Equality of attributes is also order-sensitive. Bazel map equality is not.
  - The key rejects null and interns on the hermetic name plus the repository name (canonical name, invisible-repo context, and did-you-mean suffix). The same canonical string with a different context or suffix is a different node. Zel looks up a bare canonical string. The empty name is the main repository in Bazel and none in the lookup.
  - An override stores a path fragment. Equality and string form use the normalized internal path (`.` and `..` and repeated separators collapsed, `/` separator, Windows drive prefix), with one Latin-1 code unit per path byte. Zel stores the display string: no `..` normalization, OS separators kept, and non-UTF-8 bytes become U+FFFD. The same path can compare unequal and print differently.
  - A found value holds the rule (implementation callable, transitive digest, recorded repo mapping, attribute defaults and indices, local, configure, remotable, and environ), the type-checked attribute values, the name, and a nullable original name. Zel keeps the name, an optional original name, and a spec of the bzl-file string, the rule name, and explicit attributes only. Defaults, the callable, and the recorded mapping are not in the value. Integers that do not fit in `i64` are stored as strings.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/RepoMetadataRequirements.java`
- Bazel lines: 26
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` (`incompatible_require_repo_extension_metadata`)
- Verdict: partial
- Differences:
  - Bazel's only behavior here is the shareable Skyframe input, injected by the repository module (reset and default false, then the parsed flag) and read by repository fetch and single-extension evaluation. Zel never injects that key. Rule execution and regular extension evaluation do not read the flag.
  - With mode `all`, or `root` when the defining `.bzl` is in the main repository, Bazel fails a repo-rule fetch whose return value is neither a dict nor repo metadata, and fails a non-innate module extension that returns none instead of extension metadata. Zel accepts those returns for every flag value. A returned dict is not a failure on either side. Innate `use_repo_rule` extensions ignore the mode in both.
  - The injected value is the enum, not the raw token. `false`, `all`, and `root` match case-insensitively, then boolean spellings: `true`, `1`, `yes`, `t`, and `y` become `all`, and `0`, `no`, `f`, and `n` become `false`. A bare flag is the unconverted string `"1"` and therefore `all`. `--no…` is `"0"` and therefore `false`. Zel accepts the same spellings but stores the raw string and does not rewrite boolean spellings to `all` or `false`. Invalid input errors match.
  - The precomputed value is a Skyframe dependency, so changing the mode dirties repository fetches and single-extension evaluations. Zel's fetch marker and extension-result cache do not include the flag, so changing it does not re-evaluate and cannot start failing an already materialized repo.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/RepoRule.java`
- Bazel lines: 168
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs` (`RuleDef`, `make_repository_rule`)
- Verdict: partial
- Differences:
  - `instantiate` stores only explicitly passed, non-`None` values after attribute conversion, and it drops `name`, `tags`, `deprecation`, and `visibility`. Zel stores every keyword, including `name`, `tags`, `visibility`, and `None`, in sorted order. Lockfile writing later drops `name` and `None`, so passed `tags` and `visibility` still show up in generated repo specs, and attribute key order is sorted rather than call order.
  - A passed `deprecation` is discarded in Bazel. Zel reports it as an unknown attribute. Bazel rejects a user attribute named `tags`, `deprecation`, or `visibility`. Zel accepts those names. At the call, Zel strips `tags` and `visibility` before validation, and fetch always deletes `repo_mapping` from `rctx.attr`, so a user-defined `repo_mapping` attribute never appears.
  - Zel inserts `name` into `rctx.attr` and sets it to the canonical repository name. Bazel does not put `name` on the attribute struct.
  - Bazel int attributes must fit a signed 32-bit integer. Zel accepts any `i64`. An integer wider than `i64` is stringified and then fails a string-versus-int check. `0` and `1` are legal bools on both sides. Bazel stores a boolean. Zel keeps an int until the attribute struct is built, so the lockfile JSON contains `0` or `1` instead of `false` or `true`.
  - Bazel resolves label attributes during instantiate and stores canonical `@@` labels. Zel stores the raw string and resolves it later. The extension call passes no owner, so the "no repository visible" check does not run. Label-keyed dicts do not reject keys that canonicalize to one label. A label value is accepted for a string attribute. Bazel requires a string.
  - Bazel type-checks dict keys. Zel stringifies keys, so a non-string key becomes a string and is accepted.
  - `allow_empty=False` is not enforced by Bazel's type check. Zel rejects an empty list at the call. Allowed-value text differs: Bazel uses `the value for attribute 'x' has to be one of 'a' or 'b' instead of 'c'`. Zel uses the repo-rule wording with a JSON-like list. A bad bool is `expected one of [False, True, 0, 1]` in Bazel and `expected value of type 'boolean'` in Zel.
  - Unknown-attribute suggestions use UTF-16 length and locale-sensitive lowercase. Zel uses Unicode scalars and Unicode lowercase, so non-BMP names and `I` / `İ` under a Turkish default locale change both the suggestion and the cutoff. With an empty call order, the first unknown attribute follows sorted order instead of dict insertion order.
  - `local` is stored and never read. Bazel skips the repo-contents cache and vendoring when `local()` is true. Zel always rejects `remotable` and never adds `exec_properties`. Bazel records `remotable` and adds `exec_properties` when remote repo execution is on. `configure` is forced false for every `@bazel_tools` rule.
  - On failure Bazel posts an error at the last call-stack location and throws an external-deps exception whose text is the formatted call stack. Zel returns a Starlark error, and fetch prints a doubled traceback when the message starts with `in call to '`.
  - Attribute strings stay in Bazel's Latin-1 internal encoding until lockfile JSON converts them. Zel holds UTF-8 scalars, so a non-UTF-8 byte that is a legal Bazel string cannot round-trip, and Gson lockfile integers are 32-bit.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/RepositoryFetchFunction.java`
- Bazel lines: 876
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs` (`fetch_repo_inner`)
- Verdict: partial
- Differences:
  - Bazel writes the marker as the predeclared hash plus every recorded input, in ISO-8859-1. Zel writes only `{hash}\n` as UTF-8 and drops watches, environment reads, files, and directory listings collected during the rule. A watched file or environment change that leaves the predeclared hash unchanged does not refetch. A Bazel marker whose recorded lines contain bytes 0x80–0xFF fails to read as UTF-8 and is treated as stale. A recorded path that is not Latin-1 makes Bazel fail the fetch after the rule succeeds. Zel never encodes those lines.
  - The predeclared hash is not Bazel's. Bazel mixes marker version 7, one JSON object of the attribute values, `os.name` and `os.arch` in the JVM's lowercase form, and repo-mapping rows ordered by repository name then apparent name. Zel mixes version 8, a sorted map of Starlark reprs with `name` and `None` omitted, `windows` rather than `windows 10` or `windows 11` (every other OS forced to `linux`), and mapping entries in load-walk order. UTF-16 code-unit order and UTF-8 byte order diverge for non-BMP names. Marker identity and repo-contents-cache keys therefore differ.
  - Cache eligibility follows `repository_rule(local=True)` and `configure=True`. With fetching enabled, Bazel skips the on-disk marker and the contents cache for local rules. A general force-fetch skips the cache for every repo. A configure-only force skips it for configure rules. Zel bypasses the marker and cache only when `--force` is set, and it treats "local" as the rule names `local_repository` and `new_local_repository` only. A custom `local=True` rule is reused from the hash-only marker. Configure rules are not refetched for sync `--configure`.
  - Reproducibility is the return value of the implementation. A returned dict is forced not reproducible. `None` is not reproducible unless the require-metadata flag is `all` or `root` for a main-repo `.bzl`, in which case Bazel errors persistently. Zel sets the flag as a side effect of calling `repo_metadata(reproducible=True)` even when that object is not returned, still caches in that case, and never enforces the flag.
  - Vendor mode runs before the external marker and the contents cache. An up-to-date vendored tree, a pinned repo, or an out-of-date vendored tree under `--nofetch` is symlinked and wins. A missing pinned repo errors. Local and configure rules are excluded from that lookup by default. Zel checks the external marker, then the contents cache, then the vendor dir. A hash-only marker from a previous run skips the vendor tree, so an updated vendor dir stays unused. `--nofetch` does not accept a stale vendor dir. A missing pin falls through and fetches. A missing vendor dir under `--nofetch` uses an existing external directory, including a dangling symlink, or emits the generic disabled-fetch error. A `VENDOR.bazel` that is not UTF-8 becomes an empty pin and ignore set.
  - A contents-cache hit rechecks the candidate's recorded-input file every evaluation, symlinks the external dir at the cache entry, clears the external marker, and registers a file-state dependency so deleting the entry invalidates the repo. Zel symlinks on Unix and copies on other systems, writes a hash-only external marker, and the next fetch returns on that marker without restatting the target. A garbage-collected cache entry left as a dangling symlink is treated as a successful fetch. A failed move back into the cache is ignored, so a failed symlink-back still returns success with the external directory gone. Bazel fails the fetch on that I/O error.
  - After a reproducible non-local fetch, Bazel replants symlinks and stores the repo only when the cache-safety flags say so. Symlinks into the main workspace, relative links that escape the repo, and cross-repo links make the entry ineligible for the local cache. Zel replants without rewriting main-repo links and then caches every reproducible repo whose rule name is not `local_repository` or `new_local_repository`, including repos Bazel would reject. On non-Unix the replant deletes the symlink and does not recreate it. The remote cache lookup, the warning on lookup failure, and the upload are absent, so those hits are full refetches.
  - Overrides return through a symlink of the external path at the override path. The marker is cleared, the target must be a directory and must contain a repo boundary file, and the result is excluded from vendoring. Zel checks that the path is a directory and then runs a synthesized `local_repository`, which can reuse a matching marker and write a new hash-only marker instead of clearing it.
  - Bazel deletes the external tree, clears the marker, and runs the rule. On failure it leaves the partial tree, so a later `--nofetch` will use it, and reports the fetch error once. Zel deletes the partial tree on failure, so `--nofetch` then reports the repo as missing. Creating `REPO.bazel` when no boundary file exists fails the fetch in Bazel if the write fails. Zel ignores that create error and returns success. The context is built with the ignored-subdirectory set, so paths under `.bazelignore` count as external. Zel's repository context has no such set. Remotable rules also depend on whether remote execution is enabled. Zel does not invalidate them when that flips. Bootstrap repo rules get a null main-repo mapping to avoid a cycle. Non-registry module repos in Zel go through the bootstrap closure, which skips vendor, `--nofetch`, symlink replant, and the local-rule cache exclusion. `@bazel_tools` is materialized inside fetch. This Bazel function does not special-case it.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/RepositoryFunctionException.java`
- Bazel lines: 63
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs` (message shape only)
- Verdict: partial
- Differences:
  - `getMessage()` is fixed at construction to `cause.toString()`. An `IOException` therefore becomes `java.io.IOException: ` plus the localized message, or exactly `java.io.IOException` when that message is null. An eval exception becomes the message with the stack, included only when the stack was requested and is already filled. The already-reported subclass string is the class name plus the message. Zel fetch failures are strings. Some paths concatenate `fetching {canonical}: java.io.IOException: {msg}` plus an `ERROR:` line. Marker-write, symlink-replant, and override-not-a-directory errors are a single string with no `java.io.IOException:` prefix and no already-reported class name.
  - The already-reported wrapper keeps the message and the cause and drops the original exception. A missing-package message is `no such package '<id>': <raw>`, and its package id and stack are not retained. A null message stays null, so the outer string is the class name alone. Eval wrapping therefore stores the bare message and loses the Starlark stack in the skyframe and repository-failed text. Zel puts location, traceback, and `ERROR:` into the one returned string, and splits that string on `": "` and `"Error in "`, so a message that itself contains those substrings or a newline is parsed differently from the structured Java frames.
  - Construction allows only a missing package, an I/O exception, an eval exception, and an external-package exception. Any other type throws an illegal state whose message is the exception string, with the original not set as the cause. A null cause throws. A runtime exception or an interrupt is rejected before a node error exists. Zel accepts any UTF-8 string and has no crash-on-wrong-type path.
  - Transience is caller-supplied: transient is re-evaluated on the next evaluation, persistent stays cached, and the exception is never catastrophic. Zel's sky error has a transient flag, but repository fetch does not use it. Fetch deletes the directory and runs again on the next call, so failures Bazel marks persistent are retried whenever fetch is invoked.
  - Java messages are UTF-16 strings and I/O text uses the localized message. Zel text is UTF-8. Unpaired surrogates or non-UTF-8 OS bytes cannot be stored. Creating the repo-boundary file ignores a create failure. Bazel wraps that I/O error as a transient repository exception.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/RepositoryOptions.java`
- Bazel lines: 562
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/session.rs` and `/workspace/zel-cursor/crates/zel-options/src/converters_ext.rs`
- Verdict: partial
- Differences:
  - `--repo_contents_cache` left unset stays off. Bazel enables `<repository_cache>/contents`. `--repository_cache=` still disables both. Zel also skips the "output base is inside the repo contents cache" failure.
  - The contents-cache GC max age (default `14d`) and idle delay (default `5m`) are parsed and never scheduled. Java durations throw past a signed 64-bit product. Zel saturates into `u64` seconds or milliseconds.
  - Hardlinks, timeout scaling (including Bazel's warning and forced factor `1.0` when the factor is not positive), external-file checks (default true), and required repo-extension metadata are ignored. Bazel maps boolean spellings and a bare flag to `all` or `false`. Zel keeps the raw spelling.
  - `--repository_contents_cache` warns in Zel. Bazel sets the old-name warning off and stays silent. The other old names still warn in both.
  - Path options do not run the path-fragment converter. Bazel expands a value that starts with `~/` by replacing every `~` with the user home, then drops `.`, `..`, and duplicate slashes. On Linux, Bazel keeps backslashes and treats `C:/…` as relative. Zel rewrites backslashes to slashes and treats `C:/…` as absolute. Lexical `foo/../bar` therefore differs when `foo` is missing.
  - Non-empty override, inject, and module paths replace every `%workspace%` substring, then resolve anything still relative against the working directory. Zel only special-cases the exact string `%workspace%` and the prefix `%workspace%/`, and joins other relative paths to the workspace. `--vendor_dir` does not do that replacement in Bazel. Zel's absolute-path helper does.
  - An empty path clears that name in Bazel's insertion-ordered map. Zel inserts the workspace directory. `--override_repository==/tmp` (empty repo name) is the main repository in Bazel. Zel's parser accepts it as a canonical name, then validation rejects it.
  - Inject names that are legal user-provided repo names but not module names (`Foo`, `1abc`) pass the injection converter and then fail module-name validation. Bazel only checks the user-provided repo name.
  - Direct-dependency and Bazel-compatibility checks accept any ASCII case in the options parser, but the later parse allows only `off`/`OFF`, `warning`/`WARNING`, and `error`/`ERROR`. `--check_direct_dependencies=Warning` therefore fails while parsing options into the session. Lockfile mode is lowercased and matches.
  - Module mirrors are not parsed as registry-to-URL-list entries. Bazel splits `[registry=]url[,url…]`, last flag wins per key, trims a trailing slash, applies the empty key to registries without their own list, and errors on a key that is not empty and not in `--registry`. Zel prepends each raw flag string, unsplit, as a mirror base for every registry.
  - Yanked-version allow lists do not trim, require a valid module name and a parsed version, and a bad environment value is reported as the environment variable. Zel trims, accepts any token containing `@` or the exact word `all`, compares the raw string to the module key's display form, and reports environment-variable failures as a flag parse error. `a@1.0, b@2.0`, `Foo@1.0`, `a@1.0+build`, and `a@not-a-version` do not match Bazel.
  - A negative downloader retry count is left unchanged when it is negative. Zel clamps it with `max(0)`, which disables retries.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/RepositoryResolvedEvent.java`
- Bazel lines: 141
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs` (`report_reproducible_form`)
- Verdict: partial
- Differences:
  - An empty reproducibility map is not diffed in Bazel. An empty dict, a non-dict return, and `repo_metadata()` or `repo_metadata(reproducible=False)` set "information returned" false. The stored sentence is never logged, because the fetch function emits a debug event only when new information was returned. Zel does not special-case an empty dict or a metadata struct whose attrs are missing. It treats every absent key as the attribute default and, if any non-`name` attribute differs from that default, logs a dropping line.
  - Dropped names are rendered differently. Bazel passes the filtered key set to Starlark repr, and a set falls through to `Set.toString()`: `dropping arguments [url, sha256]` (unquoted, attribute declaration order). Zel uses debug formatting of each name: `dropping arguments ["sha256", "url"]`.
  - Attribute order differs. Bazel walks field names in declaration order, skips `name` and names starting with `_`, and joins both the assignment list and the dropped set in that order. Zel walks the attribute struct, whose fields are sorted by name, so two or more modifications or drops come out in a different order.
  - Repr of a changed value differs for non-ASCII strings and floats. Bazel writes UTF-16 code units raw when the character is at least 32, escapes controls below 32 as `\xHH`, and formats floats with `%.17g` (`+inf`, `-inf`, `nan`). Zel uses Rust string debug (`"café"` becomes `"caf\u{e9}"`, U+0001 becomes `\u{1}`) and shortest float formatting (`inf`, `-inf`, `nan`). ASCII strings, bools, ints, and lists of those match.
  - Zel removes `repo_mapping` from the attr struct before the diff, so a returned change to that name is ignored. A user attribute of that name is compared in Bazel.
  - A returned dict with a non-string key never reaches the Bazel event: the cast fails the fetch. Zel displays the key and ignores it when it is not an attribute name, and the fetch continues.
  - When a change is reported, the second line matches: `Repo <canonical> defined by rule <rule> in <bzl>`, logged as `DEBUG:`.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/RepositoryUtils.java`
- Bazel lines: 215
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/vendor.rs` (`replant_symlinks`) and `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs`
- Verdict: partial
- Differences:
  - A valid repo root is only the four-name existence check (`WORKSPACE.bazel`, `WORKSPACE`, `MODULE.bazel`, `REPO.bazel`, following symlinks). Zel folds that into creating an empty `REPO.bazel` when the directory is not itself a symlink. The workspace-root walk also treats `MODULE.zel` and `WORKSPACE.zel` as roots.
  - The label-to-path lookup returns null when the package is missing (the caller restarts) and throws `Unable to load package for <label>`, listing every package-path root for a missing build file in the main repo. Zel joins the fetched repo, package, and name with no build-file check, so a label in a directory without `BUILD` or `BUILD.bazel` still yields a path. There is no remote-overlay materialization.
  - The external repository directory matches `<output_base>/external`.
  - Cache flags are dropped. Bazel returns whether the tree is safe for the local cache and the remote cache: a relative link that resolves outside the repo, a link into the main repo when main-repo replanting is off, or a cross-repo link clears the local-cache bit. Any link that does not resolve inside the repo clears the remote-cache bit. Zel rewrites only absolute targets under the external root, then still moves every reproducible non-local repo into the cache. Vendor mode always rewrites main-repo links and returns no flags.
  - Bazel normalizes the link text (`.`, `..`, repeated slashes, trailing slash; on Windows backslashes and an uppercased drive) before the prefix test. Zel keeps the raw link. `/repo/foo/../bar` becomes `bar` in Bazel and `foo/../bar` in Zel, which is a different file when `foo` is a symlink. `workspace/../outside` is not under the workspace after Bazel normalization. Zel's prefix test is still true, so vendor routing goes through `bazel-external/_main/../outside`.
  - Zel canonicalizes the external root and the repo's parent before the prefix test. Bazel compares the logical `output_base/external/<repo>` fragments, so an absolute target that points at the physical cache directory is left unchanged while Zel rewrites it.
  - A symlink read or directory-listing failure aborts Bazel and leaves later links untouched. Zel ignores those errors, returns success, and still rewrites the links it did see.
  - On Windows Bazel writes the relative target and, if that throws, restores the original target and clears both cache flags. Zel removes the symlink on every OS and recreates it only on Unix, so on Windows the link is deleted and the function can still return success.
  - Ensuring a symlink is a no-op when the normalized target is already correct, creates missing ancestors, and throws if `bazel-external` or `_main` exists and is not a symlink. Zel deletes a regular file or a directory, then recreates, and does not create missing parents. A repo path with no parent errors with `Cannot get vendor directory`.
  - If stripping the prefix fails, Zel uses depth 0. Bazel's relative path throws and fails the whole replant. When the prefix matches, the segment counts agree. Path bytes are Latin-1 versus OS strings. On Windows Bazel's uppercased drive makes `c:/repo` and `C:/repo` the same prefix, and Rust does not.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/cache/DownloadCache.java`
- Bazel lines: 404
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`RepositoryCache`)
- Verdict: partial
- Differences:
  - BLAKE3 stores 32-byte digests under `blake3/`. Zel has no BLAKE3 cache key. A 32-byte digest is always `content_addressable/sha256/<hex>/file`. SHA-1, SHA-256, SHA-384, and SHA-512 directory names and digest lengths match, under `<repository_cache>/content_addressable`.
  - The cache key is the directory name. Bazel accepts both hex cases with no trim, and verification is case-insensitive, so `AB…` and `ab…` are two entries that both verify. Zel stores lowercase only and trims while parsing, so those strings are one entry and a whitespace-padded key is stored rather than rejected.
  - A poisoned `file` makes Bazel throw the hash-mismatch sentence and then append the delete-directory suffix. Zel returns `Checksum was <actual> but wanted <expected>` plus the same suffix. A read error in Zel does not add that suffix. The download manager swallows the Bazel I/O error and downloads again. Zel propagates the error and does not repair the entry.
  - If the entry named `file` is a directory, Bazel treats it as present and fails while hashing. Zel's file check is false, so the lookup is a miss.
  - Bazel copies with attributes (mode and mtime follow the cache file) or, when hardlinks are enabled and allowed, creates a hard link. A cross-device link failure fails the lookup. Zel never hardlinks and rewrites the bytes, then sets mode `0755` or `0644`. A cache file stored as `0755` therefore stays executable on a later Bazel fetch with `executable=false`, and becomes `0644` in Zel.
  - Bazel uses the filesystem fast digest when that digest function equals the key's hash, without reading the file. Zel always hashes the bytes. Bazel also hashes in 32KiB reads and aborts if the thread is interrupted. Zel reads the whole file into memory and does not observe interruption.
  - After a hit, Bazel touches the cache file so mtime moves, ignoring I/O errors. Zel leaves mtime unchanged.
  - Canonical-id markers hash the Starlark string as Latin-1 raw bytes encoded as UTF-8, so a non-ASCII id from a UTF-8 source is hashed double-encoded. Zel hashes UTF-8 of Unicode scalars. Unpaired surrogates become `?` under Java's UTF-8 replacement. A Rust string cannot contain them. Empty and null ids skip the marker in both. A touch failure fails the put in Bazel. Zel ignores a write failure and still returns success, so a later get with that id misses.
  - Bazel streams the source into a UUID temp file and tolerates a concurrent create (on Windows, if the destination already exists, the temp file is deleted and the put succeeds). Zel writes the full buffer to a pid-and-counter temp file. If the destination cannot be replaced, the put fails and the temp file remains. Bazel can also store a computed key for SHA-1, SHA-384, SHA-512, and BLAKE3. Zel's cache put only stores a caller-supplied checksum. The no-checksum download path computes SHA-256 only.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/cache/DownloadCacheHitEvent.java`
- Bazel lines: 21
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs` (`CTX_CACHE_HITS`)
- Verdict: partial
- Differences:
  - Bazel posts a record of context, file hash, and URI only when the repository cache hits. Zel has no event type. A finished download appends the content SHA-256 and the URL for every cache result, which also includes an output that already matched the checksum and a distdir hit. Those two never post this event.
  - The file hash is the lowercase hex of the supplied sha1, sha256, sha384, sha512, or blake3 digest. Zel always stores the content's SHA-256, so a non-SHA-256 cache key is a different hash in the failure text. Blake3 is a legal Bazel cache key and is not a Zel checksum algorithm.
  - The URI is the first rewritten URL, or the cacheprobe URL (plus a dot and the archive type when set) when the rewriter leaves no URL. Zel's repository-cache hit stores the first original URL and ignores the rewriter. A pre-existing or distdir hit stores an empty string. An empty URL list stays empty instead of the cacheprobe URI. Java URI membership is case-insensitive on scheme and host, and the string form quotes non-ASCII. Zel keeps the raw Starlark string, so host case, default-port spelling, and raw non-ASCII are distinct lines.
  - Bazel dedupes with a concurrent set of hash and URI and iterates in neither insertion nor sorted order. Zel keeps duplicates in call order. Async Bazel downloads share one command-scoped map. Zel's thread-local cell only sees the evaluating thread.
  - Bazel keys hits by the download context (`repository @@name`, `module extension …`, or `Bazel module fetching` for registry reads) and prints them only on a repository-failed event for that repository string. Zel ignores the request context, synthesizes `repository @@{canonical}` only when the repository-rule implementation returns an error, and never records registry cache hits. A rule that returns successfully and then fails still reports hits in Bazel and does not in Zel. The printed block matches the Bazel sentences, but Zel embeds `INFO:` only on the first line, adds a trailing newline after the verify-hashes sentence, and prefixes `INFO: Repo {canonical} defined by rule …`.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/cache/LocalRepoContentsCache.java`
- Bazel lines: 287
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/repo_contents_cache.rs`
- Verdict: partial
- Differences:
  - Touch does not change mtime. Bazel sets it to the kernel now. Zel opens the file and writes nothing, so a cache hit does not delay GC.
  - Lookup drops a recorded-inputs file whose contents directory is missing, is a broken symlink, or is not visible. Bazel still returns that candidate. A file whose name is exactly `.recorded_inputs` throws in Bazel. Zel treats the contents path as the hash directory itself and returns it.
  - Candidate order is not the same. Bazel snapshots each mtime once and stable-sorts descending epoch millis, keeping directory order on ties. Zel re-stats on every comparison, sorts unstably, and uses full timestamp resolution, so sub-millisecond timestamps reorder entries and equal millisecond timestamps do not stay in readdir order. The first repo tried therefore differs.
  - Recorded-inputs names are Latin-1 raw bytes in Bazel. Zel uses lossy UTF-8, so a non-UTF-8 name becomes U+FFFD, the sibling contents path is a different path, and the entry is omitted.
  - Moving into the cache is not the same transaction. Bazel deletes any existing cache dir, moves the marker into trash first, renames the repo, and on cross-device failure copies with attributes. Zel never clears the destination, copies the marker only after the repo move and leaves the original marker in place, and if the marker is absent writes the predeclared-hash string and still returns success. Bazel throws before moving the repo. Cross-device fallback does not chmod, does not preserve mtime, and ignores a failed symlink recreation.
  - Cache entry names are not UUIDs. Zel uses nanoseconds plus a process counter, so two processes in the same nanosecond generate the same name. On that clash Bazel deletes the old dir and replaces it. Zel's failed rename copies into the existing directory and leaves extra old files.
  - After the move, Bazel deletes the fetch path and symlinks it, and a symlink failure fails the call. Zel creates the symlink only on Unix and ignores errors, returning success with the fetch path missing. On other systems it copies the tree instead of linking, so the workspace copy is detached from the cache entry.
  - GC does not keep the newest duplicate or protect readers. Bazel takes an exclusive lock, sorts by descending mtime, and deletes a duplicate only when the content hash was already seen. An expired file is not inserted. Zel has no lock and no interrupt check, walks directory order, and always inserts the hash before the age test. An expired duplicate visited first therefore evicts a later live copy. Among live duplicates the survivor is whichever name the filesystem returns first.
  - The age cutoff uses the cache-root mtime minus the max age, or the minimum instant when the max age is zero (no age deletion, including pre-1970 files). A negative duration moves the cutoff into the future and deletes every entry. Zel uses the current time, and zero age uses the Unix epoch, so pre-1970 mtimes are deleted. A negative age cannot be represented. Unreadable mtimes become the epoch and are then eligible for deletion. Bazel aborts GC on that I/O error.
  - Unreadable recorded inputs abort Bazel GC. Zel hashes them as empty and treats them as duplicates of each other and of real empty files. Per-directory and per-file errors are skipped. Bazel fails the whole GC. If moving a repo dir into trash fails, Zel deletes it in place and continues. Bazel has already deleted the recorded-inputs file and then throws, leaving the contents dir and stopping later entries.
  - The directory scan follows symlinks, so Zel can delete the target of a symlink to a directory. Bazel does not follow those links. Zel also skips a directory named `gc_lock`. Trash cleanup removes the whole trash directory inside GC. Bazel's idle task deletes only children, outside the exclusive lock, after a successful GC.
  - A disabled cache makes lookup, move, lock, and GC throw. Shared locks block exclusive GC while this process is reading, and an idle task waits before GC. Zel's cache is always a concrete path, and GC runs immediately with no lock.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/cache/RepositoryCache.java`
- Bazel lines: 63
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` and `/workspace/zel-cursor/crates/zel-bzlmod/src/repo_contents_cache.rs`
- Verdict: partial
- Differences:
  - Bazel keeps one nullable parent path and two live children. Setting the path to null disables the download cache and the repo-contents cache together. Zel stores two independent paths. An empty `--repository_cache=` becomes none for downloads only. A non-empty `--repo_contents_cache` stays enabled.
  - Setting the path binds the download cache to `path/content_addressable` and the repo-contents cache to `path/contents`. Zel's download entries land at `<root>/content_addressable/<algo>/<hex>/file`. The contents cache is built only when `--repo_contents_cache` is a non-empty path. With the default cache, or with only `--repository_cache=<dir>`, Bazel reads and writes `<dir>/contents`. Zel leaves the repo-contents cache off. The `contents` segment is used in Zel only for the "inside the main repo" check.
  - The parent path is null when the cache is disabled, and the info item prints `null`. Zel's repository-cache info item always prints `<output_user_root>/cache/repos/v1` and does not follow `--repository_cache=<path>` or an empty disable.
  - Both child objects exist from construction and stay disabled until the path is set. Zel has no disabled contents-cache instance. Absence is none. The downloader starts with no cache, which matches a disabled download cache, but a normal Bazel command always sets the default path and therefore also enables `<that>/contents`.
  - A later contents-cache path override leaves the content-addressable root in place. Zel rebuilds the contents cache from the explicit option and never mutates a shared parent. Supplying both paths explicitly matches. Leaving contents unset does not.
  - The only path strings in this class are the ASCII segments `content_addressable` and `contents`. Joining them onto an absolute parent produces the same child. This class has no checksum, map iteration, regex, integer arithmetic, or JSON parsing.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/ArFunction.java`
- Bazel lines: 82
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`extract_ar`)
- Verdict: partial
- Differences:
  - Bazel sets each member's mtime from the 12-digit seconds field. Zel never sets mtime. Extracted files keep the extraction time.
  - Observable mode keeps setuid, setgid, and sticky in Bazel. Zel clears those three bits and forces owner-read. Ordinary `100644` and `100755` still match.
  - Names are US-ASCII in commons-compress: each byte `0x80–0xFF` becomes one U+FFFD. Zel uses lossy UTF-8, so valid UTF-8 stays Unicode and consecutive invalid bytes collapse to a single U+FFFD. Rename lookup uses that string, so the same bytes can miss a rename key. The invalid-magic error shows `?` for those bytes in Zel and U+FFFD in Bazel.
  - A name that starts with `/` and is not a GNU offset is absolute in Bazel (`/SYM64/` becomes `/SYM64`). Zel drops empty segments, so the member is written under the destination. `..` that climbs out of the destination is written outside it in Bazel and is an error in Zel. `a/../b` stays inside in both.
  - GNU long names must match `/` plus digits, and BSD long names `#1/` plus digits, for the whole string. Zel only checks the prefix and then parses. Rust accepts `+` and whitespace, so `/ 12`, `/+12`, and `#1/+10` are long-name lookups in Zel and literal names in Bazel. Trailing junk stays a literal name in Bazel and is a parse error in Zel. Offsets or lengths above the signed 32-bit maximum fail in Bazel and can succeed in Zel.
  - The GNU string table stops on newline or NUL and strips one trailing slash. Zel stops only on newline, so a NUL-terminated entry swallows the following names, and every trailing slash is stripped (`foo//` is `foo/` in Bazel and `foo` in Zel). A blank length, a non-numeric mtime or mode, or a size above `2^31-1` fails that member in Zel and is accepted by the Java stream. A short `//` payload errors in Bazel and is a silently truncated table in Zel.
  - BSD names keep every byte, including interior NULs, and the payload length is reduced by that string length. Zel cuts the name at the first NUL. If that name starts with `__.SYMDEF`, Zel returns an invalid path and stops. Bazel writes the member. Short names `__.SYMDEF…`, `/`, and `/SYM64/` are skipped in Zel. Bazel extracts them: `/` is the empty string, so the symbol-table bytes are written onto the destination itself as a regular file.
  - Blank mtime, mode, or size fails the parse in Bazel. Zel treats every blank numeric field as 0. A negative size errors in Bazel. Zel clamps it to 0 and continues with an empty member.
  - An empty archive, or one whose only members Zel skips, leaves the destination uncreated in Bazel. Zel always creates the destination before the entry loop.
  - Bazel checks for interruption before the archive and after every member. Zel has no interrupt check.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/Bz2Function.java`
- Bazel lines: 38
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/codecs/bz2.rs`
- Verdict: partial
- Differences:
  - Historical randomised blocks are decoded by commons-compress. Zel returns `Randomised bzip2 blocks are not supported` and writes nothing.
  - On a CRC error or trailing non-`BZh` bytes, Bazel has already created the output file and written every byte read so far, then throws. Zel stashes those bytes and writes no file.
  - A basename that is exactly `.bz2` is left unchanged, so Bazel creates a file named `.bz2`. Zel strips the suffix to an empty name and errors because the path is escaping.
  - The repository extract still rewrites the real basename with the English case-insensitive map, shortest suffix first: `.bz` dropped, `.tbz` and `.tbz2` rewritten to `.tar`, `.BZ2` dropped. Zel only does a case-sensitive strip of `.bz2`, so `data.tbz2` becomes `data.t` (Bazel: `data.tar`), `DATA.BZ2` stays `DATA.BZ2` (Bazel: `DATA`), and `notes.bz` stays `notes.bz` (Bazel: `notes`). Rename maps are keyed off that derived name. `download_and_extract` appends `.bz2` to the temp name in both, and that exact suffix strips the same way.
  - A non-UTF-8 archive basename is kept as Latin-1 bytes in Bazel. Zel replaces invalid UTF-8 with U+FFFD before stripping `.bz2`, so the output filename bytes differ.
  - A rename target of `../…` or an absolute path is normalized by Bazel, and on Unix an absolute rename replaces the destination. Zel rejects a leading `..` and turns a leading `/` into a relative path under the destination.
  - Bazel creates the file with `0666` masked by the umask and does not chmod. An existing file keeps its mode. Zel sets permissions to `0644` after every write.
  - Streams Zel rejects and commons-compress may still decode include a Huffman table count of 0 or 1, code lengths outside 1 through 20, and an end-of-block-only block with a zero origin pointer. A long run-length shift panics in Zel. Java throws an I/O error.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/CompressedFunction.java`
- Bazel lines: 84
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`extract_single_file`)
- Verdict: partial
- Differences:
  - The gzip original name is decoded as ISO-8859-1. Zel accepts the name only as UTF-8 and, if that fails, ignores it and uses the archive basename. Latin-1 bytes such as `0xE9` become a different filename in Bazel and are dropped by Zel. Valid UTF-8 non-ASCII is mojibake on disk under Bazel and the original Unicode name under Zel.
  - Bazel splits the basename only on `/`. Zel also treats `\` as a separator, so a gzip name `dir\file` is stored as `dir\file` by Bazel and as `file` by Zel.
  - A gzip name that is blank under Unicode whitespace is ignored, and Bazel falls back to the suffix map. Zel keeps any non-empty basename, including a space or a newline. A basename of `.` or a normalized empty path is blank in Bazel and an escaping path in Zel.
  - With no usable gzip name, and for xz, zstd, and bzip2, Bazel strips the suffix with English case-folding and the commons-compress maps (`.svgz` to `.svg`, `.cpgz` to `.cpio`, `.taz` and `.tgz` to `.tar`, `.tbz2` and `.tbz` to `.tar`, `.txz` to `.tar`, `.bz` stripped, and uppercase `.GZ`, `.ZST`, `.BZ2`, and `.XZ` stripped). Zel only removes a case-sensitive `.gz`, `.xz`, `.zst`, or `.bz2`. A file detected as plain gzip, `foo.cpgz`, is written as `foo.cpio` by Bazel and `foo.cp` by Zel. `foo.tbz2` detected as `.bz2` becomes `foo.tar` versus `foo.tb`.
  - Rename lookup is an exact single-key lookup on both sides. Bazel then uses the destination's relative path: an absolute target is used as-is, and `..` is normalized, so `../evil` is written outside the destination. Zel drops a leading `/` and errors when a `..` segment remains. Strip prefix and strip components are ignored for this single-file path on both sides.
  - After a successful gzip write, Bazel sets the file mtime to the header mtime in milliseconds. Concatenated gzip uses the first member's name and the last member's mtime. Zel never sets mtime.
  - Bazel creates the output with `0666` masked by the umask, and an existing file keeps its mode. Zel forces `0644` for every single-file extract.
  - Bazel streams into the output file, so a later CRC or trailer failure leaves the parent directories and a partial file. Zel decompresses the whole buffer first and creates the output only after that succeeds.
  - Bazel checks for interruption before and after the copy. Zel has no interrupt check. Concatenated-member policy matches: gzip and bzip2 concatenate and reject trailing garbage, xz keeps only the first stream, and zstd reads every frame.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/CompressedTarFunction.java`
- Bazel lines: 270
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`extract_tar`)
- Verdict: partial
- Differences:
  - USTAR and GNU names and link targets are ISO-8859-1 raw bytes. Zel uses lossy UTF-8, so any non-UTF-8 byte sequence becomes U+FFFD and is written back as UTF-8. Valid UTF-8 PAX path and linkpath values match.
  - Bazel appends a process-random UUID in the charset decoder and strips every occurrence. A name or link target that contains that UUID loses those characters, and a PAX name that contains it skips the Unicode conversion. Zel never does this.
  - Bazel applies global PAX headers and both local header flags `x` and `X`. Zel parses only `x` and drops `g` and `X`, so global path, linkpath, size, and mtime never take effect. An empty PAX value removes that key in commons-compress. Zel has no global map and cannot clear one.
  - GNU and PAX sparse entries are expanded to the real byte size. Zel skips typeflag `S` and, for a normal file with sparse headers, writes the raw payload.
  - Typeflags `3`, `4`, and `6` become regular files in both, but Bazel writes the header's data bytes and Zel always writes an empty file. Any other typeflag is extracted as a regular file in Bazel and dropped in Zel.
  - Star and xstar headers use a 131-byte prefix. Zel treats every `ustar` header as POSIX and reads a 155-byte prefix, so those names gain the atime bytes.
  - GNU base-256 numeric fields with a leading `0xFF` are signed. A negative size fails the archive. Zel uses an unsigned magnitude and can accept a size above the signed 64-bit maximum or fail later with "tar number too large". A bad PAX size throws in Bazel. Zel ignores it and keeps the ustar size.
  - Only Bazel sets mtime, and only on regular files. Modes differ on the high permission bits: Bazel keeps setuid, setgid, and sticky. Zel keeps only the lower nine bits plus owner-read.
  - Hard links are created immediately, so a target that appears later fails. Zel defers them until every file is written, so forward links succeed. A hard link to a directory is a recursive tree of hard links in Bazel. Zel's hard link fails and copying a directory fails, so extraction errors. If a hard link fails for any other reason, Zel copies bytes instead of failing.
  - Symlink and hard-link targets are not passed through the component strip in Bazel. Zel's hard-link targets, and absolute symlink targets, are. Relative symlink targets are prefix-stripped and normalized in Bazel. Zel stores that relative string unchanged. A leading `..` after normalization is rejected by Zel and can be written outside the destination by Bazel.
  - Prefix suggestions are the same strings, but Bazel joins them in hash order and Zel joins them sorted. Symlink creation walks a hash map, so a failure leaves an order-dependent subset. Zel creates them in archive order.
  - An empty archive with a positive component strip and an empty prefix still throws a missing-prefix error in Bazel. Zel returns success. Zel also creates the destination before extracting, including when every entry is skipped. Bazel creates a directory only as the parent of an entry it actually writes.
  - Bazel checks for interruption before the stream and after each entry and can leave a partial tree. Zel parses the whole tar before writing, so a truncated member fails with nothing extracted.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/DecompressorDescriptor.java`
- Bazel lines: 84
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`ExtractOptions`)
- Verdict: partial
- Differences:
  - Strip count is a Java int. The builder rejects a negative count. Starlark values outside the signed 32-bit range never reach the field. Zel stores a `usize`. An int that does not fit in `i64` becomes 0 and strips nothing, and a value in `2^31 .. 2^63-1` is applied. A negative becomes 0 if the stripping check is skipped.
  - Mutual exclusion and the negative check run only in the builder, not in the record constructor. A record built with both a prefix and a nonzero strip count is legal, and decompressors then deprefix and strip. Zel returns `Only one of 'strip_prefix' or 'strip_components' can be set` and does not extract.
  - An unset prefix is absent. An explicit empty prefix is present: it matches every relative path, and an empty archive then throws. Zel has only a string, defaulting to empty. Extract reports that same missing-prefix error for an empty archive whenever the prefix is empty and the strip count is 0, including the default options, so the unset-prefix case (empty archive succeeds) is not represented.
  - A present prefix is encoded as ISO-8859-1 and compared to path bytes. Characters above U+00FF become `?`. Zel compares Unicode scalar segments. `"\u00e9"` matches a zip entry whose raw name byte is `0xE9` in Bazel and a UTF-8 name `é` in Zel. `"\u4f60"` matches `"?"` in Bazel and the character `你` in Zel.
  - Rename maps keep insertion order and UTF-16 equality. Zel uses a sorted map of UTF-8 strings. Extraction looks up the exact entry name, so key order does not change output paths. Invalid UTF-8 zip names use the replacement character on both sides, but the replacement spans differ, so a rename key can hit a different entry. Non-UTF-8 ustar names stay raw Latin-1 code units in Bazel and become U+FFFD in Zel.
  - Prefix-miss suggestions are joined in hash order. Zel joins them in UTF-8 byte order, so the possible-prefixes sentence differs when more than one directory is suggested.
  - On Windows, a backslash inside the prefix is a separator, so `a\\b` is two segments. Zel splits only on `/`, so that prefix stays one segment. The context field is stored and unused by decompressors. Zel has no context field. Archive and destination paths are separate extract arguments. Zel's options also have an added prefix, an archive type, and an absolute-path allowance, which this record does not.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/DecompressorValue.java`
- Bazel lines: 272
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`FORMATS`, `detect_kind`, `extract`)
- Verdict: partial
- Differences:
  - Zel's format table appends `tar.br` and `br`. A basename ending in those extensions, or those type names, is accepted and then fails with an unsupported-codec message wrapped as `java.io.IOException: Error extracting …`. Bazel stops with a persistent expected-suffix or no-decompressor error and does not wrap that in an extracting I/O exception. Those two extensions are inserted into every readable-formats string. Bazel's list ends at `7z`.
  - Shared extensions keep Bazel's search order, so overlapping suffixes (`.tgz` before `.gz`, `.tzst` before `.zst`, `.txz` before `.xz`, `.tbz` before `.bz2`) select the same decompressor. Matching is still case-sensitive, so `.zip` and `ZIP` fail in both.
  - A missing prefix joins the suggestion set in hash order. Zel collects suggestions in a sorted set, so the sentence differs whenever two or more prefixes exist. `m` and `z` print as `"z", "m".` in Bazel and `"m", "z".` in Zel. The empty-set wording matches.
  - Prefix suggestions keep only the first segment of a multi-segment path. Zel runs that rule after normalizing, which keeps a leading `..` from an absolute path (`/../foo/bar` suggests `..`). Bazel's absolute normalization drops that `..` and suggests `foo`. Zip and 7z pass the original entry name. Zel applies renames first, so a rename of the first segment changes the suggestion set.
  - Those suggestion strings are not the same Unicode text for non-ASCII names. Bazel prints path segments in its Latin-1 internal form. Zel uses UTF-8 lossy strings. ASCII segments match. A non-UTF-8 archive path is lossy in the `(got …)` text and is preserved as a Latin-1 path string in Bazel.
  - Repository extract treats a type as absent when its trimmed form is empty, which includes several Unicode spaces. Java's blank check does not, so a type of only U+00A0 is "no decompressor" in Bazel and extension detection in Zel. An empty type on Zel extract also falls through to the filename. An empty optional on Bazel decompress does not.
  - An interrupt becomes a bare interrupted exception. Any other I/O error is transient and reported as `java.io.IOException: Error extracting <archive> to <parent>: <message>`. Zel has no interrupt mapping and no persistent-versus-transient split, so a bad suffix is not a cached persistent failure and an interrupt is an extract error instead of a cancellation.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/GzFunction.java`
- Bazel lines: 53
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`ArchiveKind::Gz`)
- Verdict: partial
- Differences:
  - A successful single-member inflate with an ASCII original name, or with no name and a lowercase `.gz` basename, writes the same bytes under the same relative name. Strip prefix and strip components are ignored for the payload in both. The header CRC is skipped in both.
  - Bazel stores the gzip mtime, then multiplies by 1000 again, so the file mtime is `header_seconds * 1_000_000` milliseconds. Zero becomes the epoch. Zel never sets mtime. On a multi-member stream Bazel applies the last member's mtime.
  - Original-name bytes are ISO-8859-1, so each byte is one Latin-1 character. Zel accepts the name only when it is UTF-8 and otherwise uses the archive basename. A Latin-1 `0xE9` becomes U+00E9 in Bazel and is ignored by Zel. UTF-8 `C3 A9` becomes two Latin-1 characters in Bazel and `é` in Zel.
  - A blank original name, including Unicode whitespace, is missing in Bazel. Zel keeps any non-empty UTF-8 string, including whitespace-only names.
  - Bazel takes the basename after Unix normalization (only `/` is a separator). Zel splits on `/` and `\`. `a/b/..` becomes `a` in Bazel and an escape error in Zel. `dir/name/` becomes `name` versus an ignored name. `dir\file` stays one component on Unix Bazel and becomes `file` in Zel. An original name of `..` leaves the tree in Bazel and is rejected by Zel.
  - Bazel chooses the output name from the first header only. Zel uses the first member whose name is non-empty UTF-8, so a later member's name is used when the first member has no flag, an empty name, or invalid UTF-8.
  - With no usable name, Bazel lowercases with English locale and rewrites the shortest matching suffix (`.tgz` and `.taz` to `.tar`, `.svgz` to `.svg`, `.cpgz` to `.cpio`, `.wmz` to `.wmf`, `.emz` to `.emf`, and `.gz`, `.z`, `-gz`, `-z`, and `_z` removed). Zel only strips a case-sensitive `.gz`. `Readme.TXT.GZ` becomes `Readme.TXT` versus `Readme.TXT.GZ`. A forced gzip type on `pkg.tgz`, `x.svgz`, `x.z`, `x-gz`, or `x_z` follows those maps in Bazel and is left unchanged in Zel. A basename of exactly `.gz` stays `.gz` in Bazel and is an escaping path in Zel.
  - A later CRC, size, or trailing-garbage error can leave a partial output file. Zel writes only after inflate succeeds. Bazel creates the file with `0666` masked by the umask. Zel sets mode `0644`.
  - Extract also places the file under an added prefix when that option is set. This Bazel class has no added prefix. An absolute or `..` rename target can leave the destination in Bazel. Zel drops a leading `/` and errors if the first segment is `..`.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/PatchUtil.java`
- Bazel lines: 664
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/patch.rs`
- Verdict: partial
- Differences:
  - Plain multi-file diffs (a later `---` while a hunk header is already stored, with no `diff --git`) are one patch in Bazel: every hunk is applied to the last path, and a failure writes nothing. Zel flushes and writes the previous file when that `---` is seen, so a two-file plain diff updates both files while Bazel errors and leaves both unchanged. A second independent hunk of the same file can leave the first hunk on disk even though both report a content mismatch.
  - Hunks are applied one at a time with fuzz 2, but Bazel reverses the deltas after a sort by source position. Zel reverses parse order only, so out-of-order hunks that interact can edit different lines.
  - Bazel reads and writes the patch and the target as UTF-8, and the decoder replaces illegal bytes with U+FFFD. Zel treats each byte as a Latin-1 scalar and writes that low 8 bits, so a preserved `0xE9` stays `0xE9` in Bazel as the UTF-8 bytes of U+FFFD. Valid UTF-8 file bodies round-trip. Non-ASCII path text does not: Zel turns those Latin-1 scalars back into a UTF-8 path, so a UTF-8 `café.txt` in the patch is a different filename than Bazel's decoded path.
  - Chunk headers use ASCII whitespace only, and integer parse rejects counts or line numbers above the signed 32-bit maximum. Zel also skips Unicode whitespace, so a raw `0xA0` between `@@` and `-` is a valid header in Zel and a non-header in Bazel, and a count such as `2147483648` is a normal size in Zel.
  - Path trim only strips code points at or below U+0020. Zel also strips non-ASCII Unicode whitespace on `---`, `+++`, and rename paths.
  - A short hunk that misses at fuzz 0 throws an uncaught illegal argument in java-diff-utils. Zel returns that text as a patch failure. The target file is left unchanged either way.
  - A new mode shorter than the owner digit throws a string-index error (`new mode 10`). Zel returns `Truncated file mode at line …`.
  - Permission bits use effective access and change only the owner execute bit. Zel uses the owner mode triad and sets or clears all three execute bits, so preserving execute on a `0744` or `100755` file becomes `0755` in Zel and stays owner-exec-only in Bazel. A failed permission call is an I/O error in Bazel. Zel ignores permission errors and still returns success.
  - Symlinks are lexical in Bazel. Zel canonicalizes them: a symlink directory that points outside the output tree is patched through by Bazel and rejected by Zel. A dangling symlink is replaced by a regular file in Bazel and is an error in Zel.
  - Applying a patch to one path is absent. Bazel can limit a patch to one path, reject a rename of that path, and error on a zero strip when both names still have `a/` and `b/` prefixes. Zel always applies every section.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/SevenZDecompressor.java`
- Bazel lines: 149
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/codecs/sevenz.rs`
- Verdict: partial
- Differences:
  - A null or empty entry name makes Bazel throw `7z archive contains unnamed entry`. Zel stores an empty name and skips that entry, so the extract succeeds and the payload is dropped.
  - Anti-items are written by Bazel. Zel omits them.
  - An archive with no entries and no strip prefix returns the destination from Bazel and does not create it. Zel treats that as a missing prefix and returns the prefix-not-found error after creating the destination. A prefix miss also leaves the destination directory behind in Zel. Bazel creates nothing when every entry is skipped.
  - File mtimes come from the NTFS time on non-directories only. Zel ignores creation, access, and modification times, so file mtimes stay at extract time. Directory mtimes are untouched in both.
  - Bazel does not chmod. New files are `0666` masked by the umask, and an existing file keeps its mode when truncated. Zel sets every non-directory to `0644`, including when the umask is not `022`. Neither applies the Unix mode in the high half of the Win32 attributes.
  - Prefix match compares ISO-8859-1 prefix bytes to the UTF-8 entry name viewed as Latin-1. Zel compares Unicode scalars. A prefix that is the Latin-1 spelling of those UTF-8 bytes matches only Bazel. A prefix that is the single scalar U+00E9 matches only Zel. On-disk names for well-formed UTF-16 still match.
  - Prefix suggestions come from a hash set of the first segment of every original multi-segment name. Zel uses a sorted set of renamed names. Rename lookup is exact in both, but a rename of the first segment changes the suggestion list.
  - A leading `..` that survives normalization fails in both, with different sentences. Absolute paths are relativized before that check in both.
  - If both strip prefix and strip components are set, Bazel deprefixes and then strips components on the remainder. Zel ignores the prefix whenever the component count is nonzero. The usual callers reject the combination before extract.
  - A start-position property is unsupported in commons-compress. Zel ignores it and still extracts. Additional streams fail in Bazel. Zel parses that section, discards it, and extracts the main streams.
  - Header sections must be archive properties, additional streams, main streams, files info, then end. Any other order is a badly terminated header in Bazel. Zel accepts any order until the end marker. Empty-file or anti bits before the empty-stream bitset are a format error in Bazel. Zel reads those bitsets with a still-zero empty count, then treats every empty stream as a directory and not anti.
  - Commons-compress decodes a single encoded-header layer and only the first pack stream of folder 0. Zel allows up to four nested encoded headers and feeds every packed stream of the folder. A second encoded-header layer fails in Bazel and succeeds in Zel.
  - BCJ filters ignore coder properties in commons-compress, so the start offset is always 0. Zel applies a 4-byte little-endian start and errors if the property length is not 0 or 4. A nonzero offset changes the bytes. Delta distance ignores extra property bytes in Java. Zel requires exactly one property byte.
  - Counts above `1 << 28` are implausible in Zel and still legal in Java up to the signed 32-bit maximum. An encoded-header unpack size must fit in a Java int. Zel allows up to `1 << 40`. A folder larger than 1 TiB fails in Zel and can still stream in Bazel. Substream sizes use wrapping addition in Java and a checked add in Zel.
  - After each file, Bazel throws if the thread interrupt flag is set. Zel has no interrupt check.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/StripPrefixedPath.java`
- Bazel lines: 116
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`normalize`, `deprefix`)
- Verdict: partial
- Differences:
  - Entry bytes are ISO-8859-1 code units, one per byte. Zel decodes names and link targets as lossy UTF-8 first, so invalid UTF-8 becomes U+FFFD and a valid multi-byte sequence is one scalar. A Latin-1 prefix `é` (`0xE9`) matches a ustar byte `0xE9` in Bazel and does not match Zel's replacement character. The same prefix matches a UTF-8 `é` in Zel and does not match Bazel's two characters. Zip makes this worse: Bazel UTF-8-encodes the already decoded name and then this class reads those bytes as Latin-1.
  - A prefix character above U+00FF is replaced by `?`. A supplementary character is two UTF-16 surrogates, so it becomes `??`. Zel keeps the original scalar.
  - A prefix that normalizes to empty (`""`, `"."`, `"/"`, `"///"`, `"/."`) is still present. That empty relative fragment is a prefix of every relative path, so the prefix is found and the relative path is unchanged. Zel's empty segment list is the no-prefix path, so the prefix stays unfound. Non-empty archives still extract in both. A null entry or a null optional throws. Zel has no null string.
  - On an absolute path, Bazel drops `..` that would climb past the root (`/../foo` becomes `foo`). Zel only pops a real parent segment, so those become `../foo`. Prefix match, component counts, and the later escaping check all diverge. Relative `..` matches.
  - Host path rules apply. On Windows, `\` is a separator, a drive letter is uppercased, and an 8.3 segment can be expanded when that path exists. Zel splits only on `/`, keeps `c:` as a segment, and never resolves short names. On Unix, `\` is a literal in both.
  - Callers apply component stripping after deprefix and do not apply it to link targets. Zel's deprefix does both, and if both arguments are set it ignores the prefix. Extract rejects that pair, as Bazel's descriptor does. For an ordinary relative path the component drop matches. A negative count is an illegal argument in Java. Zel takes a `usize`.
  - Relative symlink targets are normalized and prefix-stripped and are not reattached to the destination. Zel copies the raw target. Absolute targets are stripped and joined back under the destination in both, but Zel's absolute test is only a leading `/` on the raw string, and it also runs the component strip. Disallowing absolute links rejects them in Zel. Bazel always rewrites them under the destination.
  - Zip uplevel checks differ. Bazel normalizes the target, then rejects only if the link's parent plus that target still starts with `..`. Zel walks the raw target and decrements a depth, so `foo/../../../x` from `a/b` is an error in Zel and is allowed in Bazel. Hard-link targets are deprefixed in both. Zel also rejects a target whose first segment is `..`. Bazel will link outside the destination.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/TarBz2Function.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`ArchiveKind::TarBz2`)
- Verdict: partial
- Differences:
  - USTAR and GNU long names and links are Latin-1. Zel decodes those bytes as lossy UTF-8, so any non-UTF-8 sequence becomes U+FFFD. Valid UTF-8 names match. PAX is UTF-8 on both sides, but commons-compress fails the extract on malformed PAX UTF-8. Zel inserts U+FFFD and continues.
  - Regular-file mtimes from the ustar field and from PAX mtime, including fractional seconds, are applied. Zel never sets mtime. Directories and symlinks are untouched on both sides.
  - Unix chmod keeps setuid, setgid, and sticky. Zel clears those three bits and forces owner-read.
  - Old GNU sparse entries and PAX sparse maps are expanded, with holes filled with zeros. Zel skips typeflag `S` and treats a sparse PAX member as a normal file whose bytes are the raw sparse payload.
  - A global PAX header is applied to later members. Zel discards it. Vendor xstar prefixes are not parsed, so those names stay un-prefixed.
  - Unknown typeflags are extracted as normal files. Zel drops them. Types `3`, `4`, and `6` become empty files in Zel even when the size field is nonzero. Bazel copies that payload.
  - Hard links are created when the entry is seen, so a link to a later member fails and aborts. Zel links after every file is written, so forward links succeed, and if the hard link fails it copies the target bytes.
  - Relative symlink targets are normalized. Zel stores the literal target. Absolute targets are rewritten under the destination on both sides.
  - A name that normalizes to `..` can be written outside the extract root. Zel returns an error once the first segment is `..`.
  - Missing-prefix suggestions come from a hash set, so the quoted list order is not sorted. Zel sorts them. The message text matches when there are zero or one candidates.
  - Concatenated bzip2 streams and randomised blocks are decoded by commons-compress. Zel decompresses concatenation but rejects randomised blocks. Group counts of 0 or 1, zero selectors, and selector indexes past the group count are accepted or capped in commons-compress. Zel requires 2 through 6 tables, at least one selector, and errors when the move-to-front index is past the group count. An empty block with a zero origin pointer is stream corruption in Java and an empty block in Zel.
  - GNU base-256 with a leading `0xFF` is a negative size and Java throws. Zel treats the field as an unsigned magnitude. A non-numeric PAX size is ignored in Zel and is a PAX parse error in Java.
  - Checksum and trailing-garbage failures are raised on the next read after every decompressed byte of that block has already been returned, and only if the tar reader actually reads that far. Zel decompresses the whole buffer first and, for trailing garbage and the two CRC errors, drops the last 1 through 512 decoded bytes before deciding the tar is complete, so the same archive can succeed with full contents in Bazel and lose the last record or fail in Zel. Bazel also leaves whatever entries were already written. Zel decides the byte buffer before creating files. The whole archive is loaded into memory, so a stream Bazel extracts can fail in Zel with an allocation error.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/TarFunction.java`
- Bazel lines: 32
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`extract_tar`)
- Verdict: partial
- Differences:
  - Both parse the raw tar bytes. The gaps are in commons-compress extraction versus Zel's tar parser.
  - USTAR and GNU names and link targets are ISO-8859-1: each header byte becomes one Latin-1 code unit, and those bytes are written back. Zel turns any non-UTF-8 byte into U+FFFD. An embedded NUL keeps the tail in Bazel and truncates the name in Zel. PAX path and linkpath are UTF-8 on both sides.
  - Regular-file mtimes are set from the ustar or PAX mtime in Bazel and left as now in Zel. Directories and symlinks are untouched on both sides.
  - Bazel keeps setuid, setgid, and sticky. Zel drops those three bits and forces owner-read.
  - A global PAX header stays in effect for later entries in Bazel and is discarded in Zel. Typeflag `X` is a PAX header in Bazel and is skipped in Zel, so the following entry keeps its ustar name and size.
  - Old GNU sparse entries and PAX sparse maps are expanded to the real file by commons-compress. Zel drops typeflag `S` and, for a normal file, writes the raw payload. When both `path` and `GNU.sparse.name` set the name, Bazel's hash-map iteration decides which one wins.
  - Any other typeflag is written as a regular file in Bazel and skipped in Zel. Character, block, and fifo entries are empty files on both sides only when the size is 0. A nonzero size is stored by Bazel and discarded by Zel.
  - A directory with a nonzero size is not consumed by Bazel, so the next header is read from that payload. Zel always skips the size rounded up to 512. A ustar name that ends in `/` with typeflag `0` or `7` stays a file on both sides.
  - Typeflag `5` gets a trailing slash before Bazel's rename lookup. Zel looks up the name without that slash unless the header already contained it.
  - Hard links are created when the entry is seen in Bazel, so a link before its target fails. Zel links after every entry, so a forward link succeeds and sees the final file. A link failure is an error in Bazel and a byte copy in Zel. A directory hard link is expanded into per-file links in Bazel. `..` in a hard-link target is created, possibly outside the destination, in Bazel and is an error in Zel.
  - Relative symlink targets are prefix-stripped in Bazel and left unchanged in Zel. Absolute targets are rebased under the destination on both sides. Zel also applies the component strip to them. Distinct symlinks are created in hash order in Bazel and in archive order in Zel.
  - A normalized path whose first segment is `..` is written, and can leave the destination, in Bazel and is rejected in Zel.
  - An empty archive with no required prefix succeeds in Bazel and does not create the destination. Zel returns the prefix-not-found error after creating the directory. With several candidate prefixes, hash order and sorted order produce different error text.
  - A binary numeric field with a leading `0xFF` is negative in Bazel and a large positive value in Zel. Sizes that do not fit in a signed 64-bit long abort Bazel. Zel accepts them up to `u64`. A PAX size that does not parse aborts Bazel and is ignored in Zel. Star and xstar headers use a 131-byte prefix in commons-compress and a 155-byte ustar prefix in Zel whenever the magic is `ustar`.
  - A truncated archive leaves every entry already written in Bazel. Zel fails before writing entries, after creating the destination. Bazel also stops on interrupt between entries. Zel has no interrupt check.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/TarGzFunction.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`ArchiveKind::TarGz`)
- Verdict: partial
- Differences:
  - A normal UTF-8 ustar or pax `.tar.gz` (modes in `0777`, no sparse map, no `..` entries) matches, including concatenated gzip members and per-member CRC-32 and ISIZE. Timestamps and the edge cases below do not.
  - Bazel writes each regular file's tar or pax mtime, including fractional seconds. Zel never sets mtime.
  - Bazel keeps setuid, setgid, and sticky. Zel stores only the lower nine bits and forces owner-read.
  - USTAR and GNU long names and link targets are Latin-1. Zel turns any non-UTF-8 byte sequence into U+FFFD, so the path bytes on disk change. Pax path and linkpath are UTF-8 on both sides. A strip-prefix code point above U+00FF is `?` under ISO-8859-1 and is kept as Unicode in Zel.
  - Bazel creates hard links when the entry is read, so a link to a later entry fails. Zel creates hard links after every file entry, so that archive succeeds. If the hard link fails, Zel copies the bytes and continues. Bazel aborts.
  - Symlink creation walks a hash map. Zel walks archive order. Nested symlink paths can therefore produce different trees.
  - Absolute symlink targets are prefix-stripped on both sides. Zel also applies the component strip to those targets. Bazel does not.
  - An entry that normalizes to `..` is written outside the destination by Bazel. Zel returns an error and writes nothing. A hard link whose target normalizes to `..` is rejected by Zel and is created by Bazel.
  - Typeflag `S` and other unknown typeflags are skipped by Zel. Commons-compress extracts old GNU sparse entries as expanded files and unknown typeflags as regular files. Pax sparse maps are expanded by Bazel. Zel either reads the sparse payload as ordinary file bytes or hits a short read. Global pax headers apply to later entries in Bazel and are ignored by Zel.
  - A negative pax size throws in Bazel. Zel ignores the bad pax value and uses the header size. A negative GNU base-256 size throws in Bazel. Zel treats the high bit as a large positive value. Illegal octal in uid, gid, or mtime throws in Bazel. Zel does not parse those fields, so extraction continues. Header checksums are not enforced on either side.
  - A gzip CRC, ISIZE, or trailing-garbage error aborts Bazel. Bytes inflated in that same read are not returned, and the extract fails after any earlier entries were already written. Zel appends the member, then drops only the last 1–512 bytes and still extracts when that prefix contains a finished tar. The same trailer can be a failed extract in Bazel and a successful, longer tree in Zel.
  - Prefix suggestions are the same set, but Bazel joins them in hash order and Zel joins them sorted, so the possible-prefixes sentence differs. With no required prefix, an empty archive succeeds in Bazel. Zel still reports a missing prefix when the strip prefix is empty and there are no entries.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/TarXzFunction.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`ArchiveKind::TarXz`)
- Verdict: partial
- Differences:
  - The Java xz stream concatenates streams and skips 4-byte zero padding between them. Zel stops after the first stream footer, so a tar that continues in a later stream is only partially extracted. Trailing non-xz bytes are ignored by Zel. Bazel throws when the tar reader still needs bytes to finish the end marker.
  - Check identifiers other than none, CRC32, CRC64, and SHA-256 throw in xz-java. Zel accepts the other identifiers and does not verify them.
  - The LZMA2 dictionary-size property is only range-checked. xz-java uses a circular dictionary of that size and rejects longer distances. Zel will decode those matches. A huge declared dictionary is allocated up front by xz-java and is not allocated by Zel.
  - A later block whose checksum fails can still be accepted when the bytes already decoded contain the tar end marker. The Java stream fails the read that enters that block.
  - USTAR and GNU long names and links are raw header bytes. Zel uses lossy UTF-8, so invalid UTF-8 becomes U+FFFD and the path bytes change. PAX path and linkpath are UTF-8 with the same replacement on both sides.
  - Global PAX headers are applied to later entries by commons-compress and dropped by Zel. Extended PAX in Zel only honors path, linkpath, and size. Sparse maps are ignored, so sparse files stay unexpanded. GNU sparse typeflag `S` is skipped instead of extracted. A non-numeric PAX size is ignored by Zel and is an I/O error in Bazel. PAX sizes above the signed 64-bit maximum parse in Zel and fail in Java.
  - GNU base-256 numeric fields with a leading `0xFF` are negative in commons-compress. Zel treats that byte as a positive value, so size and mode can disagree and the tar stream desynchronizes.
  - Unknown typeflags are extracted as regular files by Bazel. Zel skips them. Regular-file mode keeps setuid, setgid, and sticky in Bazel and drops those bits in Zel. Bazel also sets the archive mtime. Zel leaves the extraction-time mtime.
  - Hard links are created when the entry is seen. A link that appears before its target fails in Bazel and succeeds in Zel. A hard link to a directory is expanded recursively by Bazel. Zel's hard link fails and the copy does not copy a directory, so extraction errors. If a file hard link fails, Zel copies bytes instead of failing.
  - Relative symlink targets are normalized (`foo/../bar` becomes `bar`). Zel stores the raw target string. Symlinks are created from a hash map, so when one link is a path component of another the creation order is hash order. Zel uses archive order.
  - Missing-prefix suggestions are joined in hash order in Bazel and in sorted order in Zel, so the error text differs.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/TarZstFunction.java`
- Bazel lines: 35
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`ArchiveKind::TarZst`)
- Verdict: partial
- Differences:
  - Bazel caps the zstd window (`1<<31` on 64-bit, `1<<30` on a 32-bit JVM). A larger window fails. Zel never caps the window descriptor and will extract archives Bazel rejects.
  - An empty `.tar.zst` is a successful empty tar stream in Bazel. Zel rejects it as a truncated frame header. With no prefix, Bazel returns the destination. Zel always errors on zero tar entries.
  - Bazel only pulls zstd bytes until the tar end records and the rest of the current block are present, and close does not finish the frame. A bad content checksum, content-size mismatch, or later concatenated frame past that point is ignored. Zel always decodes the whole file. A normal tar whose frame checksum is wrong can therefore succeed on one side and fail on the other.
  - A zstd or tar error mid-archive leaves files Bazel already wrote. Zel decompresses fully before creating entries, so those failures leave the destination empty.
  - A nonzero dictionary id fails both, but the text differs: the JNI dictionary mismatch versus Zel's unsupported-dictionary sentence, which is not rewritten to the Java message.
  - USTAR and GNU long-name bytes are Latin-1. Zel's C-string decode uses lossy UTF-8, so any non-UTF-8 name becomes U+FFFD on disk. PAX path is UTF-8 on both.
  - Bazel applies global PAX headers to later entries. Zel ignores them, so a global path, linkpath, or size changes names and payloads only in Bazel.
  - Unknown tar typeflags, including GNU sparse, are regular files in Bazel. Zel writes nothing for them.
  - Bazel chmods regular files with the archive mode forced owner-readable, so setuid, setgid, and sticky survive, and sets the header mtime. Zel keeps only the lower nine bits plus owner-read and leaves the extract-time mtime.
  - Hard links are created when the entry is read, so the target must already exist. A directory target is replicated by recursively hardlinking children. Zel creates hard links only after every file, so a link to a later entry succeeds, a missing target is an error, a directory target fails, and a failed hard link is copied into a new inode.
  - Relative symlink targets are normalized (`foo/../bar` becomes `bar`). Zel stores the raw target string. A hard-link target that still escapes with `..` is an error in Zel and a link outside the destination in Bazel.
  - Bazel stops on one all-zero record, then reads a second record and throws if that read is short. Zel treats the first all-zero record as end of archive and does not require the rest of the end marker.
  - Prefix suggestions are joined in hash order versus sorted order, so the same missing prefix with two or more candidate directories produces a different message. Deferred symlinks are a hash map versus archive order. When one symlink path is a prefix of another, the tree can differ.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/XzFunction.java`
- Bazel lines: 50
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/codecs/xz.rs`
- Verdict: partial
- Differences:
  - Bazel case-folds with the English locale and maps `.xz` to strip, `-xz` to strip, and `.txz` to `.tar`, and only when the suffix is shorter than the whole name. Zel only strips a case-sensitive `.xz`. With type `xz`, `foo.XZ` becomes `foo` in Bazel and stays `foo.XZ` in Zel. `a-xz` becomes `a` versus `a-xz`. `pkg.txz` becomes `pkg.tar` versus `pkg.txz`. A basename of exactly `.xz` stays `.xz` in Bazel and fails in Zel.
  - `İ` (U+0130) expands to `i` plus a combining dot under English lowercase, so the suffix cut uses the wrong UTF-16 length (`aİ.xz` becomes `aİ.`). Zel strips `.xz` from Unicode scalars and keeps `aİ`.
  - Non-UTF-8 basename bytes are kept as Latin-1. Zel replaces them with U+FFFD, so the written name differs.
  - An absolute rename target replaces the destination in Bazel. Zel drops the leading `/` and writes under the destination. A `..` target escapes the destination in Bazel and is an error in Zel.
  - An LZMA2 dictionary property byte above 37 is rejected by xz-java. Zel accepts property bytes 0 through 40 and does not allocate that dictionary, so the same stream succeeds in Zel and fails in Bazel. Even for a smaller property, xz-java allocates the full dictionary with no memory cap. Zel never does.
  - Check identifiers other than none, CRC32, CRC64, and SHA-256 throw in xz-java. Zel accepts every identifier 0 through 15, skips the reserved check field, and still emits the payload.
  - Match distances are limited to the LZMA2 dictionary window in xz-java. Zel allows any distance back to the last dictionary reset, so a stream that repeats past the declared dictionary fails in Bazel and decompresses in Zel.
  - On a corrupt block after the output file is opened, Bazel leaves an empty or partial file and then throws. Zel writes the output only after decompression succeeds.
  - Bazel creates the file as `0666` masked by the umask and does not chmod. Zel forces `0644`, including when the file already existed.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/ZipDecompressor.java`
- Bazel lines: 218
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/archive.rs` (`extract_zip`)
- Verdict: partial
- Differences:
  - Bazel writes regular-file mtimes from the ZIP DOS timestamp (JVM local timezone, 2-second resolution; dates outside the DOS range become 1980-01-01 local). Zel leaves the extraction-time mtime.
  - With no Unix mode in the high 16 bits of the external attributes, Bazel chmods `0755`. Zel chmods `0644`. The same split applies when the high half is 0 even if the host is Unix.
  - Bazel uses those high 16 bits as a Unix mode for any host. Zel honors them only when the central-directory host byte is Unix. Otherwise a directory or symlink mode becomes a regular file, or a directory only if the name ends in `/` or the Windows directory bit is set, and a deflate payload is written as file bytes.
  - An unrecognized low byte makes Bazel throw. Zel stores the entry as a `0644` file. Bazel keeps setuid, setgid, and sticky. Zel keeps only the lower nine bits plus owner-read.
  - A symlink whose uncompressed size is 256 through 4096 fails in Bazel. Zel accepts it and only errors above 4096 bytes. An empty target becomes a symlink to `.` in Bazel. Zel's empty symlink fails the extract.
  - Bazel reads the symlink body with one read. A short read fails even if more bytes remain. Zel reads the full declared size. Symlink text is raw bytes via ISO-8859-1. Zel uses lossy UTF-8. A relative target that starts with the strip prefix is rewritten by Bazel and left unchanged by Zel. With a component strip set, Zel also strips that many leading segments from an absolute target. Bazel only strips a leading `/`.
  - Entry paths are the UTF-8 name viewed as ISO-8859-1 code units, while the prefix is the Java string encoded as ISO-8859-1. A Unicode prefix `é` therefore does not match a UTF-8 zip entry `é/…`. Zel compares Unicode strings, so those two cases are reversed. A NUL in a name is truncated in Bazel and fails the path call in Zel. Ill-formed UTF-8 can become a different number of replacement characters, which changes the rename key, the path, and prefix matching.
  - `../` segments survive normalization and are written outside the destination. Zel rejects a relative path whose first segment is `..`.
  - Duplicate central-directory names keep the last entry in Bazel, at the first name's position. Zel extracts every entry, so a symlink followed by a file of the same name ends as a symlink in Zel and as the file in Bazel.
  - Compression methods other than store and deflate throw in Bazel. Zel inflates Deflate64 and copies any other method as stored bytes.
  - If the output path of a symlink already exists as a file or directory, Bazel throws. Zel deletes a file or an empty directory and creates the link. Distinct symlink paths are created in hash order in Bazel and in central-directory order in Zel.
  - Missing-prefix suggestions are a hash set of the first segment of the original entry name. Zel uses a sorted set of the renamed name, so the message order and the non-ASCII text differ.
  - This class applies the prefix and then the component strip. Zel ignores the prefix whenever the component count is nonzero, and extract errors if both are set. Zel also creates the destination before entries. Bazel creates directories only for entries it materializes. Bazel can throw an interrupt after a file copy. Zel has no interrupt check.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/ZstFunction.java`
- Bazel lines: 43
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/codecs/zstd.rs`
- Verdict: partial
- Differences:
  - A raw, RLE, or compressed block whose size is greater than the frame window and at most 128 KiB is rejected by zstd-jni. Zel only enforces 128 KiB, so the same frame succeeds and its bytes are written.
  - English lowercasing strips a case-variant suffix such as `.ZST`. Zel's strip is byte-exact, so a forced `zst` archive named `File.ZST` is extracted as `File` by Bazel and as `File.ZST` by Zel. Extension detection itself is case-sensitive on both sides.
  - Bazel path strings are Latin-1. Zel replaces a non-UTF-8 basename with U+FFFD before the suffix strip and before the rename lookup, and the output path is UTF-8. Valid UTF-8 names round-trip to the same bytes.
  - Rename lookup is exact after the suffix strip. Bazel then keeps an absolute rename target as an absolute path and normalizes `..` out of the destination. Zel drops a leading `/` and errors when the name is empty or the first segment is `..`.
  - Bazel writes the output with mode `0666` masked by the umask and does not chmod. Zel sets the file to `0644`.
  - On a decode or checksum error, Bazel has already streamed bytes into the destination file. Zel decodes the whole frame first and creates no file on error.
  - Both decode every concatenated frame and skip skippable frames. An end of file in the middle of a frame is a truncated source. One to three trailing bytes after a finished frame are also a truncated source in zstd-jni. Zel reports a bad magic number, which is rewritten as an unknown frame descriptor.
  - A frame content-size mismatch is data corruption from libzstd. Zel's mismatch sentence is not rewritten. A nonzero dictionary id is a dictionary mismatch in zstd-jni and an unsupported-dictionary sentence in Zel. Checksum failure text matches after the Java-message rewrite. Other literal and bitstream failures stay as Zel's own strings.
  - Strip prefix and strip components do not change the single output name on either side. Zel still places that file under an added prefix when the option is set. This Bazel class has no added prefix.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/CheckContentLengthInputStream.java`
- Bazel lines: 75
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`http_get`)
- Verdict: partial
- Differences:
  - Bazel counts bytes actually returned and compares only when a read returns end of file or on close. A body longer than the expected size is still delivered. Exhausting it throws `Bytes read {actual} but wanted {expected}`, which the download manager retries by type. Zel never returns more than the declared length, ignores trailing socket bytes, and treats "at least that many bytes" as success. A short body is `Download size mismatch: Content-Length was {n} but got {len} bytes` (argument order reversed) and the partial buffer is discarded.
  - Close checks after the delegate close, including when the caller read nothing, so an early close with unread bytes still fails if the counts differ. If the delegate close throws, the length check is skipped. A matching end-of-file check is repeated on later end-of-file reads and on close. Zel checks once after the bounded read and has no close-time check.
  - An I/O error from the delegate does not compare lengths. A later close still compares the short count and can throw the mismatch. Zel returns the read error and does not also report a size mismatch.
  - The counts are signed 64-bit values and the running total wraps. A negative expected size mismatches any normal count. Zel parses the header as a `usize` and compares with no wrap, because the read is capped.
  - An invalid content length makes Zel fail and is not retryable. Bazel catches the parse error and skips this wrapper, so the download continues with no length check. Both accept plain decimal and a leading plus. Zel uses the first content-length after trim and ASCII lowercasing, and skips the check when transfer-encoding contains `chunked`. This class has no chunked logic. The HTTP stream still installs it whenever the length parses.
  - Java treats only `-1` as end of file. A zero return does not increment and does not check. A non-minus-one negative from `read` is counted as one data byte. Rust treats a zero-length read as end of file, so a premature zero-length read can report a mismatch. Skip goes through read, so skipped bytes count. Available never triggers the check.
  - The HTTPS path does not apply this check. Bazel wraps the connection stream for both HTTP and HTTPS.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/Checksum.java`
- Bazel lines: 167
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/hash.rs`
- Verdict: partial
- Differences:
  - `blake3-` plus 32 decoded bytes is a valid Bazel checksum. Zel rejects it. Zel also has no BLAKE3 hex path. 64 hex digits are always SHA-256.
  - SRI base64 accepts only the standard alphabet, allows omitted padding, and rejects a bad pad. Zel treats `-` as `+` and `_` as `/`, strips every trailing `=`, then decodes, so those strings and base64url payloads succeed whenever the decoded length matches.
  - Bazel matches the whole string: a case-sensitive algorithm prefix and then base64 of the entire remainder, with no trim. Zel trims Unicode whitespace and parses only the first whitespace-separated token. A leading or trailing space, or a second integrity token, fails in Bazel and is a successful first-token checksum in Zel.
  - Hex parsing requires the entire string to match the digit length, with no trim, and reports `Invalid SHA-256 checksum`. Zel trims, then reports `Invalid hex checksum` or `Invalid sha256 checksum` with the expected character count. Surrounding whitespace is accepted only by Zel.
  - `toString()` is lowercase hex. Zel's display is the integrity form. The hex method matches Bazel's string form, and the integrity method matches for SHA-1, SHA-256, SHA-384, and SHA-512.
  - Bazel stores whether the original form was integrity, and mismatch text prints another digest in that same form. Zel does not store that flag. Verify always emits lowercase hex. Integrity wording is applied later only when the request integrity string is nonempty.
  - Hash codes are different functions of the digest and the algorithm, so hash-map bucket order would not match.
  - A null string fails as `Invalid SHA-256 checksum 'null'`. A null integrity string throws. A Rust string cannot represent those inputs.
  - A missing checksum is an I/O exception. An invalid checksum is a checked exception, not an I/O exception. Zel uses a string error for both, with no separate exception type.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/ContentLengthMismatchException.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`http_get`)
- Verdict: partial
- Differences:
  - Bazel is a typed I/O exception storing signed actual and expected sizes, with message `Bytes read <actual> but wanted <expected>`, including negatives and the minimum long. Zel has no such type. The plain HTTP path returns `Download size mismatch: Content-Length was <expected> but got <actual> bytes` (operands swapped), later wrapped as an error downloading the URLs.
  - The download manager retries when this exception is the failure or only a suppressed throwable. Zel marks that short-read error retryable only. There is no suppressed-exception walk, so a non-retryable error that merely carried this failure would not be retried.
  - Bazel records whatever longs the caller passes and reports them at end of file or close when they differ, including a body longer than the content length. Zel parses the header as a `usize` and caps the read. Only a short body errors. Extra bytes are dropped and the prefix is success. A leading plus is a Java long but an invalid content length here, and that error is not retryable. Values above the signed 64-bit maximum cannot be stored in this exception. On 64-bit, Zel accepts them through the unsigned maximum. A wrapped long count is printed as a negative decimal. Zel's length does not wrap.
  - HTTPS uses curl, not this message. A short body is curl exit 18. An over-long body is not this mismatch.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/DelegatingDownloader.java`
- Bazel lines: 77
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel keeps a constructor default and a nullable override. Download uses the override when it is set and the default after construction or after clearing the override. Zel has no such type. Repository fetches always call the downloader built for that evaluation.
  - The remote module installs the gRPC remote downloader, so file downloads go through the remote asset API. Local HTTP fallback happens only inside that remote downloader when the local-fallback flag is set. Registry reads stay on the HTTP downloader and never enter this class. Zel lists the remote-downloader flags and does not read them, so those flags do not change the fetch path, and there is no remote-then-local fallback or credential propagation to a remote downloader.
  - The delegate field is not volatile. A set on another thread has no happens-before with download, so a reader can keep the previous downloader. One call is either the default or the override. A swap during an in-progress call does not retarget that call. Zel builds a new owned downloader per evaluation, so nothing can retarget a later fetch.
  - Every argument is forwarded unchanged: the URI list, the header map (every value kept, caller map order), credentials, an optional checksum, the canonical id, the destination, the event handler, the client environment, the archive type, and the context. I/O and interrupt exceptions propagate with no wrapping. Zel uses URL strings, a sorted map of one header value per name, a nested auth map instead of credentials, separate sha256 and integrity strings, no event handler or client environment, and the archive type is applied by the repository context before the call. Failures are strings with no interrupt type. An empty context is replaced by the first URL. This method would forward the empty string.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/DownloadManager.java`
- Bazel lines: 602
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`Downloader::download`)
- Verdict: partial
- Differences:
  - Checksum hits run in a different order. Bazel rewrites URLs first, then hashes the destination, then the repository cache, and only then the distdir. If the rewriter dropped every URL it throws a cache-miss error and never looks at the distdir, even when downloads are disabled. Zel checks the output file, cache, and distdir on the original URLs before rewrite or the disable flag, so a distdir hit can succeed, and a miss with downloads disabled reports that download is disabled.
  - Distdir names differ. Bazel searches only the main rewritten URL's basename, after UTF-8 percent-decoding and `.`/`..` normalization, plus the destination basename. Zel searches every original URL's basename, percent-decoded as UTF-8 with U+FFFD for bad escapes, with no `.`/`..` collapse, so `%2F`, `..`, and non-UTF-8 escapes pick a different file and a decoded `..` can join out of the distdir.
  - With a type present, Bazel treats the output as a directory and writes a sanitized basename. An empty basename becomes `temp`. The type suffix is appended only when the basename does not already end with it, and each UTF-16 code unit outside the safe set becomes `_`. Zel takes the raw last path segment, defaults to `archive`, always appends the type, and does not sanitize. A blank type is treated as absent.
  - A matching file already at the destination is returned unchanged. Zel chmods it to `0644` or `0755`. Cache and distdir installs in Bazel copy attributes or, when allowed, hardlink. A cross-device link error is swallowed and the download continues. Zel always writes a private copy and never hardlinks.
  - A corrupt or unreadable repository-cache entry is ignored before downloading again. Zel's cache error fails the fetch. After a successful download, a cache put failure fails the call in Bazel. Zel warns and still returns success. Bazel also touches the cache file on a hit.
  - Setting retries to 0 disables Bazel's loop. Zel only applies the flag when it is greater than 0, and the default is already 5, so 0 still retries. Bazel retries content-length mismatch, socket exceptions, and unknown host, including one level of suppressed exceptions, with no sleep in this class. On the streaming download, unknown host is wrapped twice and is not retried. On the single-URL read, a suppressed unknown host is retried. Zel marks unknown host non-retryable, retries 408, 429, 503, connect failures, and its own content-length error with exponential sleep, and does not retry HTTP 500 or 502 here.
  - An interrupted I/O exception becomes an interrupt. A thread interrupt and a phase cancellation abort before any I/O. Zel has no interrupt or phase cancellation.
  - Disable-download text uses the fetch context. Zel's request context is the first URL, so the same failure names the URL.
  - Auth is not the same map. Bazel builds credentials from credential helpers falling back to static credentials, and adds Basic only for URLs the rewriter marked rewritten, with userinfo encoded as ISO-8859-1. Netrc is read as ISO-8859-1. Zel has no credential helper. Starlark auth is looked up by the post-rewrite URL string. Userinfo and netrc are applied for every fetched URL. Netrc is UTF-8, so a Latin-1 file with bytes at or above 0x80 is skipped, and non-ASCII passwords are UTF-8 in the token. Host lookup uses the URL host as written. Bazel's host is lowercased. Headers are a sorted map of one value per name instead of a list-valued map.
  - The single-URL bzlmod read ignores the disable flag, skips the distdir, applies netrc to the original URL then the rewriter, and rejects a null body. Zel's registry fetch does not use that method: no rewriter, no netrc, no manager retry. Unknown host is permanent. 404 is a missing file. A process-wide map memoizes both hits and failures. Lockfile checksums can be served from the repo cache, but a checksum miss is reported after the fetch.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/DownloadProgressEvent.java`
- Bazel lines: 101
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs`
- Verdict: partial
- Differences:
  - Progress text is non-empty only when bytes read are positive. With a total it is the binary size plus a US-locale percent of bytes over total, and a total of zero is forced to 100 percent. Without a total it is that size plus the raw count with the JVM default locale grouping separator. Zel's bar is the URL and an elapsed-seconds suffix. The byte count is written and never read, and nothing calls the progress update.
  - A non-positive byte count, including a negative long, yields an empty string. A Rust count cannot represent that negative. The size string is binary and US-locale: `N B` under 1024, otherwise KiB, MiB, GiB, and TiB with one decimal and half-even rounding. `1048575` becomes `1024.0 KiB`. Past 1 TiB the unit stays TiB. Counts above 2^53 are not exact in double, so both the tenth and the percent can diverge from the integer ratio. A ratio above 1 prints above 100 percent. A negative total prints a leading minus. The two formatters are shared and unsynchronized, so overlapping calls can corrupt the text. Zel's byte printer is decimal and is not used for downloads.
  - The resource identifier is the original URL's string form: scheme lowercased, non-ASCII and illegal characters percent-encoded, so distinct input strings can collapse to one fetch. Zel keys and prints the raw string. Later truncation cuts with UTF-16 length in Bazel and with Unicode scalar count in Zel.
  - The actual URL is stored and may be null. It does not affect the progress string or the resource id. Zel has no redirect URL.
  - Finished is a field. The one-argument constructor is zero bytes, empty total, not finished. A later unfinished event with a non-positive count clears the suffix while the fetch stays active. Zel's finished handler only removes the URL and does not keep a final size or total.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/Downloader.java`
- Bazel lines: 56
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`Downloader::download`)
- Verdict: partial
- Differences:
  - Bazel always fetches and streams into the destination. Zel returns before any network I/O when the existing output matches the checksum, and it also serves the repository cache and the distdir and applies the URL rewriter and the disable flag inside this call. A checksum hit therefore keeps the old bytes in Zel and is replaced by a fresh download in Bazel.
  - The archive type is passed through and suppresses gzip when the extension is `gz` or `tgz`. Zel has no type argument. Bazel sends `Accept-Encoding: gzip` and then gunzips only when the URL extension is not one of the archive suffixes and the type is not gzip. Zel never sends that header and always gunzips a gzip content-encoding, so a `.gz` payload that the server labels gzip is stored compressed by Bazel and uncompressed by Zel.
  - Headers are a map of lists: one HTTP line per list entry, insertion order, with the encoding and user-agent forced afterward. Zel stores one value per name, emitted in sorted order. A caller user-agent cannot replace Bazel's. On plain HTTP, Zel always adds `User-Agent: zel` as well as the caller's header. On HTTPS, Zel does not add that header and curl's own user agent is used. Non-ASCII header values are ISO-8859-1 in Java and UTF-8 in Zel.
  - Auth is refreshed per request URL, including after a redirect. A metadata failure is a warning and the request continues without those headers. Zel has no credentials parameter: static basic or pattern auth, URL userinfo, or netrc. On a redirect that changes host, port, or scheme it drops authorization and cookie and does not re-resolve credentials for the target.
  - The client environment selects the proxy. Zel has no client-env argument: plain HTTP is a direct TCP connection, HTTPS is the system curl binary. There is no fair semaphore around connects.
  - Bazel follows 301, 302, 303, and 307, up to 40, and treats 308 as a permanent error. Zel also follows 308, and the default cap is 20. Bazel accepts 206 as success. Zel accepts only 200. A non-numeric content length is ignored in Bazel and is a hard error in Zel. Ports outside 0 through 65535 fail Zel's port parse.
  - Bazel takes an already built checksum, including BLAKE3. Zel takes sha256 and integrity strings, errors if both are set, and has no BLAKE3. Bazel hashes while streaming into the destination, so a failed transfer leaves a truncated file. Zel buffers the full body, checks, then renames a temp file into place. Zel also forces mode `0755` or `0644`. Bazel creates the file with the process umask and does not take an executable flag.
  - Bazel uses 8 attempts, jittered exponential backoff from 100 ms, a connect timeout of 1 second doubling to 10 seconds, and a 20 second read timeout. Zel defaults to 5 extra passes over the whole mirror list, delay 100 ms doubled with no jitter, and a 60 second timeout. An empty URL list is `Error downloading [] to <dest>` in Bazel and the same text plus `: no URLs given` in Zel. The canonical id and context are unused by the HTTP downloader. Zel uses the canonical id as a cache identity and the context in the disable-download error.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/HashInputStream.java`
- Bazel lines: 94
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` and `/workspace/zel-cursor/crates/zel-bzlmod/src/hash.rs`
- Verdict: partial
- Differences:
  - Bazel hashes only bytes actually pulled through read, skip, or transfer, and checks the digest only when a read hits end of file or on close. A consumer that stops early, or closes before draining the delegate, hashes a prefix, so a correct full-file checksum fails. Zel hashes the entire fetched buffer once, after the fetch returns, and only then accepts or rejects it.
  - On a mismatch, the read that sees end of file throws and does not return end of file. Bytes already returned stay returned, and every later end-of-file read or close throws the same error again. A matching stream returns end of file and later close succeeds. Zel returns the whole body or an error. A bad checksum never writes the output and is not a stuck stream.
  - Close closes the delegate first and skips the check if that close throws. A truncated read that is then closed therefore reports a prefix checksum mismatch instead of the I/O error. Zel does not verify a partial body.
  - A zero-length read does not check, including at end of file. Available does not update the hasher. Mark is unsupported and reset throws, so the hasher cannot be rewound. Zel has none of these stream operations.
  - The mismatch text is `Checksum was %s but wanted %s`, with both sides in the checksum's own format: lowercase hex, or the subresource-integrity prefix plus padded base64. Zel's verify always prints lowercase hex. The download loop rewrites that to the integrity form only when an integrity string was supplied, then wraps it and tries the next URL. Registry fetches always compare lowercase SHA-256 hex.
  - Bazel can stream a BLAKE3 checksum. Zel rejects `blake3-` before any compare, and it verifies only the first whitespace-separated integrity token. Bazel's parser base64-decodes the remainder of the string, so a second token fails there.
  - The hasher is not locked. Two overlapping checks can throw an illegal state instead of the checksum error. Zel's verify is a pure function of the buffer.
  - For a fully consumed SHA-1, SHA-256, SHA-384, or SHA-512 payload the digest and the hex or integrity mismatch sentence match.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/HashOutputStream.java`
- Bazel lines: 92
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` and `/workspace/zel-cursor/crates/zel-bzlmod/src/hash.rs`
- Verdict: partial
- Differences:
  - Bazel hashes each write and copies those bytes to the delegate immediately. The comparison runs only in close, after the delegate close. Zel hashes the finished in-memory body and commits the output only after a match, via a temp file and rename.
  - A mismatch still leaves the bad bytes in the Bazel delegate. Close then throws `Checksum was %s but wanted %s`. Zel drops that body, tries the next URL, and the final error is wrapped as an error downloading the URLs.
  - The first close finalizes the hasher once and caches the actual digest. Later closes repeat that same result and ignore bytes written after the first close. If the delegate close throws, the check is skipped, so a wrong digest is not reported. Zel checks the buffer it is about to store, with no second close and no close-failure path that hides a mismatch.
  - Write updates the hasher before the delegate write. A throw from the delegate still advances the digest, so a later successful close can accept or reject bytes the delegate did not store. Zel hashes only the slice passed to the output write.
  - Flush only flushes the delegate. Zel verifies only the completed body.
  - Hex checksums print as lowercase hex on both sides. An integrity checksum makes Bazel print the algorithm prefix plus standard padded base64. Zel's download loop uses that text only when an integrity string is present. Cache lookup itself always prints hex and appends the delete-directory sentence.
  - BLAKE3 can be this stream's hasher. Zel rejects `blake3-`, so that checksum is never checked.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/HttpConnector.java`
- Bazel lines: 330
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/registry.rs` (`fetch_http_with_retries`)
- Verdict: partial
- Differences:
  - Zel only uses this port for bzlmod registry GETs. Connector attempts are never set, and the timeout scaling and max retry timeout flags are not applied. Bazel scales connect and read timeouts and caps the pre-jitter backoff when the max retry timeout is nonzero.
  - Bazel returns a live connection for 200 and 206 and leaves an idle read timeout on that stream. Zel buffers the whole body inside curl and uses a total deadline of at least 20 seconds, so a slow transfer and a stalled read fail differently.
  - `file:` is opened directly, with the scheme compared case-insensitively, and is not retried. The registry fetch only accepts a lowercase `file://` prefix, rejects a non-local authority, and reads the file itself.
  - No caller headers. Bazel applies the per-URL header map, drops `Accept-Encoding` for archive extensions, and otherwise sets a bazel user-agent when unset. Curl sends its own user-agent and `Accept: */*`, which changes the selected response.
  - Bazel honors `http_proxy` and `HTTP_PROXY` (and the https pair), Java proxy properties, and `http.nonProxyHosts`, and sets proxy authorization itself. Curl ignores uppercase `HTTP_PROXY` and uses `no_proxy` instead.
  - Redirects are delegated to curl with a maximum of 40. Curl follows 308 and follows 40 hops. Bazel does not follow 308 and throws on the 40th redirect response, so only 39 hops are followed. A redirect loop is an immediate unrecoverable error. Curl's loop exit is treated as a retryable I/O error.
  - Location handling is curl's. Bazel rejects a missing location and a non-http target as retryable I/O errors, inherits the fragment, copies userinfo on the same origin, and preserves the location's original encoding. A 301 replaces the original URL, so later retries use the post-301 URL. Zel always retries the input URL.
  - 404 and 410 become a missing file. Bazel throws file-not-found and does not retry. Other permanent statuses are only a string, not the unrecoverable exception with the reason phrase.
  - 403, 408, 429, and 5xx other than 501 and 505 are retried in both cases, but Zel must first download the error body. A large 403 can hit the time limit and be classified as a connect timeout.
  - Backoff starts at `100 * 2^attempt` and then diverges. Jitter is a random factor in `[0.75, 1.25)` truncated to an int, not clock nanoseconds. A socket timeout still sleeps 1 ms in Bazel and does not sleep in Zel. Zel also stops doubling earlier. Bazel's scaling keeps doubling until the integer saturates.
  - Connect-timeout doubling is hard-coded from 1 second to 10 seconds. TLS is permanent for several curl exits, using lossy UTF-8 stderr. Bazel retries an SSL exception unless the message contains `certificate` or `CertPathValidatorException`, so a transient handshake is not retried in Zel. Unknown host is permanent in both, but Zel's message includes userinfo and port. An illegal URL or port is an immediate unrecoverable error in Bazel. The same curl failure falls through to the retryable branch. Interruption aborts in Bazel and is ignored by Zel.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/HttpConnectorMultiplexer.java`
- Bazel lines: 153
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`headers_for`, `http_get`, `curl_get`)
- Verdict: partial
- Differences:
  - Bazel always sends `Accept-Encoding: gzip` and `User-Agent: Bazel/` plus the release name. Those two keys are applied after the caller headers, so the caller cannot replace them. Credential metadata is applied later and can replace them. Zel's HTTP writer emits `User-Agent: zel`, `Accept: */*`, `Connection: close`, and `Host` first, then the caller headers, so a caller user-agent is a second field. Zel does not send `Accept-Encoding` unless the caller supplied it. HTTPS keeps curl's own user-agent unless the caller set one.
  - Caller headers are a map of lists: each list entry is a separate header line, in insertion order. Same-key merges keep the last value. Zel stores one value per key, iterated in Unicode-scalar order. Header names compare case-sensitively in both.
  - The header function copies the base headers, then credential metadata, and credential headers replace the same key. Static credentials match the whole URI. An I/O error from the credential lookup warns and continues with no auth headers. Zel adds authorization only when no existing name equals `authorization` in ASCII case-fold, so a caller authorization wins. The auth map is keyed by the raw URL string. A missing or unreadable netrc is skipped with no warning. Zel has no credential-helper header map.
  - The header function is invoked again for every connect and redirect URL, so netrc and per-URI credentials are recomputed for the URL being requested. On resume, a key that is already present throws. Zel builds headers once. It drops authorization and cookie when scheme, host, or port differ, and does not look up auth for the new host. There is no range resume.
  - A lost read reports progress and reconnects to the URL of the successful connection. Zel fails the attempt on a dropped read and does not emit that line.
  - Connect throws an interrupted I/O exception when the thread interrupt flag is set, after the scheme check and before any connect. Zel does not check interruption.
  - Schemes other than `http`, `https`, and `file` throw an illegal argument with no message. A null scheme throws. Zel returns `Unsupported URL scheme` after lowercasing the scheme.
  - The checksum and type are passed into the stream, which hashes while streaming and skips gunzip for gzip content-encoding when the path or type ends in `gz` or `tgz`. Zel checks the checksum only after the full body is buffered, and gunzips when content-encoding is exactly `gzip`, including `.gz` payloads.
  - Header values this class assembles are written as ISO-8859-1, and values outside U+00FF are rejected. Zel writes those values as UTF-8.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/HttpDownloader.java`
- Bazel lines: 296
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`download`, `fetch_one`, `http_get`, `curl_get`)
- Verdict: partial
- Differences:
  - Zel has no separate downloader for streaming versus buffering. Bazel streams to the destination, warns, and posts a fetch event per URL. The in-memory read posts no progress, and on total failure rethrows the last I/O exception with earlier failures suppressed. An empty URL list is an illegal argument only for the in-memory read. Zel always returns one string, always may warn, and never preserves exception type or suppressed causes.
  - An empty URL list for the streaming download is `Error downloading [] to <dest>` with no suffix. Zel appends `: no URLs given`.
  - On total failure Bazel builds the error from the URI list's normalized form and suppresses every per-URL failure. Zel keeps only the last error string, joins the raw rewritten URL strings, and drops earlier failures except when a warning was recorded.
  - A read socket timeout is recorded as a new I/O exception whose message is the cause's string, then the next URL is tried. Other interrupted I/O becomes an interrupt, the rest of the URL list is not tried, and no failure is recorded. Acquiring the semaphore can throw with no event. Zel has no interrupt path. Timeouts are ordinary retryable I/O strings.
  - Warnings use the exception class name plus the message. A null message concatenates as `null`. Zel synthesizes a class name from the message (file-not-found, unrecoverable HTTP, connect, or plain I/O) and stores it in a thread-local rather than the event bus. There is no fetch event.
  - Bazel tries each URL once per call and retries inside one connect: 403, 408, 429, and 5xx except 501 and 505, with jittered backoff, a connect timeout doubling from 1 second to 10 seconds, a 20 second read timeout, and up to 40 redirects. Zel's outer loop does up to 5 extra passes only for its own retryable flag, and it suppresses the warning until the last pass. HTTP retryable codes are only 408, 429, and 503, so 403, 500, 502, and 504 fail once. Unknown host is not retried inside this call. The timeout scaling, connector attempts, and parallel-download flags are not applied here.
  - There is no fair semaphore. Concurrent fetches are unbounded. The client environment is ignored, so proxy variables do not change the connection.
  - Zel writes an HTTP/1.1 GET with connection close, user-agent `zel`, and accept anything, and does not send accept-encoding. Bazel forces gzip encoding and a Bazel user-agent after user headers and uses the platform HTTP client. Header values and the request target are UTF-8. Java header values are ISO-8859-1 and the request line comes from a URI. A non-UTF-8 header byte fails the Zel download. Java accepts those bytes as Latin-1.
  - Headers are one value per key, iterated in key order. Bazel can send repeated values. Credentials are basic or bearer from an auth map, URL userinfo, and the user netrc.
  - Zel follows 301, 302, 303, 307, and 308 up to 20, detects cycles by the full URL string, and on a host, port, or scheme change drops authorization and cookie. Bazel does not treat 308 as a redirect, allows 40 redirects, and a 301 replaces the URL used for later connector retries. Cross-scheme HTTPS is handed to curl. Curl's fail flag turns HTTP error statuses into a stderr string instead of the status line.
  - 206 is success in Bazel and a hard error in Zel. Bazel resumes a dropped body with a range request. Zel has no resume. Invalid content-length is ignored by the stream and is a hard error in Zel. Bazel requests gzip and decompresses unless the URL or type extension is `gz` or `tgz`. Zel does not send accept-encoding, but if content-encoding is gzip it gunzips even for those extensions. The archive type is not a field on the request.
  - Under 32 KiB, Bazel checks the hash before opening the destination. At or above that size it writes the body and only then fails the hash, leaving the bad bytes in place. Zel buffers the whole body, checks, and renames a temp file only on success. A checksum miss tries the next URL and does not mark the pass retryable.
  - The canonical id is unused by this class. Zel uses it for repository-cache identity, and the same function also does the distdir, the disable flag, URL rewrite, executable mode, and parent-directory creation. A failed write returns immediately and does not try the remaining URLs. Bazel records that error and continues the URL list.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/HttpStream.java`
- Bazel lines: 179
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`http_get`, `curl_get`)
- Verdict: partial
- Differences:
  - Range resume exists only in Bazel. If accept-ranges has a token equal to `bytes`, a read error reconnects up to 3 times with a range request and requires the content-range to start with that offset. Zel never reads accept-ranges and never sends a range. A mid-body failure fails the attempt. Only a full GET is retried from byte 0.
  - An unparseable content-length is ignored in Bazel. Zel's size parse failure is a hard error and is not retried. This splits results for an empty value, a leading plus, a sign, trailing junk, and any value that does not fit in the platform size.
  - Zel trims header values. Bazel does not. A content-length with trailing space and a short body is a size mismatch in Zel and a successful short body in Bazel. A content-encoding of `gzip ` is decompressed in Zel and left compressed in Bazel.
  - Duplicate response headers: Java uses the last value. Zel uses the first. Conflicting content-length or content-encoding lines change the enforced size and whether the body is gunzipped.
  - If transfer-encoding contains `chunked`, Zel dechunks and ignores content-length. Bazel still applies the length check whenever the length parses, counting the dechunked bytes, so a mismatched length fails Bazel and succeeds in Zel.
  - Zel returns success at exactly the declared length and does not read past it. Bazel counts until end of file and throws if the count differs. A short body is a retryable error in both. Gzip decode errors are not retried in either.
  - Bazel gunzips only when content-encoding is exactly `gzip` or `x-gzip` and neither the connection path, the original URL path, nor the archive type ends in `gz` or `tgz`. Zel gunzips when the first content-encoding equals `gzip` ignoring ASCII case. There is no `x-gzip` exception and no archive type. `x-gzip` is decompressed only by Bazel. `GZIP` is decompressed only by Zel. A `.gz` URL that is also gzip-encoded stays compressed in Bazel, so the checksum covers the gzip bytes. Zel stores and checksums the decompressed payload.
  - HTTPS never reaches that gunzip. Curl is invoked without compressed decoding and does not look at content-encoding. Bazel applies the same gzip rules to HTTPS.
  - Complete concatenated gzip members match. Trailing bytes that are not a full next member do not. Zel returns garbage-after-gzip or an unexpected end of header. The Java gzip stream consumes that tail and returns success with only the finished members.
  - Response header lines must be UTF-8 in Zel. A header byte in 0x80 through 0xFF fails the whole HTTP download. Java accepts those bytes as ISO-8859-1.
  - The stream's URL is the post-redirect connection URL. Zel's result URL is the requested URL, before redirects.
  - A Bazel read checks the thread interrupt flag and throws, which aborts the download instead of trying another URL. Zel has no interrupt check.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/HttpUtils.java`
- Bazel lines: 138
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`Url::parse`, `Url::join`)
- Verdict: partial
- Differences:
  - A Location that is not an absolute path (`omg`, `foo/bar`, `../d`) makes Java throw `Could not merge`. Zel resolves it against the base directory. Neither side removes `.` or `..` segments.
  - A query-only Location keeps an empty path in Java, so the base path is dropped. Zel keeps the base path. On an absolute path against a URL that has a query and fragment, Java keeps the fragment and Zel drops it.
  - Java copies the original userinfo when the Location has no host and when the host string and port are equal. An omitted port is `-1`, so an explicit port 80 does not match, and the compare is case-sensitive. The copy crosses schemes. Zel clears userinfo on relative, path-absolute, and scheme-relative joins, and never copies the base userinfo onto an absolute Location.
  - Java keeps a Location fragment and otherwise inherits the request URI fragment. Zel has no fragment field and truncates at `#`.
  - When Java must rebuild the URI, it re-quotes decoded components, so path `%2F` becomes a real slash and query escapes become raw characters. The shortcut returns the Location unchanged when nothing is inherited. Zel keeps the raw path and query.
  - Zel treats any Location containing `://` as absolute, so a query value that contains that substring fails as an invalid scheme. Java parses the query form and rejects a relative form that contains it. A single-slash `http:/foo` is absolute in Java and a relative segment in Zel.
  - Java preserves an empty path and an empty query. Zel rewrites an empty hierarchical tail to `/` and a query-only tail to `/?`. An empty Location is merged, not treated as missing.
  - Java's port is a 32-bit int, and an omitted port is `-1`. A port of 99999 is a successful redirect target, and a non-numeric port substitutes the original host. Zel parses ports as an unsigned 16-bit value and returns an invalid-port error.
  - `URI.create` throws for a space or a bad escape. Zel accepts both as a raw path.
  - Both reject a non-http(s) result. Java's message is `Bad Location:` plus the raw header. Zel's message names the unsupported scheme. A missing header is `Remote redirect missing Location.` versus `Redirect <status> without Location header`. Java uses the last Location header and Zel uses the first. Java decodes header bytes as ISO-8859-1. Zel requires UTF-8, so a Latin-1 byte fails the read.
  - Scheme checks accept mixed case and keep the Location's scheme and host casing. Zel lowercases the scheme and leaves the host as written. A path with no scheme is rejected by both, but the error text differs. Spaces and bad escapes are a bad URL in Java and valid HTTP URLs in Zel. An empty URL list is an illegal argument in this class and `no URLs given` in Zel.
  - The extension helper is the substring after the last dot, with only ASCII A–Z lowered. Zel has no counterpart, and the HTTP fetch gunzips every gzip content-encoding. It does not skip archive extensions and does not suppress accept-encoding for them.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/InterruptibleInputStream.java`
- Bazel lines: 88
- Zel counterpart: none (`http_get` and `curl_get` do not check a thread interrupt)
- Verdict: missing
- Differences:
  - Bazel checks the calling thread's interrupt flag at the start of every read. If it is set, the flag is cleared and the read throws an interrupted I/O exception without reading the delegate. Zel never checks a thread interrupt or the process cancel flag around the body read, so a cancelled build keeps reading until end of file, an HTTP or curl error, or a socket timeout.
  - Clearing the flag means a later read on the same thread proceeds unless something sets the flag again. Zel has no per-thread interrupt bit. The process-wide cancel stays set, but this download path does not read it.
  - Available, mark, reset, and close do not check the flag, so they neither throw nor clear it. Close still closes the delegate. Zel does not expose a stream: HTTP buffers the whole body, and HTTPS waits for curl and then reads the temp file.
  - Skip and transfer go through the overridden read, so an interrupt fails the next internal read after any bytes already copied. Zel has no per-read boundary. One read runs to completion, and the output is written only after the full body is in memory.
  - The interrupt check runs before the delegate sees the arguments. An interrupted read of a null buffer throws the interrupt instead of a null-pointer exception, and a bad offset throws the interrupt instead of an index error. Zel's read APIs have no null buffer or offset pair.
  - A socket timeout is a subclass of the interrupted I/O exception and passes through unchanged. Callers retry or fall through on that type, and they do not retry a plain interrupt from this check. The HTTP downloader turns it into an interrupt and does not try the next URL. Zel turns a read timeout into a retryable error, then sleeps and retries and still tries later URLs. Setting the timeout can fail silently, so a failed setup can block with no deadline. Curl sets a connect timeout only, so an HTTPS body transfer is not bounded by the configured timeout.
  - If the interrupt flag is set when the next read would otherwise return end of file, Bazel throws and does not consume that read. Zel returns the bytes already received, including an empty body, as success.
  - A read already blocked inside the delegate is not polled. Zel's blocked socket read returns on data, end of file, or the receive timeout. A blocked curl child is not interrupted.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/ProgressInputStream.java`
- Bazel lines: 148
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs` (download bar is unused by the downloader)
- Verdict: partial
- Differences:
  - Bazel posts a progress event for the original URL, the current URL, zero bytes, and the total before any byte is read. Zel buffers the whole body and never posts a start, progress, or finished fetch event. The download-started and download-finished handlers have no callers. The byte count on the tracker is ignored.
  - The throttled log line uses the default locale and a 200 ms interval. The first line is not at time zero, at most one line is emitted per window, and the count is the cumulative total at the read that crossed the deadline. A negative interval throws before that initial event. A zero interval reports on every positive read in the same millisecond. Zel emits no such line.
  - The "via" host is added only when the current host is non-null and not equal to the original host. Java lowercases the host and strips IPv6 brackets. The same host with a different path, port, or userinfo does not add "via". Zel's URL parser lowercases only the scheme, so host case and punycode would not match Java even if a via suffix existed.
  - A single-byte read adds one for every return other than end of file, including the data byte zero. A bulk read adds the count only when it is positive. An I/O error does not add. Close posts a finished event only if the delegate close returns, and it does not emit the downloading line. A second successful close posts another finished event. The counter wraps to negative. The progress text treats a non-positive count as empty. Zel has no counter: a short read or curl failure fails the fetch with no finished event.
  - The total is forwarded only on the progress event. At zero bytes the progress string is empty. Otherwise it is a binary size plus either the raw byte count in the JVM locale, when the total is absent, or a US-locale percent. The downloading line's grouping can disagree with the bar. An unparsable content length is ignored. Zel parses the length only to size the body. Gzip is counted as compressed wire bytes in Bazel because the progress stream sits under the gzip stream. Zel gunzips only after the full buffer and reports neither size.
  - The curses UI refreshes a row keyed by the original URL, then the progress, and only after more than 3 seconds an elapsed time, with a sample size of 3. Running downloads are stably sorted by start time, so tied timestamps keep hash-bucket order. Zel's bar, if it were called, would include elapsed seconds from second zero, a default sample of 8, and a different overflow line. Downloads would be sorted by start time with ties in URL key order. Truncation uses UTF-16 length and a scheme-host-tail shorten. Zel uses Unicode scalar count and cuts the prefix. Java URL string form percent-encodes. Zel reprints the parsed host and path without that encoding.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/ProxyHelper.java`
- Bazel lines: 386
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`http_get`, `curl_get`)
- Verdict: partial
- Differences:
  - Plain HTTP opens a TCP connection to the origin host. For an `http` URL, Bazel selects `http_proxy`, then `HTTP_PROXY`, then the Java proxy host and port, and routes the connection through that proxy unless a bypass entry matches.
  - HTTPS passes no proxy argument and inherits the process environment, so curl applies `https_proxy`, `http_proxy`, `ALL_PROXY`, and `NO_PROXY`. Bazel reads the per-invocation client env, uses only `https_proxy` then `HTTPS_PROXY` then the Java https proxy properties, and never reads `ALL_PROXY`. A later client's proxy reaches Bazel and stays invisible to an already-started Zel server.
  - An empty `https_proxy` hides `HTTPS_PROXY` and the Java property and becomes no proxy. Curl treats an empty proxy variable as unset and continues with the other variables.
  - Bypass is two lists, both case-sensitive against the lowercased host. `no_proxy` is split on commas with ASCII whitespace. A leading dot matches only a suffix. Any other entry matches the exact host or a dotted suffix. `*` is a literal. There is no CIDR. Then `http.nonProxyHosts` is split on `|`. A leading `*` means ends-with, a trailing `*` means starts-with, and a pattern that both starts and ends with `*` uses only the leading rule. An IPv6 host has no brackets, so `[::1]` does not match. Zel has neither list, so localhost follows curl's no-proxy rules.
  - The proxy address must match a fixed pattern. Uppercase schemes, `socks4a` and `socks5h`, a path other than one trailing slash, and bracketed IPv6 fail. A username with no password throws even when the Java proxy password is set. `http` and `https` both become a cleartext HTTP proxy. Every socks scheme becomes SOCKS and the JVM still uses the default SOCKS version, so `socks4` is spoken as SOCKS5. Curl uses TLS to an `https` proxy, speaks the SOCKS version in the scheme, and accepts IPv6 literals.
  - The port is a signed 32-bit parse. `65536` parses and then the socket address throws. Leading zeros are accepted. Curl rejects ports outside 1 through 65535.
  - URL userinfo overrides the Java proxy user and password. Userinfo is decoded as UTF-8, `+` becomes a space, and the basic token uses the Latin-1 storage of those UTF-8 bytes. Curl leaves `+` as `+`. Plain HTTP never sends proxy authorization.
  - The first call that has both a username and a password installs a process-wide authenticator and never replaces it. A later password still changes the per-request header on plain HTTP. HTTPS CONNECT keeps the first password. Zel does not install an authenticator.
  - Env values are internal Latin-1 strings. Rust strings are Unicode scalars, and the process env drops non-UTF-8 entries, while the curl child still inherits the raw environ. A null host with a nonempty no-proxy list throws before the scheme check.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/ProxyInfo.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no value that stores a direct, HTTP, or SOCKS proxy plus a nullable proxy username and password. The no-proxy sentinel has both credentials null, so it has no credentials and no authorization header.
  - Credentials are present only when both fields are non-null. Empty strings still count. One null field yields no header. Zel never applies that rule.
  - The authorization header is null, or `Basic ` plus standard padded Base64 of the Latin-1 internal bytes of `username + ":" + password`. Those bytes are the raw UTF-8 octets after the internal conversion. A string whose coder is UTF-16 throws instead of emitting a header. A Rust string is Unicode scalars and cannot hold unpaired surrogates.
  - Zel never sets `Proxy-Authorization`. The nearest encoder builds origin `Authorization` only. Missing login and password default to empty, URL userinfo is percent-decoded with invalid UTF-8 replaced by U+FFFD, and the payload is UTF-8. That is a different header and a different credential rule.
  - Plain HTTP downloads open a direct connection and ignore `http_proxy`, `https_proxy`, `all_proxy`, and `no_proxy`. Bazel connects through the proxy and, when both credentials are set, sends `Proxy-Authorization` on that connection.
  - HTTPS fetches invoke curl with no proxy, proxy user, or noproxy argument. Curl may still honor process environment proxies, including userinfo, using curl's own decoding. Zel does not decode proxy credentials into Bazel's internal Latin-1 form, does not require both username and password, and does not return a direct-connection sentinel for no-proxy hosts.
  - The remote proxy option accepts only `unix:` sockets for the remote cache and carries no HTTP Basic proxy credentials.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/RetryingInputStream.java`
- Bazel lines: 145
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Nothing in Zel resumes a body read. The download path buffers the whole response and, on a retryable failure, repeats the GET from offset zero with the original headers. Bazel keeps a signed count of bytes already returned and, when that is greater than zero, reconnects with `Range: bytes=<count>-`, then accepts the new stream only if the content-range starts with `bytes <count>-`. Offset zero reconnects with an empty header map and does not look at the content-range.
  - The content-range test is a case-sensitive prefix, not a parsed range. Offset 10 therefore also accepts `bytes 100-…` and `bytes 10…`. Both the request and the expected prefix use the JVM default format locale, so non-ASCII zero digits can be emitted, and the connection writes header values as ISO-8859-1. The failure text inserts the locale grouping separator. A Rust format of the same integer is always ASCII digits with no grouping. Zel never builds either string.
  - The resume budget is 3. The fourth read failure is thrown with the previous three causes suppressed, in list order. Close errors before reconnect are discarded. An interrupt from reconnect is rethrown with no suppressed list. Any other reconnect I/O error is the thrown exception and the read failures are suppressed on it. Zel errors are plain strings. There is no suppressed chain, and a failed attempt drops the partial body instead of keeping a byte offset.
  - Every I/O error is resumed except an interrupt that is not a socket timeout. A plain interrupt aborts immediately and does not increment the resume count. End of file is returned and does not increment the offset. A bulk read that returns zero adds zero. The offset wraps past the maximum long, so the range value can become negative. Zel has no interrupt-versus-timeout split. Body read errors, timeouts, and short content-length bodies are retryable whole-request failures, while this class would surface a clean end of file to the caller.
  - After a successful reconnect the new socket is wrapped so later reads throw if the thread is interrupted before touching the socket. The same read is reissued. Bytes already returned stay counted. A throwing read is treated as having contributed none, and the caller buffer is not cleared. Available and close forward to the current delegate and do not retry. Zel sets one read timeout per connection and never opens a second connection mid-body.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/UnrecoverableHttpException.java`
- Bazel lines: 24
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`FetchError.retryable`, `record_warning`)
- Verdict: partial
- Differences:
  - Bazel uses a real I/O exception subclass. The connector skips retries by type, and the downloader prints the class plus the message. Zel has no such type. The warning class name is guessed from the message.
  - That guess misses real instances whose messages do not match the prefixes. TLS text containing `certificate`, and a wrapped illegal-argument message, are this type and are not retried. Zel reports HTTPS failures as a GET failure and labels them a plain I/O exception. Curl exit 35 is retried. A port above 65535 is an invalid-port error and is also labeled a plain I/O exception.
  - The same guess over-labels: any message that merely starts with `Checksum was`, `Redirect loop`, or `Unknown host` is reported as this class.
  - Java does not retry this exception. Codes below 500 other than success, redirect, and the retried 403, 408, and 429, plus 404 and 410 as file-not-found, and 501 and 505, are permanent. Other 5xx are retried. Zel sets retryable only for 503, 408, and 429, so 403, 500, 502, and 504 are not retried, while the warning still calls them a plain I/O exception.
  - 206 is success in Java and a failure of this class in Zel. 308 is this class and is not followed in Java. Zel follows it.
  - A missing status becomes this exception with message `GET returned -1`. Zel parses the status as an unsigned 16-bit value. A non-numeric status line is a retryable invalid-status warning. Reason phrases are ISO-8859-1 in Java and UTF-8 in Zel. Invalid UTF-8 becomes that retryable I/O error instead of an unrecoverable status message.
  - Java throws `Redirect loop detected` at 40 redirect responses, with no cycle set. Zel throws that same message on the first repeated URL. A chain longer than the redirect cap (default 20) fails with a too-many-redirects message, labeled a plain I/O exception and not retried. Java still follows that chain up to 40.
  - Checksum failures both start with `Checksum was` and are not retried on the same URL. With integrity set, Zel replaces the digest text with the algorithm and base64 form. Nothing in Zel maps this type plus that prefix to the gRPC aborted code.
  - The constructor sets no cause. Callers attach the original SSL or unknown-host exception as suppressed. Zel keeps only the message string.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/UrlRewriter.java`
- Bazel lines: 366
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`UrlRewriter`)
- Verdict: partial
- Differences:
  - Bazel takes one config path. An empty path installs an empty rewriter. Zel merges a list, and any empty entry disables rewriting for every path. A missing file is `Unable to find downloader config file` in Bazel and that sentence prefixed with `Failed to parse downloader config:` in Zel.
  - Bazel decodes the file as UTF-8 and replaces bad sequences. Zel fails the load on non-UTF-8. Bazel splits lines on a bare carriage return. Zel does not. Tokenizing is ASCII whitespace versus Unicode whitespace.
  - Allow and block hosts are kept verbatim and compared case-sensitively to the URI host. Zel lowercases the config token and the URL host, so mixed case matches only in Zel. Zel does not percent-decode the host before that compare.
  - The rewrite subject in Bazel keeps host case. Zel lowercases the host before matching, so a mixed-case pattern matches Bazel and not Zel, and a lowercase pattern matches a mixed-case host only in Zel.
  - A replacement that already starts with `http://` or `https://` is left unchanged, so an https URL can be rewritten to http. Zel always prepends the original scheme, producing a doubled scheme. Under a block-all and allow rule, that URL's host is `http` and Zel drops a URL Bazel keeps.
  - An illegal URI character throws out of the rewrite. Zel drops only that candidate and, if some rule matched, does not fall back to the original URL.
  - Replacements use Java's replace-first. A `$` that is not a group reference, a trailing backslash, and an unknown named group throw. `$10` is the longest group number that exists, then leftover digits stay literal. Zel treats a non-group `$` as literal, a trailing backslash as literal, and a bad named group as empty. It consumes the whole digit run, so `$10` with one group becomes empty. Urlencode forms are rejected or percent-encoded in Zel. Bazel 9.3.2 has no urlencode check and throws or inserts the raw capture.
  - The matcher is Java regex versus Zel's scalar regex. `.`, classes, and counts use UTF-16 code units in Bazel and scalars in Zel. Java `$` also matches before one trailing line terminator. Java has backreferences, lookaround, inline flags, and character classes that fail to compile or match different text in Zel. A backslash before a non-BMP character in a Zel replacement can panic.
  - The all-blocked message assumes the directive starts at column 0. Leading whitespace shifts the saved message. Zel searches for the directive and then skips one character.
  - Auth is added only when the URL was rewritten. Userinfo wins over netrc, encoded as ISO-8859-1 after UTF-8 percent-decoding. Otherwise the first credential value is used. Zel has no rewritten flag. It may add userinfo or netrc on URLs that were not rewritten, skips userinfo if an authorization header is already present, encodes UTF-8, and uses bearer when a netrc machine has a password and no login.
  - Netrc uses `NETRC`, else the user profile on Windows and home elsewhere, resolves a relative path against the working directory, reads ISO-8859-1, and throws on parse failure. Zel checks `NETRC`, then home, then the user profile on every OS, uses the process current directory, reads UTF-8, and turns a parse error into no credentials.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/UrlRewriterConfig.java`
- Bazel lines: 226
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`UrlRewriter::parse_into`)
- Verdict: partial
- Differences:
  - Allow and block hosts are stored exactly as written. Zel ASCII-lowercases them while parsing, so a mixed-case block matches the lowercased URL host in Zel and matches nothing in Bazel, because the URI host is already lowercased and the comparison is case-sensitive.
  - Tokens are split on ASCII whitespace, then only the wider Unicode set is trimmed from the ends. Zel splits on that whole set. A non-breaking space, next-line, or ideographic space between a directive and its argument is one token in Bazel and a real separator in Zel.
  - Lines break on a newline, a carriage return plus newline, and a lone carriage return. Zel breaks only on newline and carriage-return-plus-newline. A lone carriage return between two directives is two directives in Bazel and one line in Zel. A comment ended by a lone carriage return also drops the following directive in Zel.
  - The all-blocked message is a fixed substring counted from the start of the line in UTF-16 units. Leading whitespace shifts the window and corrupts the saved text. Zel finds the directive and keeps the message. With a multibyte separator, Bazel rejects the line as a single token. Zel's offset lands inside that character and the stored message is empty, so the blocked-URL text still gains a trailing separator.
  - Rewrite patterns are Java regex, applied as a full match. Zel accepts a different language and captures different text. Java accepts inline flags, lookaround, possessive quantifiers, atomic groups, named groups, backreferences, hex and Unicode escapes, and class intersections that Zel rejects or treats as literals. Java's dot does not match a carriage return or the Unicode line separators. Zel's dot matches every scalar except newline.
  - A repetition range with the minimum above the maximum, and a bound above the signed 32-bit maximum, are illegal in Java. Zel parses the bound as a size and, when the minimum is above the maximum, still requires the minimum number of repetitions.
  - Regex error text and indexes differ. Java indexes are UTF-16 code units. Zel indexes are Unicode scalars, so a supplementary character shifts the reported index.
  - Replacement strings are stored raw and interpreted only after a rule matches. Zel rejects, at parse time, any replacement that still contains `urlencode(` after stripping the supported forms. Bazel 9.3.2 has no urlencode support and accepts that line. On a match, Zel percent-encodes the group. Java either leaves the text or throws.
  - A braced group that starts with a digit throws in Java and is that group in Zel. A missing group throws in Java and expands to empty in Zel. `$10` with one group is group 1 plus a literal 0 in Java, and group 10 in Zel. A trailing backslash or dollar throws in Java and is copied literally in Zel. A backslash before a non-ASCII character yields that character in Java. Zel advances two bytes and panics when that offset is not a UTF-8 boundary.
  - Invalid UTF-8 is replaced and parsing continues. Zel fails the file read before parsing.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/UrlRewriterParseException.java`
- Bazel lines: 37
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs` (`UrlRewriter::parse`)
- Verdict: partial
- Differences:
  - Java keeps the detail message and a nullable location apart. The exception string omits the location. Zel has one string, so every error value already contains the location text.
  - The user-facing Bazel sentence adds `at {location}` only when the location is present. A zero column is dropped, and a zero line drops the line too, so a config error prints `at path:line`. Zel hardcodes `at {path}:{line}`. Those match only while the line is nonzero. A line-0 location prints as the bare path. Zel never stores that location.
  - A missing file surfaces with no `at`. Zel's missing-file string matches that. I/O does not: Java stores only the I/O message and drops the cause. Rust interpolates the error's display, including the kind and the OS error number.
  - Invalid UTF-8 is replaced, then this exception can still carry a real line location and U+FFFD. Zel fails the read first and never records a line.
  - Path bytes that are not UTF-8 become Latin-1 code units in Bazel and U+FFFD in Zel. Java messages can hold unpaired surrogates. A Rust string cannot.
  - Java splits lines on `\n`, `\r`, and `\r\n`. Rust splits only on `\n` and `\r\n`, so a lone carriage return stays inside the line. Java's line counter is a 32-bit int and wraps. Zel's number does not.
  - A null detail message is legal and formats as `null`. Zel errors are always a real string.
  - Invalid-regex messages are Java's description and a UTF-16 index. Zel stores the Rust regex text with `near index` rewritten to `at index`, so both the failure set and the index text differ.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/NeedsSkyframeRestartException.java`
- Bazel lines: 28
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel uses a dedicated evaluation-exception subclass as an unwind token. Its only constructor sets the message to exactly `Internal exception`. Fetch and extension evaluation catch that type and return null so Skyframe restarts, and the message is not reported. Zel has no such type. Every Starlark failure becomes a user-visible fetch or extension error.
  - The same text from `fail("Internal exception")` stays a normal evaluation exception and is reported. Restart is chosen by Java type, not by the message. Zel cannot separate a restart from that failure.
  - After the exception leaves a Starlark call, the message would include a traceback. The restart catches drop that. Any Zel error with that text is shown to the user.
  - Throw sites are a recorded-input lookup that still has missing values, a label whose package lookup is not yet present, and a recorded-input batch that left dependencies missing. Those abort the in-progress Starlark call and re-run the function once the dependencies exist. Zel resolves the label's repo inline and reads the filesystem and client env in the same call. There is no silent restart and no second evaluation.
  - Zel's Skyframe restart is a function returning no value only when missing values are already set. That value cannot be raised from a repository or module-extension builtin, and those evaluations are not SkyFunctions.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/RepoMetadata.java`
- Bazel lines: 41
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs` (`repo_metadata`)
- Verdict: partial
- Differences:
  - Bazel stores reproducibility on the returned value and caches from that value alone. A legacy dict return is always not reproducible. Calling `repo_metadata(reproducible=True)` and then returning none, a dict, or a later false metadata stays non-reproducible. Zel sets a sticky flag inside the call, so an earlier true still marks the repo reproducible. The struct field is not read.
  - An empty attrs map emits no debug line in Bazel. That covers an empty dict and metadata with no attrs. Zel treats a returned dict, including an empty one, and a metadata struct whose attrs are absent or not a dict, as every omitted attribute at its default, and logs a dropping line for every public non-name attribute that differs.
  - Bazel walks attributes in declaration order. Dropped names use a Java set's string form, unquoted and in hash order. Zel walks attributes sorted by name and uses Rust debug quoting. Modified strings use Starlark repr in Bazel and Rust debug in Zel, so a control character is `\xHH` versus `\u{…}`.
  - Bazel turns a null attribute default into none before comparing. Zel fills a missing default with an empty string, zero, false, an empty list, or an empty dict. An omitted key can be a drop in one implementation and unchanged in the other when the current value is that zero value and the Bazel default is none.
  - Bazel requires string keys and fails the fetch otherwise. Zel accepts any dict and matches keys by display.
  - The Bazel value's type is `repo_metadata`, it has no Starlark fields, and it is unhashable. Equality is record equality, so an omitted attrs argument and an empty dict are the same. Zel returns a readable struct, hashable when its fields are. Omitted attrs are none, which compares unequal to an empty dict. A hand-built struct that contains the attrs field is diffed as metadata, and its reproducible field is ignored. Bazel accepts only a real metadata value or a dict.
  - With the require-metadata flag set to all, or root when the rule's file is in the main repo, Bazel errors if the implementation returns neither a dict nor metadata. Zel defines the flag and does not consult it during fetch.
  - A reproducible local rule skips symlink replanting and the contents cache. Zel skips those steps only for the rule names `local_repository` and `new_local_repository`.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkBaseExternalContext.java`
- Bazel lines: 2414
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs`
- Verdict: partial
- Differences:
  - `read` is ISO-8859-1: one Starlark code unit per file byte, and `file` writes those bytes back. Zel strings are Unicode scalars. Valid UTF-8 is returned decoded, so length is the scalar count. Bytes that are not UTF-8 become U+00xx, and `file` writes them as UTF-8, so a non-UTF-8 round-trip grows. `legacy_utf8` is ignored on both sides.
  - `execute` stdout and stderr are ISO-8859-1 locally and US-ASCII on the remote path. Zel uses lossy UTF-8.
  - `getenv`, `which`, `os.environ`, and the execute environment use Bazel's repo env. Zel uses the full client environment. `os.name` is Java's lowercased `os.name` versus Zel's `windows`, `linux`, or `mac os x`. The child environment is a sorted map in Zel and insertion order in Bazel.
  - `which` ignores relative PATH entries, trims the program, splits on the platform separator, and retries with `.exe` on Windows. Zel splits only on `:`, searches relative entries, and does not trim or append `.exe`.
  - Timeout must fit in a signed 32-bit int, must be positive, and the deadline is rounded milliseconds, optionally via process-wrapper. Zel takes a 64-bit integer, defaults to 600 seconds when it does not fit, ignores scaling, and treats a non-positive timeout as an immediate exit 256. Empty arguments throw in Bazel and return code 0 in Zel. A failed spawn is exit 256 with the command exception text in Bazel and exit 1 with a process-wrapper string in Zel. Signal 15 aborts the fetch in Bazel and is `128 + signal` in Zel. Quiet false copies both streams to stderr in Bazel. Zel logs them and strips a trailing newline. There is no remote-exec path, so remotable rules stay local.
  - Strip components is a Java int. Zel uses a 64-bit integer, and a value that does not fit becomes 0. The wasm memory limit is a Java long. Zel casts to a size and uses 64 MiB when the integer does not fit.
  - With no checksum, Bazel drops every http URL. Zel still fetches http when the host is localhost or the loopback addresses.
  - Pattern auth uses regex replacement, so `$` and `\` in the password are replacements, and the password placeholder is substituted before the login. Zel substitutes literally, login first. Two auth keys that are equal as URIs but not as strings make Bazel throw. Zel keeps both strings. A header value that is a list is one header line per element in Bazel and a single comma-joined value in Zel.
  - Allow-fail download failures return only success false in Bazel. Zel also sets an error string. On success Bazel sets integrity to the requested algorithm and adds sha256 only for SHA-256. Zel always reports the SHA-256 hex and its integrity form. BLAKE3 is accepted by Bazel and rejected by Zel. Zel trims sha256 and uses only the first integrity token. Bazel does neither.
  - Passing both strip-prefix spellings is an error in Bazel. Zel drops the camel-case spelling when the snake-case one is already set.
  - A non-blocking download is cancelled on context close, and a successful fetch with a token still pending fails. Zel keeps tokens in a process-global list and does not fail the fetch if wait is never called. Extract retries deleting its temp dir and then fails. Zel ignores the removal error.
  - A label passed to path, read, execute, or extract is watched unless the no-implicit-watch flag is set. Zel resolves the label and does not record that watch. With the wasm flag off, Bazel hides those methods. Zel still exposes them and errors only on the call. The wasm input buffer is UTF-8 rather than Latin-1 bytes.
  - The first recorded value still wins, but Bazel warns and schedules a refetch when the same input changes during the evaluation. Zel keeps the first value silently. Outside-workspace and symlink-prefix classification differs, and the digest algorithm is not necessarily SHA-256. Progress reports are discarded in Zel.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkExecutionResult.java`
- Bazel lines: 255
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs` (`repository_ctx.execute`)
- Verdict: partial
- Differences:
  - Bazel decodes stdout and stderr as ISO-8859-1, so length equals the byte length. Zel uses lossy UTF-8, so a valid multibyte sequence is one scalar and each invalid sequence is U+FFFD. ASCII matches. Length, indexing, equality, and dict keys differ for any non-ASCII output.
  - A signal death is `128+signal` in both, except signal 15: Bazel throws and does not return a result, and it also treats a normal exit 143 as SIGTERM. Zel returns code 143 and the captured output.
  - On this class's own deadline, Bazel destroys the child with SIGTERM, then returns code 256, empty stdout, and stderr `Timed out`, dropping captured output. If the child ignores SIGTERM, that wait never finishes. The repository caller also prefixes process-wrapper with a whole-second timeout while the Java deadline is rounded milliseconds, so the wrapper usually dies first and this class reports code 142. Zel has no wrapper and no timeout scaling. It polls, SIGKILLs only the direct child, and always returns 256 and `Timed out`.
  - A non-positive timeout throws before the process starts in Bazel. In Zel, a timeout that does not fit becomes 600 seconds, and zero or negative expires immediately. Bazel's Starlark timeout is a signed 32-bit int.
  - Spawn failure returns 256 and the I/O exception text in Bazel. Zel returns code 1 and a process-wrapper execvp line. That code-1 line is what Bazel produces only when the wrapper is actually prepended. A missing wrapper itself still hits the 256 path in Bazel.
  - An empty argument list throws in Bazel. Zel returns code 0 and empty streams.
  - If reading the pipes fails after a successful exit, Bazel returns code 0 and empty streams. Zel returns whatever bytes were already read with the real exit code.
  - Quiet false tees the raw bytes onto standard error. Zel appends two log lines after exit, decodes them as lossy UTF-8, strips only a trailing newline, and skips an empty stream.
  - The process environment is the client map with overrides applied in both. Bazel keeps hash-map order. Zel sorts keys, so programs that read environ order can differ.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkOS.java`
- Bazel lines: 83
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs` (`os_value`)
- Verdict: partial
- Differences:
  - `name` is the live `os.name` property lowercased. Zel returns a compile-time token: `mac os x`, `windows`, or `linux`. On Windows Bazel keeps the version. FreeBSD, Android, SunOS, and anything else become `linux`. A system property changes Bazel only.
  - `arch` is the live `os.arch` property lowercased. Zel returns `aarch64` for an aarch64 target, `x86_64` for any other macOS target, and `amd64` for every other target. That does not match `x86`, `arm`, `ppc64le`, `s390x`, `riscv64`, or an x86_64 JVM under Rosetta.
  - `environ` is Bazel's repository env, including action env unless the incompatible flag is set. Zel applies the strict filter and repo env only, so action env never appears. On Windows, Bazel rewrites slashes in the workspace path before substituting the workspace token. Zel inserts the path unchanged.
  - Bazel decodes client env as ISO-8859-1. Zel decodes those bytes as lossy UTF-8. Non-ASCII keys and values therefore differ, and so do length, indexing, and hash.
  - Dict iteration follows UTF-16 code-unit order in Bazel and UTF-8 byte order in Zel. Those orders diverge for supplementary characters. JSON encoding sorts keys the same way.
  - Equality is reference equality of the single OS value per context. Zel builds a new value on every read and compares it by field contents. Two contexts with the same fields compare equal in Zel and unequal in Bazel, and a plain struct with those fields compares equal to the OS value in Zel.
  - Adding a struct is rejected in Bazel. Zel merges struct fields and errors only when a field name is on both sides.
  - JSON and text-proto encoding fail in Bazel because this type is not a structure. Zel encodes `arch`, `environ`, and `name`.
  - Unknown fields name `repository_os` and offer a hint in Bazel. Zel names `struct` and lists the three attributes. Calling `name` and indexing or iterating the value use different error text.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkPath.java`
- Bazel lines: 213
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs`
- Verdict: partial
- Differences:
  - `readdir` returns filesystem order in Bazel. Zel sorts the names as Unicode strings.
  - Zel decodes names as lossy UTF-8, so invalid bytes become U+FFFD. Bazel keeps each filename byte as one Latin-1 code unit, so non-ASCII names, string form, later existence checks, and equality with a UTF-8 literal all differ. A lossy child path can then fail `exists` for a file that is present.
  - A directory read error in Bazel is a transient repository exception whose message includes a JNI line number, and one bad record fails the call. Zel reports the path and errno text with no JNI line and skips individual directory-entry errors.
  - Bazel watches directory entries only on the context stored in the path object. Zel records those inputs on every live context.
  - Basename, dirname, exists, is-dir, and realpath are struct fields in Bazel, so calling them tries to call the resulting string and fails. Zel's method arm returns the field value, so those calls succeed. The reverse also differs: Bazel attribute lookup of `readdir` and `get_child` yields a bound method. Zel errors, although `dir` still lists them.
  - Child type errors name the index differently. Unexpected keywords are reported in call order in Bazel and in sorted order in Zel.
  - On a relative receiver, Zel drops a leading `..`, so `../x` becomes `x` and a path whose string is `..` has an empty basename and no dirname. Bazel keeps a leading `..` and uses basename `..`. The dirname of a single relative segment is the empty path, not none. Absolute paths agree, including `..` above `/`.
  - Realpath in Bazel walks links itself with a fresh limit of 32 per segment. Zel uses the operating-system canonicalize, which on Linux limits the whole walk to 40, and always formats failure with a fixed JNI line. Chains of 33 through 40 links therefore succeed in one implementation and fail in the other. A relative path makes Bazel throw. Zel returns a working-directory-absolute path.
  - Path repr uses Starlark quoting in Bazel and Rust debug in Zel, so non-ASCII and controls are escaped differently. Hashing the string then follows UTF-16 code units versus Unicode scalars.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkRepositoryContext.java`
- Bazel lines: 661
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/ctx.rs`
- Verdict: partial
- Differences:
  - `original_name` is the definition's original name when that string is nonempty, and the canonical name otherwise. Zel returns the canonical name for both `name` and `original_name`, so an extension repo whose apparent name differs reports the canonical name.
  - `delete` deletes a path that is under the repo directory, outside every package root, or covered by `.bazelignore`. Zel returns false and leaves the file whenever the path is not under the repo directory, including ignored workspace paths. A failed removal is an error in Bazel. Zel returns false.
  - `rename` treats the destination as existing only when the path exists, and that call follows symlinks, so a dangling symlink is replaced. Zel treats any symlink metadata success as existing, including a dangling symlink.
  - `template` reads and writes the file as ISO-8859-1. Every code point above U+00FF becomes `?`. Zel replaces on UTF-8 bytes: a key in U+0080 through U+00FF matches only the UTF-8 sequence, and a non-ASCII value is written as UTF-8. With umask `022` and executable true, Bazel ends at mode `0744` and Zel forces `0755`. With executable false and umask `077`, Bazel leaves `0600` and Zel forces `0644`.
  - `patch` requires the directory to lie in the repo and accepts a string, label, or path. Zel applies a string directory as given, so a directory outside the repo is written, and a label or path directory is ignored and the patch is applied to the repo root. Strip outside the signed 32-bit range fails in Bazel. Zel uses a 64-bit integer and turns a value that does not fit into strip 0. Watch-patch is honored in Bazel. Zel never reads it. Patch text is UTF-8 in Bazel, so an invalid sequence becomes U+FFFD. Zel keeps each patch byte. Hunk headers use ASCII whitespace only in Bazel and accept Unicode whitespace in Zel.
  - `watch_tree` records a file input and a recursive directory digest. Zel only records a directory marker, and the outdated check has no directory-tree case, so changes under the directory do not refetch. Bazel takes only a path. Zel accepts an exclude keyword and then ignores it.
  - `repo_metadata` returns an opaque value with no fields. Reproducibility is taken from the object the implementation returns. Zel returns a normal struct whose fields are readable, and sets the repo reproducible as a side effect of the call. Calling it with reproducible true and returning none caches and replants symlinks in Zel and does not in Bazel. Non-string keys fail the cast in Bazel and are accepted in Zel.
  - Execute is remote when the remote-exec flag is on, the rule is remotable, and a remote executor exists. Arguments must be strings or labels, exec properties are sent, and stdout and stderr are US-ASCII. Zel always runs execute locally and never reads exec properties for that call.
  - On a filesystem without native symlinks, Bazel copies an existing file or creates a junction. Zel returns that symlink is not supported on this platform.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkRepositoryDebugModule.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel registers this module. Before each command it replaces the state with an empty map, a reported set, and the reporter, then subscribes on that command's event bus. After the command it nulls those fields. Zel has no per-command subscriber or map.
  - The definition event stores the canonical name to the definition sentence at the start of a fetch, before the rule runs. A Skyframe cache hit does not post again. After the command the map is empty, so a later command that only reuses a cached fetch has nothing to print. Zel never records this pair.
  - A request prints only when that name is the first success for the command and the lookup is nonempty. The line is an info event. A request that wins before the definition is stored stays silent if the definition arrives later. A null key or value throws. Cycle reporters post this for each repository key. Zel never emits that info line for those cycles.
  - The only similar text is a different trigger. Zel prefixes a failed rule implementation with an info sentence and may log a debug line when a reproducible attribute form is returned. Bazel prints the definition line only on request. Zel prints it on implementation failure with no request, and does not print it for a cycle that never failed the implementation. There is no one-shot dedupe and no command-end clear.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkRepositoryDefinitionLocationEvent.java`
- Bazel lines: 20
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs`
- Verdict: partial
- Differences:
  - Bazel posts this event at the start of every repo fetch, and the debug module stores the name for the rest of the command. A later request prints that string once as an info event. Zel has no event, map, or request. Cycle reports never add where the repo rule was defined.
  - Zel only emits a similar sentence when the implementation fails, and appends a debug line when reproducibility attributes differ. Successful fetches, restarts, load failures before the implementation, and metadata-requirement errors do not record or print it. Bazel still stores the event in those cases.
  - The Bazel info line is its own event, so the UI filters can drop it, and each repo name prints at most once per command. A later fetch of the same name overwrites the entry but does not print again. Zel splices the info sentence into the error string, so it is shown whenever that error is shown, and every failed run repeats it.
  - The payload uses the unambiguous canonical label form. Zel rebuilds the label by stripping every leading `@` and splitting on the first `//` and `:`. An apparent label is reprinted with a double at-sign rather than the canonical repo, and a label with no colon puts the remainder in the target name. The rule name is the exported symbol, not the rule id.
  - A null name or definition is a legal record but the concurrent map throws instead of storing it. The Rust format sites use real strings and skip the line when the rule never runs.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkRepositoryModule.java`
- Bazel lines: 260
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs`
- Verdict: partial
- Differences:
  - The cc-configure migration builtin is absent. With the flag on, Bazel fails with the rules_cc migration error. Zel never defines the name.
  - Repository rules and module extensions are not limited to `.bzl` initialization. Zel accepts the call wherever the host function is bound.
  - The export name is the alphabetically first global in that file, and only if the name is still empty. Bazel overwrites in source order, so the last assignment wins. Re-export from a later file does not rename the rule in Zel.
  - Repr of a repository rule is always a numbered placeholder. Bazel prints an anonymous rule before export and the canonical label plus name after.
  - Calling an unexported rule fails with a different sentence. Outside an extension it fails with a different sentence as well.
  - Attribute checks and the repo-spec snapshot run inside the call, so a bad call aborts the extension function immediately. Bazel only records the call and type-checks after the implementation returns, so later statements still run.
  - Keyword args are stored from a sorted map, so spec and lockfile attribute order is sorted by name. Bazel keeps insertion order. Integers that do not fit in a signed 64-bit value are snapshotted as decimal strings.
  - Only `name` is reserved. Bazel also rejects `tags`, `deprecation`, and `visibility`. Zel always injects `repo_mapping` and then strips it, so a user-declared attribute of that name is not settable.
  - Attribute intake keeps only kind, default, mandatory, values, and allow-empty. File filters, providers, configuration, executable, configurable, and per-attribute docs are dropped. Names are not rewritten, so a leading underscore stays, and a function default on a public name is not rejected. An empty name is accepted.
  - Tag class reads only the keyword attrs. The positional form yields a class with no attributes. A non-dict attrs value becomes an empty schema instead of a type error.
  - Documentation is discarded for repository rules, module extensions, and tag classes. The bzl transitive digest and recorded repo-mapping table are not stored on the rule at definition.
  - Remotable is always rejected and exec properties are never added. Bazel enables both when remote repo execution is on.
  - Typed parameters are not enforced. Extra positionals and unknown keywords are ignored. Local, configure, and the OS and architecture flags use truthiness, so `1` is true. None for those flags, for environ, or for tag classes is treated as the default. The implementation need not be callable.
  - Environ drops non-strings, turns labels into canonical strings, and accepts tuples, depsets, and selects. Bazel requires a sequence of strings, and repository-rule duplicates are collapsed at definition.
  - Facts version becomes zero for non-integers and negatives, and values above the unsigned 32-bit maximum truncate. Bazel requires an int in the signed 32-bit range and then rejects negatives.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkWasmExecutionResult.java`
- Bazel lines: 100
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/wasm.rs` (`run_wasm_execution`)
- Verdict: partial
- Differences:
  - The type is `wasm_exec_result` in Bazel and `struct` in Zel. A missing field names the Bazel type versus a struct and lists the three attributes.
  - String and repr use the type name and the field order return code, output, error message. Zel prints a struct in key order error message, output, return code. Nested quoting uses `\xHH` for code units below 32 in Bazel and `\u{…}` for non-printable scalars in Zel.
  - Equality is reference identity, so two results with the same fields are unequal and are different dict keys. Zel compares and hashes the three fields.
  - Length errors in Bazel and returns 3 in Zel. Adding two results is unsupported in Bazel. Zel tries a struct merge and only errors when a field name is in both operands.
  - JSON encoding fails in Bazel. Zel encodes an object in key order.
  - Success and failure payloads match the factories. The stored return code is a signed 64-bit integer. Bazel's caller clamps a negative or above-`2^32-1` Wasm code to `4294967295` before constructing the result, while Zel sign-extends the 32-bit value, so a Wasm return of `-1` is `-1` in Zel and `4294967295` in Bazel.
  - Output is one character per ISO-8859-1 byte. The error message is a normal string: a supplementary-plane character counts as two UTF-16 units in Bazel and one scalar in Zel.

### `src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkWasmModule.java`
- Bazel lines: 296
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/wasm.rs`
- Verdict: partial
- Differences:
  - The Starlark object is not a `wasm_module`. Its type is a host object, repr includes the resolved path and the allocator, and attribute access fails. Bazel repr quotes the original argument and the allocator name, and the path attribute returns the original path, label, or string. Two loads of the same file compare equal in Zel and unequal in Bazel.
  - Zel stores only the path and allocator name and re-reads the file on every execute. A path containing `|` is split into the wrong path and allocator name. Bazel keeps the parsed module.
  - Bazel accepts any export whose name equals the allocator. Zel requires a function export.
  - Magic, version, and truncated-header strings match. Other failures differ. Zel does not validate the module, skips unknown sections, and is lenient on LEB128. Non-UTF-8 export names fail in Zel. Element and data segments are read as the MVP form only.
  - Execute has no timeout. Bazel runs the call on another thread and, on timeout, returns code `-1` and a timed-out error. The default is 600 seconds and the timeout argument is honored. Zel never reads the timeout. Watch is ignored.
  - Memory limits differ. Bazel uses at least one page, raises that only if the module's initial size is larger, then clamps the initial size down to the host limit. A zero limit is one page. More than one memory throws. A negative limit becomes zero or negative pages. Zel sizes the memory to the module minimum, including zero pages, does not clamp that minimum, and sets the grow limit to the smaller of the declared maximum and the host pages. A negative limit is cast to a size and becomes a huge cap. Multiple memories are not rejected. Imported memory is allocated. Instantiation has no host memory and fails in Bazel.
  - Bazel disables the start section and then calls an export named `_initialize`. Zel runs the start section first, calls `_initialize` only if it is a function, and discards an initialize error. WASI preview1 imports are stubbed to success. Unresolved imports fail instantiation in Bazel. An active data segment that does not fit grows the buffer in Zel and traps in Bazel.
  - An empty input still allocates in Bazel. Zel skips that call and passes pointer 0. Input bytes in Bazel are the Latin-1 internal bytes of the Starlark string. Zel copies UTF-8, so any character at or above U+0080 changes both the buffer and the length.
  - Bazel treats pointer 0 as null and rejects a long outside the signed 32-bit range. Those failures are wrapped as an error executing the user function even when the allocator failed. Zel uses the same null text with no prefix, reports allocator traps under the allocator name, and sign-extends a negative pointer. A pointer at or above 2^31 can be in range in Zel and fail in Bazel.
  - Bazel clamps a return code below 0 or above `0xFFFFFFFF` to `4294967295`. Zel sign-extends a 32-bit value, so `0xFFFFFFFF` is `-1`. A missing or non-integer result becomes 0 in Zel and throws or is clamped in Bazel.
  - Both turn each output byte into a scalar in U+0000 through U+00FF. Bazel returns an empty string when the length is not positive. Zel reads an unsigned length, so a high bit yields a multi-gigabyte length. An out-of-bounds output pointer traps in Bazel and becomes an error result. Zel keeps the return code and returns an empty output with an empty error message.
  - The interpreter does not match. Branches inside conditionals also leave the enclosing block. Block ends are found by scanning raw bytes. Indirect calls always use table 0 and do not check the type. Addresses do not wrap at 32 bits. Several 64-bit opcodes are shifted, and compares, counts, floats, saturating truncation, sign-extension, bulk memory, and SIMD are unsupported. Trap text differs.
  - A bad binary passed straight to execute throws in Bazel. Zel returns code `-1` and an error message. The success object is a real wasm result in Bazel and a struct in field-sorted order in Zel.

### `src/main/java/com/google/devtools/build/lib/repository/ExternalPackageException.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No external-package exception that marks a repository failure as transient or permanent.

### `src/main/java/com/google/devtools/build/lib/repository/RepositoryFailedEvent.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No event is posted when a repository fetch fails.

### `src/main/java/com/google/devtools/build/lib/repository/RepositoryFetchProgress.java`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No fetch-progress event for repository rules.

### `src/main/java/com/google/devtools/build/lib/repository/RequestRepositoryInformationEvent.java`
- Bazel lines: 32
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No event requests repository information for the UI after a fetch.
