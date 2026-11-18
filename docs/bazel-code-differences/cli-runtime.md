# Bazel vs zel code differences: cli runtime

Category rule: Command dispatch, runtime, server, build tool, command-line handling, Bazel commands, crash reporting, and output filtering.

- Failure entries: 251
- Missing: 80
- Partial: 171
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `src/main/java/com/google/devtools/build/lib/bazel/commands/FetchCommand.java`
- Bazel lines: 261
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/session.rs` (`run_fetch`)
- Verdict: partial
- Differences:
  - [P3] Option checks match on the common path. `--nofetch` is `ERROR: You cannot run fetch with --nofetch` and exit 2. More than one of `--all`, `--configure`, `--repo`, and residue targets is `Only one fetch option can be provided for fetch command`, also exit 2. Bare `fetch` and `--all` both fetch every repo. The three success lines match.
  - [P1] Fetch-all failures, repo-fetch failures, repository-directory failures, and interrupts exit 8. A null skyframe exception falls back to `Unexpected error during fetching all external deps.` Zel maps those to exit 2. There is no interrupt path, so `Fetch interrupted: …` and the skipped no-build-finished event do not exist. Target-fetch failures are exit 2 in Bazel under `QUERY_EVALUATION_ERROR`. Zel still labels that exit as an options-parse failure.
  - [P1] An illegal `@@` name fails inside repository-name creation (`Invalid repo name: …`, exit 8). Rust accepts the suffix with no charset check. `@@` is the main repo in Java. Zel returns `Repository '@@' is not defined`. An unknown apparent name is a failure sky value and `Fetching some repos failed with errors: No repository visible as '@…' from main repository` (exit 8). Zel uses that sentence, but a single error whose text matches a hard-coded prefix list is returned without the wrapper. Java sanitizes control characters in the name. Rust does not. Repeated `--repo` values are deduped in Bazel and fetched twice in Zel.
  - [P1] Apparent-name mapping failures keep the mapping exception's own exit code (often 37 or 48) and are prefixed `Invalid repo name: `. Zel resolves the module graph before choosing a mode, so a bad `MODULE.bazel` fails `fetch --repo=@@…` before any repo fetch, without that prefix. Bazel passes `--keep_going` and the loading-phase thread count into the mapping and evaluates repo keys in parallel. One skyframe error aborts before failure values are joined. Zel walks repos on one thread and ignores both flags, so the reported error and the set of repos actually fetched change when several repos fail.
  - [P1] `--force` injects the command id into a general force key, or into a configure-only key when `--configure` is also set, so only `configure=True` rules skip the cache. Zel stores one force flag on every fetch, so `--force --configure` also refetches non-configure dependencies. `--configure` itself skips any rule whose `.bzl` path contains `bazel_tools` (`tools/osx/xcode_configure.bzl` is fetched by Bazel) and treats a failed rule lookup as "not configure" instead of failing the command.
  - [P1] Target fetch is not a `--nobuild` build. Bazel injects `--nobuild` at computed-default priority, then runs loading, analysis, toolchains, and configuration. An explicit `--build` overrides that injection and runs the execution phase. Zel never injects `--nobuild`, never analyzes, and ignores fetch errors, then still prints success. It also loads `@@repo//...` for each fetched repo, which is wider than the configured target graph and still misses toolchains and aspects. Pattern errors are a fixed transcript (`Elapsed time: 0.050s`, `0 processes`, exit 2), not the build's failure-detail message.
  - [P2] `--target_pattern_file` is not read. Bazel uses it instead of residue (ISO-8859-1, `#` comments stripped), rejects residue plus the file (exit 2), and if the file yields targets while `--repo` is set reports `Target patterns and --repo cannot both be specified` at exit 8, because residue was empty so the exclusive-option check passed. Zel ignores the file and either fetches `--repo` or, with no residue, fetches everything. A leading `+project` residue is dropped by Bazel. Zel keeps it as a target pattern.
  - [P2] Bazel posts a no-build event and, on the normal return, a finished event with the real exit code. Option-validation failures post neither event. Pattern-file and interrupt failures post only the start event. Zel's fetch path never enters that build-event stream.

### `src/main/java/com/google/devtools/build/lib/bazel/commands/FetchOptions.java`
- Bazel lines: 74
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` (`all`, `configure`, `force`, `repo`), applied in `session.rs` and `fetch.rs`
- Verdict: partial
- Differences:
  - [P3] Names, `fetch`-only scope, help text, effect tags, boolean default `false`, and repeatable `--repo` match. Unset `--repo` is an empty list on both sides. `--all` is only an exclusivity bit. With no mode selected both fetch every repo. `--force` is outside that exclusivity count on both sides.
  - [P2] `--repo` is a list of strings, one converter result per occurrence. Zel types it as a string list and comma-splits during conversion. `run_fetch` reads raw strings, so `--repo=@a,@b` is still one repo name there. Invocation-policy allow and disallow uses the split list, so a policy value of `@a,@b` allows separate `--repo=@a` and `--repo=@b` in Zel and rejects them in Bazel.
  - [P1] `--configure` keeps a repo in Bazel when the repo rule's `configure()` is true. Zel returns false when the `.bzl` path contains `bazel_tools`, and also when loading the rule fails, so built-in configure repos are skipped and a definition error does not fail the command.
  - [P1] With `--force`, Bazel injects a configure-only force key when `--configure` is also set and a general force key otherwise. The configure key skips the contents cache only for `configure` rules. Zel sets one force flag on every fetch in that invocation, so non-configure dependencies materialized during `--force --configure` skip the marker and the repo-contents cache as well.

### `src/main/java/com/google/devtools/build/lib/bazel/commands/ModCommand.java`
- Bazel lines: 838
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/commands.rs` and `/workspace/zel-cursor/crates/zel-bzlmod/src/modcommand/mod.rs`
- Verdict: partial
- Differences:
  - [P3] The subcommand set, case-insensitive parse, argument validation, `--base_module` / `--from` / `--extension_usages` / `--extension_filter` resolution, the depth defaults, and the post-print summary `Results may be incomplete as N extension(s) failed.` match.
  - [P2] `dump_repo_mapping` wraps stdout in UTF-8 or US-ASCII. A name that is not US-ASCII throws and becomes an uncaught illegal state. Zel always returns a UTF-8 string.
  - [P2] Extension-repo JSON is written in insertion order (host mapping, then sibling repos, then overrides). Zel loads that mapping through a sorted map and sorts the keys, so the JSON object is alphabetical.
  - [P2] Invalid repo names sanitize control characters (`\r` stays escaped, other controls become `<?>`). Zel interpolates the raw argument.
  - [P1] A repo-definition evaluation error is `Unexpected error during repository rule evaluation.` with `INVALID_ARGUMENTS`. `NOT_FOUND` is `In repo argument <key>: no such repo` for `--all_repos` and `--all_visible_repos` as well as explicit args. Zel turns a provider error into none: explicit args get `no such repo`, and the two `--all_*` modes skip that repo and keep going.
  - [P2] `show_extension` writes the `##` header and `Fetched repositories:` line, then throws `No extension … exists`. That prefix stays on stdout. Zel buffers the same text and, on error, returns empty stdout.
  - [P1] Any Skyframe error is `MOD_COMMAND_UNKNOWN` (exit 37). Cycle info is printed first by the cycle reporters. An interrupt is exit 8. An abrupt exit keeps that exception's exit code. Zel returns the session-open error as-is. A missing `MODULE.bazel` is exit 48. There is no interrupt result and no those two cycle reporters.
  - [P2] The extension-failure count is the inspector error list. Zel deduplicates the strings, so identical messages shrink `N`. Those lines are appended after stdout. Bazel reports them on the event bus before the executor prints.
  - [P1] `tidy` runs the `buildozer` binary with client env, workspace cwd, and ISO-8859-1 stdin. Exit code 3 (no edits) is success and skips the per-fixup info events. Any other failure is `BUILDOZER_FAILED` (exit 2). Extension errors after a successful buildozer run are `Failed to process N extension(s) due to errors.` Zel rewrites `use_repo` in-process, does not run buildozer, still emits fixup info lines when the file bytes do not change, and on extension errors returns the joined error text with no summary.
  - [P2] Every command result carries a `ModCommand` failure detail. Zel only returns an exit code. The process exit code matches for 2 and 37. The failure category does not. Argument errors use the product name from the command environment. Zel always writes `bazel`.

### `src/main/java/com/google/devtools/build/lib/bazel/commands/RepositoryFetcher.java`
- Bazel lines: 137
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/session.rs` (`resolve_repo_arg`, `--repo` loops)
- Verdict: partial
- Differences:
  - [P3] Names that do not start with `@` produce the same error, wrapped as `Invalid repo name: ...`. Apparent names use the same alphabet (no `+`). `@` and an empty apparent name both fail that check.
  - [P2] A missing apparent name becomes `No repository visible as '@name' from main repository`. Bazel still returns that as a failure inside the result map. Zel returns it from argument resolution and never evaluates the repo. For a single `fetch --repo`, both end as `Fetching some repos failed with errors: ` plus that sentence. The reported sentence matches because the failure string uses the name without the spellcheck suffix.
  - [P1] Canonical `@@name` is validated with `[A-Za-z0-9_.+-]*`, and `.` and `..` are rejected. Zel strips `@@` and does not validate. `@@foo bar`, `@@.`, `@@..`, and `@@@` are invalid in Bazel. Zel tries to fetch them: `@@.` and `@@..` join the external directory or its parent, and the others become `Repository '@@…' is not defined`.
  - [P2] `@@` is the main repo in Bazel, then usually `Failure("Repository '@@' is not defined")`. Zel special-cases `@@` with that same sentence and does not fetch. A real main-repo spec or override would still be fetched only in Bazel.
  - [P1] Bazel parses every argument before any fetch and dedupes by repository-name equality. Zel resolves and fetches in one loop. An invalid name after a valid one still fetches the earlier repo and drops earlier fetch errors. Duplicates, including `@apparent` and `@@canonical` of the same repo, run twice under `--force` in Zel and once in Bazel.
  - [P2] Missing repos and invisible repos are successful sky values. Rule failures, I/O, and a null exception are a fetcher exception. `keepGoing` stays false, so one throwing repo hides every other result. Zel treats not-defined, invisible, and rule errors as one list. One error is returned raw when it contains `An error occurred during the fetch of repository` (and a few other substrings); otherwise, and whenever there are two or more, it joins with `"; "` under the fetching-failed prefix.
  - [P2] A mapping-resolution exception leaves this class unwrapped. Its message is already `Error computing the main repository mapping: ...`, and only the apparent-name path computes that mapping. The command then prefixes `Invalid repo name: ` and keeps that exception's exit code. Zel fails session open for every `fetch` and `vendor`, including canonical-only `--repo`, with the mapping sentence and no `Invalid repo name:` prefix.
  - [P1] Vendor uses the returned map: success repos are vendored unless excluded, then all failure strings are reported as a Java list. Zel aborts on the first resolve error and, on a later fetch error, stops mid-loop, so earlier repos stay vendored and the prefix is the fetching-failed sentence with no brackets.
  - [P2] Control characters in apparent-name and canonical-name errors are rewritten (`\r` stays escaped, other C0 and DEL bytes become `<?>`). Rust formats the raw characters. An interrupt is a separate Bazel failure. Zel has no interrupt path. Fetch evaluation uses the loading-phase thread count. Zel walks `--repo` on one thread, so which rule error surfaces, and event order, differ when more than one repo fails.

### `src/main/java/com/google/devtools/build/lib/bazel/commands/TargetFetcher.java`
- Bazel lines: 80
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/session.rs` (`fetch_targets`)
- Verdict: partial
- Differences:
  - [P1] Success is a result, not a build result. Vendor then vendors the repos this walk happened to materialize instead of the configured-target keys, so the vendored set is not the analyzed graph.
  - [P1] The walk only follows rule dependency labels, which include every `select()` branch and key and skip aliases, toolchains, aspects, platforms, and other analysis-only repos. A `--nobuild` build fetches the configuration-selected graph and those repos, and does not fetch unselected branches.
  - [P1] After a dep repo is fetched, Zel resolves `@@{canonical}//...`. Bazel only loads packages reached from the requested targets. Repos already named on the command line are recorded first and are not `//...`-walked; the same repo discovered only as a dep is. That changes which transitive repos appear.
  - [P1] Transitive-closure errors are discarded, and both fetch sites ignore errors while still recording the repo as fetched. Those fetches become success. Bazel treats an unsuccessful build as a target-fetcher exception, including a failed repository fetch.
  - [P1] Pattern failure is a fabricated log: fixed elapsed time and process count, and the fetching-failed sentence is appended only when some error text contains `BUILD file not found`. Any other unsuccessful build uses the real failure-detail message. One bad pattern also skips closure fetching of the patterns that did resolve. Bazel still runs the build request, and `--keep_going` changes how far that build gets, before success fails it.
  - [P2] Leading dashes are stripped only on the eager `@` / `@@` prefetch. `-@repo//:t` therefore fetches `repo`, while pattern resolution still receives the original negative pattern. Java passes the target list through unchanged, so a leading `-` is an excluded pattern and a fetch of only negative patterns fails the build. An all-negative list in Zel can return success.
  - [P1] The pattern resolver is built with an empty package, so `:foo` and `...` are workspace-root relative. The Java request uses the command environment, so the client working directory changes which targets are loaded.
  - [P1] Unmapped apparent repo names fall back to the apparent name, and a failed repo-argument resolution is skipped. Bazel fails that build. There is no injected `--nobuild`, so a command-line `--build` cannot be overridden, and a failure to parse `--nobuild` cannot raise an illegal state.

### `src/main/java/com/google/devtools/build/lib/bazel/commands/VendorCommand.java`
- Bazel lines: 436
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/session.rs` (`run_vendor`)
- Verdict: partial
- Differences:
  - [P3] `--vendor_dir` required and `--nofetch` rejected (exit 2, same messages). Residue versus `--repo` is mutually exclusive (exit 8). Modes are targets, `--repo`, or all. Success lines match. `ignore` and `pin` skip a copy. `file:` registries are not copied.
  - [P2] Fetch-all errors, repo-fetch errors, the combined vendoring-failed list, and I/O (`Error while vendoring repos: …`) exit 8 at this commit. Zel maps those to exit 2. Option errors stay 2 on both sides. Target-analysis failure is already exit 2 in Bazel.
  - [P1] Bazel fetches the whole set before copying anything. Zel copies inside the loop, so a later failure leaves earlier repos in `--vendor_dir`. `--repo` also stops at the first bad name or fetch. Bazel still vendors every success and then reports every failure as one list.
  - [P1] Bazel drops `local` rules, `configure` rules, ignored repos, pinned repos, and `--override_repository`. Zel only skips `bazel_tools`, `local_repository` and `new_local_repository` from `bazel_tools`, overrides, ignore, and pin. `configure=True` rules and other `local=True` rules are copied. A missing pinned repo is an error in Bazel and a silent skip in Zel. Ignored repos are still fetched in Bazel and never fetched in Zel.
  - [P1] Target mode forces `--nobuild` and runs a real build, then walks that graph, so toolchains, platforms, and other analysis deps are included and configuration flags change the set. Zel only follows package rule labels and then vendors whatever that walk fetched. `--target_pattern_file` and project-file targets are never read, so a file-only invocation takes the vendor-all branch. Stripping a leading `-` turns an excluded pattern into a positive pattern.
  - [P1] Up-to-date repos are recopied. Bazel skips when the external symlink already points at the vendor dir or the marker bytes match, so unpinned local edits survive. Zel always copies onto a temp dir and replaces the tree. Bazel moves a non-cache repo and copies a contents-cache recorded-inputs file to the marker. Zel copies the external marker only when that path exists. Bazel deletes the vendor marker for pinned repos during fetch. Zel leaves it.
  - [P1] Registry files are `_registries/<host lowercased>/<decoded path>` (port and query dropped). Bazel rejects case and port collisions, skips URLs that are already files, and downloads with checksum, cache, and netrc. A download error fails the command before any repo is copied. Zel writes the raw string after `://` (host case, port, query, and `%XX` kept), re-downloads every time, does not check the checksum, and ignores fetch and write errors, so the command still exits 0. `+` becomes a space in Java, so the same URL can land on a different filename.
  - [P2] Progress for targets is `Vendoring dependencies for targets...` versus `… for the requested targets...`. Bazel emits the no-build events and then deletes repository-directory nodes for vendored repos. Zel does neither. `VENDOR.bazel` is read as ISO-8859-1 in Bazel and as UTF-8 in Zel, so a non-UTF-8 file fails only in Zel. Symlink replanting is Unix-only, so on Windows the vendor tree is a plain copy with absolute links left in place.

### `src/main/java/com/google/devtools/build/lib/bazel/commands/VendorOptions.java`
- Bazel lines: 35
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` (vendor `--repo`)
- Verdict: partial
- Differences:
  - [P3] The omitted flag is an empty list in both. `--repo=null` is the literal string in both. Repeat order is preserved, and `vendor help` uses the vendor help text, not the fetch one.
  - [P2] Each Bazel occurrence is one string, so `--repo=@a,b` and `--repo=` are the single elements `"@a,b"` and `""`. Zel comma-splits and turns an empty string into an empty list before appending. The vendor command reads raw values, so those inputs still select the same repos. Invocation-policy allow and disallow compares the split list, so `--repo=@a,b` is checked as `@a` and `b`, and `--repo=` is missing from the converted list.
  - [P2] A set-value policy with several `--repo` values overwrites the parsed options on each value, leaving only the last raw string. Bazel appends every policy value onto the repeatable list.
  - [P2] Argv is turned into Rust strings with lossy UTF-8 before parsing. A non-UTF-8 `--repo` byte is replaced with U+FFFD. Bazel keeps those bytes in the Java string.

### `src/main/java/com/google/devtools/build/lib/bazel/coverage/BazelCoverageReportModule.java`
- Bazel lines: 151
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` (`combined_report`) and `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - [P1] `--combined_report` parsing is case-insensitive in both, and an illegal value such as `html5` is the same command-line error. Zel then rejects any stored value other than the exact strings `none` and `lcov`, so `--combined_report=LCOV` or `NONE` exits 2 with `ERROR: Invalid value for --combined_report: 'LCOV': Not one of [none, lcov]` and does not build. Bazel accepts those and either merges (`LCOV`) or returns a null factory (`NONE`).
  - [P1] On build completion, Bazel adds build-tool logs named `coverage_report.lcov` and `baseline_report.lcov` pointing at the report artifacts. Zel always posts an empty build-tool-logs event.
  - [P1] The helper's argv is the report generator's exec path plus `--reports_file=` and `--output_file=`. The info line is printed only after that action succeeds or on a cache hit. Zel hardcodes the list-file and report names and joins the location with `/`. For `coverage`, a label generator that was analyzed is used. For `test --collect_code_coverage` the generator is not added to the configured roots, so Zel runs the embedded merger, ignores its exit status, still prints the location line, and writes an empty file if the output is missing. A failing label generator skips the location line, which matches Bazel.
  - [P2] Zel skips the merge when there are no test jobs, `--nobuild` is set, or split coverage post-processing is on without fetching all coverage outputs and a test failed to build. Bazel still builds the wrapper whenever the option is `LCOV` and the builder returns actions, and still registers the log collector. The Java method also propagates an interrupt from that construction. The Rust path does not observe thread interrupt.

### `src/main/java/com/google/devtools/build/lib/bazel/coverage/CoverageArgs.java`
- Bazel lines: 51
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` (inlined report inputs; no `CoverageArgs` type)
- Verdict: partial
- Differences:
  - [P3] The Java type is an immutable nine-field bag passed into the coverage helper. Zel never builds that value. The loop keeps an execroot, two path lists, list-file names, and an optional generator path.
  - [P2] The compact constructor throws if any field except `htmlReport` is null. The detail message is the parameter name, except `actionOwner`, whose detail is null. `htmlReport` is the only nullable field. A missing generator is `None`: that spawn is skipped, and the `INFO: LCOV coverage report...` line is still written. A null report generator never produces a `CoverageArgs`.
  - [P3] `htmlReport` can be non-null. Bazel's own module always passes null. Zel has no HTML-report slot, so a non-null HTML output and its extra instrumented-file inputs cannot be represented. The stock null case matches: no HTML output.
  - [P1] The report generator is an executable plus runfiles. Zel stores a path only when `--coverage_report_generator` is a `//` or `@` label found on a configured target, plus a display path whose first `-fastbuild/bin/` is rewritten to `-opt-exec/bin/`. A non-label value becomes none. Runfiles are not kept. Otherwise it falls back to the installed merger from `coverage_tools`.
  - [P2] Coverage artifacts are a stable-order nested set and may be empty (the baseline args). Zel keeps only paths that exist and have size greater than 0, in configured-target order for baselines and job order for `coverage.dat`, then appends baselines onto the combined list. Zero-length or not-yet-written artifacts stay in the Java set and are dropped in Rust, so the reports-file contents differ.
  - [P2] Directories, the artifact factory, and the artifact owner are not stored. Bazel uses them to create derived artifacts under `<workspace execroot>/bazel-out/_coverage/...`. Zel writes `<execroot>/bazel-out/_coverage/{baseline,coverage}_lcov_files.tmp` in place and does not record an artifact owner. `workspace_name` is on the test-phase input and is not used for these paths. The relative `bazel-out/_coverage` layout matches only when execution is already in that workspace's execroot.
  - [P3] The action owner is stored for the report action. Zel does not keep one. Failure text uses the first job's label and rule location. Record equality covers every component, including the nested set. Zel has no equivalent comparison, and it overwrites the list files and report files directly.

### `src/main/java/com/google/devtools/build/lib/bazel/coverage/CoverageReportActionBuilder.java`
- Bazel lines: 381
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` (`run_tests`)
- Verdict: partial
- Differences:
  - [P1] Bazel registers four analysis actions under the chosen test's action owner. Zel never builds those actions. After tests it writes the list files and runs the merger in-process, so there is no coverage-report mnemonic, progress string, action key, or cache hit. A Bazel cache hit still emits the location info and does not re-execute. Zel always re-invokes the tool.
  - [P2] Owner selection keeps the maximum action owner: an owner with an empty exec-properties map wins over one with properties, then the larger label, then the larger configuration checksum. Zel attributes failures to the first job and does not consult exec properties. Labels are ASCII, so UTF-16 versus UTF-8 ordering would not diverge. The missing comparator does.
  - [P2] Incompatible targets are skipped, and if every test is incompatible the builder returns null with no event. An empty target list also returns null. Zel has no incompatible-platform skip. It skips the report only when there are no jobs, `--nobuild`, or a split-coverage post-processing failure.
  - [P1] If the nested set of coverage artifacts plus baseline artifacts is empty, Bazel reports `Cannot generate coverage report - no coverage information was collected` and returns null. Zel does not emit that error. It still writes both report files, prints both location lines, and, for the default merger, appends `WARNING: There was no coverage found.` when the output is empty.
  - [P1] Baseline inputs are the providers' `baseline_coverage.dat` artifacts for every configured target. Zel ignores those artifacts, skips tests unless `--instrument_test_targets`, skips implicit labels, and writes its own baseline records with zero function and line counts. Bazel's baseline writer emits only `SF:` and `end_of_record`, so the merged LCOV differs.
  - [P1] The reports file lists every shard and run `coverage.dat` in stable nested-set order, including empty files, each path followed by a newline. Zel lists one already-merged `coverage.dat` per job, then baselines, drops zero-length files, and joins with newlines and no trailing newline. A path outside the execroot stays absolute. Per-shard files are merged once in Bazel and twice in Zel.
  - [P1] The spawn uses an empty environment, empty execution info, and 750MB / 1 CPU, and it adds the generator executable, the path list, the coverage artifacts, and the runfiles tree. Zel inherits the server environment, does not reserve that resource set, does not stage the runfiles tree, and sets argv[0] to the absolute host path. Bazel can still schedule the spawn remotely. Zel always runs locally.
  - [P1] A default-merger failure fails the action. Zel ignores the default merger's status, discards stderr, and writes an empty report if the output is missing. A custom generator label that exits non-zero does fail the Zel build, but only `//` or `@` labels are honored. A non-null HTML report adds that artifact and the instrumented files. Zel has no HTML output and never hands the reports to the important-output handler. Zel treats `--combined_report=branch` like `lcov`. Bazel's report-type enum rejects `branch` before this builder runs.

### `src/main/java/com/google/devtools/build/lib/bazel/debug/DebuggingOptions.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` (`experimental_workspace_rules_log_file`)
- Verdict: partial
- Differences:
  - [P3] Name, help text, type description `a path`, documentation category, no abbreviation, not repeatable, and the command set match. The default string `"null"` is a special null path (the converter is not run). Zel stores no default, which produces the same help text and the same omitted proto default. An explicit value `null` stays the literal path on both sides. The old category `verbosity` is stored as empty. Runtime help groups by documentation category, so that does not change help output.
  - [P2] Setting the flag does not log. Bazel opens the path relative to the working directory and writes delimited workspace events, and aborts on I/O errors. An omitted flag leaves the field null and skips the log. Zel accepts the flag and keeps the string, then never opens a file, so a set path and an omitted flag both do nothing.
  - [P2] The path converter is not implemented, so the stored value is the raw string. Java expands a value that starts with `~/` by replacing every `~` with the user home, then normalizes `.`, `..`, repeated slashes, and a trailing slash (`a/../b` becomes `b`, `a//b/` becomes `a/b`, `foo/..` becomes empty, `/../a` becomes `/a`). On Windows the same converter also accepts backslashes and drive prefixes. Invocation-policy allow and disallow lists compare converted paths, so Bazel treats `a/../b` and `b` as the same value and Zel does not. Canonical flag text still uses the unconverted string on both sides. An explicit empty value is a non-null empty path in Bazel, so the log stream is still created. Zel stores `""` and still does not open a file.

### `src/main/java/com/google/devtools/build/lib/bazel/debug/WorkspaceRuleEvent.java`
- Bazel lines: 401
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] There is no `WorkspaceEvent` type, builder, or writer. The nearest code is the unused flag `experimental_workspace_rules_log_file`. Repository methods perform download, extract, file, read, execute, which, and wasm work and never build this record.
  - [P2] Setting that flag produces no delimited `WorkspaceEvent` log. Bazel opens the path before the command and fails the command on init (`WORKSPACES_LOG_INITIALIZATION_FAILURE`) or on close or write (`WORKSPACES_LOG_WRITE_FAILURE`).
  - [P2] None of the 15 oneof payloads are recorded, including the execute environment merge (common then custom, so custom keys overwrite), download URL strings, symlink target and path assignment, the empty `OsEvent`, or wasm input bytes. `logMessage()` protobuf text is not produced.
  - [P3] There is no Rust path where Java null omission, integer or boolean unboxing, or hash-map overwrite order could diverge, because the event is never constructed.

### `src/main/java/com/google/devtools/build/lib/bazel/debug/WorkspaceRuleModule.java`
- Bazel lines: 104
- Zel counterpart: none (flag row only, `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`)
- Verdict: missing
- Differences:
  - [P3] An unset flag leaves the path null and does not open a file or register a subscriber. Zel also does nothing with a missing key. The outcomes match only while the flag is omitted.
  - [P2] A set flag resolves the path (`~/` expanded, `.` and `..` normalized) against the working directory, opens an asynchronous output stream, and writes each event as a length-delimited `WorkspaceEvent`. Close waits for the writer thread and surfaces the first write or close failure. Zel accepts the flag and drops the string. No file is created and repository-rule calls do not emit events. The path converter stores the raw string.
  - [P2] An init `IOException` reports the exception message and records `Error initializing workspace rule log file.` with `WORKSPACES_LOG_INITIALIZATION_FAILURE` (exit 2). `exit` stores that exception and returns, so the subscriber is still registered and later events are ignored because the stream stayed null. Zel does not abort. Those failure-detail codes exist in the proto and are never built.
  - [P2] A write or close `IOException` throws with `WORKSPACES_LOG_WRITE_FAILURE` (exit 36). The detail is the exception message, or `Error writing workspace rule log file.` when that message is null. The stream is cleared before the exception propagates. Zel has no close path.
  - [P3] Null debugging options emit `Installation is corrupt: could not retrieve debugging options` and return without exiting, so the command continues with no log. Zel has no equivalent check.
  - [P3] The event-bus subscriber only runs when the log path is non-null. Bazel still subscribes after a failed open because `exit` does not throw. That control-flow difference does not show up in Zel, which never subscribes.

### `src/main/java/com/google/devtools/build/lib/bazel/debug/workspace_log.proto`
- Bazel lines: 189
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Bazel writes length-delimited `WorkspaceEvent` protos when `--experimental_workspace_rules_log_file` is set. Zel registers the same flag and help text, and nothing opens that path or emits a delimited record, so a Bazel workspace-log parser sees an empty log.
  - [P2] Every message is absent: execute (arguments field 2, timeout, merged environment map, quiet, output directory), download, download-and-extract, extract, file, read, delete, patch, the empty os event, rename, symlink, template, which, load-wasm, and execute-wasm (input bytes and memory limit), plus the `WorkspaceEvent` oneof (fields 3–17), `location`, and `context` (`repository @foo` or `module extension …`).
  - [P2] Those repository operations still run, but they do not record caller location, context, or the per-event fields (URLs as strings, sha256 versus integrity, strip prefix, rename map, executable bit, patch strip count, wasm module path, allocate function, timeout). `os` returns a host value and does not produce the empty `OsEvent` arm Bazel uses to mark an `os` access.
  - [P3] Proto3 defaults would matter only if a Rust encoder were added: omitted strings are empty, bools false, ints 0, and an empty `OsEvent` still selects that oneof arm. `ExecuteEvent.arguments` is field 2 with no field 1, so a 1-based Rust field number would not parse as Bazel's `arguments`. Map wire order is not significant. A 32-bit timeout truncates the same way as Java `Integer.intValue()` past 2^31−1. None of that encoding exists in Zel today.

### `src/main/java/com/google/devtools/build/lib/bugreport/BugReport.java`
- Bazel lines: 483
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/daemon.rs`
- Verdict: partial
- Differences:
  - [P2] Crash handling posts a fatal event with the product and a Java stack, writes abrupt-exit and failure-detail files, optionally bug-reports, then halts the JVM with that exit code. Zel catches the panic, leaves the server up, and returns a finished response with exit 37, a command-failure category rather than a crash category, and an internal-error line.
  - [P3] The user-visible message is only the panic payload when it is a string. Any other payload becomes a fixed sentence with no class name, cause chain, or stack. The Rust panic hook still prints location to the server log. Java stack text is UTF-16. Zel only keeps Unicode scalar payloads.
  - [P3] Out-of-memory is exit 33, including a wrapped error. The message names the product, optional extra text, and an optional heap-dump path. The dump failures are swallowed, and bug reports are skipped. A second check prefixes the log differently for a detected out-of-memory versus an out-of-memory error type. Zel never does this. The heap-dump and thrashing flags are metadata only.
  - [P3] The product name comes from the runtime, or unknown if unset. Setting a null runtime throws, and a second runtime throws outside tests. Zel's panic text has no product name.
  - [P2] Before halt, Bazel writes the abrupt-exit file only when halting and always tries the failure-detail file, then the heap dump, then crash cleanup. Zel never writes that abrupt-exit file. A dropped RPC is always process exit 37, not 33 and not the crash detail.
  - [P3] A lock serializes crashes. A crash already in progress can return without reporting. If the handler throws, stderr gets both stacks and halt runs again. If the command-thread handler panics before the response is sent, the receiver reports a generic panic and the server stays up. Keep-alive returns instead of halting. Zel has no halt-versus-keep-alive context.
  - [P3] In the unknown test type, halt throws a security exception that is then rethrown. The original throwable stays available to the test. Other test types really halt. Zel's force-panic flag only forces a panic that becomes the exit-37 response.
  - [P3] Bug-report and unexpected-state methods do not exist. In tests they throw. Outside tests, unreleased binaries log that they are not logged. Released binaries send a remote report with a severity and a joined argument list. Unexpected state is a rate-limited warning. Argument filtering drops nulls and client-env and default-override arguments. Zel does not filter crash arguments.

### `src/main/java/com/google/devtools/build/lib/bugreport/BugReporter.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no bug reporter: no injectable default, no unexpected-state log, no fatal or non-fatal bug report, and no crash handler. The nearest behavior is a catch around command dispatch and per-node evaluation. Those keep the process up and turn a panic into an error.
  - [P3] In a test, unexpected state and bug reports throw. Zel never fails the process for an unexpected state. A command-thread panic becomes a finished run response.
  - [P3] Outside tests, unexpected state logs a warning, rate-limited, and the format string is the already concatenated message, so percent signs are interpreted by the logger. The test path uses locale-sensitive formatting. Zel has neither prefix, rate limit, nor those format semantics.
  - [P3] Released builds only: a non-release binary logs that it is not logged and skips remote logging. Otherwise the preamble depends on whether an out-of-memory was detected, and fatal, out-of-memory, and non-fatal reports use different severity and text. Arguments that start with the client-env or default-override prefix are dropped. Zel has no remote bug sink, no release gate, and does not filter crash context that way.
  - [P3] The crash handler takes a lock. A crash already in progress returns without halting. Exit code is the crash's detailed code, so an out-of-memory is exit 33 and other crashes are the crash's code. The message includes a Java stack, a fatal event, and an optional heap-dump path. Zel's command panic uses exit 37, a command-failure category rather than a crash category, and a short internal-error line. There is no Java stack, no fatal event, and no heap dump. Skyframe panics become a persistent node error and do not stop the server.
  - [P2] Before any bug report, Bazel writes the exit-code file when halting, always tries the failure-detail file, and on out-of-memory dumps the heap. Then it halts the JVM without shutdown hooks. A throwable from that sequence prints both stacks and halts again. Zel does not halt, dump the heap, or write those crash files. The command lock is dropped so later commands run.

### `src/main/java/com/google/devtools/build/lib/bugreport/Crash.java`
- Bazel lines: 72
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel type pairs a throwable with a detailed exit code. Construction freezes the classification: a root cause of out-of-memory becomes crash out-of-memory (exit 33), a detector override uses that category, and anything else is crash unknown (exit 37). The detail message is `Crashed: ` plus at most five causes, each truncated to 2000 UTF-16 code units, and each cause keeps at most 1000 stack frames. Zel never builds this crash detail or either out-of-memory category.
  - [P2] The command-thread panic path always reports internal error 37 with a command-failure category, not a crash category. An out-of-memory panic stays 37. Only a string payload is kept. Any other panic becomes a fixed sentence. Skyframe turns a panic into a persistent function error, not exit 33 or 37.
  - [P2] Encoding writes only the code submessage. Causes and the out-of-memory category cannot be encoded. Synthesizing a detail from numeric exit 33 or 37 uses the first error line or `command failed`, not `Crashed:`, and it carries no cause chain.
  - [P3] String form includes the throwable and the exit code. There is no value equality, so equality is identity. Zel's exit code is value equality and has no throwable field.

### `src/main/java/com/google/devtools/build/lib/bugreport/CrashContext.java`
- Bazel lines: 167
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel type carries this crash-handling policy. Halt, keep-alive, and halt-or-return-if-a-crash-is-already-in-progress are not representable. Nothing in the crates chooses among those three policies.
  - [P3] The command-thread panic path always stays in-process: it catches the panic, writes an internal-error line, and returns exit 37 with a command-failure category. It does not halt the process, and it has no crash-in-progress latch.
  - [P3] Arguments are snapshotted into an immutable list. A later edit of the source list is invisible, and a null list or element throws. Disabling the bug report and storing extra out-of-memory text are not attached to any Zel crash.
  - [P3] The heap-dump path stores any string, including null, and does not check the dump suffix. The only Zel hit is the flag help text. Nothing writes the invocation heap dump from a crash context.
  - [P3] The default handler prints the event kind and message to standard error. A null message prints the characters null. Replacing the handler does not reject null. Zel writes the internal-error line on the RPC stream and has no per-crash event handler.
  - [P3] The string form omits the heap-dump path and the return-if-crash-in-progress flag. There is no equivalent display.
  - [P3] Exit labeling only names an already chosen exit 33 as crash out-of-memory and exit 37 as crash unknown. It does not decide halt, bug-report emission, extra text, or a heap-dump path.

### `src/main/java/com/google/devtools/build/lib/buildtool/AnalysisAndExecutionPhaseRunner.java`
- Bazel lines: 355
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs` and `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` (`run_build_actions`)
- Verdict: partial
- Differences:
  - [P2] Bazel runs this class only when both analyze and build are on, marks the combined profile phase, and calls the view update with execution included and the skymeld overlap percentage (default 100). The execution threshold is the top-level key count times the percentage over 100, and the size times percentage product is a 32-bit multiply that wraps before the division. Zel always finishes analysis, then runs actions. It never reads the overlap flag, so early targets do not run while later targets are still being analyzed, and a later analysis failure does not leave those earlier outputs behind.
  - [P1] With `--noanalyze`, Bazel posts `Loading complete.`, posts the no-analyze event, skips analysis and execution, and returns success unless loading failed. Loading errors become a build-failed exception whose message says the command succeeded but there were pattern or loading errors. Zel never reads the flag, so the same invocation still analyzes and executes.
  - [P1] On test or coverage, when coverage collection is set and the instrumentation filter was not passed, Bazel compiles the heuristic and writes it onto the core options before analysis. An empty test set becomes the inclusion `^//`, and the info lines are skipped. A pattern syntax error aborts configuration. Zel prints the same two info lines and then leaves the default filter in place, including for an empty test set. A package name that is an invalid or different Java regex therefore fails Bazel and is ignored by Zel.
  - [P1] The heuristic string is a sorted set of package prefixes in Java string order (UTF-16 code units). Nested prefixes are removed, then joined. Zel uses a sorted set of Unicode strings and a slash boundary check. Supplementary characters can change which nested prefix survives. Bazel also adds suite tests via the label's package name. Zel only walks string lists for the tests attribute, while that attribute is a label list, so suite members in other packages are omitted unless those targets are already resolved.
  - [P1] Java regex filters split on an unescaped comma, do not unescape `\@`, union the pieces, and match with find. Zel unescapes `\@`, compiles a full match, and uses Unicode scalars. That converter is not applied to the heuristic in Zel. It is applied when the user passes the filter, which is the filter Zel actually keeps.
  - [P1] Explicit target patterns drop any raw pattern whose first UTF-16 unit is minus, reparse the rest, and keep only single targets. Wildcards are not explicit. A reparse failure says the pattern failed even though it was previously parsed. That set and the skip-incompatible flag are arguments to the view update. Zel does not build that set and does not read the flag.
  - [P2] After a normal return, Bazel always reports `Found` with the raw int size and no thousands separators, then posts a skipped abort for each skipped target. Zel inserts commas at 10,000. It omits the build `Found` line when the build failed without keep-going, and omits the test line when test keep-going aborts testing or sharding fails. It posts no skipped-target abort.
  - [P2] The top-level analysis watcher forwards analyzed, aspect, test, and coverage-artifact events to every module, then unregisters. Zel has no such callbacks. Bazel also throws a pending module exception after the filter step and before analysis. Zel has no pending-module abort there.
  - [P2] The progress text for this phase is `Loading complete. Analyzing...` with two spaces. Zel never emits that string. The loading-complete tracker method is not called from the server command path.

### `src/main/java/com/google/devtools/build/lib/buildtool/AnalysisPhaseRunner.java`
- Bazel lines: 583
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs` (`default_instrumentation_filter`, `instrumented_prefix`, and the `Found` lines). Analysis itself is `analyze_roots` in `analysis_phase.rs`.
- Verdict: partial
- Differences:
  - [P1] `--noanalyze` is ignored. Bazel posts `Loading complete.`, posts the no-analyze event, skips analysis, and if loading or pattern parsing already failed throws that the command succeeded but there were pattern or loading errors. Zel always analyzes.
  - [P2] There is no pending-exception throw before analysis and no after-analysis module callback, so a module cannot abort or mutate the build between analysis and the `Found` lines.
  - [P2] Project-file evaluation is absent. `--scl_config` (Bazel treats null and empty as unset; the Zel default is the string `null`), project-config enforcement, Skyfocus, and remote analysis-cache mode never load `PROJECT.scl`. Bazel errors when Skyfocus is combined with analysis-cache upload or download, and when upload or Skyfocus sees more than one project file. Zero project files leave the active-directory matcher empty. One file installs it. The scl config rewrites the build options from the project file. None of that runs in Zel.
  - [P1] The coverage instrumentation heuristic is only printed, not installed. Bazel, when coverage needs a filter and the flag was not set explicitly, replaces the filter before analysis. An invalid pattern fails configuration. With no test packages the computed value is `^//` and no info line is printed, but the option is still replaced. Zel prints the info lines only for coverage, or test with coverage collection, and only when the parsed options lack the key, then leaves the default filter in place.
  - [P1] The heuristic's input set differs. Bazel uses tests after loading-phase suite expansion, including implicit tests and nested suites, and adds every such target's package. Zel scans pre-expansion targets, keeps only test rules and test suites, and for a suite reads the tests attribute as strings: only `//pkg` and `:name` contribute a package. External and relative labels are dropped. Nested suites are not walked.
  - [P1] Package-prefix stripping matches for ordinary names. It is not the same regex engine. Bazel compiles the result with `java.util.regex` and `find`, so `.` and other metacharacters in a package name are regex syntax. Zel never compiles this string. Its regex filter would wrap each piece, drop a leading minus as an exclusion, turn `\@` into `@`, and full-match. Java substring length is UTF-16 code units. Zel walks Unicode scalars, which diverges only for unpaired surrogates.
  - [P1] Explicit target patterns are absent. Bazel reparses the command-line patterns with the main-repo mapping and the relative working directory, skips a pattern whose first character is minus, keeps only single-target labels in unambiguous canonical form, and on a parse failure throws that the pattern failed even though it was previously parsed. That set is what makes an incompatible target explicitly requested. Zel has no platform-incompatibility check, so skipping incompatible explicit targets does nothing.
  - [P1] Skipped-target events are absent. Bazel posts an aborted event per skipped target with reason skipped, the original label, a configuration id (null when the configuration is missing), and the skip description. Zel posts no such event.
  - [P2] Analysis bookkeeping is absent: the two-space `Loading complete. Analyzing...` progress line, the analysis-complete event (evaluated counts, mnemonics, elapsed time truncated to integer milliseconds, package-manager stats, cache-invalidated bit), the configuration-key set, the test-filtering-complete event, and the top-level status events. Zel's analyze-roots failure text is a separate view port, not these events.
  - [P2] `Found` lines differ in when they are printed, when they are omitted, and how the number is formatted. Bazel prints immediately after a successful analysis. Counts are raw Java ints, so 10000 is `10000`, and the build-minus-test subtraction can go negative. Zel groups digits from 10000 (`10,000`) and saturates the subtraction, so the other count never goes negative. A normal build prints `Found` only after execution, and only when the build succeeded or keep-going is set. Explain prints it before execution. Test and coverage with explain print only the test count up front. Otherwise the combined test line is printed after the test phase and is skipped when tests abort. The count is successful expanded roots with implicit run-under and coverage labels removed, and tests rejected by filters are counted as non-tests. Empty pattern lists print a zero `Found` line before analysis. This runner is not entered for that case in Bazel.

### `src/main/java/com/google/devtools/build/lib/buildtool/AqueryProcessor.java`
- Bazel lines: 265
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` and `aquery.rs`
- Verdict: partial
- Differences:
  - [P0] Bazel compiles inputs, outputs, and mnemonic patterns only along the outermost function spine. `let`, union, and any filter not on that single spine do not become filters, so those calls only pass targets through. Zel records every evaluated filter call into one global list and then requires every pattern of that kind to match, so a union of two inputs filters and a let-bound filter filter actions Bazel leaves unfiltered.
  - [P1] A filter nested under a non-filter on that spine fails before analysis, saying filter functions produce actions and cannot be the input of other function types. Zel evaluates the inner filter, keeps the pattern, and returns the outer function's configured targets.
  - [P2] A bad pattern on that spine fails at construction with wrong query syntax plus the pattern-syntax exception and a UTF-16 index. The same pattern off the spine fails later, during eval, as an illegal pattern, and keep-going turns that into a reported error with a successful command. Zel always uses wrong query syntax at eval time and returns a command-line error immediately, including under keep-going.
  - [P1] Regex subjects and patterns are Java UTF-16 on the exec-path string, whose path characters are raw bytes in U+0000–U+00FF. Zel full-matches Unicode scalars of the path string. A non-BMP character is two Java units and one Rust scalar, so `.`, quantifiers, lookaround, and the near-index caret move. A surrogate escape compiles in Java and fails in Zel. Case-insensitive mode is ASCII-only in Java. Zel case-folds with Unicode case conversion, so `(?i)ss` does not match `ß`. `(?u)` and `(?U)` are parsed and ignored, so a digit class stays ASCII.
  - [P1] Skyframe state with a one-argument filter compiles the pattern and dumps the in-memory graph through it. Zel still errors that filter functions must have exactly two arguments except when skyframe state is used. A non-empty expression never takes the empty-expression dump branch.
  - [P1] Bazel walks every done configured-target value whose lookup key is that sky key, skipping nulls from failed analysis, plus every aspect, including targets left from earlier server commands. Proto and text dumps run in parallel, and an interrupt is swallowed so the command still returns success. JSON dumps sequentially in done-map order. Zel dumps only the last resolver's analyzed targets, in analysis order, with aspect targets appended. Collect also pulls aspects for each node, so the same label and configuration emit base and aspect actions twice. Include-aspects does not gate Bazel's skyframe aspect dump. Zel honors it.
  - [P1] Streamed proto is a legal skyframe format. Zel rejects it. Other illegal formats share the message that skyframe state must be used with proto, textproto, or jsonproto. An unparsable expression with a bad format is a parse error in Bazel. Zel ignores the parse failure while collecting patterns and reports the format error instead.
  - [P2] Skyframe dump failures are a command-line expansion failure, a template expansion failure, or an output failure. Zel drops unreadable templates and still prints. Include-param-files does not turn command lines on in this dump. Zel prints arguments whenever either flag is set. Include-pruned-inputs selects original inputs plus scheduling dependencies. Zel never reads that flag and matches action inputs only.
  - [P2] JSON skyframe output is protobuf JSON with HTML-safe escaping, two-space pretty print, and one trailing newline. Zel's pretty JSON matches camelCase, default omission, int64-as-string, and the same character escaping, but repeated fields are inline rather than a pretty array, and action and configuration ids follow analysis order instead of skyframe iteration order.

### `src/main/java/com/google/devtools/build/lib/buildtool/BaselineClDiffEvent.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel defines a postable that stores one signed 64-bit changelist delta and returns it unchanged. No crate defines that event or any equivalent field.
  - [P3] After handling diffs, the sequenced executor posts it only when both the previous and current versions are non-null, with the value of current minus prior, wrapping on overflow. Zel's sync only returns a report of the diff strategy, modified-key count, dirtied-node count, and whether loading options changed. It never subtracts versions or posts an event.
  - [P3] Zel's graph version is an internal unsigned 64-bit evaluator counter starting at 0. It is not a changelist delta and is not published to the reporter or to BEP.
  - [P3] Inherited postable behavior is also absent: reporting re-posts the same object, and tagging ignores the tag. Zel's reporter only handles string events, so nothing can subscribe to this delta.

### `src/main/java/com/google/devtools/build/lib/buildtool/BuildObjectTraverser.java`
- Bazel lines: 176
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/dump.rs` (`--memory`). No domain-specific object-graph traverser exists. `memory_mode` only accepts the same flag words.
- Verdict: partial
- Differences:
  - [P2] Interned is true for every string, label, package identifier, and repository name. On a reachable dump, an already-seen instance is walked and charged again for each owner. Zel counts each sky key once and never re-attributes shared strings or labels.
  - [P3] Paths and path fragments are recorded as one edge to the internal Java string (UTF-16 code units; path bytes are the platform or Latin-1 internal string) and other fields are not reflected. Zel stores a normalized UTF-8 string and does not walk or size it. A non-ASCII or unpaired-surrogate path therefore has a different string object and a different shallow size.
  - [P2] Admission drops precomputed values, workspace-status values, and configuration objects when the corresponding report flags are false, including subclasses. Those objects are not marked seen and their outgoing references are not followed. Zel accepts the same mode words and still counts the same fixed function list, including precomputed. There is no workspace-status or configuration-value cut.
  - [P2] The same method always rejects rule classes, providers, package types, late-bound defaults, and Starlark semantics, so those subgraphs are neither counted nor traversed. Zel keeps rule classes, provider instances, and semantics as ordinary values and the memory report does not exclude them.
  - [P2] When those flags are true, the configuration, precomputed, and workspace-status objects are admitted and then reflected. Zel never reflects fields, so summary, count, and bytes all print the same per-function node tally.
  - [P3] Array-item and field context are always null, so this traverser adds no edge context. Java null references are skipped by the walker. Zel has no per-edge context and no null object identity.
  - [P3] Ignored fields use class identity: a Starlark config transition skips its rule-transition cache, and a Starlark aspect skips its definition cache. The superclass walk still skips those inherited fields on subclasses. A different class with the same field name is not skipped. Zel has neither cache nor a field-ignore list.
  - [P2] This class does no arithmetic, regex, or JSON. The substitute dump is a map sorted by function name with node counts, covering only ten functions. Bazel's class histogram is sorted by byte totals descending, with equal totals left in map encounter order. A needle prints a class-name path on Bazel's stderr and is ignored by Zel. Shallow, deep, transitive, and full do not change Zel's table.

### `src/main/java/com/google/devtools/build/lib/buildtool/BuildRequest.java`
- Bazel lines: 470
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/analysis_phase.rs` (request-wide pieces also in `lib.rs`, `exec_phase.rs`, `zel-exec` `scheduler.rs`, and `observability.rs`)
- Verdict: partial
- Differences:
  - [P3] There is no request object. Options are checked on the command result, then the startup result. Zel reads startup options only for the digest function. Construction throws unless the request, package, language, loading, analysis, execution, keep-going, loading-thread, and UI option classes are all present. Zel substitutes per-flag defaults.
  - [P2] Validation warns when the converted int local-test-jobs is greater than the converted int jobs. Zel never compares those 32-bit values and never prints that warning. Zero does not warn in Bazel because it is not greater than jobs.
  - [P1] The options description is a shell-escaped string over every non-hidden parsed option, explicit and implicit. The escaper leaves only a small ASCII set unquoted, and turns a quote into a quoted quote. Non-ASCII UTF-16 units are quoted. That string is the build-started options description and the explain header. Zel always passes an empty description and writes `Build options:` plus a newline.
  - [P2] Emacs makes Bazel print entering and leaving directory lines on the request stderr. Zel only turns color and curses off.
  - [P1] Test or coverage with coverage collection and no explicit instrumentation filter sets the needs-filter bit, and Bazel writes the package heuristic into the core options before analysis. Zel prints the same info text but leaves analysis and instrumented sources on the static default, so the set of instrumented files differs.
  - [P1] Output groups start from the sorted set of default, temp files, and the hidden top-level group only when every token starts with plus or minus. A bare token drops that set. Zel treats only default as implicit, so temp files are not built unless named. Any output group whose name starts with `_hidden` is always built, including when a bare group replaces the defaults and Bazel drops the hidden top-level group.
  - [P2] The comma converter keeps empty fields. Those do not start with plus or minus, so defaults are turned off. Zel drops empty comma fields, so that flag is an empty selection and defaults stay on. The same drop means an aspects list with an empty name skips that name in Zel and fails aspect loading in Bazel.
  - [P1] After plus and minus, Bazel adds the validation group, or the top-level validation group when the validation aspect is experimental, so a later minus cannot remove it. Aspects append the native validate-target aspect unless it is already listed. Zel requests validation whenever run-validations is true and never consults the experimental flag for the group or the aspect list.
  - [P1] Should-run-tests puts default back on every top-level target after a replacing list. The artifact helper unions default-info files with the default output group. Zel adds default-info files and runfiles only when default is selected or the configured target is a test, so non-test roots under a test command with a replacing output group differ, and runfiles stored only in the hidden top-level group are still built.
  - [P1] Aspect parameters are one name-equals-value per occurrence. The name is non-empty and commas stay in the value. The map keeps insertion order and any duplicate key fails view creation. Zel stores an entry and the option list ignores entries, so analysis keeps an empty map: parameters are not applied, duplicates do not fail, and comma-containing values never reach the aspect.
  - [P0] Reporting incompatible targets defaults to true and is false for cquery. Zel never checks target-platform incompatibility.
  - [P2] Action-conflict checks default to true. Cquery sets them false. Zel's build, test, run, and coverage paths always fail analysis on action-graph conflicts. Cquery and aquery do not run that check, so aquery does not fail conflicting actions.
  - [P2] The run-command bit is a case-sensitive command-name check and is part of the target-completion key. Zel's analysis cache does not record the command, so run and build can reuse one cache entry.
  - [P2] Expanding filesets is the experimental flag, default false, and is passed into completion. Zel never reads that flag, so filesets stay unexpanded in BEP output files when it is true.
  - [P0] Starlark options forward the parser's sorted map of typed values, in UTF-16 key order. Zel keeps a string map. Valid Unicode sorts the same. List build settings are joined with commas, and bool and int settings stay strings. User options are a hash-order map used for the project file and the remote analysis cache. Zel does not apply that map.

### `src/main/java/com/google/devtools/build/lib/buildtool/BuildRequestOptions.java`
- Bazel lines: 589
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` (schema; runtime is split across `zel-server`, `zel-exec`, `zel-cmdline`, and `zel-util`)
- Verdict: partial
- Differences:
  - [P1] `--noanalyze` is accepted and ignored. Bazel skips loading and analysis and exits 0 after target-pattern parsing. Zel still analyzes and can fail the build.
  - [P1] `--output_filter` is validated as a Java regex, then applied as a literal contains check on action stdout and stderr only. Bazel compiles with dot-all and uses find on the Unicode form of the tag, so a dot matches newlines, metacharacters are operators, warnings are filtered, and supplementary characters are UTF-16 code units.
  - [P2] `--explain` relative paths are joined to the execroot and `~/` is not expanded. Bazel writes relative to the workspace or home and expands a leading `~/`. A failed write is a warning in Bazel. Zel ignores the I/O error.
  - [P2] `--show_result` is re-parsed from the raw token as a decimal signed 64-bit integer. Integer decode means Bazel treats `010` as 8 and `0x10` as 16. Zel treats `010` as 10, and a failed parse of `0x10` falls back to 1. Zel prints all results or none based on the root count. Bazel keeps a budget of non-empty results and can omit nothing-to-build targets to stay under the limit.
  - [P2] Hide-aspect-results is stored as the raw comma-separated string and never read, so those aspect results are still printed. The validation-aspect flag is stored and never adds the validation aspect.
  - [P1] Aspect parameters are entries, and the option list only walks strings and labels, so every parameter is dropped and aspects keep their defaults. Duplicate keys therefore do not fail the build. Bazel builds an insertion-order map and fails analysis on a repeated key.
  - [P2] Output groups drop empty comma fields. A flag of a lone comma is two empty strings in Bazel, which replaces the default group with nothing. Zel treats it as unset and still builds the default group.
  - [P1] Convenience symlinks are interpreted from the raw spelling, not the converted enum. `IGNORE`, an uppercase clean, `no`, `n`, and `f` fall through to normal and still create links. Bazel's converter maps those to ignore, and yes, y, and t to normal, case-insensitively. The BEP event flag is ignored: the convenience-symlink payload is always empty, including when the flag is at its default true.
  - [P1] The symlink prefix uses the product name when the flag is unset, and an explicit empty string stays empty. Zel hardcodes `bazel` when creating links, and clean treats an empty prefix as `bazel-`, so it deletes the wrong names.
  - [P2] Async execution without an explicit max stays at jobs. Bazel's default max is 5000 and the ceiling is the max of jobs and the min of 5000 and the async max. Zel does not clamp the max, so 99999 starts that many workers. The max is also decimal-parsed, so octal and hex disagree with integer decode. Values above 5000 for jobs are capped in both. Bazel logs the too-high warning. Zel does not.
  - [P1] Jobs auto and filesystem-thread auto use available parallelism and meminfo. Bazel uses host capacity, which is cgroup-aware, so the resulting integer can differ. The filesystem-thread flag is not applied to the filesystem diff.
  - [P2] These flags are parsed and then unused: the progress interval, project-config enforcement, output-tree tracking, the directory-creation cache (and the parser accepts negative sizes, non-positive durations, and untrimmed keys that the Java spec rejects), the skymeld overlap percentage, merged skyframe analysis and execution, and the aquery dump format and output file. The skyframe error-handling refactor flag is a Bazel no-op.

### `src/main/java/com/google/devtools/build/lib/buildtool/BuildResult.java`
- Bazel lines: 371
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` (`BuildOutcome`)
- Verdict: partial
- Differences:
  - [P1] Success is true only when a non-null detailed exit code equals success, including the numeric code, name, and infrastructure flag. Zel's success is an empty failed-target set, no cycle, and no fatal, so a test-only failure, tests-failed, or no-tests-found still leaves success true, while an action-graph cycle fails it with no exit code. Build false returns success and no fatal, dropping a config or remote error already computed in the same function.
  - [P2] An unset detailed exit code synthesizes a crash failure detail: a cycle-tolerant cause walk, out-of-memory when that cause is an out-of-memory error or the detector is set, otherwise unknown, a crashed message, a cause chain capped at 5, each message truncated to 2000 UTF-16 code units (a supplementary character can be split into a lone surrogate), and a stack capped at 1000 frames. Zel stores only a fatal int for config or remote setup and never builds that crash proto. Setting an unhandled throwable rejects anything that is not null, a runtime exception, or an error, and keeps the previous crash. Zel has no throwable or catastrophe slot, so a keep-going build cannot be marked catastrophic.
  - [P1] Elapsed seconds are stop minus start over 1000 on signed 64-bit wall-clock millis, which wraps and can be negative, and throws if either timestamp is zero. Zel's elapsed time is monotonic from execution-phase entry, so the failure trailer's three-decimal value rounds differently and does not include analysis. The success path prints a different instant, and the command elapsed helper returns zero when no command start is set. A backwards monotonic clock panics. BEP build-finished time is millisecond time at the call, zero before the epoch, not the stored stop time.
  - [P1] Target results are nullable live collections. Test targets are an unmodifiable view of the same store. Actual, successful, and skipped targets are the caller's collection and may be null when a phase did not run. Successful aspects are a nullable set of aspect keys. Zel keeps string sets of canonical labels with no configuration. One failed config marks every target with that label failed, duplicate labels collapse, and under nokeep-going every non-test top-level label is inserted once any printable failure exists, including targets that built. Notest-keep-going never sets a stop-on-first-failure bit. Platform-incompatible skipped targets and successful aspects are not recorded.
  - [P1] Convenience symlinks default to an empty insertion-ordered map and are returned as stored. Zel creates the links but does not retain the map. After analysis, BEP is an empty convenience-symlink list. Paths are Rust strings, so a non-UTF-8 byte that a path fragment can hold cannot be a symlink key.
  - [P1] The tool-log collection appends in order to three lists, rejects mutation after freeze, requires freeze before the event, copies bytes into a byte string, keeps URI futures (unfinished, cancelled, or failed futures are dropped later), and appends `.gz` on gzip, with a null name becoming the four characters `null` plus `.gz`. The local-file list is live, so edits after freeze still appear in the event. Zel takes only name and path pairs, with a file URI, no inline bytes, no futures, no gzip rename, and no freeze, and finish always posts an empty list. A BES close error then replaces success with code 38. Bazel leaves that in the post-build callback detail and does not change success unless the caller also sets the detailed exit code.

### `src/main/java/com/google/devtools/build/lib/buildtool/BuildResultPrinter.java`
- Bazel lines: 416
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs` and `exec_phase.rs` (`print_build_failure_trailer`)
- Verdict: partial
- Differences:
  - [P2] Show-result is an int, default 1, charged only for skipped targets, failed targets, failed aspects, and successes that have at least one displayed artifact. Empty nothing-to-build results are free, and they are omitted only when the number of displayable targets plus printable aspects is greater than the limit. Crossing the budget drops every result line already collected. Zel prints success lines only when the valid-root count is at most the limit, so two targets where one has outputs and one has nothing still produce lines in Bazel at a limit of 1 and produce none in Zel. An unparsable value becomes 1 in Zel. Bazel rejects it before this code. A limit of −1 suppresses Zel success lines and, on the failure path, falls back to 1 so a single failure is still printed. Bazel starts the budget at −1 and prints no target line. The most negative int wraps on decrement to the maximum int after the first charged result. Zel never decrements a counter.
  - [P1] Bazel prints three target classes, in analyzed-target order: skipped, then successes, then failed to build. Zel has no skipped line. On a failed build it returns through the failure trailer before the success block, so it never mixes up-to-date and failed lines. With nokeep-going, every non-test top-level label is forced into the failed set and printed sorted when that set's size is within the limit, including targets whose actions succeeded. With keep-going, Zel prints neither the failed lines nor the up-to-date lines. Bazel still prints both.
  - [P1] Test configured targets are ordinary targets here. Zel never puts test targets in the failed-printable set, so a test that failed to compile does not get a failed-to-build line from this path.
  - [P2] An alias label follows the alias chain, so success and failure lines name the final target. Skipped lines use the original label. Zel keeps the output label as the alias.
  - [P2] Source files and rule classes whose name contains `$` are dropped before the budget. A generated-file target is dropped when its generating rule is also analyzed and that rule's files contain every file of the output target. Zel only special-cases a source-file rule class but still counts it in the valid roots, so one source file plus one real target exceeds a limit of 1 and suppresses the real target.
  - [P2] Displayed artifacts are the important outputs of the requested groups. Groups whose names start with underscore are built and not printed. Default is the file provider's files plus the default output group, deduplicated, excluding sources and runfiles trees. Save-temps therefore shows temp files on success. The test command adds default back even if output groups replaced it. Zel, with no output groups, prints only non-source default-info files. It does not add temp files, does not treat a leading underscore as hidden when a group is requested, does not dedupe across groups, and appends in flag order with default files last. Hide-output-group-results is honored by Zel and is not a Bazel option.
  - [P2] After a failure, Bazel prints a temp path for each existing temp-files artifact. Zel does not.
  - [P1] Aspect results share the same budget and are printed after the target lines. Hiding aspect results drops those class names. A validation aspect named ValidateTarget is not printed, and if it is not in the successful-aspect set its base target is reported as failed. Zel stores both flags and never reads them. It prints no aspect lines and does not fail a target because a validation aspect failed.
  - [P1] Pretty paths walk the convenience-symlink map in creation order and return the first path-segment prefix, prefixed with one `../` per workspace-relative cwd segment. A prefix of `/` and an empty map leave the artifact path. Zel only rewrites an exec path under the cpu and compilation-mode bin directory, with no `../` for a non-root cwd. If several configs make bazel-bin ambiguous, Bazel drops that symlink. Zel still rewrites the default prefix. Non-UTF-8 paths are Latin-1 path fragments in Bazel. Zel formats UTF-8 strings.
  - [P2] The verbose-failures hint is printed only when this method recorded a failure. Sandbox debug does not affect it. Zel prints the hint for every unsuccessful build outcome except when verbose failures or sandbox debug is set, including when no failed-to-build line is printed. The workspace-status failure path always names the first configured target and always prints the hint.
  - [P3] Show-artifacts is only a stale comment here. Zel does not implement it either.

### `src/main/java/com/google/devtools/build/lib/buildtool/BuildTool.java`
- Bazel lines: 1914
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs` (`cmd_build_or_test`), with execution in `exec_phase.rs` and the output filter in `zel-exec`
- Verdict: partial
- Differences:
  - [P2] Bazel loads the project file and, when it yields flags, re-parses native options at command-line priority while skipping Starlark-looking prefixes, then parses Starlark options. It posts a non-replaceable canonical command line and an options-update event, then rebuilds build options. An empty project-flag list only releases the tentative canonical command line. A non-null project-resolution target list is evaluated with a no-op reporter. Zel never applies project-file or scl-config flags.
  - [P2] Active-directory and project-read failures become an invalid-project configuration error. Upload and dump mode lets the experimental active-directories flag replace the project matcher, and an empty matcher warns and falls back to full serialization. Download mode always uses an empty matcher. Zel has no active-directory trie and no Skyfocus pass after a successful build.
  - [P2] The remote analysis cache is absent, including frontier upload, the metadata RPC, the 20-second client lookup, the install checksum versus the server-checksum override, the minimum long version when workspace diff info is missing, and the hit-rate line. Download with no service warns and evaluates locally. The cache JSON log is a Gson writer on an ISO-8859-1 stream: code points above U+00FF are written as `?`, angle brackets and ampersands are not HTML-escaped, and non-finite numbers are rejected. Bazel sorts the per-function breakdown with UTF-16 compare. Zel accepts the cache flags and does not perform these steps.
  - [P2] On a non-crash exit Bazel deletes dirty nodes when the version window is not −1, always posts build info, and closes the cache JSON log. An error or unchecked runtime exception is rethrown and skips that cleanup. Zel does not collect by version window and writes workspace status only on paths that reach the status-file writer.
  - [P0] Skymeld clears the syscall cache, overlaps analysis and execution, still installs convenience symlinks and records analyzed targets when a later exception is thrown, and reports execution failure with a null message so it is not printed again. The non-skymeld path warns for each exclusive test when the strategy forces parallelism, then runs those tests in parallel. Exclusive-if-local is demoted with no warning. It runs the analysis post-processor, posts the no-execution event when execution is skipped, then throws a delayed analysis failure. Zel always finishes analysis before execution, never demotes exclusive tests, and does not post the no-execution event. A test strategy other than standalone or exclusive fails before analysis.
  - [P2] After a successful non-skymeld build, a non-null aquery dump format writes an action graph without command lines, param files, or file-write contents. A bad format fails with the skyframe-state output message. Text output uses the platform charset. Zel does not write this dump.
  - [P2] The skyframe memory dump runs only after targets return, and only for a sequenced executor. The comma-separated format keeps empty tokens and does not trim. Unknown tokens fail. Missing incremental state fails with the not-tracked sentence. Zel ignores the flag.
  - [P1] The output filter is a Java pattern applied with find to the Unicode form of the tag. Non-ASCII internal Latin-1 strings are re-decoded as UTF-8, and invalid UTF-8 becomes U+FFFD. A dot matches one UTF-16 code unit and does not match line terminators. Digit, word, and space classes are ASCII. The filter hides non-error, non-debug events whose tag does not match. Unset means show everything. Zel uses a literal contains check on the action owner and clears that action's stdout and stderr only.
  - [P2] Validation warns when local test jobs is greater than jobs. Zel never emits that warning.
  - [P1] Request processing maps failures to distinct detailed codes. A test-exec exception is still success but the message is reported. Invalid configuration and abrupt exit are catastrophes. A post-execution dump is the skyframe-state-after-execution code. Interrupt prefers a crash or pending abrupt exit, otherwise reports that the build was interrupted and posts the interrupted event. A null message is not reported. Stop records the clock before the build-complete event. A crash skips that event but still posts frozen tool logs. An interrupt during finish replaces a success code and otherwise keeps the failing code unless it is already interrupted. Zel folds these into fewer exit codes, and a test failure is tests-failed from this same function.

### `src/main/java/com/google/devtools/build/lib/buildtool/CollectionObjectTraverser.java`
- Bazel lines: 168
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No Zel type implements this traverser or a memory measurer. `--memory` only checks the mode words, then prints per-function node counts from a sorted map. It never walks lists, sets, maps, multimaps, compact maps, or nested sets, and it never reports shallow bytes.
  - [P3] Bazel charges a handled collection under a fixed context name and does not reflect its fields. A list or set is sized as the object's shallow size plus a synthetic object array of `size()` slots, not the real capacity and not linked-list nodes. A map or multimap adds `size() * 32`. That size is a 32-bit int promoted to a long, so the product does not overflow for any legal Java collection. A compact map adds a synthetic object array and skips the interned offset table. Unhandled objects return −1 so the JVM layout computer runs. Zel has no equivalent byte formula, reference width, or alignment rounding.
  - [P3] Logical contents are the only edges: list and set elements, each map entry's key and value, each multimap entry, and compact-map keys in index order. Internal tables, entry nodes, and per-key multimap lists are not walked. A null edge is dropped, so null elements, keys, and values add nothing. Calling the traverser on null itself throws because the pattern switch rejects a null selector. Zel maps cannot hold null keys, and nothing performs this walk.
  - [P3] Hash map, set, and multimap iteration order is unspecified, so the first-owner identity set and the needle stack depend on bucket order. Linked and compact maps are insertion-ordered. Zel's only related output is sorted by function name and is not an object graph.
  - [P3] Nested-set children are read by reflection. A missing field fails class initialization. An object-array child, including the shared static empty array, is edged with an array context. A singleton element or a future is edged with a null context and then reflected. Cached depth and order are not followed. Empty sets therefore contribute one shared zero-length array, owned by whichever node sees it first. Interned is always false. Zel's nested set uses an optional node: none is empty with no sentinel array, and a leaf versus an array stays distinct, so a leaf that is itself an array is not relabeled. There is no deserialization-future child and no reflective init failure.
  - [P3] Interned is always false, admission is always true, and array, field, and ignored-field hooks always return null. Interned collections are counted once for the first sky value, this traverser never blocks a walk, and it never renames a reflected field or array slot. No Zel hook records those decisions.

### `src/main/java/com/google/devtools/build/lib/buildtool/CommandPrecompleteEvent.java`
- Bazel lines: 23
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/observability.rs` (`BuildObserver::finish`)
- Verdict: partial
- Differences:
  - [P1] Bazel's type is a zero-field marker. Its only subscriber ignores the instance and posts a build-metrics event from the collected action summary, memory, targets, packages, timing, cumulative stats, artifacts, graph stats, worker metrics capped by the published maximum, the worker pool, dynamic execution, and the remote analysis cache. Zel has no type and no subscriber. Finish posts an empty build-metrics message.
  - [P2] Bazel posts the marker on the event bus after the output filter is set to everything and after command-complete notification (an interrupt there records an interrupted exit and re-interrupts, then the post still happens), and before every module after-command and before the after-command event. A subscriber exception is subject to fatal event-bus exceptions. Zel calls the post inline after console-event conversion, profiler finish, and exec-log close, then finishes BEP, writes the profile, then the UI after-command. There is no bus, no interrupt from command-complete, and no after-command event.
  - [P2] The marker is posted for every command that reaches after-command. Zel runs finish only from build or test. Module, fetch, vendor, and other non-build commands never emit this event.
  - [P3] This class carries no exit code. The separate deprecated runtime event does, and Zel does not implement that type either. In the same finish function Zel already has the exit code and uses it for build finished, BES success, and the abort reason before the metrics post. Bazel's subscriber cannot read an exit code from this event.
  - [P1] When BEP is not deferred, Zel's finished handler posts build finished, then empty build metrics, then tool logs, then convenience symlinks. Finish posts tool logs before build metrics. Bazel's marker exists so metrics are published before the after-command BEP close, ahead of the build-complete event.
  - [P3] The Java type has no fields, strings, numbers, maps, regexes, or JSON. Encoding, integer width, map order, and JSON parsing do not change results for this class.

### `src/main/java/com/google/devtools/build/lib/buildtool/ConfigSymlink.java`
- Bazel lines: 59
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/symlinks.rs` (`convenience_links`; roots from `BlazeDirectories`)
- Verdict: partial
- Differences:
  - [P1] Link paths map every target configuration through the getter and the artifact root, then distinct on path string equality. The caller creates the link only when that set has size 1. Size 0 deletes it, and a larger size deletes it and warns that targets use multiple configurations. Zel always emits one target per suffix, under `bazel-out/{cpu}-{compilation_mode}/bin` or `testlogs`, including with no targets or when configured targets use another output directory.
  - [P2] This class always returns the genfiles directory: `genfiles` when merge-genfiles is false, otherwise `bin`. It ignores the build-request options, so skip-genfiles does not apply here. The production override returns an empty set, and the default is to skip. Zel drops genfiles when skip is set or the genfiles directory is empty, and the genfiles root is always `bin`, so turning the merge flag off still links genfiles at `bin` when the skip is off.
  - [P3] The repository name is passed into the getter. With sibling repository layout, a non-main repository inserts a repo segment. Zel has no repository argument. The only Bazel caller passes the main repository, so the usual main-repo path matches.
  - [P2] The link name is the prefix plus the suffix and ignores the workspace base name. Zel uses the same concatenation for bin, testlogs, and genfiles. A prefix containing an unpaired surrogate is kept in the Java link name. Zel flag values are Unicode scalars. Bazel filesystem paths are Latin-1. Zel path strings are UTF-8, so a non-UTF-8 output-base byte changes the root path relative to the Zel target. No integer-width, regex, or JSON behavior in this class.

### `src/main/java/com/google/devtools/build/lib/buildtool/CqueryProcessor.java`
- Bazel lines: 80
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/post_analysis_query.rs` and `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs`
- Verdict: partial
- Differences:
  - [P2] Consistent labels use the label string with the effective Starlark semantics. With unambiguous label stringification off, a main-repo label is `//pkg:name`. Zel's printer always emits `@@//pkg:name` and never reads that flag. Non-main repos stay canonical on both sides when the flag is off.
  - [P2] Display form comes from the inverse repository mapping, the first apparent name in insertion order. Zel copies entries into a hash map with first-wins. An unordered mapping can pick a different apparent name when two names share one canonical repo. Zel also inserts a synthetic bazel_tools entry, so an unmapped bazel_tools label prints with one `@` instead of `@@`.
  - [P0] Config re-keys each label in the analysis graph. Target uses that label's own top-level configuration. Null looks up a null configuration. Any other id is a prefix of the full checksums. Zel only keeps nodes that already carry a matching config. It treats every null-config node as target, uses one global top config, and its map is keyed by short hash and only contains the top, core, and exec configs plus configs visited while walking, so an unvisited checksum is unknown and two configs with the same short hash overwrite each other.
  - [P2] The no-target-in-configuration message uses the full inner expression text. Zel truncates it.
  - [P0] Query settings drive dependency filtering. Notool-deps keeps exec-configured deps when the parent is itself a tool configuration, and keeps null-config deps of a target-config parent. Zel drops every exec edge whenever tool deps is false. Noimplicit-deps drops implicit, toolchain, and aspect-private edges. Zel's analysis deps do not consult implicit deps.
  - [P2] Nodep-deps false fails with the settings that remain after removing the other filters. Zel always prints a fixed list that includes aspects.
  - [P1] Top-level aspects are passed into the accessor and, with include-aspects, become aspect deps and files-output artifacts. Zel records them but calls analyze-with-aspects only for aquery, so cquery ignores them. Explicit aspects are not implemented on the cquery path.
  - [P1] Files output uses the top-level artifact context: default, temp files, and the hidden top-level group, plus validation when run-validations is on. Plus and minus adjust that set. The default group unions files-to-build with the default output group. Empty groups are skipped, and only important artifacts are printed. Zel, with no output groups, prints default-info files only.
  - [P3] Extra functions are config plus the runtime query functions, last name wins. Zel's function list is the default query functions plus config.

### `src/main/java/com/google/devtools/build/lib/buildtool/ExecutionProgressReceiver.java`
- Bazel lines: 240
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs` (`UiStateTracker`; totals fed from `observability.rs`)
- Verdict: partial
- Differences:
  - [P2] Bazel's fraction is completed action-lookup keys over exclusive tests plus enqueued action-lookup keys, with both sets concurrent and idempotent. Enqueue inserts only action-execution keys. Completion inserts into both sets. Evaluation of an action-execution key counts every state, including errors and cache hits that never ran. Zel keeps 64-bit counters. Completed increments only when a completion removes an id that start put in the active set. A second completion of the same id does nothing. A second start of an id still active can raise the enqueued count by one without a second map entry. Two actions that share one string id collapse to one.
  - [P2] Zel's denominator is replaced, not accumulated. Analysis done sets the total to the sum of action lengths across configured targets, so an action shared by two targets counts twice and actions that are never started stay in the total. The printed denominator is exclusive tests plus the max of enqueued and completed, so a later total below the completed count still displays as N / N. Bazel's sets only grow, and a completion cannot exist outside the enqueued set.
  - [P2] Exclusive tests are a Java int (status artifacts of exclusive tests, or zero from the execution tool) and are added into the denominator and into the in-flight check before those actions are enqueued. When they later enqueue, both terms include them. Zel's exclusive-test setter has no caller, so that count stays zero.
  - [P2] In-flight for the cached-actions line is completed less than exclusive tests plus enqueued. Zel uses execution progress and enqueued greater than completed, and ignores exclusive tests. With exclusive tests still pending and no other gap, Bazel reports in-flight and Zel appends that no actions are running. After the total leaves enqueued above completions and nothing is active, Zel reports checking cached actions even though those keys were never enqueued.
  - [P3] The bracket text matches for every non-negative int: English grouping, so 999 and 1,000. Counts above 2^31−1 diverge. Set size and the int addition saturate or wrap, and in-flight is then false for any non-negative completed count. Zel stores an unsigned 64-bit count and prints the full grouped value. The add wraps only at 2^64, and debug builds panic. The completion-delta subtract is also 32-bit, so a saturated set size yields zero and the watchdog treats completions as inactivity.
  - [P2] There is no Zel skyframe receiver. On success Bazel posts a top-level target built event, an aspect built event, and for a build driver either nothing when skipped, one aspect event per map key in insertion order, or one target event from the wrapped configured target. A null new value throws. Failed non-action nodes post nothing. A pruned action posts an action-change event with the clock and does not change the sets. Zel posts none of these events.
  - [P2] Inactivity monitoring is absent. Bazel's monitor reports started from a non-empty enqueued set, pending from the status reporter's count, and the next-completion wait by sleeping one second at a time up to the timeout (non-positive returns zero immediately) and returning the completion delta, or zero. While exclusive artifacts are not building, it shows currently executing actions and posts an inactivity event. An interrupt from sleep propagates. Zel's progress refresh does not implement this wait or these events.
  - [P3] This class does not compare strings as UTF-16, compile a regex, decode Latin-1, or parse JSON. The only map walk is insertion order for aspect-built events, which Zel does not emit.

### `src/main/java/com/google/devtools/build/lib/buildtool/ExecutionTool.java`
- Bazel lines: 1079
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` (`run_build_actions`), with the forest in `zel-exec` and symlinks in `symlinks.rs`
- Verdict: partial
- Differences:
  - [P0] Execroot and symlink-forest failures abort the build. Zel discards the plant result and continues.
  - [P1] Bazel plants the forest from the loading-phase package-root map, including partial top-level links, a multi package-path forest, a skip of external, and sibling-repository layout. Zel always links every top-level workspace entry and puts external repos only under `execroot/external`. Sibling layout is a no-op.
  - [P2] Case conflicts use ASCII lowercasing and raw path bytes. Zel uses Unicode lowercase, so a Turkish capital I gains a combining dot, and lossy conversion collapses invalid UTF-8 names to U+FFFD. A non-UTF-8 byte makes Zel ignore the whole bazelignore file. Bazel still applies those lines.
  - [P2] Bazel deletes and recreates the temp actions directory and fails the build if that fails, logging the directory's entry names on delete failure. Zel never creates or wipes that directory.
  - [P2] Disabling output-file checks makes Bazel report nothing modified unless the output service says the tree was fully deleted, so externally edited outputs are not invalidated. Zel never reads the flag. Output-tree tracking is passed to the output service, and finalize runs only when every analyzed target was built. Zel has no output service.
  - [P1] Explain is workspace- or home-relative, truncated each build, and written as UTF-8 build options plus the non-default options description, then only depchecker lines. Open failure warns and skips the file. Zel joins a relative path onto the execroot, prints the info line even if the parent cannot be created, writes a blank options line only when the file is absent, and appends an executing-action line for cache misses. An existing file is not reset.
  - [P2] Emacs prints entering and leaving directory lines. Zel only turns color and curses off.
  - [P1] Convenience symlinks follow top-level target configurations. Mixed configs use the top-level config only when one output directory name matches it. Otherwise the links are deleted and Bazel warns. The BEP event posts the real protos when the flag is set. Log-only does not touch the filesystem. Failures are one warning. Zel always targets the command-line cpu and compilation mode, posts an empty convenience-symlink node whenever analysis ran, and log-only prints an info line. The product prefix is hardcoded. Links are skipped when build is false.
  - [P1] Action-cache load failure aborts. A corrupt cache is quarantined, a warning is printed, and an empty cache is retried. Save runs only after a completed build, not after a catastrophic error. Save failure reports an I/O error and still posts cache statistics. Zel turns a bad file into an empty map, stores a load warning and never prints it, swallows save errors, and saves even when the build fails or build is false. The encoding is not Bazel's journal. Dump order is sorted on both sides.
  - [P2] Resource configuration uses local test jobs, or the max int when it is zero. CPU-load scheduling uses host load plus a window and at most three times the CPU count. CPU requests are admitted at 0.6 times. Disabling the allow-one flag fails an oversized idle request. Zel treats zero as unlimited, ignores the window, sums action estimates instead of host load, checks the full CPU request, hardcodes allow-one-when-idle, and casts the test cap to a 32-bit int so values above the max wrap. There are no separate local and dynamic queues.
  - [P2] A max stdout/stderr of −1 is stored as a bit-shifted maximum int and passed into the executor. Zel treats any negative value as the maximum usize, so output between those sizes is printed in Zel and skipped in Bazel. Zel's skip note names temp paths that were never created.
  - [P1] The up-to-date filter still runs non-test actions under check-tests-up-to-date and blocks only test-runner actions. Check-up-to-date blocks both. Zel sets the flag only from check-up-to-date and applies it to every scheduled action.
  - [P2] Discard-analysis-cache, and a non-incremental executor, clears analysis nodes before artifacts are built. Zel drops them only after the command returns, and notrack-incremental-state does not clear them on this path.
  - [P2] Bazel posts execution starting, building progress, an execution-finished event, and a phase-complete event with elapsed milliseconds only if the stopwatch was started. The inactivity watchdog uses the progress interval. A catastrophic error still ends listeners and posts those events, but skips the cache save, result printer, emacs leave line, and finalize. Successful targets and aspects keep analyzed order. Zel has no watchdog, no those progress lines, no catastrophic split, and prints up-to-date targets from valid roots minus a set of failed labels.
  - [P2] The test action context is restricted to the test strategy while the executor is built. Zel only maps exclusive, standalone, and local onto test tags inside the test runner. An unknown strategy is rejected before analysis.

### `src/main/java/com/google/devtools/build/lib/buildtool/InstrumentationFilterSupport.java`
- Bazel lines: 140
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs` (`instrumented_prefix`, `default_instrumentation_filter`)
- Verdict: partial
- Differences:
  - [P1] Bazel installs the returned string on the instrumentation filter through the regex-filter converter, so it is what the collector matches against the label string. Zel only prints the two info lines and never writes the flag. Instrumented sources keep the default exclusion filter unless the user passed the flag. The logged default is not the filter that runs.
  - [P2] An empty target collection makes Bazel return `^//` with no events, and the caller still installs that inclusion regex. Zel returns none and leaves the default exclusion filter in place.
  - [P1] Bazel's production input is the tests to run after full suite expansion and tag, size, timeout, and language filters. The suite rule itself is not in that set. Zel is called with the unresolved command-line targets and keeps any test or test-suite rule, so a suite contributes its own package and the test filters do not change the set.
  - [P1] Suite members are not collected in Zel. Bazel reads tests as a label list and adds each package name. Zel matches only string lists, while the tests attribute is a label list, so that branch never runs. The string parser also drops external labels and does not resolve relative labels. A suite whose tests live in other packages therefore yields only the suite package in Zel. Bazel's installed filter is the sorted packages of the expanded test targets. An empty tests list happens to match, because both sides then use that one package.
  - [P2] A non-test passed straight into the filter contributes its package in Bazel. Zel skips it. A configurable tests value makes Bazel throw. Zel ignores it.
  - [P3] For already-normalized package strings, the prefix stripping and filter assembly match: one of the suffixes internal, public, and tests, then the first javatests and test/java rewrites, sorted unique packages, drop a later package only when it is under the previous one, and rewrite `^//[/:]` to `^//`. ASCII suffixes do not diverge under Latin-1 versus UTF-8, and both sets sort those bytes the same way. Path-fragment starts-with still normalizes dot, dot-dot, and duplicate slashes before the ancestor check while leaving the original text in the pattern. Zel compares the raw strings, so those forms stay as two filters in Zel and collapse to one in Bazel. Legal package names are already normalized.
  - [P2] When Bazel installs the string, the converter splits on unescaped commas and compiles each piece as a Java regex. A package containing a comma, parenthesis, or dot changes which labels match, and an illegal pattern fails configuration. Zel never parses the heuristic string.

### `src/main/java/com/google/devtools/build/lib/buildtool/OutputDirectoryLinksUtils.java`
- Bazel lines: 394
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/symlinks.rs` (`removeOutputDirectoryLinks` is in `commands/clean.rs`)
- Verdict: partial
- Differences:
  - [P2] Normal mode with skip-genfiles (default true) still reserves the genfiles name and deletes an existing symlink. Zel omits that name, so a leftover genfiles link stays, and if the workspace directory basename is `genfiles` Zel points that name at the execroot. Bazel's first-name-wins set drops that execroot link.
  - [P1] A config symlink creates bin, testlogs, and genfiles only when every target config yields one distinct path. Zero candidates delete the link with no warning. More than one deletes it and warns that the links were cleared because targets self-transition to multiple configurations. Zel always links the single directory it was given and never emits that warning.
  - [P2] Extra symlink definitions passed after the standard list are honored. Zel only knows the built-in names. On a duplicate name Bazel keeps the earlier definition. Zel's write order is workspace, out, bin, testlogs, genfiles, and a later write replaces an earlier link.
  - [P2] The out link targets `execroot/(productName + "-out")`. Zel always uses `execroot/bazel-out` and does not use the product name for that path. The product name only fills a missing prefix.
  - [P1] Bazel creates the link target before ensuring the symlink. Failure reports that the directory cannot be created and leaves any existing link in place. Zel never creates the target, so it will point at a missing path or a file, and it unlinks a wrong symlink before the new symlink call.
  - [P2] Link paths and readlink comparisons go through path-fragment normalization, with no symlink walk. An existing link whose text normalizes equal is left untouched. Zel compares the raw readlink to the request string and passes that string to symlink, so `..` is resolved by the kernel through intermediate symlinks.
  - [P2] Workspace basenames and path bytes are Latin-1 internal strings. Zel takes a string and lossy file-name conversion, so a non-UTF-8 directory name becomes U+FFFD. An empty workspace path becomes the literal name `workspace` and the links are absolute under `/`. There is no JSON in this class.
  - [P2] Log-only performs no filesystem operations and emits no info lines. It still records create and delete protos and still warns on ambiguous links. Zel prints an info line that the symlink was not created and returns no proto list.
  - [P1] Failures are one warning that one or more convenience symlinks failed, plus a cannot-create line (two spaces; not-a-symlink text names the path). Zel prints one line per link and ignores remove errors. Stat errors other than not-found are failures in Bazel. On non-Unix, Zel's create call is compiled out after the unlink.
  - [P1] Clean deletes every standard name, including genfiles, and reports the name and the I/O message. Zel clean only deletes names the convenience-link list returned, so a skipped genfiles link is kept, and unlink errors are discarded.
  - [P1] Removing output-directory links deletes each link by its relative name, then removes the directory and empty parents. An absolute prefix does not delete directories. A relative prefix deletes that path and empty parents. Zel's clean path records the full path in the failure line. For an absolute prefix, joining the parent replaces the workspace, so an empty `/tmp` can be removed. For a trailing slash the parent is empty and no directory is removed.
  - [P1] Every considered name becomes a convenience-symlink proto. Create's target is relative to the output base, including when mkdir or symlink later fails. Delete has no target. Log-only is included. Last-wins applies if normalized fragments collide. Zel returns only console strings. The server posts an empty convenience-symlinks event. Ignore mode is not checked inside the Java method. Zel returns immediately and leaves the filesystem unchanged. A prefix of `/` is a no-op in both.

### `src/main/java/com/google/devtools/build/lib/buildtool/PathPrettyPrinter.java`
- Bazel lines: 72
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs` and `exec_phase.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel builds `../` repeated by the workspace-relative working-directory segment count and rejects an absolute directory. Zel never inserts that prefix, so a build whose cwd is a relative subdirectory prints `bazel-bin/...` where Bazel prints a chain of `../`.
  - [P1] Bazel walks the convenience-symlink map in insertion order and rewrites the first target that is a segment-aligned ancestor, including an exact match. A shorter earlier target beats a longer later one. Zel rewrites only an exec path that starts with the cpu and compilation-mode bin prefix, so files under bazel-out, testlogs, genfiles, or the execroot stay as execroot plus exec path.
  - [P2] When several configurations make the bin symlink ambiguous, Bazel omits it and the next ancestor wins. Zel still rewrites any path under the top-level bin prefix, and leaves a different config directory absolute instead of under the out link.
  - [P2] A symlink prefix of `/` makes Bazel return the input unchanged. Zel's show-results treats `/` like ignore, clean, false, and 0 and prints execroot plus exec path. The run display does not treat `/` as a disable, so the bin prefix becomes `/bin/...`. Summary rewriting still substitutes the testlogs prefix.
  - [P2] Run's run-under path keeps only the basename, and the target path keeps only the suffix after the last `/bin/`. Bazel keeps the relative path under the link. A path with no `/bin/` is printed as the bin prefix plus the entire exec path.
  - [P2] With relative test-log paths, Bazel pretty-prints each log through the same map. Zel's live buffer globally replaces the execroot prefix with the empty string. The summary then globally replaces the testlogs directory with the prefixed name. Neither form gains the `../` cwd prefix.
  - [P1] Those replacements run on lossy UTF-8. Bazel path strings are Latin-1 raw bytes, and the prefix check compares those bytes. An invalid UTF-8 byte is preserved and can still match. Zel turns those bytes into U+FFFD, so the prefix misses. The replace is also unanchored: later text in the test buffer that contains the directory string is rewritten. Bazel only rewrites the path it was given, and only on a segment boundary.

### `src/main/java/com/google/devtools/build/lib/buildtool/PostAnalysisQueryProcessor.java`
- Bazel lines: 245
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` and `/workspace/zel-cursor/crates/zel-server/src/post_analysis_query.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel deletes old nodes and applies invalidation before the query, then refuses the command when incrementality is off, with exit 2 and the analysis-query prerequisite message. Zel never drops dirty nodes and still runs cquery and aquery under those flags.
  - [P2] A query result that is unsuccessful but not thrown is still formatted, then the callback closes in fail-fast mode. That drops every cquery formatter and the aquery summary or proto flush. The processor ignores the detailed exit code, so the command can still exit 0, and an empty-results info line is emitted before any formatter output whenever the aggregate set is empty. Zel still prints the full formatted stdout, exits 1, and skips the empty-results line for an empty aquery summary.
  - [P1] Any query exception from formatter setup or evaluation is, with keep-going, only an error line and is not rethrown, so the exit stays 0 and there is no formatter. That includes the starlark expr and file clash, and a file read failure whose message includes the I/O exception text. Zel always exits 1, adds the build-did-not-complete line, and the read error is only the path with no OS error text.
  - [P2] Without keep-going, a query exception or formatter I/O becomes a view-creation failure and the process exit is parsing failure, not the failure-detail metadata code. Formatter I/O is reported as an I/O error whose metadata exit is 36, overridden to 1. With keep-going, that I/O error is only reported. Zel has no that I/O message. A format error exits 1.
  - [P2] The output file is opened and truncated before evaluation. Open failure is exit 36 and the query does not run. Close failure names the file. Zel formats first and writes afterward, so a failed query still creates the file, and a write failure returns 36 without printing the query's stderr.
  - [P2] An unknown output format is invalid, with the same formatter lists, and exit 2, but only after analysis. Zel, on a non-empty expression, resolves patterns with keep-going forced on before that error, so an analysis or pattern failure is reported as skip or warning text and the process exits 2 for the format.
  - [P1] Transitive configurations are the successful skyframe values only, sorted by full checksum into a map keyed by checksum. A duplicate checksum throws. Config prefix matches walk that checksum key set. Zel's map is keyed by short hash, first insert wins on a short-hash collision, seeded with the top, core, and exec configs plus every key registered during analysis, and equal checksums are deduped, so a short prefix can bind a different configuration.

### `src/main/java/com/google/devtools/build/lib/buildtool/PostBuildCallback.java`
- Bazel lines: 32
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The callback is a single method that receives the configured-target collection, invoked only after the build returns normally. The returned failure detail is stored while the detailed exit code stays success, so the build result still reports success when the detail is non-null. Mobile install is the only in-tree caller that then fails the command from that detail. Zel has no post-success callback and no separate failure-detail field. Mobile install calls build or test and, on success, only checks the package rule class.
  - [P2] A null return means callback success. A null callback argument skips the hook. Every in-tree command except mobile-install passes null. The successful-target collection can still be null on that path. Mobile install treats a null collection as success after a warning that no targets were found, and a size other than one as multiple targets specified (exit 2). Zel rejects an empty residue before the build and never receives the built collection.
  - [P3] An interrupt from the callback replaces the success exit with interrupted (numeric 8) and does not store a post-build failure detail. Any other exception escapes that catch and is handled as a crash. Zel has no interruptible post-build hook.
  - [P3] This file does not compare strings, iterate maps, parse JSON, or use regex or integer arithmetic. The collection order is caller-defined. The only implementation uses size and the single element, so order does not change its result.

### `src/main/java/com/google/devtools/build/lib/buildtool/PostExecutionDumpException.java`
- Bazel lines: 26
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no post-execution dump error type and no build-phase handler. The aquery-after-build format and output file, and the Skyframe memory dump, are only option definitions. A build that sets them still succeeds. Bazel throws this checked exception from those dumps, and the build tool maps every instance to the skyframe-state-after-execution action-query code (exit 1) with the exception message, then prints that message only. It does not set catastrophe and it does not keep the wrapped failure's own action-query code (command-line expansion exit 2, template expansion exit 2, or output failure exit 6).
  - [P3] The one-argument constructor stores the cause's `toString` as the message: the binary name, or the name plus the cause message when that message is non-null. The reported line and the failure detail are that string, not the cause message alone. A null cause stores a null message. Reporting then skips the error event, and setting a null failure-detail message throws, which the outer catch treats as a crash. No current call site passes a null cause into that constructor. The I/O, command-line, and template constructors do, so the user-visible text includes the Java class name.
  - [P3] The two-argument constructor keeps the given string even when the cause is null. Memory-dump failures use that form, so the detail text is the explicit sentence and the cause is not appended. Java concatenation turns a null I/O message into the four characters `null`. An empty message is non-null, so Bazel still emits an error event with an empty string.
  - [P3] Messages are Java strings. An unpaired surrogate in a cause message or in the explicit message is retained. Protobuf string encoding of that message cannot round-trip those units, and a Rust string cannot hold them. This class does no regex, integer, map-order, or JSON work.

### `src/main/java/com/google/devtools/build/lib/buildtool/SkyframeBuilder.java`
- Bazel lines: 306
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` (`run_build_actions`, `run_tests`)
- Verdict: partial
- Differences:
  - [P0] Before execution Bazel detects modified output files with the checker and the filesystem-thread count (null request options become 200; otherwise the flag, default 200, auto is host CPUs, max is the max int). Zel never stats the output tree, so outputs changed on disk or only present remotely are not invalidated and can be reused.
  - [P1] The action-execution salt is mixed into the action-cache digest. Zel's cache key has no salt, so toggling the remote cache or those exec properties does not drop cached actions. The last execution time range only increments a counter and does not change invalidation.
  - [P2] The max stdout/stderr flag is the converted int: −1 becomes a bit-shifted maximum int, and the converter rejects anything outside that range. Zel parses a raw signed 64-bit integer, treats any negative as unlimited, and skips when the length is greater than the max (default 1 MiB). Output at or above about 1 GiB is suppressed in Bazel and still emitted in Zel.
  - [P1] Coverage artifacts owned by the coverage-report key are removed from the main set and evaluated only after every exclusive test, including when earlier phases already recorded errors. Zel builds all analyzed actions first, then tests, then writes the coverage report only for test or coverage when there is at least one test job and split-coverage postprocessing did not fail. A build with coverage artifacts still builds them in Bazel. Zel's default merger ignores a non-zero generator status and can still announce the report. Bazel records that failure as an execution exit code.
  - [P0] Targets to skip are subtracted from targets to build, parallel tests, and exclusive tests. Zel has no execution-time subtraction, so incompatible or environment-skipped tests that Bazel drops are still built and run.
  - [P0] Parallel tests run inside the main evaluation, so shards overlap, and exclusive tests are only built there. After that, each exclusive target runs one at a time, so runs and shards do not overlap, in union order. Zel runs every test after the build: parallel targets overlap but shards inside a target are sequential. Exclusive-if-local plus local is not exclusive. Streamed test output reverses the exclusive list. A strategy that forces exclusive tests parallel in Bazel is rejected in Zel unless it is exclusive or standalone.
  - [P1] With the error-handling refactor off, execution errors are collected and the exclusive loop and coverage still run, then one failure is thrown using the more-important exit code. Priority is infrastructure over ordinary, plus one when a failure detail is present. Ties keep the earlier code. The flag on throws on nokeep-going and skips the rest. Zel's nokeep-going returns before any test or coverage if the build failed. Notest-keep-going stops later exclusive tests. The command exit prefers build failure, then test failure, and does not apply that comparator.
  - [P3] If an exclusive test reports success with a null exit code and an empty key-name list, Bazel throws and skips finalizers. Zel records no status and continues the summary.
  - [P2] The progress denominator is a 32-bit sum of exclusive tests' status artifacts, taken before skipped targets are removed, so it wraps at 2^31 and still counts skipped exclusive tests. Zel never sets exclusive tests. The inactivity watchdog is not implemented. A null request uses 200 filesystem threads and then throws on the progress interval. A null UI options throws on the stdout cap. Zel uses flag defaults.

### `src/main/java/com/google/devtools/build/lib/buildtool/SkyframeMemoryDumper.java`
- Bazel lines: 350
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/dump.rs` (`--memory`)
- Verdict: partial
- Differences:
  - [P2] Zel does not walk the node value graph. Shallow, reachable, transitive, and full dumps are absent. It tallies node cardinality for ten functions and omits every other sky function. Bazel counts retained objects and bytes reachable from the node value, with a shared identity set so each object is counted once.
  - [P2] Collection and display modes do not change Zel's payload. Reachable, shallow, transitive, and full differ on whether interned objects are counted, and a non-summary display turns on per-class maps. Full prints a JSON object. Per-class output is sorted by byte totals descending, with ties keeping encounter order. Zel always prints a width-12 count and the function name in sorted-map order.
  - [P2] Needle, transient, configuration, precomputed, and workspace-status flags are accepted and ignored. Bazel threads them into the object traversers. Builtins always traverse the rule-class provider with transient fields forced on so those objects sit in the seen set. Shallow mode also pre-marks objects reachable from each direct dependency.
  - [P2] The subject and the node string are ignored, so a missing node still yields the global tally. Bazel shallow, deep, and transitive dump that one node. A null graph entry fails the command, and a null direct-dep entry throws. Full roots are only done entries whose reverse deps are empty, then post-order over direct deps. In-progress nodes are not roots. Zel counts matching keys whether or not they are done. An empty graph makes Bazel print an empty JSON object and Zel print nothing.
  - [P2] Shared objects in a full dump are assigned to whichever worker inserts them first, so per-node counts can change between runs. Output keys are sorted with UTF-16 compare. Equal names keep concurrent-map encounter order. Zel's map order is UTF-8 byte order, which diverges when a name contains a supplementary character.
  - [P2] JSON quoting exists only on the Bazel side. The writer does not HTML-escape, writes controls as short escapes, and encodes the line with the JVM charset. A null string would be bare null. Zel prints raw function names.
  - [P2] Numbers are Java long retained-byte and object counts. Zel reports node counts only, never bytes or per-class histograms. Transitive failures become an illegal state. Full failures become a dump-failed exception. A direct-dep cycle can deadlock the all-as-list wait. Zel returns success after the listing.
  - [P2] The experimental skyframe memory dump calls the full summary dump with a null needle. The flag is only registered in Zel, so no post-build memory JSON is written.

### `src/main/java/com/google/devtools/build/lib/buildtool/SymlinkForest.java`
- Bazel lines: 490
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/layout.rs` (`plant_symlink_forest`)
- Verdict: partial
- Differences:
  - [P2] The package-root map plant is absent. Zel always does the single-root eager listing. With one main-repo root and no root package, Bazel links only the first path segment of required packages and still links a bazel-prefixed segment. It does not link other top-level files. Zel links every top-level entry that passes its filter.
  - [P1] Multi-root package-path planting is absent: longest prefix, parent-before-child directories, a real directory where a path has more than one root, ancestor symlink elision, per-entry links under a package, and a swallowed I/O error that skips that directory. The empty-fragment pass links top-level names that do not start with the product prefix and whose exec path does not exist, and it does not apply the ignore list or the external exclusion.
  - [P2] Case conflicts: both eager implementations group names and plant only groups of size 1. Bazel keys with ASCII lowercase. Zel uses lossy UTF-8 and Unicode lowercase. German sharp s and sigma collide only in Zel, so neither name is planted. Bazel plants both. Zel's returned set is never used to plant the one spelling a package needs.
  - [P2] Non-UTF-8 file names stay raw bytes in Latin-1 internal strings. Zel replaces invalid UTF-8 with U+FFFD, so the execroot link name can differ, the symlink target can dangle, and distinct byte names can collapse into one conflict group.
  - [P1] The ignore check compares bazelignore prefixes and REPO.bazel ignored-directory globs to the absolute path with only the drive slash removed, so a repo-relative entry does not match and the link is still planted. A star or double-star pattern matches that absolute path and suppresses every top-level link. Zel never reads ignored directories. It trims lines, treats a hash comment only after trim, strips a leading dot-slash and a trailing slash, drops any entry containing a slash, and on invalid UTF-8 ignores the whole file. Bazel decodes UTF-8 with replacement and still parses, and an absolute ignore path fails the build. Zel then skips a top-level name whose basename equals one of those strings.
  - [P2] External is always excluded in Zel. Bazel excludes the source external directory only when sibling layout is off. With sibling layout, Bazel links the source external tree and plants each repo beside the execroot. Zel has no sibling mode and does not delete every symlink in the execroot's parent.
  - [P1] External repos: Bazel links the repo path to the package root, creates the external directory on the first such repo, and skips a repo already in the in-memory set. Creating a symlink throws if the path already exists. Zel scans the output-base external directory, keeps only directories, drops names starting with `@` or `.`, sorts names, and skips an existing path instead of failing. Repos that are not directories are omitted.
  - [P1] Both unlink symlinks without following them. Bazel delete-tree changes permissions and retries. Zel does not, so an unreadable tree fails the plant and can leave the rest uncleaned. Zel creates the execroot before listing. Bazel does not, and a missing execroot fails the listing.
  - [P0] The wrapper then symlinks each action source input that is not already present, and on symlink failure copies the file bytes. Bazel only plants directory symlinks. The server discards plant errors. Bazel plant failures abort the build. Non-Unix builds compile out the symlink call, so Zel returns success and plants nothing. The production prefix is hardcoded. Bazel uses the product name.

### `src/main/java/com/google/devtools/build/lib/buildtool/TargetValidator.java`
- Bazel lines: 37
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs` (`cmd_run`)
- Verdict: partial
- Differences:
  - [P1] The only validator warns and continues when keep-going is set, and throws a loading failure otherwise. Zel always errors and returns before the build, so keep-going still produces no outputs.
  - [P2] With keep-going and more than the allowed targets, Bazel warns, then warns for non-executables only until the second target that is not the run-under label, and skips the rest. Zel emits only the single-target error and does not emit per-target not-executable warnings.
  - [P1] A label run-under raises Bazel's limit from 1 to 2 and is identified by parsed label equality, so a pattern that expands to the run target plus that label is valid. Zel counts only the run pattern, so that same pair is rejected. A non-executable run-under target fails this hook in Bazel. Zel does not apply the executable check to it and can run the main target with no wrapper.
  - [P1] Run-under is shell-tokenized before the label test. Zel splits on whitespace, so quotes and escapes change whether the prefix is a label or a command.
  - [P2] Bazel validates the post-filter loading-phase set. Zel counts resolved patterns with none of those filters. No-expand leaves a test suite in the set. Bazel then rejects it as not executable, while Zel still expands it.
  - [P1] Suite expansion runs for every suite before the count. Zel expands only when the unresolved set has size 1, and on expansion error keeps the suite. A test suite is not executable in Bazel. Zel treats it as executable, so a failed or skipped expansion can build the suite.
  - [P0] Bazel allows a target if it is an input or output file, an executable rule, a test rule, or an alias, bind, label flag, or label setting. Zel allows a binary by class-name suffix even when executable is false, any rule whose executable attribute is true, tests, test suites, and files. An alias or label flag of a binary is accepted by Bazel and rejected by Zel.
  - [P1] An empty target set is no targets found to run with exit 2. Zel prints a slightly different sentence and returns build failure, exit 1, without the build-failure trailer.
  - [P2] The single-target text keeps the first five labels in collection order, then sorts that subset. Bazel's set order follows loading-phase insertion, and suite members come from a hash set. Zel's implicit suite members follow sorted name order, so a pattern that expands past five targets can list a different truncated selection.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/BuildCompleteEvent.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`build_finished`)
- Verdict: partial
- Differences:
  - [P1] Stream conversion always sets the deprecated finish-time millis from the build result's stop time as well as finish time. Zel writes only finish time. Any non-zero stop time is on the Bazel event and absent in Zel.
  - [P3] In-range millis, including negatives, split the same way. Millis outside the timestamp range make Java throw. Zel still emits seconds and nanos.
  - [P2] The constructor copies the stored stop time, set immediately before post. Zel passes millisecond time after console replay and profiling. That helper returns zero when the clock is before the Unix epoch. Java current time is negative.
  - [P3] Children are the caller's collection. The one-argument constructor uses an empty list. Zel always announces tool logs then metrics. That matches the only production call and differs for the empty-children constructor or any other id list.
  - [P2] An unset detailed exit code is replaced by a crash failure detail whose message starts with `Crashed:`, crash code unknown, and metadata exit 37, and that detail is copied onto build finished. Zel emits a failure detail only when the caller passes one. A null build result throws before the null check. A null detail on an otherwise set exit code is omitted on both sides.
  - [P3] Overall success compares the full exit code, including name and infrastructure flag. Zel uses a numeric code of zero. Every exit code in this tree agrees, because only success is code 0.
  - [P1] The event keeps the live build result. The streamer classifies aborts from it: out of memory, internal for any other unhandled throwable or catastrophe, incomplete when stop-on-first-failure is set, otherwise no reason from this event. Reasons accumulate, and more than one sets a multiple-reasons description. Zel replaces the reason with user-interrupted when the exit code is interrupted, and incomplete otherwise, with an empty description, including successful builds.
  - [P2] For test, coverage, and run, the streamer drops a non-crash build-complete event so a later testing or run event is the published build finished. Zel posts this single event. Those later Java events use the exit-code constructor, so the published event has no failure detail, while Zel attaches one whenever finish is given one. A crash still posts this event and may be followed by another completing event with the same id.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/BuildInterruptedEvent.java`
- Bazel lines: 22
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The type is an empty marker. Zel has no such type and nothing posts an equivalent event.
  - [P3] Bazel posts it only when an interrupt is the sole exit, with no crash exit code and no pending abrupt exit. That post is the only input to three subscribers. Zel never sends that signal. Global cancel and the final exit code are separate conditions.
  - [P2] The streamer appends user-interrupted to an insertion-ordered set. The last reason is the aborted reason. The description is empty for one non-OOM reason, the OOM message if that is the only reason, or a multiple-reasons line when no-analyze, no-build, or OOM were already recorded. Zel stores a single pair and replaces any earlier reason. At command end it always sets user-interrupted if the final code is interrupted, otherwise incomplete, with an empty description. Unposted children of a non-interrupted command are therefore aborted as incomplete instead of unknown. An interrupt that loses to a crash or pending abrupt exit does not post this event, so it does not force user-interrupted. Zel still uses user-interrupted whenever the final code is interrupted.
  - [P2] The aggregating test listener sets a halted bit, so a later target failure reports remaining tests as halted before testing. Zel only recognizes that status string. It does not set it from an interrupt event. In-flight tests stop for notest-keep-going or because global cancel kills spawns.
  - [P2] Remote execution sets a flag so upload errors are dropped, and shutdown calls shutdown-now and re-interrupts the current thread. Zel remote has no such flag, so interrupt does not suppress remote-cache upload warnings or force that shutdown path.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/BuildStartingEvent.java`
- Bazel lines: 130
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`build_started`)
- Verdict: partial
- Differences:
  - [P1] The deprecated start-time millis field is never set. Bazel sets the request start time. Proto3 omits zero, so any real start is missing on the wire and in JSON.
  - [P2] Start time is the command start minus the startup-time flag, and can be negative. Zel uses millisecond time at begin-build and does not subtract startup time. That helper returns zero when the clock is before the epoch and truncates a wide millisecond count. For in-range values the split matches, including negative millis. Java throws outside the protobuf timestamp range. Zel still writes seconds and nanos.
  - [P2] The build-tool version is the raw build label, an empty string, or the dev override. Zel always sends `release 9.3.2`.
  - [P1] The options description is the shell-escaped non-hidden parsed options. Zel always passes an empty string, so the field is omitted.
  - [P1] The uuid is the command id from the invocation id, the internal environment variable, or a random UUID, and a new random id when the attempt is greater than 1. Zel mixes time and pid into a v4-shaped string, ignores the invocation-id flag, and collides if two events are posted in the same millisecond in the same process.
  - [P1] The working directory is the client cwd. Begin-build passes the workspace directory for both the working directory and the workspace directory, so a build started in a subdirectory reports the workspace root as the cwd.
  - [P2] The workspace directory is set only when a workspace exists. Zel always sets it, from the process cwd, the flag, or `/`.
  - [P2] Host is the cached local host name, or unknown. Zel uses the hostname environment variable, or empty. Those strings differ. A non-ASCII name is a Latin-1 internal string, and protobuf UTF-8-encodes those code units.
  - [P2] User is the cached user-name property, and null throws. Zel uses the user environment variable, or empty. They differ when that variable is unset or overridden. On a Latin-1 JVM a non-ASCII user name is not converted the same way.
  - [P2] The working directory and workspace are Latin-1 path strings stuffed into proto strings. Zel paths are Unicode strings. The current directory uses lossy conversion, and path fragments rewrite backslashes and drop dot segments.
  - [P1] With the started-children helper, the announced child ids match. The coarser started event announces only progress 0 and build finished. An empty target list returns before begin-build, so no build started is posted. Bazel still fires the event with an empty pattern id.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/ConvenienceSymlinksIdentifiedEvent.java`
- Bazel lines: 61
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`convenience_symlinks`)
- Verdict: partial
- Differences:
  - [P3] The event id matches: an empty convenience-symlinks id, no children, and stream conversion ignores the context and only fills that payload.
  - [P3] Java copies the supplied list in order. Zel rebuilds each entry from path, target, and a create bool. The action is create when the bool is true and delete when it is false, and the target is written only for create. A delete proto that still has a target is emitted without it. An unknown action cannot be represented. An empty target on create is omitted by both proto3 encoders. Repeated-field order is preserved.
  - [P3] A null list throws. Zel's slice cannot be null. Bazel's caller passes a non-null list.
  - [P1] Every Zel post passes an empty slice. The experimental BEP flag is never read. Bazel posts this event only when that flag is true, with the symlink protos, empty when the mode is ignore. With the default flag and mode normal, clean, or log-only, Bazel's BEP lists the links and Zel's event is empty. With the flag false, Bazel posts no event and Zel still posts an empty one after analysis.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/CriticalPathEvent.java`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel stores the aggregated critical path and returns that same reference, with no copy and no null check. Reporting posts the event, and tagging ignores tags. Zel has no such type, so nothing is delivered to a metrics subscriber.
  - [P1] That subscriber sets the build-metrics critical-path time from the aggregated elapsed nanos. A duration that does not fit throws. Zel finishes with an empty build-metrics message, so the field stays unset.
  - [P2] The payload (total duration, spawn metrics, and the component list) is never published. The nearest number is a 64-bit millisecond sum of wall time along predecessor finish times, then a max across actions. It is printed as a float divided by 1000 with two decimals and is not the event's duration or spawn-metric breakdown. A float cannot represent every integer millisecond above 2^53, and a failed action contributes zero.
  - [P1] Profile consumers walk the components in reverse and log every critical-path component. Zel logs only the single longest action span.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/ExecRequestEvent.java`
- Bazel lines: 74
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`exec_request`)
- Verdict: partial
- Differences:
  - [P1] The constructor is never called from the Zel stream. Bazel posts this event after a successful run when the script path is unset. Zel's BEP stream never contains the exec-request payload. The script path skips the event in Bazel as well.
  - [P2] Environment variables are omitted. Bazel copies every env entry's name and value bytes, in list order, including duplicate names. Zel leaves the repeated field empty.
  - [P2] The clear list is omitted. Bazel copies each byte string in order.
  - [P2] Argv is not the separate redacted list. Bazel writes the redacted argv: the residue-include flag defaults to false, so residue is stripped, and true writes the full argv. Zel writes every string in its argv slice and does not read that flag.
  - [P2] Working directory and argv bytes differ for non-ASCII. Bazel copies bytes built with ISO-8859-1: each char at or below U+00FF is one byte, and chars above that become `?`. Zel uses UTF-8, so any code point at or above U+0080 differs in the BEP bytes and in JSON base64. NUL is kept on both sides. Proto3 still omits empty bytes and a false should-exec on both.
  - [P3] When the node is built, the id and children match: an empty exec-request id, no children, and no order constraint.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/ExecutionPhaseCompleteEvent.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The event holds one signed 64-bit millisecond count, returned unchanged, with no range check, so negative values are kept. No crate defines this event or an equivalent field.
  - [P1] The metrics collector copies that value into execution-phase time. Zel posts an empty build-metrics message and never sets the field, so the published metric stays zero.
  - [P2] The remote output service uses the event as the signal to finalize the lease service. Zel has no subscriber, so that finalization does not run.
  - [P3] The nearest timing is the build outcome's elapsed duration from the start of the action runner, including the nobuild early return. That duration is not truncated to a signed millisecond count and is not published as this event.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/ExecutionProgressReceiverAvailableEvent.java`
- Bazel lines: 33
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no event type. Bazel posts it from the skyframe builder and from the first some-execution-started event so subscribers share one mutable progress receiver. Zel never posts that object.
  - [P2] The UI handler stores the receiver and forces an immediate refresh. The progress bar prints the progress string only when that reference is non-null, and it prints checking cached actions only when actions are in flight. Zel turns on execution progress inside the phase-started method and formats the bracket from its own counters. Those two Bazel signals are independent, so the bracket and the cached-actions line can appear before execution is marked started, or stay absent after it has started if this event was never posted.
  - [P2] The skymeld tracker also moves the build status from configuration to analysis-and-execution, or from analysis-complete to execution. Zel has no such transition.
  - [P3] The field is a raw nullable reference with no copy. A null post leaves the UI with no execution bracket. Zel has no optional receiver and no shared identity.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/ExecutionStartingEvent.java`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel snapshots the remaining active targets with an immutable copy: source order and duplicates are kept, a null collection or element throws, and the getter returns that unmodifiable list. Zel has no event or field that retains this collection.
  - [P2] In-tree subscribers ignore the targets but treat the post as the non-skymeld execution-start signal. Metrics increments the build count. The summary module stamps execution start from the clock, which the stats summary uses as the preparation and execution split. The execution-graph module opens the writer when the graph log is enabled. None of those run from an equivalent post.
  - [P1] The nearest hook only sets execution started and execution progress. It does not carry targets, increment a build count, record an execution-phase clock, or open an execution-graph log. The empty build-metrics message means the missing build-count increment changes published metrics.
  - [P2] Skymeld does not post this event. It uses a pending-execution event, and only the first one counts. Zel has no substitute for either path.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/MainRepoMappingComputationStartingEvent.java`
- Bazel lines: 21
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs` (`main_repo_mapping_computation_started`)
- Verdict: partial
- Differences:
  - [P2] The Java type is an empty marker posted immediately before the main-repo mapping is computed. The only subscriber ignores the instance. Zel has the two methods that copy the subscriber, and nothing calls them. The phase does not run.
  - [P2] Because it is not entered, Zel never sets build-running at this boundary. In Bazel that flag turns on date logging, progress drawing, and stdout and stderr buffering for the whole mapping computation. It also never sets the status to computing main repo mapping, never clears the additional message, and never does the immediate refresh.
  - [P2] This post is what first starts the CLI update thread. Build started does not. Without the event, a long module-resolution or repo-mapping phase has no progress line and no rate-limit catch-up thread.
  - [P2] With timestamps, the subscriber prints the current date in the JVM default zone at this boundary, after build-running is set and before the status change. Zel's method never adds the date.
  - [P2] If the Zel method were called, a phase change still skips starting the update thread when cursor control is off. Java starts that thread whenever build-running is set, including a non-curses progress terminal.
  - [P3] With skymeld, the tracker only sets the computing status. It does not assign the base status string and does not clear the additional message. The visible line is still the computing text. Zel has no skymeld tracker. A later read of the suffix still sees the previous message under skymeld and is empty on the non-skymeld path.
  - [P3] The marker has no fields, so encoding, integer width, regex, map order, and JSON do not apply. The status text is ASCII and matches when the tracker method actually runs.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/NoAnalyzeEvent.java`
- Bazel lines: 21
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] The type is an empty marker. The only subscriber appends the no-analyze abort reason. Zel has the enum and the abort-reason method, but no type and no post, so the reason is never recorded.
  - [P2] When no-analyze is the only abort reason, pending and frontier events use that reason and an empty description. Zel's finish path always overwrites its single abort slot with user-interrupted or incomplete, so those events abort as incomplete.
  - [P2] If another reason is recorded later, Bazel keeps the list: the last reason is the enum and the description lists every reason. Zel stores one pair and drops earlier reasons, so that description never includes no-analyze.
  - [P1] Callers post the event only when the analysis phase is off, after the loading-complete progress line, and may then fail the build from a loading-phase failure detail. Zel accepts the flag and never reads it, so analysis and execution still run.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/NoExecutionEvent.java`
- Bazel lines: 21
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] The type is an empty marker. The only effect of posting it is storing the no-build abort reason. Zel never records that reason.
  - [P1] Bazel posts it after a successful analysis phase when execution is off and does not run execution. Announced but unposted children, including each target-completed id, are then aborted with no-build. Zel skips actions but still emits target complete with success and output-group named sets.
  - [P2] On close Zel overwrites the abort reason with user-interrupted or incomplete, so leftover announced events are not no-build. Bazel reports the last stored reason, and if more than one was recorded sets a multiple-reasons description. A lone no-build leaves the description empty. This file has no payload.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/ProfilerStartedEvent.java`
- Bazel lines: 32
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] No Zel type is this postable. Bazel always constructs one event and posts it after profiler init. The summary module stores it and, on build complete, if the profile is non-null, stops the profiler and publishes it into the tool-log collection, as a local file or a URI future when streaming uploads are on, before BES shutdown. Zel never posts an equivalent event. Finish emits empty tool logs, finishes BEP, and only then writes the profile, so the profile is absent from tool logs and is not uploaded as a build-event artifact.
  - [P3] A null payload skips stop and publish. Zel's closest state is an optional profile path, none when generation is off or the parent directory is missing. That create error is queued and emitted only in finish, and a failed write is a later error. There is no output object that can publish a name plus a local path or a URI future.
  - [P2] Because stop happens in the subscriber, Bazel's trace omits later after-command samples. Zel marks finish and writes action-count and resource series inside finish before the write, so those samples are in the file.
  - [P3] This class does no string, integer, charset, regex, map, or JSON work. The experimental post flag is an unused no-op and is not read here. The dispatcher posts unconditionally. Zel only lists the flag.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/ReleaseReplaceableBuildEvent.java`
- Bazel lines: 32
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The class is only a build-event id. Nothing posts an equivalent signal. The event node has posted-after and completes-build only, and no replaceable flag.
  - [P2] Release looks that id up in the pending multimap, removes the single pending replaceable event, and posts that payload immediately. Other events buffered on the same id stay buffered. The boolean return makes cleanup emit aborted only for those leftovers. Zel posts aborted for every still-pending prerequisite and drops the buffered node, so a held replaceable payload is never sent.
  - [P1] Production use is the canonical structured command line. The dispatcher buffers a replaceable canonical line. The build tool then either posts a non-replaceable replacement, which discards the buffered one, or posts this release, which flushes the tentative event before analysis. Zel stores one canonical line and emits it during analysis done, or after build finished when analysis never ran. It never replaces that snapshot, so project-file option changes do not update the canonical line, and on early failure the event is ordered after build finished.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/RunBuildCompleteEvent.java`
- Bazel lines: 40
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`build_finished`)
- Verdict: partial
- Differences:
  - [P2] The Java type only forwards an exit code, a finish time, and a child-id collection, so the build-finished event has failure detail unset. The run command passes the exit code and drops any failure detail. Zel sets a failure detail whenever the default-for helper returns one, which it does for every non-success code, synthesized from stderr text.
  - [P1] The two-argument constructor announces no children. The three-argument constructor keeps the caller's collection in iteration order and does not copy it. Run uses tool logs and metrics plus the exec-request id when the script path is null. A null collection throws. Zel always announces tool logs and metrics, then posts empty payloads. The exec-request builder is never posted, so a successful run without a script path is missing that child, and a failed run still emits tool logs and metrics instead of aborting an empty child list.
  - [P2] Bazel writes both finish time and the deprecated finish-time millis. Zel writes only finish time. JSON and binary BEP omit the millis field.
  - [P2] Finish time is the caller's long: build stop time on success, the exception's finish time on a failed run, and the clock when no target is given. Zel uses millisecond time at finish. A clock before 1970 becomes zero. Java keeps a negative instant. The success-path value is also later than Bazel's build stop time because finish runs after profiler and UI work.
  - [P2] For run, Zel rewrites the parsing-failure name to build-failure while leaving numeric code 1. This event would emit the exit-code name it was given. Overall success matches for every registered exit code, because only success is code 0.
  - [P1] Run returns before any observer when the residue has no target, the pattern expands to multiple targets, the target is not executable, or run-under is empty or fails to parse. Those paths produce no build finished. Bazel posts this event at least for an empty residue, which completes the BEP stream.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/StartingAqueryDumpAfterBuildEvent.java`
- Bazel lines: 51
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no type or post for this marker. The dump-format and dump-file flags do not build or dispatch the event. Subscribers therefore never see the signal that the post-build dump posts, whether the path is local or a streaming upload. Nothing in this Bazel tree reads the getters, so the in-tree effect is delivery only.
  - [P3] The streaming constructor stores the upload context and leaves the local path null. The path constructor stores a path and leaves the streaming context null. Both references may be null. The dump name returns the Java string as stored, which may be null, with no charset conversion or path normalization. There is no equality. Tagging returns the same object, so the event is not tag-filtered. Reporting posts this object.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/TestFilteringCompleteEvent.java`
- Bazel lines: 90
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs` (`test_filtering_complete`)
- Verdict: partial
- Differences:
  - [P1] Bazel keeps four fields: a snapshot of remaining targets, a nullable list of tests to run, a snapshot of skipped tests, and the caller's configuration map by reference. Zel only assigns the total test count. The target, skip, and configuration getters have no data to read. Nothing calls the Zel method, so the total stays zero and the progress text keeps a zero denominator.
  - [P3] A null test-target list is stored and returned, and the test-provider loop is skipped. An empty collection has size zero. The UI sets the total only when the collection is non-null. Zel always overwrites the total, including with zero, and has no null state.
  - [P3] When the test-target list is non-null, Bazel throws unless every target has a test provider. Zel does not check.
  - [P3] Copying the lists throws for a null collection or a null element, and keeps iterator order and duplicates, so size counts duplicates. Zel has no list.
  - [P3] The configuration map is not copied, so a later put or remove is visible. Lookup uses map equality and throws on a null map, a missing key, or a null value. Zel has no lookup.
  - [P3] Java collection size is a 32-bit int, and the immutable list cannot exceed the maximum int. Zel stores a 64-bit count.
  - [P2] Bazel's handler refreshes immediately. Zel updates the tracker and does not refresh, so the new total is drawn only on a later refresh.
  - [P2] Bazel's UI also records an empty action set for every test target whose label is non-null. A null label still counts in the size. Zel does not insert into the test-action map from this function. A target with no test action yet is absent, so grouped test progress differs, and a null label cannot be stored.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/TestingCompleteEvent.java`
- Bazel lines: 43
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`build_finished`)
- Verdict: partial
- Differences:
  - [P2] This event always uses the exit-code constructor, so the detailed exit code stays null and build finished never sets a failure detail. Zel writes a failure detail when the option is present, and the test command always passes a default detail into finish, so a failed test build-finished carries a failure detail that Bazel omits.
  - [P1] Bazel sets the deprecated finish-time millis field to the long argument, which JSON emits as a string. Zel sets only finish time, so the millis field is absent whenever the value is non-zero.
  - [P2] The timestamp value differs. This event stores the caller's finish time, the build-phase stop, before the test summary is posted. Zel passes millisecond time at finish, after tests and console draining, so finish time is a later instant. Before the Unix epoch, that helper returns zero instead of a negative value.
  - [P2] The exit-code name is unchanged, so parsing failure stays parsing failure. Finish rewrites that name to build failure for test before build finished. The numeric code stays 1.
  - [P3] Timestamp conversion rejects a finish time outside the protobuf range. Zel still fills seconds and nanos. In-range millis, including negatives, match. Overall success matches for every constructible exit code. The event id and the tool-log then metrics children match.

### `src/main/java/com/google/devtools/build/lib/buildtool/buildevent/UpdateOptionsEvent.java`
- Bazel lines: 29
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no event that carries the updated options result. Bazel posts it only when project-file evaluation returns a non-empty build-options list, after those native and Starlark flags are parsed and before build options are created. Zel does not reparse the project file or post this event, so a listener cannot refresh options from that parser.
  - [P3] The Java type only stores the constructor argument and returns that same reference. Null is allowed, and there is no copy or equality. This tree has no subscriber, so the missing event does not change in-tree build outputs by itself. A listener would also see later mutations of that parser, and a null parser, which Zel has no equivalent value for.

### `src/main/java/com/google/devtools/build/lib/cmdline/BatchCallback.java`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Process may run many times for one callback and does not deduplicate inside a call or across calls. Zel has no batch callback. Query evaluation returns a finished node set and target-pattern reading returns a finished list, so a consumer never sees partial batches, duplicates are not delivered as streamed, and a failure cannot leave earlier batches already emitted.
  - [P3] The null callback is one shared no-op. Process ignores its argument and does not iterate it, so a lazy iterable is not forced. Pattern preparation passes that instance so evaluation only requests dependencies and produces no target list. Zel has neither this discard callback nor that sky function.
  - [P3] Process throws a checked exception that must also be a query-exception marker, and a separate interrupt. The safe callback fixes the error type to an unchecked marker, so it cannot throw a checked query exception. Interrupt is cancellation, not a query error. Zel has no callback error channel and does not split thread interrupt from query failure.
  - [P3] The callback is thread-safe, so concurrent process calls can interleave batch order. Zel has no callback with that concurrency contract.

### `src/main/java/com/google/devtools/build/lib/cmdline/BazelCompileContext.java`
- Bazel lines: 42
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs` (`label_ctx`) and `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs` (`EvalContext.file_name`)
- Verdict: partial
- Differences:
  - [P3] Bazel is a pair attached only as client data of the compilation module: a nullable label plus the parser filename. Zel has no compile-only client-data object. The label side is a package identifier plus a repository mapping, set before both resolve and execution. The name side is the eval-context file name, defaulting to BUILD.
  - [P3] The label may be null. The empty prelude never builds this object, so compile-time guards see no label and deny access. Zel's prelude path always uses the prelude label and sets the label context.
  - [P3] Bazel stores a full label. Zel stores a package identifier plus a repository mapping, which this type does not carry. The target name is dropped. The only Bazel reader uses the repository name and the package fragment, so the dropped target does not change that predicate.
  - [P2] That reader allows a predeclared only when the client data is this type and the label's repo and package match the allowlist. Zel never consults the label context when binding predeclareds, so repo-restricted names are not filtered by the file being compiled.
  - [P1] The filename is stored and not read back. Locations come from the parser input. Zel's file name is the location file and a dialect switch: late loads are allowed unless the name ends in `.bzl`, and top-level if and for bans run only for `.bzl` files when static names are on. A path that does not end in `.bzl` therefore compiles with BUILD load rules. Bazel chooses prelude versus bzl from the compile key, not from this string.
  - [P2] The filename is a Java string of the VFS path, one char per raw byte. Zel's path string is UTF-8. Non-UTF-8 path bytes cannot round-trip. The `.bzl` suffix itself is ASCII, so the suffix check matches when the path is valid Unicode.

### `src/main/java/com/google/devtools/build/lib/cmdline/BazelModuleContext.java`
- Bazel lines: 210
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs` (`BzlModule`) and `/workspace/zel-cursor/crates/zel-starlark/src/syntax/ast.rs` (`ModuleEnv`)
- Verdict: partial
- Differences:
  - [P2] Source text for the load graph and doc comments is Latin-1. Bazel always parses bzl bytes so each byte is a character in U+0000–U+00FF. Zel package loading uses lossy UTF-8 and bzlmod uses strict UTF-8. A non-ASCII load path therefore resolves to a different label, and the transitive digest changes with it. The default UTF-8 enforcement is a warning and still parses Latin-1. Error refuses the file. Zel never applies that tri-state.
  - [P1] The transitive digest is SHA-256 of the file digest, then each direct load's digest in source order, including duplicate loads, then for BUILD-dialect files the builtins digest, which also folds autoload digests. Zel hashes raw bytes and child digests, then only the exports bzl, not autoloads, and still does that when builtins injection is off. The module runtime skips builtins, but a cycle contributes an empty digest and an unreadable file contributes a native hash or a recorded hash string instead of failing the load. A bzl override stores 32 zero bytes. The Java getter returns the live byte array, so later in-place writes change every consumer.
  - [P2] Loads are the insertion-ordered map of loaded modules keyed by the raw load string, first occurrence kept. The recursive walk does not descend when the visitor returns false, so doc extract reports only the topmost unknown loads. Zel does not keep that module graph. Transitive bzl loads re-lex UTF-8 and, on lex or parse failure, drop that file's edges. It always walks the full closure.
  - [P1] Doc comments are the insertion-ordered map of global name to comments in binding order, and unused doc-comment lines are the other hash-colon comments in source order. Zel's lexer discards hash comments, so both are empty: symbol docs are missing and unused-doc-comment errors never fire. The parser path used in extract errors is not stored. Doc extract is only dependency-walked.
  - [P2] The string form is the label's canonical form, which Starlark prints as the function and module origin. Package-loaded modules never set the label string, so Zel prints a function with no origin. Bzlmod sets it only on a fresh eval.
  - [P0] Looking up the context returns null unless the client data is this type. The innermost-or-fail helper throws that the builtin can only be used during bzl initialization. The throw variant throws when there is no innermost function or no context. Zel's current label context is optional and falls through: a BUILD Label call parses canonically when the context is missing, so a canonical label in a BUILD file succeeds, and otherwise uses the BUILD package's mapping instead of the bzl package context.
  - [P1] The module key distinguishes BUILD, bzlmod, builtins, and scl loads of the same label. Zel keys a module only by canonical label and does not store the key.

### `src/main/java/com/google/devtools/build/lib/cmdline/BazelModuleKey.java`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] There is no module key. The bzl-load function name is only a name, and a sky key never returns it, so a bzl load is not a hermetic skyframe node. Dump still lists that function.
  - [P2] The interface's function name is always the hermetic bzl-load name. Zel's function name is only a static string and has no hermeticity, so restart, shareability, and function-filtered queries cannot match.
  - [P0] Loads are two string caches, not a label plus a key kind. The package loader keys by canonical form and stores both success and failure. Cache invalidation has no callers, so a changed bzl stays cached for the process. The module runtime uses another map keyed by a repo-prefixed string that is not canonical form for the main repo, and inserts only successes, so a failed module load is retried and can later succeed in the same process.
  - [P2] Bazel keys that share a label but differ by implementing class are unequal. Both caches collapse a label to one string, so those loads share one module. The prelude load also skips the cache and always records the prelude label while reading the given path, so the real prelude label is evaluated again on a normal load.
  - [P3] The fake key's function name throws, and equality is object identity. Nothing throws on a test module key or distinguishes two keys with the same label.
  - [P3] Inherited sky-key defaults have no counterpart. Cache lookup is UTF-8 string equality. A Java label is UTF-16, so an unpaired surrogate cannot be a Rust cache key.

### `src/main/java/com/google/devtools/build/lib/cmdline/IgnoredSubdirectories.java`
- Bazel lines: 211
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/lookup.rs` (`ignored_reason`, `parse_bazelignore`)
- Verdict: partial
- Differences:
  - [P3] There is no ignored-subdirectories value. Zel never prefixes, unions, filters for a directory, or checks that all paths are under a directory. Recursive package walks always recheck the full ignore list. The under-check looks only at prefixes and treats a prefix equal to the directory as not under it. Bazel uses that as a recursive package-key precondition.
  - [P2] The matching entry returns the first matching prefix or pattern. The set and the union keep insertion order and drop duplicate patterns. A recursive pattern warns that it was filtered out by an ignored directory and stops. Zel has no such warning. An ignored recursive pattern with no other packages fails as no targets found. Package-lookup text also differs: a deleted package always cites deleted packages, while Zel cites bazelignore or ignore directories in REPO.bazel.
  - [P2] Prefix match is a path-fragment segment boundary, not a string prefix. The empty fragment matches every relative path, so a bazelignore line of `.` or `foo/..` ignores the whole repo and the matching entry returns an empty string. Zel drops empty fragments, so those lines ignore nothing.
  - [P1] Absolute prefixes make construction throw. Zel's parser returns an error, and the caller uses a default, so one absolute line drops the whole file and the build continues.
  - [P2] Bazel does not trim bazelignore lines. A leading space is part of the path. A space-only line is a prefix of spaces. Zel trims first, so a spaced hash is a comment and a spaced name ignores the trimmed name.
  - [P2] Pattern match is a prefix glob on a raw slash split, not a normalized path fragment. A leading dot must be written explicitly unless the segment is exactly star or double-star. Zel's segment match has no dot rule, so a star-dot-rs pattern matches a hidden file. A question-mark pattern matches a leading dot in Zel and not in Bazel.
  - [P2] Parentheses are dropped when Bazel compiles a segment to a regex, so a parenthesized name matches the name. Zel treats them as literals. A Bazel dot does not match line terminators. Zel's star and question mark do.
  - [P2] A question mark and a regex dot are one UTF-16 code unit. Internal path strings are Latin-1 bytes, so a question mark is one byte. Zel's question mark is one Unicode scalar, and directory names go through lossy UTF-8. A UTF-8 accented character is one question mark in Zel and two bytes in Bazel. Invalid UTF-8 collapses to U+FFFD. A non-UTF-8 bazelignore fails in Bazel and is silently replaced in Zel.
  - [P0] Glob and subpackages do not descend into a directory that matches. Zel's glob walker never consults ignores, so files under an ignored directory still match.
  - [P1] Execroot planting uses the matching entry on the relative target, including REPO.bazel patterns, so a star ignore skips every top-level link. Zel only equality-checks top-level bazelignore names after stripping a leading dot-slash and a trailing slash. A dotted vendor path and a dot-dot path are ignored by Bazel and still linked by Zel.

### `src/main/java/com/google/devtools/build/lib/cmdline/Label.java`
- Bazel lines: 877
- Zel counterpart: `/workspace/zel-cursor/crates/zel-cmdline/src/lib.rs` (`Label`); Starlark surface in `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`, `/workspace/zel-cursor/crates/zel-starlark/src/types/value.rs`, and `/workspace/zel-cursor/crates/zel-bzlmod/src/rt.rs` (`resolve_label`)
- Verdict: partial
- Differences:
  - [P0] Parsing with a repo or package context forces the packages `visibility` and `conditions` into the main repo when the label has no `@repo`. Zel uses the mapping of the empty apparent name, which is the mapping owner, so `//visibility:public` and `//conditions:default` parsed from another repo stay in that repo.
  - [P0] Starlark `Label.relative` rewrites `//…` by prefixing the current repo, so those two packages are not redirected to main. A single-`@` argument is parsed as a canonical `@@` name, so the apparent name skips the caller repo mapping. Bazel resolves it through that mapping. Zel also accepts `package_relative`, which `Label` does not define.
  - [P1] An unmapped apparent repo becomes a non-visible repository whose canonical text is `@@[unknown repo 'NAME' requested from @@CTX]` plus a suggestion when a close name exists. `repo_name`, `workspace_name`, `repo_root`, and `workspace_root` then throw. Zel stores a normal visible name with no suggestion, and leaves `bazel_tools` and `platforms` unmapped-but-valid, so those fields succeed.
  - [P1] `@//pkg:name` looks up the empty apparent repo. In Bazel that is non-visible. Zel returns the owner repo. Canonical parse treats it as main only because the fallback owner is main.
  - [P2] Target validation rewrites a name that ends in `/.` by dropping that suffix. Label create and Starlark `same_package_label` keep `foo/.`. The parse path does strip it, so `//pkg:foo/.` and `same_package_label("foo/.")` disagree. Create errors also omit the suggestion that the caller may have meant `:target`.
  - [P0] The Starlark attribute `repo_root` is missing. `workspace_root` is `""` or `external/<name>` and ignores sibling repository layout (`../<name>`). `workspace_name`, `workspace_root`, and `relative` stay available when the deprecated-label-API flag is false. Bazel disables them. The default is true.
  - [P1] String conversion always uses the unambiguous canonical form (`@@//pkg:name` in the main repo). Bazel uses the canonical form without the extra `@@` when unambiguous stringification is off. The default is on. Command-line expansion is always the canonical form. Zel has no separate command-line rendering.
  - [P2] Debug print chooses the apparent repo name by sorted-map key order. Bazel inverse lookup is the first entry in insertion order, so two apparent names for one canonical repo can print differently.
  - [P0] Starlark comparison rejects labels. Bazel orders by repo name, package path, then target name with UTF-16 compare. Internal strings are Latin-1 raw bytes, so byte order matches Rust order for valid UTF-8. A non-BMP character sorts by surrogate code units in Java and after every BMP character in Rust.
  - [P2] Target-name checks walk Java code units and allow every unit in U+0080–U+FFFF, including bytes that are not UTF-8 and unpaired surrogates. Zel walks Unicode scalars and allows every non-ASCII scalar, so an ill-formed UTF-8 target cannot be represented. ASCII rules match on well-formed text. Syntax exceptions are not run through control-character sanitizing.
  - [P1] Starlark `Label()` turns a missing or non-string argument into `//:default`. Bazel requires a string. `foo:bar` fails as a colon in the target name. Bazel fails because an absolute label must begin with `@` or `//`.
  - [P3] Path-fragment conversion and the containing directory are absent. Bazel refuses the path conversion when the name is `.`. The containing directory is the package for a single normalized segment and otherwise the parent of `package/name`.
  - [P0] Module `Label()` skips label validation, rejects a bare `foo` instead of treating it as package-relative, and for a repo shorthand takes the default target as the segment after the last `+` (`@@foo+1.0` becomes target `1.0`; Bazel uses `foo+1.0`). It also does not pin `visibility` and `conditions` to the main repo.

### `src/main/java/com/google/devtools/build/lib/cmdline/LabelConstants.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: partial
- Differences:
  - [P3] There is no constants type. The ASCII literals are inlined: `external`, `modextwd`, `WORKSPACE`, `WORKSPACE.bazel`, `MODULE.bazel`, `MODULE.bazel.lock`, `REPO.bazel`, `VENDOR.bazel`, and `//command_line_option:`. Creating a path fragment of these literals does not rewrite them. `//external` and `//command_line_option` are not shared package identifiers.
  - [P2] The experimental external path prefix is `..`. With sibling repository layout, Bazel exec paths and symlink planting use `../<repo>` and will plant a real top-level `external` directory. Zel's flag text says the flag is a no-op, sibling layout is always false, and external paths always use `external/<repo>/`.
  - [P2] The external package is the main-repo package `external`. Bazel never loads `external/BUILD`: lookup returns no BUILD file before `.bazelignore`, and package loading always fails with the WORKSPACE-deprecation message. Directory walks skip the main-repo `external` directory unless sibling layout is on. Loading a Starlark file from that package fails. The symlink forest does not plant `//external`. Zel only substitutes that sentence when the path string is `external`. Lookup still returns found if `external/BUILD` exists, applies deleted packages and `.bazelignore` first, walks `external/` subdirectories, and does not reject `load("//external:…")`.
  - [P1] Native options are the prefix `//command_line_option:` or any label whose package is that package. `@//command_line_option:cpu`, `@@//command_line_option:cpu`, `//command_line_option`, and a relative `:cpu` written in that package are native options, and two spellings that canonicalize to one label are a duplicate-setting error. Zel only strips the prefix `//command_line_option:`, so those other spellings are Starlark flag keys. The prefix itself is ASCII. `//command_line_option:` fails canonical parse in Bazel. Zel stores an empty option name.
  - [P1] Runfile paths use segment prefix checks on `external` and `..`. `external/<repo>/file` becomes `../<repo>/file`, and a path that is exactly `external/<repo>` becomes `../<repo>`. The runfiles manifest then drops one leading `..` segment. Zel short paths strip the string `external/<repo>/` (the slash is required) and format `../{repo}/{rest}`, so `external/<repo>` becomes `../<repo>/external/<repo>` and an empty remainder becomes `../<repo>/`. Manifest entries strip the prefix `../`, so a bare `..` is keyed as `<workspace>/..`.
  - [P1] The directory constants are path fragments joined with `/`. Zel joins the output base with the OS path type for `external` and `modextwd`, so on Windows those paths use `\`. Bazel path strings stay `/`.
  - [P2] Path-fragment creation always turns `\` into `/` before the `external` check. A main-repo fragment `external\` is an invalid package name on Bazel. Zel collapses it to `external` and emits the `//external` deprecation error. The constant strings are ASCII, so UTF-16 versus Unicode scalars do not change them.

### `src/main/java/com/google/devtools/build/lib/cmdline/LabelParser.java`
- Bazel lines: 233
- Zel counterpart: `/workspace/zel-cursor/crates/zel-cmdline/src/lib.rs` (`Parts`, used by `TargetPattern`; duplicate `LabelParts`, used by `Label`)
- Verdict: partial
- Differences:
  - [P2] Syntax errors are not passed through control-character sanitizing. Bazel rewrites the whole message: carriage return becomes the two characters backslash and `r`, and every other character in 0–31 plus DEL becomes `<?>`. That covers package errors, target errors, repository-name validation failures, and the absolute and `...` checks. Zel interpolates the raw label, package, target, and repo. Acceptance is unchanged. The exception text differs whenever one of those strings contains those characters.
  - [P3] Splits are on ASCII (`@`, `//`, `:`, `/...`, `/.`), so a valid Unicode scalar sequence yields the same repo, package, triple-dot flag, and target, including the single `/.` strip after validation. Java then walks UTF-16 code units in the label validator. A lone surrogate in the target is allowed, because every code unit at or above 128 is always allowed. A Rust string cannot contain an unpaired surrogate, so that label cannot be parsed. Package and repo names reject non-ASCII on both sides.
  - [P3] Parsing null throws. Rust parse takes a string and has no null input. A missing repo is null versus none and is treated the same.

### `src/main/java/com/google/devtools/build/lib/cmdline/LabelSyntaxException.java`
- Bazel lines: 24
- Zel counterpart: `/workspace/zel-cursor/crates/zel-cmdline/src/lib.rs` (`LabelSyntaxError`)
- Verdict: partial
- Differences:
  - [P2] Java string form is the class name plus the message, or only the class name when the message is null. Wrappers copy that string into their own message. Zel's display is the inner string alone, so those wrapped errors drop the class-name prefix.
  - [P3] A null message is allowed. Concatenation prints the word null, and an invalid-configuration exception turns it into an empty failure detail. The Rust error is always a string. An empty string displays as empty, not null.
  - [P3] Construction records a JVM stack trace. Printing the stack and cause chains include those frames. A cause can be attached later. The Rust error has no backtrace and no source.
  - [P3] The message is a Java UTF-16 string and can hold lone surrogates. The Rust error stores Unicode scalars only. Well-formed text matches. A lone surrogate cannot be stored. Debug also quotes and escapes the text. Java string form does not.
  - [P3] Java equality is identity. The Rust error compares equal when the message text matches.

### `src/main/java/com/google/devtools/build/lib/cmdline/LabelValidator.java`
- Bazel lines: 350
- Zel counterpart: `/workspace/zel-cursor/crates/zel-cmdline/src/lib.rs` (`validator`)
- Verdict: partial
- Differences:
  - [P3] Package-name and target-name validation agree with Java on every Unicode scalar string: the same accept and reject decisions and the same error text. ASCII allowlists, non-ASCII targets, double-slash and dot-only package segments, and the target dot, slash, parent, trailing `/.`, and carriage-return rules match. Java walks UTF-16 code units and Rust walks scalars, but for well-formed text those checks only care about ASCII plus "any non-ASCII is allowed in a target and forbidden in a package."
  - [P3] An unpaired UTF-16 surrogate is a normal Java character. Target validation accepts it because the allowed range includes every unit from 128 through 65535, so a target whose only character is U+D800 is valid. Package validation rejects it. A Rust string cannot contain those units, so that target cannot be accepted.
  - [P2] Internal strings are Latin-1: one character per byte, so target byte 0xFF is U+00FF and is allowed. The Rust functions take UTF-8 scalars. Valid UTF-8 still matches. Invalid UTF-8 cannot be passed as a string. Path fragments use lossy conversion, which turns that byte into U+FFFD. U+FFFD still passes target validation, but the name is no longer U+00FF.
  - [P3] Null throws from package validation, target validation, the absolute check, and absolute-label parse. A Rust string has no null.
  - [P3] Absolute-label parse, absolute-label validation, and the package-and-target result are not in the validator. Label parts follow the label parser, not this class. `@repo//pkg:name` and `@@repo//pkg:name` drop the repo here and return `//pkg:name`. Zel keeps the repository. `@foo` with no `//` is `//:foo` here, with no repo-name check. Zel validates the repository name and builds a repo-qualified label. `@.` is `//:.` here and an invalid repository name in Zel. A non-absolute string is only an invalid label here. Label-parts parse validates the package first, so a parent-directory package reports an invalid package name instead of an invalid label. `//foo/...` is an invalid package name here, with a suggestion of `:...`. Zel strips a trailing `/...` and then reports that the package name cannot contain `...`. `//pkg:name/.` stays target `name/.` here. Label-parts validation strips a trailing `/.` to `name` after validation.

### `src/main/java/com/google/devtools/build/lib/cmdline/PackageIdentifier.java`
- Bazel lines: 373
- Zel counterpart: `/workspace/zel-cursor/crates/zel-cmdline/src/lib.rs` (`PackageIdentifier`)
- Verdict: partial
- Differences:
  - [P2] Create, create-in-main-repo, and unchecked create keep any non-null path fragment (absolute, `..`, `:`, non-ASCII, `//`). Zel create rejects absolute fragments and any path that fails package-name validation after normalization, so `..` and `C:/foo` fail while Bazel stores them. `foo/../bar` normalizes to `bar` on both paths and is accepted.
  - [P2] String form for the main repo is the package path (`foo/bar`; the root package is empty). Zel display is always the canonical form (`//foo/bar`; the root package is `//`). For a visible non-main repo both produce `@@name//pkg`.
  - [P1] Canonical form is the repository canonical form plus `//` plus the package (main repo prefix empty). Zel matches that for a visible repo. Unambiguous form uses the name-with-at, so the main repo is `@@//pkg`. A non-visible repository prints the unknown-repo form from canonical, string, and unambiguous form. Zel stores only the bare name, and the at-name is always `@@` plus that name.
  - [P2] Display form appends `//` and the package path to the main-repo display repo: `//pkg` for main, `@apparent//pkg` when the inverse mapping has an apparent name, `@@canonical//pkg` otherwise. A null mapping forces the `@@` form for every non-main repo. Zel has no package display form.
  - [P3] Package path, exec path, runfiles path, top-level directory, and source root are absent. The package path is `external/<repo>/<pkg>` or the package alone when sibling layout is requested. Exec path is the package for main, `../<repo>/<pkg>` when sibling layout is on, and `external/<repo>/<pkg>` otherwise. Runfiles path is the package for main and `../<repo>/<pkg>` otherwise. The top-level directory is empty when the package path is empty and otherwise the first segment.
  - [P3] Discovering a package from an exec path is absent. Bazel requires a relative path. With the files flag it uses the parent directory, and an empty path has a null parent and throws. The external prefix is `..` in sibling layout and `external` otherwise. A path under that prefix takes the repository name of segment 1 and the remainder as the package. Any other path is a main-repo package. The result can be a subdirectory of a real package.
  - [P2] Parse returns an identifier: a colon is an error, then the input plus a dummy target is parsed, a null repo becomes main, any other repo is created unvalidated, and the package string becomes a path fragment. `foo/...` is accepted with the suffix removed. Zel only returns success or failure and drops the repo and the stripped package. Bazel syntax errors pass through control-character sanitizing. Zel leaves those characters in the error string.
  - [P2] Equality includes repository visibility and the suggestion suffix as well as the name and the package path. Compare orders by repository name then path-fragment compare and returns 0 for a visible repo and a non-visible repo that share the name and the path. Zel orders by the bare name then UTF-8 path bytes and has no visibility fields, so those two identifiers are equal. A non-BMP character orders by UTF-16 code units in Java and by UTF-8 bytes in Rust. Bazel keeps package and repo strings as Latin-1 raw bytes. A Rust string stores Unicode scalars, so a non-UTF-8 path is a different value or cannot be stored.
  - [P3] Bazel interns instances in a weak sky-key interner and caches a 32-bit hash. Zel allocates a new value each time and uses a different hash mix, so hash-bucket order of these keys differs. Equality of two visible identifiers with the same name and the same normalized path still matches.
  - [P3] The leaf codec writes the repository and then the path fragment, and deserializes through create, which re-interns. Zel has no package-identifier codec.

### `src/main/java/com/google/devtools/build/lib/cmdline/ParallelVisitor.java`
- Bazel lines: 385
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No type implements this visitor. Query closure is a single-threaded search that returns one node set. It does not schedule visit batches, stream a callback, or stop sibling work.
  - [P3] Bazel only enqueues keys returned by the unique-visitation helper. Discovery runs once per enqueued key. Completion runs once per key in that visit after result tasks are submitted. Fail-fast leaves discovered keys unvisited and without a completion callback.
  - [P3] Pending keys are partitioned. The visit batch size must be positive, and the last partition is shorter. An empty iterable submits no tasks. Result keys are partitioned by the process-results batch size. The result task does not dedupe: every key is passed to the output function and then the callback. Those callbacks run concurrently with later visits, so batch membership and callback order follow the shared pool.
  - [P3] The scheduler drains the queue only when the task count is below the minimum or the visit queue is at least the callback size, then sleeps one millisecond. Keys enqueued during the sleep share a later batch. A negative minimum never forces a drain. A non-positive callback size makes the size check always true, so every pass drains. Shared ownership does not shut the pool down.
  - [P3] Fail-fast classifies interrupt and checked wrappers as critical with no log. Any other runtime exception is critical and logged. An error is as critical as possible and is logged. The propagated throwable is the most severe one stored. A checked exception whose runtime class is not the expected class becomes an illegal-argument exception and is critical and logged. Awaiting termination rethrows the expected exception or an interrupt from the cause. Any other runtime exception or error escapes unwrapped. The caller interrupt bit is set when the flag is set. On stop, the remaining queue is not drained.
  - [P3] An interrupt during sleep sets the interrupted flag and interrupts other worker threads. In-flight visits or callbacks can therefore be cut off, and the visited set depends on that race. The more-work check reads the task count before the queue so a task cannot enqueue and finish between the two checks and be dropped.

### `src/main/java/com/google/devtools/build/lib/cmdline/QueryExceptionMarkerInterface.java`
- Bazel lines: 39
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The interface has no methods. The only runtime type is a nested runtime exception with a null message and cause and a stack trace filled on construction. Nothing in the tree constructs it. It exists so callers can pass that class versus the query-exception class. Zel has no marker type and no class token.
  - [P3] Async pattern evaluation and directory search catch an exception and keep it only when the class token matches. Any other exception, including a query exception when the token is the marker, is rethrown as an illegal state. The visitor task rethrows every runtime exception before that test, then checks the class and wraps only a matching checked exception. A checked exception of the wrong class becomes an illegal-argument exception whose message includes the class name, the exception message, and the full stack trace. Because the marker is unchecked, it would skip that check if it were thrown. A query exception is checked, so it is subject to the check and does not match the marker class.
  - [P2] Zel reports those failures as a pattern error or a query error and never selects a crash-versus-query-failure path from an exception class token. A mismatch Bazel aborts as an illegal state or illegal argument is a returned error in Zel, so the failure text, exit status, and whether evaluation continues can differ. This file has no encoding, integer, regex, map-order, or JSON behavior.

### `src/main/java/com/google/devtools/build/lib/cmdline/RepositoryMapping.java`
- Bazel lines: 171
- Zel counterpart: `/workspace/zel-cursor/crates/zel-cmdline/src/lib.rs` (`RepositoryMapping`)
- Verdict: partial
- Differences:
  - [P0] A miss returns an unvalidated name made non-visible, with the context repo and a suggestion. Equality includes the context repo and the suggestion, and the at-name is an unknown-repo form. Zel returns a visible repository whose name is the apparent string, so later label canonical forms and fetches treat it as a real repository. The Starlark Label builtin rewrites some misses outside this type. The mapping get does not.
  - [P1] The suggestion walks entries in insertion order, keeps the first strictly closer candidate, caps distance from the UTF-16 length, and lowercases with the locale default, so Turkish I differs from Rust Unicode lowercase. Zel appends no suffix.
  - [P1] An empty-string get is a normal map lookup. The empty mapping and the builtins mapping both store empty to main, so a double-slash label in builtins still resolves to the main repo and a miss says it was requested from the builtins repo. Zel returns the owner repo before looking at entries, so an explicit empty entry is ignored and a missing empty key still resolves to the owner.
  - [P3] The empty mapping's entries contain empty to main. The always-fallback mapping is an empty hash map with a default main owner. Empty-string get matches only for that empty main-owner case.
  - [P2] Inverse lookup is absent. Bazel returns the first apparent name in insertion order whose value equals the canonical name. The cached inverse keeps that same first key. Zel has no inverse. Entries follow the recorded vector only when the ordered constructor stored exactly as many unique keys as the map. Otherwise iteration is hash order, which is not insertion order and is not stable, so the apparent name chosen when several names share one canonical repo changes.
  - [P3] Additional mappings are absent. Bazel inserts the additional map, then the existing map, and keeps the last, so existing keys win. A null argument becomes an empty map. Zel has no merge on this type.
  - [P2] Equality is order-independent map equality plus the context repo. Zel also compares the order vector, so the same pairs in a different recorded order are unequal. The hash is a cached 32-bit mix, and a known-hash constructor trusts the caller, so a stale hash splits buckets while equality stays true. Zel implements neither a hash nor a separate hash input.
  - [P2] Duplicate apparent names in the ordered constructor keep the last value, the order length no longer matches, and iteration falls back to hash order. Bazel copy rejects duplicate keys. Only last-wins keeps the last value, still in builder order.

### `src/main/java/com/google/devtools/build/lib/cmdline/RepositoryName.java`
- Bazel lines: 406
- Zel counterpart: `/workspace/zel-cursor/crates/zel-cmdline/src/lib.rs` (`RepositoryName`)
- Verdict: partial
- Differences:
  - [P0] There is no non-visible repo. Bazel keeps the original name plus the context repo and a suggestion suffix. The at-name, string form, and canonical form become `@@[unknown repo '<name>' requested from <context><suffix>]`, with a did-you-mean suffix when the spell checker has one, and a nested unknown-repo form when the context repo is itself non-visible. Equality includes those fields, so the same name from two contexts is not equal, and main is false even when the name is empty. Label repo name and workspace root throw before using the name. Zel mapping get returns an unvalidated visible repo whose canonical name is the apparent string, so `@missing//pkg:t` is `@@missing//pkg:t` and compares equal to a real `missing` repo. The Starlark Label path instead stores the bracket text as the name: no suggestion, name and exec paths use that bracket text, and workspace root returns it instead of failing.
  - [P1] Unvalidated create does not validate. Bazel's cache loader runs validation, so `.`, `..`, and anything outside the repo charset throw. The empty name and `_builtins` return the main and builtins instances. Zel stores any string.
  - [P2] Display is the canonical form, so the main repo prints as empty. Bazel string form is the at-name, which is `@@` for the main repo. The at-name helper matches only for visible repos.
  - [P2] Validation acceptance matches for Unicode scalars: Java word characters are ASCII, and the matcher does not accept a trailing newline. The error text does not. Bazel sanitizes control characters. Zel embeds the raw characters. The same gap is in the copies of user-provided repo-name validation. The module-name pattern is not on the Rust type. Those copies accept the same ASCII names. Unpaired UTF-16 surrogates exist only in Java and fail the pattern. A Rust string cannot hold them.
  - [P2] Display form is missing. Bazel's inverse is the first inserted apparent name and, if the repo is non-visible, skips the inverse and returns the unknown-repo form. Zel's shorthand display walks a sorted map, so the alphabetically first apparent name wins when several map to one canonical repo.
  - [P2] Exec path is missing. Bazel returns empty for main, `../<name>` when sibling repository layout is on, and `external/<name>` otherwise. Zel workspace root and artifact exec paths always use `external/<name>`. Sibling layout is hard-coded false.
  - [P3] Creating a name from a path fragment is missing. Bazel returns null unless the path has more than one segment and starts with `external` (or `..` in sibling layout). Otherwise it creates segment 1 and returns the remainder, or null if that segment is not a valid repo name.
  - [P3] There is no leaf codec. Bazel serializes the name, context repo, and suggestion and deserializes with the private constructor, which skips validation and can restore a non-visible or otherwise illegal name. The hash is a cached 32-bit mix of those three fields. Rust hash and order are the bare UTF-8 name.

### `src/main/java/com/google/devtools/build/lib/cmdline/ResolvedTargets.java`
- Bazel lines: 184
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/resolve.rs` (`ResolveOutcome`)
- Verdict: partial
- Differences:
  - [P1] Bazel keeps two sets. Add inserts into targets and takes the element out of filtered targets. Remove inserts into filtered targets even when the element was never in targets. Merge applies the other filtered set first and the other targets second, so an element listed in both ends in targets. Filter walks only the current targets, in their old order. Empty ignores filtered targets. Zel's outcome is one vector. A negative pattern only retains that vector, so a later check cannot tell "removed by a filter" from "never requested". Bazel's "all specified test targets were excluded" is empty targets with a non-empty filtered set. Zel emits that text when the pre-analysis list still contains a test rule and the configured test count is 0.
  - [P2] Has-error is a sticky boolean, independent of the sets. Set-error, merge, and merge-error only turn it on. Failed is the shared empty instance with the bit set. Empty is the shared empty instance with the bit clear. A result can hold targets and an error together. Zel records an error vector and has no separate bit. When keep-going is off, loading deletes targets whose package contains errors and fails the command.
  - [P2] Set membership is equality. Target and rule do not override equals, so membership is reference identity. Label equality is package identifier plus name, using Java string equality. Zel deduplicates and subtracts with label equality on the same two fields. Two cloned targets with one label collapse to the first. Re-adding an element already present does not move it.
  - [P2] The default builder is insertion-ordered, and build copies that order. Wildcard order is the package map order: Bazel sorts target names by UTF-16 code units. Zel sorts by UTF-8 bytes. Those orders match for BMP names. A supplementary character sorts with the surrogates in Bazel and after U+FFFF in Zel, so `//pkg:all` result order differs. The concurrent builder is unordered, rejects null, and its error-bit update is a non-atomic read-modify-write. Nothing in this tree calls it. Zel has no concurrent builder.
  - [P3] A builder accepts one null. Copying into an immutable set throws on that null. Zel's target is never null.
  - [P3] String form lists targets, filtered targets, and the error bit in encounter order. The outcome debug form is targets, errors, and events.

### `src/main/java/com/google/devtools/build/lib/cmdline/StarlarkThreadContext.java`
- Bazel lines: 74
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs` (`EvalContext.main_repo_mapping`)
- Verdict: partial
- Differences:
  - [P3] Storing the context installs it in the Starlark thread-local and throws if that slot is already set, so a second context aborts evaluation. Zel has no thread-local map. The main-repo mapping is an ordinary field that can be overwritten, and there is no shared slot that BUILD, bzl, analysis, repo-rule, and module contexts all occupy.
  - [P3] Getting the mapping calls the stored supplier on every read and propagates an interrupt. Label debug-print swallows that and prints the canonical form. Zel snapshots the map once at context creation and never re-fetches, so a mapping that appears or changes after construction is invisible, and an interrupt during the lookup cannot force the canonical form.
  - [P1] A null supplier, or no thread context, makes Bazel debug-print canonical labels. Zel's none does the same, but the field is set only in the module runtime. Package, bzl, analysis, and repository-rule evaluation leave it unset, so printing a label there stays canonical where Bazel's stored supplier would print the apparent name.
  - [P2] The supplier returns a mapping whose inverse is the first apparent name in insertion order. Zel copies into a sorted map and finds the first entry. When two apparent names map to one canonical repo, the printed name differs.
  - [P2] Bazel's mapping carries the context repo and repository identity, including non-visible repos. Display form throws unless the context repo is main, and a non-visible repo prints an unknown-repo form without inverse lookup. Zel stores only apparent-to-canonical strings and compares the strings, so a non-main mapping is inverted instead of rejected, and a name that only matches as a string can be shown as apparent.

### `src/main/java/com/google/devtools/build/lib/cmdline/TargetParsingException.java`
- Bazel lines: 79
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No type stores a detailed exit code on a target-pattern failure. A label syntax error is a message string. A pattern error stores the pattern, the message, and optional package-error info. Neither keeps a cause or a failure-detail code.
  - [P1] The code constructors build a failure detail and take the exit code from that code. Most codes are exit 1. The pattern-file codes are exit 2. The unknown code is exit 37. Zel records a target-pattern parse failure only when stderr contains the parsing-failed sentence. The other codes are not preserved. A pattern failure that does not print that warning is classified as a non-action execution failure.
  - [P2] The detailed-exit constructors store the supplied detail unchanged, so its message and category can differ from the exception message. Zel has no slot for a caller-supplied detail. The default uses the first error line, which for a failed pattern is the skipping line rather than the exception message.
  - [P3] A null message or a null detailed exit code throws before this exception exists. A Rust string cannot be null, so that input cannot take the null path and never becomes a failure detail.
  - [P2] The filesystem constructor sets the message to the cause message with no null check, chains the cause, and sets a transient inconsistent-filesystem error, exit 36. A null cause message throws instead of returning this exception. Zel has no such exception. An I/O failure during pattern resolution stays a string and, when the warning line is present, is exit 1.
  - [P2] The constructors that take a cause keep it. Skyframe uses that cause and marks the filesystem wrappers catastrophic. A pattern error drops the cause, so transience and catastrophic versus ordinary cannot follow the wrapped exception.
  - [P3] The message is a Java UTF-16 string. Protobuf serialization encodes it as UTF-8 and replaces unpaired surrogates. A Rust string cannot hold unpaired surrogates, so a message that is legal in Java but not UTF-8 does not round-trip the same way.

### `src/main/java/com/google/devtools/build/lib/cmdline/TargetPattern.java`
- Bazel lines: 951
- Zel counterpart: `/workspace/zel-cursor/crates/zel-cmdline/src/lib.rs` (`TargetPattern::parse_with`); eval in `/workspace/zel-cursor/crates/zel-packages/src/resolve.rs`
- Verdict: partial
- Differences:
  - [P0] Unmapped apparent repos stay visible under the apparent name. The mapping miss does not become a non-visible repository and does not add a suggestion. `@missing//pkg:t` can resolve as repo `missing` instead of the unknown-repo form. `@//pkg:t` is always the mapping owner. Bazel returns that entry only when the empty name is mapped and otherwise a non-visible empty name, which is not the main repo.
  - [P2] The parser rejects a non-main current repo with a non-empty relative directory. Zel still joins that directory onto the external repo.
  - [P2] Package-identifier create does not re-check the fragment. After joining the relative directory, Zel rejects a leading `..` or an absolute fragment, so `:bar` from offset `..` fails in Zel and is package `..` in Bazel. An absolute offset makes the path-as-target interpretation throw in normalize. Zel stores that path.
  - [P1] On Unix, backslash is not a separator. Zel path-fragment creation rewrites backslash to slash, so a relative directory containing a backslash changes every relative pattern and every path-as-target.
  - [P2] Path-as-target failures use the normalized workspace path and a failure-detail code. Zel uses the raw pattern, with no failure-detail code. A path that normalizes to the root package makes Bazel label construction throw an invalid-target-name error after control-character sanitizing. Zel stops on an empty name and reports the filename error instead.
  - [P2] Label-syntax text is copied into the target-parsing exception with a syntax-error code after sanitizing. Zel keeps the raw controls and has no target-pattern code.
  - [P2] Wildcard conflict uses the processed suffix (`//pkg:all/.` is `:all` and looks up target `all`) and only when the package is absolute. Zel classifies the stripped name as `:all` but looks up the raw suffix `all/.`, so an existing target `all` is used by Bazel and the rules wildcard is expanded by Zel.
  - [P1] Targets-below-directory warns that the pattern was filtered out by an ignored directory and returns no targets when an ignore entry hits that directory, and it passes excluded subdirectories into the walk. Zel has no such early success, emits no such warning, and does not apply an exclusion set. An ignored or later-subtracted tree becomes no targets found or a package error on the positive pattern.
  - [P2] Absolutize is absent. Absolutize-then-parse turns offset `foo` plus `bar` into `//foo/bar`. Direct parse, which Zel implements, leaves `bar` a path-as-target `foo/bar`.
  - [P2] Wildcard target order follows each language's string order, so star, all, and recursive patterns differ when names contain supplementary characters.

### `src/main/java/com/google/devtools/build/lib/cmdline/TargetPatternResolver.java`
- Bazel lines: 162
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/resolve.rs` (`PatternResolver`)
- Verdict: partial
- Differences:
  - [P3] The only executable body runs the directory search on the calling thread, ignores the executor, and returns a completed future. Zel has no future, no exception-class test, and no cancelled-versus-failed split. Expand and find-packages-below return a result string.
  - [P2] A target-parsing exception becomes a failed future whose cause is that exception, so the detailed exit code stays available. Zel keeps only the message string, so a caller that classifies the exit from the failure detail cannot do so.
  - [P1] An interrupt becomes an immediately cancelled future: the exception is discarded, get throws cancellation with no cause, and the interrupt flag is not restored. Zel has no interrupt or cancellation. The directory walk runs to completion and can still return targets.
  - [P1] A process-package-directory exception is thrown immediately as an illegal state, not stored in the future. The message concatenates the directory and the original pattern. The filesystem-error exit code is no longer the thrown type. Zel treats a directory-read failure as stop-this-directory and a stat failure as continue, so the same inconsistency can yield a partial package set or "no targets found" instead of that crash.
  - [P3] Any other exception for which the exception class matches becomes a failed future. Every remaining exception, including runtime exceptions, is rethrown as a new illegal state before a future is returned. Errors are not caught. Zel records expected pattern failures and continues to the next pattern. An unexpected failure panics.
  - [P0] The sync call delivers targets through the batch callback before this method maps the outcome, and nothing rolls those batches back if the future is then failed or cancelled. Zel's expand for targets-below-directory keeps every collected target only when every package succeeds. The first package failure drops the output.

### `src/main/java/com/google/devtools/build/lib/outputfilter/AutoOutputFilter.java`
- Bazel lines: 140
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P1] None, all, packages, and subpackages never build or install an output filter. All still shows action output. Packages and subpackages do not limit it to command-line packages. The reporter has no output filter. The output-filter flag is a different flag, an owner substring, and does not implement this enum.
  - [P1] None is show everything and all is show nothing. The default none only looks the same because nothing is filtered. All is a real result change: Bazel drops tagged non-error output, including system-owner actions.
  - [P2] Packages compiles a pattern of unknown, the internal platform, and a sorted package alternative that requires a colon immediately after the package. Subpackages also matches a slash. The filter uses find on the Unicode-converted tag. The caret binds only to the package alternative, so unknown and the internal platform match as unanchored substrings.
  - [P2] Packages are inserted in UTF-16 order. A name starting with java or javatests is rewritten so those two packages become one alternative. Packages with no slash are left unchanged. Zel does not rewrite or sort packages into a regex.
  - [P2] Subpackages drops a package only when it starts with the immediately previous kept package plus a slash. A name that sorts between a parent and a child keeps the child. That order is UTF-16 code-unit order. A UTF-8 sort differs for supplementary characters and can change which packages are collapsed.
  - [P3] The package regex joins with a bar and does not escape metacharacters. A package containing regex syntax changes matches, and an invalid fragment makes compilation throw. An empty target list becomes an empty group.
  - [P2] The pattern is built from internal package strings, while the matched tag is converted from Latin-1 bytes as UTF-8. The pattern is not in Unicode character class. A UTF-8 regex on the same labels would disagree on non-ASCII tags even if the filter were ported.
  - [P2] Flag parsing matches the enum converter for these ASCII constants: case-insensitive, and an invalid value names the four choices. Zel keeps the raw spelling and never maps it to a filter.

### `src/main/java/com/google/devtools/build/lib/outputfilter/OutputFilteringModule.java`
- Bazel lines: 103
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`output_filter`)
- Verdict: partial
- Differences:
  - [P1] With the output filter unset, Bazel stores the auto filter and, when target parsing completes, installs the filter for the labels before subtraction. None shows everything. All hides every tagged non-error, non-debug event and successful action stdout and stderr. Packages and subpackages compile a regex from a sorted package set that rewrites java and javatests and drops a package when the previous sorted name is a path prefix. Package names are not regex-escaped, so a name such as foo-plus throws during that event and fails the build. Zel parses the four modes and never reads the value, so those modes leave warnings and action output unchanged and never fail on a package name.
  - [P1] An explicit filter is compiled with dot-all and find on the Unicode-converted tag, with the tag being the label print. That filter is installed at build start, so later tagged warnings are dropped, and successful action stdout and stderr is dumped only when it matches. Test-runner actions that show output unconditionally still print test logs. Zel keeps the raw string and, only after a successful action, clears stdout and stderr when the owner string does not contain that string. Analysis warnings stay, and test logs are cleared with every other action.
  - [P1] Find plus dot-all treats anchors, alternation, and inline flags as a Java regex. A dot is one UTF-16 code unit and also matches newlines. Zel searches for those characters as a literal substring. An empty pattern matches every tag in both. A pattern that is valid Java and not a literal therefore shows a different set of targets. Both reject a pattern that does not compile.
  - [P2] Bazel re-reads the pattern and the tag from Latin-1 internal bytes as UTF-8 before the UTF-16 match. Zel compares the flag's UTF-8 bytes to the UTF-8 label, so a non-UTF-8 package name, or a dot or Unicode escape against a supplementary character, can match on one side only.
  - [P2] When both flags are set, Bazel keeps only the explicit regex. Zel also ignores auto in that case and applies the literal contains check.

### `src/main/java/com/google/devtools/build/lib/runtime/AfterCommandEvent.java`
- Bazel lines: 21
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No empty event is posted after every command, regardless of which command ran or how it terminated, for listeners to run final cleanup.

### `src/main/java/com/google/devtools/build/lib/runtime/AggregatingTestListener.java`
- Bazel lines: 347
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel registers one aggregator per test when filtering finishes, skipping aliases, and under Skymeld one aggregator shared by an alias and its actual target. Zel only filters tests while scheduling them and does not skip or collapse aliases.
  - [P1] Each finished test action updates that target immediately and can publish its summary as soon as the target is complete. Zel runs shards, repeats, and flaky attempts inside one execution and reports the target only after the test phase.
  - [P2] After the build, targets that are neither successful nor skipped become `BLAZE_HALTED_BEFORE_TESTING` if the build was a catastrophe or was interrupted, `NO_STATUS` under `--check_tests_up_to_date` or stop-on-first-failure, otherwise `FAILED_TO_BUILD`; skipped targets become `NO_STATUS`. Zel marks targets already in the failed set `FAILED_TO_BUILD` (`NO_STATUS` only with `--check_tests_up_to_date`) and tests never started after `--notest_keep_going` as `NO_STATUS`. It never records `BLAZE_HALTED_BEFORE_TESTING` and does not apply the build’s skip set.
  - [P2] An analysis failure or a failed target completion marks the target unbuilt, and a later failure is ignored once a summary was posted. Zel does not rewrite a test from those events, and it does not ignore a second failure after the summary exists.
  - [P2] The final report orders summaries by summary order, copies the real target’s summary onto an alias, and for validation failures prints `NO_STATUS` when stopping on the first failure, otherwise the status aggregated with `FAILED_TO_BUILD`. Zel sorts by cache, uncached run count, status, then label, and does not rewrite alias or validation-failed targets.
  - [P0] The result is the most important per-test system failure, otherwise success when every non-skipped target passed, otherwise `TESTS_FAILED`. Zel returns `BUILD_FAILURE` for `FAILED_TO_BUILD`, a failed coverage report, or any `NO_STATUS` under `--check_tests_up_to_date`, and `TESTS_FAILED` for other non-passing statuses. It does not prefer a per-test system failure, and skipped targets are not removed from the pass comparison.
  - [P3] A duplicate aggregator at filter time, a result with no aggregator, or a summary count that does not equal the target count is a hard error. Zel does not enforce those. A null actual-target or successful-target collection also skips build-complete handling, which Zel has no equivalent for.

### `src/main/java/com/google/devtools/build/lib/runtime/BlazeCommand.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P1] There is no post-parse `editOptions` step after rc-file and command-line options are parsed. The method here changes nothing unless a command overrides it. Zel injects command-required flags by rewriting argv before parsing (`coverage` prepends `--collect_code_coverage`), so a later explicit flag wins. Bazel parses that flag at `SOFTWARE_REQUIREMENT` priority, which outranks the command line.
  - [P2] A command returns only an exit code. Server shutdown and a client-side exec are separate dispatcher outputs, wired for `shutdown`/`clean` and `run`. Any command result can carry either request. `mobile-install` does not return an exec request for the deployer.

### `src/main/java/com/google/devtools/build/lib/runtime/BlazeCommandDispatcher.java`
- Bazel lines: 1091
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P2] The exclusive command lock lives in `/workspace/zel-cursor/crates/zel-server/src/daemon.rs` (`run_locked`), not in `dispatch_command`. An in-process `dispatch_command` call only mutexes build, test, coverage, run, query, cquery, and aquery. Bazel locks every `exec`.
  - [P2] The lock wait is not passed into command setup. Bazel records `waitTimeInMs` (0 if the lock is free on the first try) and gives it to `CommandEnvironment`. An empty client description is rewritten to `unknown`, and the wait line always includes `(server_pid=…)`. Bazel keeps the given description and omits the pid suffix when the pid is unknown (`-1`). Cancel while waiting returns a finished `INTERRUPTED` result (`Build interrupted`). Bazel surfaces `InterruptedException` from `commandLock.wait`.
  - [P3] There is no shutdown latch. After a command that shuts the server down, Bazel stores a reason and later `exec` calls print it and return `PREVIOUSLY_SHUTDOWN` without running the command (`crash` versus `explicitly by client …`). Zel sets `termination_expected` / `shutdown_requested` and the daemon process exits. It does not reject a later command with that reason.
  - [P1] There is no remote-cache eviction retry around the command. Bazel reruns on `RemoteCacheTransientErrorException`, keeps one build request id, prints `Found transient remote cache error, retrying the build...`, and stops once attempted ids reach `remoteRetryOnTransientCacheError`.
  - [P1] `execExclusively`'s per-command setup is not a dispatcher stage. Missing as dispatcher behavior: cached options data and a first parse round when the command analyzes and `MODULE.bazel` exists; profiler start/stop tied to `enable_tracer` (YES, or AUTO for query, analyzing commands, or a profile path), the profile convenience link, memory-profiler stop, stdout/stderr flush, and the timestamp-granularity wait; `--starlark_cpu_profile` start at 10ms and stop, with the Starlark CPU profile init and write failure codes; `BlazeModule.beforeCommand` for every module with exit-code merge, and `afterCommand` in a finally; an uncaught throwable becomes a keep-alive crash shutdown. A daemon panic becomes `BLAZE_INTERNAL_ERROR` / `COMMAND_FAILURE_UNKNOWN` and the server stays up.
  - [P2] An early option failure does not replay stored events, post `NoBuildEvent`, and print only the rc notes that mention the bad option. Parse errors are printed directly by the command handler.
  - [P1] Stdout and stderr are not wrapped in the dispatcher: no tee to output listeners, no ANSI strip when color is off (with `binaryStdOut` / `binaryStdErr` exempt), no buffering of non-binary streams, and no second ANSI-allowing UI handler for later output such as `run`.
  - [P3] On Linux, Bazel warns when `sun.jnu.encoding` is neither UTF-8 nor ISO-8859-1. Zel does not.
  - [P1] Analyzing commands do not, inside the dispatcher, sync package loading, read `incompatible_remove_ctx_py_fragment` and `incompatible_remove_ctx_bazel_py_fragment`, compute the main repo mapping, inject module `--flag_alias` flags, re-parse options with that mapping, then parse Starlark options, posting `NoBuildEvent` if sync or mapping is interrupted. Zel resolves the module graph up front for a fixed command set and folds flag aliases into analysis configuration.
  - [P1] The dispatcher does not post the unstructured, original, and canonical command-line events or `toolCommandLine`. Bazel redacts the unstructured line for `run` when residue is excluded from BEP, and marks the canonical event replaceable only when the command analyzes.
  - [P2] `--announce_rc` startup-option provenance is printed by the client before the RPC (`/workspace/zel-cursor/crates/zel-client/src/lib.rs`), including when option parsing will fail. Bazel emits those notes from the dispatcher only after early exit has succeeded.

### `src/main/java/com/google/devtools/build/lib/runtime/BlazeCommandResult.java`
- Bazel lines: 159
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No command-result value requires a detailed exit code and carries an optional exec request, a shutdown flag, response extensions, and idle tasks, including success, failure-detail, crash-shutdown, and exec factories that reject a null exec request and can pair an exec request with a non-success code.

### `src/main/java/com/google/devtools/build/lib/runtime/BlazeCommandUtils.java`
- Bazel lines: 174
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/help.rs`
- Verdict: partial
- Differences:
  - [P3] Startup, common, and per-command option classes are fixed tables in `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` (`ALL_STARTUP_OPTIONS`, `COMMON_OPTION_CLASSES`, `COMMAND_OPTION_CLASSES`, `FRAGMENT_OPTION_CLASSES`) for the stock Bazel modules. Caller-supplied `OptionsSupplier`s and a `ConfiguredRuleClassProvider` are not unioned in, and a command class with no `@Command` does not raise `IllegalStateException`.
  - [P2] `getOptions` returns a deduplicated set of options classes: common classes, `@Command.options()`, fragment classes when `usesConfigurationOptions()` is true, each supplier's `getCommandOptions(name)`, then the same walk for every `inheritsOptionsFrom` base. Zel `command_all_options` returns option definitions whose `commands` list contains that command, sorted by name. `get_command_allowed_classes` returns `None`.
  - [P3] `expandHelpTopic` loads a `resource:` template with `ResourceFileLoader` relative to the command class and throws `IllegalStateException` on I/O failure or when the text does not contain `%{options}`. `expand_help_topic` substitutes `%{product}`, `%{command}`, and `%{options}` in an already-loaded template, trims, appends `\n\n`, and on medium verbosity appends the `help --long` / `--short` hint. `command_template` embeds known command texts at compile time and uses `%{options}` for any other name.
  - [P3] `getUsage` reads `@Command.name()` and `@Command.help()` and describes the full `getOptions` class set (`describeOptions` drops undocumented options). `command_usage` uses the baked template and documented definitions only, and it also renders the non-command topic `startup_options` from `ALL_STARTUP_OPTIONS` and `startup_options.txt`.
  - [P2] With a template that already contains `%{options}`, product substitution inside the options block happens first, then `%{product}`, `%{command}`, and `%{options}` in the template, then trim and the medium-verbosity footer.

### `src/main/java/com/google/devtools/build/lib/runtime/BlazeModule.java`
- Bazel lines: 500
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No module extension point: empty defaults for startup, workspace, command, analysis, and execution hooks, singleton-or-null service providers, `ModuleFileSystem`, and `ModuleEnvironment` are absent.

### `src/main/java/com/google/devtools/build/lib/runtime/BlazeOptionHandler.java`
- Bazel lines: 775
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/rc.rs`
- Verdict: partial
- Differences:
  - [P2] A workspace-only command outside a workspace fails with `The '<command>' command is only supported from within a workspace (below a directory having a MODULE.bazel file).` Zel reports `Bazel '<command>' must be invoked from within a workspace` (or a capitalized `--product_name`). If the workspace's parent contains `DO_NOT_BUILD_HERE`, Bazel refuses with `<product> should not be called from a <product> output directory` and the path stored in that file; Zel does not.
  - [P2] Rc `--flag_alias` is rejected unless the rc command is exactly `build`, `common`, or `always` (`build:foo` and `test` included). Zel accepts it. An rc command named `sync` is warned and dropped by Bazel and kept by Zel.
  - [P2] `--announce_rc` notes follow rc-chunk order, and a chunk whose source is `client` is `Options provided by the client`. Zel merges every option from the same path into one note and prints those notes in path order.
  - [P2] A leading `+<project file>` on an analyzing command is read (only one such argument, and only with `--experimental_allow_project_files`) and its options are parsed at rc priority. Zel treats that token as a target.
  - [P0] After rc and command-line parsing, Bazel runs `editOptions` before invocation policy: `--test_output=streamed` forces `--test_sharding_strategy=disabled` and `--test_strategy=exclusive` at software-requirement priority; `coverage` forces `--collect_code_coverage` at that priority and sets `--test_timeout=300,600,1200,3600` as a computed default; `cquery` forces `--nobuild`, `--build_manual_tests`, `--noexpand_test_suites`, and `--nobuild_tests_only` (and `--output=transitions` / `--include_config_fragments_provider` when those flags require them); `aquery` forces `--nobuild`; `mobile-install` injects its aspect and output groups. Zel only prepends `--collect_code_coverage` at command-line priority (so `--nocollect_code_coverage` disables it), doubles the resolved test timeout under coverage, and applies streamed exclusivity and disabled sharding only while running tests.
  - [P1] The module invocation policy is merged first and the client policy last, so policy overrides rc, command-line, and `editOptions` values. Zel forwards startup `--invocation_policy` on the run request and does not enforce it on the command, and a malformed policy is not rejected there.
  - [P0] Starlark flags are parsed only for commands that analyze: the label must load as exactly one target, non-flags are unrecognized, values are type-checked, and a non-boolean flag without `=` whose next token is not an option is an error ending in `Did you mean <flag>=<value>?`. Zel records any `--//` or `--@` flag on every command, stores a missing value as `true`, and only resolves the label later while building a configuration.

### `src/main/java/com/google/devtools/build/lib/runtime/BlazeRuntime.java`
- Bazel lines: 1948
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P1] Zel starts one persistent server from `--workspace_directory`, `--output_base`, `--install_base`, `--output_user_root`, `--port`, and the request/response cookies, then binds `127.0.0.1`. Bazel also has a `--batch` process that runs a single command and then exits after forcibly destroying child processes, and it transcodes argv into its internal encoding before either path.
  - [P1] Bazel rejects a non-absolute `--install_base`, `--output_base`, `--output_user_root`, or `--failure_detail_out`, takes a shared install-base lock when locking is on, links JNI and prints a reinstall hint on failure, and publishes an abrupt-exit status file and a failure-detail file. Zel skips those checks and side effects.
  - [P3] Bazel builds the runtime from modules and services: one filesystem, optional artifact filesystem, startup and server hooks, rule classes, package settings/validator/overhead/loading listener, one project-file provider, one query helper, module invocation policy, annotation-registered commands that reject a duplicate name, and at most one metrics module. Zel always uses the local filesystem, a built-in command match, and a process-wide product name that defaults to `bazel` and is not lowercased.
  - [P2] `dispatch_command` treats `--help`, `-help`, and `-h` as `help`, parses `--client_env` only before `--` (otherwise the process environment), and returns command id `cmd-0`. An unknown command prints both `Command '<name>' not found.` and `Try '<product> help'.`. A bad `--digest_function` startup option exits with code 2 before the command runs. The gRPC loop replaces the id with `<pid>-<n>`.
  - [P2] The daemon runs one command at a time: `block_for_lock` waits and reports the other client, otherwise it returns “Another command (…) is running” with the lock-held code. Cancel, stream reset, or a dropped client interrupts the command; a panic becomes an internal error. Batch mode’s non-blocking lock, invocation-policy dispatch, and in-process spawn of an `ExecRequest` (inherited stdio, cleared and set environment, child status) are absent; `run` only returns the exec request.
  - [P2] The server writes `server.pid.txt` from its own pid and stops on SIGTERM, idle timeout, low free memory (under 5% after 10s idle), shutdown, or a missing pid file while idle. Bazel reads a launcher-written pid (a missing or corrupt file is a filesystem abrupt exit), interrupts the RPC server on SIGINT, and in batch mode a second interrupt restores the default signal handler while a watcher warns every 10s.
  - [P2] Idle work saves the action cache once after 10s when idle-server tasks are enabled. Bazel’s per-command idle tasks are GC/interner shrinking, install-base garbage collection, and action-cache garbage collection from max age, delay, and threshold, plus an optional memory profile.
  - [P1] `--nokeep_state_after_build` does not reset the in-memory graph. Zel drops analysis nodes for `--discard_analysis_cache` and warns when `--notrack_incremental_state` is set without `--nokeep_state_after_build`. Bazel’s after-command path resets the evaluator unless state is kept, lets module cleanup replace a non-infrastructure exit code, and lets the first stored exit code win across threads.
  - [P0] There is no crash cleanup that posts crash and command-complete events, no OOM heap-dump path, and no module shutdown on crash or normal shutdown. A requested shutdown deletes the output base after removing server status files.
  - [P1] Build-like profiles go to `<output_base>/command.profile.gz`, or to `--profile` (gzip only when the path ends in `.gz`). Bazel retains `command-<id>.profile.gz` according to `--profiles_to_retain`, can send the profile through BEP upload or redirected instrumentation output, and records client startup, extract, and wait durations. Zel logs zero-length launch and init marks and reports a missing parent directory as “Error while creating profile file”.

### `src/main/java/com/google/devtools/build/lib/runtime/BlazeServerStartupOptions.java`
- Bazel lines: 561
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P2] `option_sources` stays a raw string. `OptionSourcesConverter` builds a map: an empty input is an empty map; otherwise the value is split on `:`, `_C` is turned into `:` and then `_U` into `_`, and names are paired with the following value (a final unpaired name maps to `""`). A repeated name fails.
  - [P1] Path options that use `PathFragmentConverter` (`install_base`, `output_base`, `output_user_root`, `server_jvm_out`, `failure_detail_out`, `workspace_directory`, `default_system_javabase`) are not converted that way. A non-empty value starting with `~/` does not have every `~` replaced with the user home before normalization. Only `output_base`, `output_user_root`, and `install_base` are normalized later, and without that home expansion.
  - [P2] `digest_function` is accepted as any string while startup options are parsed. This class rejects an unregistered name at conversion time (case-insensitive match against the registered hash names).
  - [P2] The integer flags are range-checked as decimal text (`io_nice_level` ≤ 7, `max_idle_secs` ≥ 0, `connect_timeout_secs` in 1..=3600, `local_startup_timeout_secs` ≥ 1, `command_port` in 0..=65535). This class uses the plain integer converter, so any `int`, including hex and octal forms, is accepted.

### `src/main/java/com/google/devtools/build/lib/runtime/BlazeService.java`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No service interface that a Blaze module can register as a singleton for the server lifetime.

### `src/main/java/com/google/devtools/build/lib/runtime/BlazeWorkspace.java`
- Bazel lines: 509
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P2] In a workspace, startup writes `<output_base>/README` (ISO-8859-1; first line `WORKSPACE: <workspace path>`, then the fixed warning text) and `DO_NOT_BUILD_HERE` under the output base and under `execroot/`, each containing the workspace path. Write failures are warnings. Zel creates the output base and writes neither file.
  - [P3] The output-base filesystem type is read once at construction and returned for the life of the server. Zel does not record it.
  - [P1] The action cache stays unloaded until the first build that needs it, then loads via `CompactPersistentActionCache` over `action_cache/`, `action_cache.bad/`, and `action_cache.tmp/` (profiled; I/O errors go to the reporter). `--nouse_action_cache` drops the in-memory reference so the next enabled build reloads from disk. `clearCaches` clears memory and deletes those three directories. Idle GC trims an already-loaded cache by stale-entry fraction and max age and does not load a cache only to collect it. Zel loads `action_cache/action_cache.bin` at server start; a bad file becomes an empty cache with an unreported warning and is not moved aside; `--nouse_action_cache` uses a throwaway empty cache for that build and leaves the persistent cache in memory; idle work only saves the file after 10 seconds and does not trim by age or threshold; clean deletes that one file and clears memory.
  - [P3] The previous build’s execution window is a closed millisecond range from command start to the runtime clock, or null if the clock moved backwards. Zel only tracks elapsed time of the current command.
  - [P2] Command init builds the command environment on the calling thread (new event bus, package locator, syscall cache, quiescing executors reset from options, idle-task results, extensions, attempt number, build-request id override, config-flag definitions, and a new resource manager) and installs the client env on Skyframe. Between commands the event bus is dropped and the Skyframe evaluator can be reset. Zel stores the client env on the runtime and runs the command without that environment, event bus, package locator, syscall cache, or per-command executor reset, and clears the Skyframe graph only on clean.
  - [P1] The package locator is the virtual source root (no existence check, null output base) when one is set; otherwise it is built from `--package_path`, the output base, and the workspace, and is null when the command has no package options or there is no workspace. Zel loads packages from the workspace directory and does not apply `--package_path` or a virtual source root.

### `src/main/java/com/google/devtools/build/lib/runtime/BlockWaitingModule.java`
- Bazel lines: 106
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No module that blocks command start until `--block_for_lock` waiters are released or the wait is interrupted.

### `src/main/java/com/google/devtools/build/lib/runtime/BuildEventArtifactInstrumentationOutput.java`
- Bazel lines: 92
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No instrumentation output that starts a build-event artifact upload of type log with no local path and returns that upload stream.
  - [P2] No publish step that refuses to run before an upload has started and registers the upload URI future on the build-tool log collection under the output name.
  - [P3] No builder that requires a name and uploader and ignores create-parent because the output is never written to a local path.

### `src/main/java/com/google/devtools/build/lib/runtime/BuildEventArtifactUploaderFactory.java`
- Bazel lines: 50
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No factory creates a per-command build-event artifact uploader, including a local-files factory that ignores the command environment and returns a new local uploader.
  - [P3] No default shutdown for an uploader reused across commands, and no failure when uploader setup hits an invalid package-path symlink.

### `src/main/java/com/google/devtools/build/lib/runtime/BuildEventArtifactUploaderFactoryMap.java`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No registry of named build-event artifact uploader factories. A null strategy name selects the alphabetically first registered factory when any exist, and any other missing name falls back to the local-files uploader factory.

### `src/main/java/com/google/devtools/build/lib/runtime/BuildEventStreamer.java`
- Bazel lines: 1098
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs`
- Verdict: partial
- Differences:
  - [P1] `post` sends every node. Bazel drops successful non-extra `ActionExecuted` events unless all actions are published, drops test summaries with zero runs, drops `BuildCompleteEvent` for `test`/`coverage`/`run` unless the build crashed, and drops `TargetParsingCompleteEvent` when the only pattern failed. `publish_all_actions` is stored and never consulted.
  - [P2] Vacuous test summaries, when target summaries are enabled, are treated as posted so events waiting on them (target summaries) flush immediately instead of being aborted at the end. That path does not exist.
  - [P2] Replaceable events are not buffered, released, or discarded. Bazel keeps at most one replaceable event per id, posts it on release, drops it silently once that id is posted, and on abort posts the replaceable event instead of an abort when nothing else is waiting.
  - [P1] Events blocked on an id that never arrives become one aborted event for that id with no children. Zel aborts that id with the blocked ids as children, which announces them so each is aborted again.
  - [P2] A completing event that arrives while events are still waiting on `build_started` does not synthesize an aborted `build_started` announcing the initial progress id, the completing id, and those waiting ids. The branch is empty, so they stay pending until the generic abort.
  - [P2] Abort reason is a single caller-supplied pair, overwritten on each update. Bazel keeps every reason, reports the latest, uses the OOM exit text for a lone out-of-memory abort, and uses `Multiple abort reasons reported: …` when there are several. Interrupt, `--noanalyze`, `--nobuild`, crash, catastrophe, and incomplete stop-on-first-failure are not recorded by the streamer.
  - [P1] Posting does not dedupe and emit configuration events, and it does not name, expand, or split artifact groups (`maxNamedSetEntries` only splits when at least 2) from events that report artifacts. Output-group file modes are not applied. `configuration_posted` is only a manual helper.
  - [P2] A `NoBuildEvent` that does not carry its own finished event also completes the stream. Zel completes only when `completes_build` is set.
  - [P2] Stdout and stderr are appended internally, empty strings are dropped, and each stream is concatenated into one progress event. Bazel pulls chunks from an output provider and pairs them (one progress event per stdout chunk, the last of those sharing the first stderr chunk, then the rest of stderr). The stderr-after-stdout flush state is never read, so alternating output is not flushed on that transition.
  - [P2] `close` before any event has opened the stream, with only order-constrained events pending and no buffered output, does not abort those events and does not emit a final progress event. Bazel still does both.
  - [P1] After `close`, later posts are dropped, including ids still expected in the final set. Bazel still forwards those remaining ids. Shutdown is a synchronous `close` plus error strings. Bazel records per-transport close and half-close futures, and half-close means the client has finished sending but must keep the payload until the server acknowledges.

### `src/main/java/com/google/devtools/build/lib/runtime/BuildEventStreamerUtils.java`
- Bazel lines: 57
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - [P2] `BLAZE_HALTED_BEFORE_TESTING` maps to BEP `TOOL_HALTED_BEFORE_TESTING`. Zel underscoring the display name yields `BLAZE_HALTED_BEFORE_TESTING`, which the allowlist rejects, so the BEP status becomes `NO_STATUS`.
  - [P2] `REMOTE_FAILURE` maps to BEP `REMOTE_FAILURE`. Zel `TestStatus` has no remote-failure value, so that status is never produced.

### `src/main/java/com/google/devtools/build/lib/runtime/BuildMetadataEvent.java`
- Bazel lines: 68
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel posts this event only after the build-started id. Zel leaves `posted_after` empty, so the streamer does not hold it until build started.
  - [P1] Bazel fills `BuildMetadata.metadata` from the map when the stream proto is built, and a repeated key replaces the previous value. Zel copies the pairs when the node is built, and a repeated key is appended as another map entry.
  - [P1] The only post site passes an empty list, so the event payload is always empty. Bazel emits the supplementary key-value pairs supplied for that build.

### `src/main/java/com/google/devtools/build/lib/runtime/BuildPhase.java`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No mapping from how far a target got to a status string and success bit: `parsing-failed`, `loading-failed`, `analysis-failed`, `not-built`, `not-analyzed`, `build-failed`, `blaze-halted`, and `build-skipped` are failures; `test-filtered`, `target-filtered`, `built`, and `target-skipped` are successes.

### `src/main/java/com/google/devtools/build/lib/runtime/BuildSummaryStatsModule.java`
- Bazel lines: 253
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - [P1] The default build-complete info line is `Elapsed time: %.3fs` from the build request stop time minus start time. With critical-path profiling enabled (the default), that line also includes `Critical Path: %.2fs` from the aggregated critical path, remote stats omitted. Zel always prints both clauses: the success path in `/workspace/zel-cursor/crates/zel-server/src/lib.rs` uses time since just before action execution, and the failure trailer uses `BuildOutcome.elapsed` from inside action execution. The printed critical path is the scheduler’s longest dependency chain of action wall times. `--experimental_enable_critical_path_profiling=false` still computes and prints it.
  - [P2] `--experimental_stats_summary` replaces that line with the process summary, `Total action wall time %.2fs` (sum of spawn execution wall times), `Execution critical path %.2fs (setup %.2fs, action wall time %.2fs)` when the path is non-empty, and `Elapsed time %.2fs (preparation %.2fs, execution %.2fs)`. Execution length runs from the first execution-start signal (`ExecutionStartingEvent`, or the first `TopLevelTargetPendingExecutionEvent`) to `ExecutionFinishedEvent`; preparation is command elapsed time minus that window. Zel parses the flag and keeps the default two-line summary.
  - [P1] The process summary is `N process(es): …` or `0 processes.`, ordered as action-cache hits, disk cache hits, remote cache hits, internal, then other runners. Bazel counts each completed action, counts an action as non-internal once if it had any spawn, counts every spawn runner, and takes action-cache hits from action-cache statistics (hits stay out of the total). Zel rebuilds the counts after scheduling: it always includes one workspace-status action, skips memoized outputs, counts at most one runner per completed action, and the success path forces `0 processes.` for `--check_up_to_date` and `--check_tests_up_to_date`.
  - [P1] On build complete, Bazel writes build-tool log values `elapsed time` (`%f` seconds), `critical path` (full path text, including the component list, when profiling ran), and `process stats` (the summary string). It stops the profiler there, publishes the profile into that collection, and reports `Error while writing profile file: …` on I/O failure. It also info-logs the full critical path and the slowest components, and posts the aggregated path. Zel posts an empty build-tool-logs event and writes the profile at command finish with the same error string. The profile records only the single longest action as a critical-path component.

### `src/main/java/com/google/devtools/build/lib/runtime/CacheFileDigestsModule.java`
- Bazel lines: 83
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No executor-init path reconfigures a computed-file-digest cache when `--cache_computed_file_digests` changes, disables it when the size is 0, or snapshots cache stats. No command-complete path logs accumulated hit, miss, hit-rate, and eviction counts or the per-command delta. The flag is parsed into `ExecOptions.cache_computed_file_digests` and never applied.

### `src/main/java/com/google/devtools/build/lib/runtime/ClientOptions.java`
- Bazel lines: 138
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P1] `--default_override` uses the same acceptance rule: `rcfile:command=option`, first `:` and first `=`, a non-empty command, an option text that may be empty, and an rc index that parses as a Java `int` and must be ≥ 0 (`0` is accepted). The port keeps the original text. It does not turn that into an rc index, command, and option, and it does not apply the option to that command (`command:config`, `common`, `always`, and `startup` included). `canonicalize-flags` removes `--rc_source` and `--default_override` and does not emit the override as a command flag. `--default_override` with the value in the next argument is removed there without the integer check. The `--flag=value` form is checked.
  - [P2] `--rc_source` is one string per occurrence. The port splits each occurrence on commas, and an empty `--rc_source=` adds no entry. Those strings are not the rc-file list the override index selects.
  - [P0] `--client_env` is a repeatable `name=value` list: the value may be empty or contain `=`, and a later assignment replaces an earlier one for the same name. The runtime map is filled only from `--client_env=name=value` tokens taken out before command parsing. When that form is absent, the process environment is used. The Bazel option records only the parsed assignments, so an omitted flag leaves the map empty. `--client_env` with `name=value` in the next argument is not copied into the map.

### `src/main/java/com/google/devtools/build/lib/runtime/Command.java`
- Bazel lines: 181
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/mod.rs`
- Verdict: partial
- Differences:
  - [P2] `BuildPhase` is absent. Bazel `executes()` is true only for `EXECUTES`, `analyzes()` for `ANALYZES` and `EXECUTES`, and `loads()` for `LOADS`, `ANALYZES`, and `EXECUTES`. Zel does not use that hierarchy to enable project-file handling and Starlark build-setting parsing only for analyzing commands, Skyfocus only for executing commands, or the first options-parse round only when the command analyzes and `MODULE.bazel` exists.
  - [P1] `binaryStdOut` and `binaryStdErr` are not applied. With color off, Bazel strips ANSI on a stream unless the flag is set, and it buffers a stream unless the flag is set. `query`, `aquery`, `cquery`, `info`, and `dump` set `binaryStdOut`, so their stdout is unbuffered and not stripped. Zel buffers every command's stdout and stderr until the command finishes and never strips ANSI from command output.
  - [P2] `run` completion is `label`. Bazel sets `label-bin`, which `help completion` prints as `BAZEL_COMMAND_RUN_ARGUMENT`.

### `src/main/java/com/google/devtools/build/lib/runtime/CommandCompleteEvent.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No event posted when a command finishes, carrying the exit code and the command name for listeners that run after the command.

### `src/main/java/com/google/devtools/build/lib/runtime/CommandDispatcher.java`
- Bazel lines: 98
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P1] Errors-only dispatch is not applied. The run request's quiet flag is never read, so a quiet invocation still prints informational events.
  - [P3] Wait-versus-fail locking is not part of command dispatch. `dispatch_command` always blocks, and only for commands that load external repositories. The gRPC daemon applies wait versus immediate failure before that call.
  - [P1] An invocation policy supplied to dispatch is ignored. A policy is enforced only when `--invocation_policy` is among the command arguments.
  - [P2] Command extensions supplied to dispatch are ignored, and nothing reports extensions while the command runs. Extensions are attached only on the final response, and only for `info`.

### `src/main/java/com/google/devtools/build/lib/runtime/CommandEnvironment.java`
- Bazel lines: 1202
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P0] The client environment is only the parsed `--client_env` entries, with the last value for a name kept. Zel does that when the flag is present, and copies the server process environment when it is absent, so repository rules and inherited action variables see server variables.
  - [P1] Inside a workspace, a `--client_cwd` that still contains `..` after normalization, or an absolute cwd outside the workspace, aborts with `CLIENT_CWD_MALFORMED`; a relative cwd is joined onto the workspace. If that check fails, the abrupt exit is queued and the working directory falls back to the workspace. Outside a workspace the working directory is the process cwd. Zel accepts the flag, uses an absolute cwd under the workspace as the relative directory, and otherwise uses an empty relative directory (the raw path for `bazel run`).
  - [P1] The command id is `--invocation_id`, otherwise `BAZEL_INTERNAL_INVOCATION_ID` (with a deprecation warning; a missing or malformed value becomes a random UUID), and a new UUID when the attempt number is greater than 1. The build request id is `--build_request_id`, otherwise the dispatcher override, otherwise a random UUID. Zel always synthesizes a time-and-pid id, and the BES attempt number stays 1.
  - [P1] `repoEnv` starts as the client environment, or only `PATH` (and `PATHEXT` on Windows) when `--experimental_strict_repo_env` is set. `nonstrictRepoEnv` always starts from the full client environment. For loading commands and `info`, `--action_env` set and unset update both maps unless `--incompatible_repo_env_ignores_action_env` (default true). `--repo_env` then sets, inherits, or unsets, expanding `%bazel_workspace%` with backslashes on Windows. Zel matches the strict base and `--repo_env` expansion using the host path, leaves `--action_env` out when that incompatible flag is off, and drops the nonstrict map, so local repository operations and credential helpers see the strict environment.
  - [P2] Allowlisted action variables (loading commands and `info`) and test variables (analyzing commands and `info`) keep inherit names after last-wins set and unset, then only names present in the client environment, in sorted order. `info` prints them as `build --action_env=NAME=value` and `build --test_env=NAME=value`. Zel treats every flag value without `=` as an inherit, in flag order, so a later `NAME=value` or `=NAME` leaves an earlier bare name in the list.
  - [P1] A non-positive `--http_timeout_scaling` warns and uses 1.0. A non-positive `--http_max_parallel_downloads` aborts with `BAD_DOWNLOADER_CONFIG`. The downloader uses `--http_connector_attempts`, `--http_connector_retry_max_timeout`, that parallel limit, and the scale. Zel's fetches use a fixed attempt count and timeout and do not abort for a non-positive parallel limit.
  - [P2] For analyzing commands and `clean`, the output service is the single module service or a local one; two providers are an error. Executing commands prepare Skyfocus, and a loading command on a focused graph aborts with `DISALLOWED_OPERATION_ON_FOCUSED_GRAPH`. Zel keeps a local output layout and runs those commands with no Skyfocus check.
  - [P2] Incremental state follows startup `--batch`, `--keep_state_after_build`, `--track_incremental_state`, `--heuristically_drop_nodes`, and `--discard_analysis_cache`: batch plus discard turns tracking off and warns, `--notrack_incremental_state` without `--nokeep_state_after_build` warns, heuristic dropping is ignored while tracking stays on, and the graph is reset when tracking is off or the tracking flag changes. Zel prints that notrack warning and drops analysis nodes at the end of the command when `--discard_analysis_cache` is set.
  - [P2] Package-loading sync runs once per command and passes the command id, client environment, repository environment, timestamp-granularity monitor, and whether the command executes. A second call throws. Zel sync injects loading options and diffs the filesystem, and it can run again. There is no timestamp-granularity wait.
  - [P3] The first module `exit` is kept and interrupts the command thread when it is raised from another thread. Precomplete freezes further interrupts, clears the interrupt bit, and can replace the command exit code. Zel has no pending module exit that overrides the command result.
  - [P2] The recorded command start is earlier by `--startup_time`, and the wait time is the server queue time plus `--command_wait_time`. Zel's elapsed time starts at dispatch and ignores both flags.

### `src/main/java/com/google/devtools/build/lib/runtime/CommandLineEvent.java`
- Bazel lines: 565
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs`
- Verdict: partial
- Differences:
  - [P1] Original, canonical, and tool events are one five-section command line. Startup options are always empty, and command options are the explicit `--` args split on the first `=`, rebuilt as `--name` or `--name=value`. Bazel original startup options are only the client options whose source is empty, stored as combined form alone, or the active startup options when that list is absent. Canonical startup options drop `ignore_all_rc_files`, `blazerc`, `master_blazerc`, `bazelrc`, `master_bazelrc`, and `invocation_policy`, then append a parsed `--ignore_all_rc_files`.
  - [P1] Parsed options omit effect tags, metadata tags, and source. Original command options are not limited to command-line priority, and canonical command options are not the canonical list plus every Starlark option. Starlark flags are not emitted as `--<flag>=<value>` with the value field left unset when the value is null.
  - [P2] The executable chunk is always `bazel`.
  - [P2] Residue is every argument that does not start with `-`. For `run`, when residue reporting is off and the residue is non-empty, Bazel keeps the first residue chunk and, if more remain, one `REDACTED` chunk.
  - [P2] The tool event is that same skeleton with empty options and residue. `--experimental_tool_command_line` only validates the string: empty stays empty, non-base64 is accepted, and base64 that is not well-formed protobuf fails with the malformed-value error. Bazel posts a default command line for empty input, a `tool`-labeled command line with one unlabeled chunk for non-base64, and the parsed command-line proto for valid base64. The event id remains the tool command line.
  - [P2] The canonical event is not replaceable, so a later canonical command line cannot replace the one posted before analysis. There is no explicit command-line hash over Starlark options, non-hidden canonical options sourced from `command line options`, and the residue.

### `src/main/java/com/google/devtools/build/lib/runtime/CommandLinePathFactory.java`
- Bazel lines: 150
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No factory that turns a command-line path flag into a filesystem path, expanding `~/` and rejecting a path that escapes the workspace when that is required.

### `src/main/java/com/google/devtools/build/lib/runtime/CommandLogModule.java`
- Bazel lines: 135
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P1] Zel writes `<output_base>/command.log` only after the command returns, as the entire stderr buffer followed by the entire stdout buffer (`/workspace/zel-cursor/crates/zel-exec/src/layout.rs`). Bazel tees stdout and stderr onto one stream in emission order, then flushes and closes that stream when the command finishes.
  - [P2] Zel turns the log off only when a startup argument is exactly `--nowrite_command_log` or `--write_command_log=false`. Bazel reads the command option `write_command_log` (default false); the client forwards the startup flag (default true) as that command option, and a later command-line value overrides it. Other boolean spellings and a command-position flag leave Zel writing.
  - [P2] With local instrumentation writes, Bazel unlinks `<output_base>/command.log` at the start of every command, including `clean` and when logging is off, and warns `Unable to delete command log: …` if the unlink fails. Zel leaves the previous file in place whenever it does not write a new one.
  - [P2] `--redirect_local_instrumentation_output_writes` makes Bazel skip that unlink and send the log through the instrumentation-output factory (remote destination when configured, otherwise a local file plus the warning that remote redirection is unsupported). Zel always writes the local file and ignores the flag.
  - [P2] Bazel reports `Unable to open command log: …` as a warning and continues with no listener, and reports `I/O exception closing log: …` as an error (or on stderr if the command environment is already cleared). Zel discards write errors.
  - [P2] `info` key `command_log` uses the same description and a trailing newline. Bazel prints the logical `<output_base>/command.log` path. Zel (`/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`) prints that path after canonicalizing the output base.

### `src/main/java/com/google/devtools/build/lib/runtime/CommandNameCacheImpl.java`
- Bazel lines: 66
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/rc.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel `get` returns the command name plus every command reachable through `inheritsOptionsFrom`. An unknown name throws; the exception message is that name. Zel `get_command_ancestry` does not fail: an unknown name produces only `always` and `common` and omits the requested name.
  - [P3] The Bazel set contains only those command names. Zel always prepends the rc pseudo-commands `always` and `common`, so a lookup for `build` is `build` in Bazel and `always`, `common`, `build` in Zel.
  - [P3] Bazel walks breadth-first, records the command before its bases, and skips a command already visited, so a cycle yields a finite set. Zel walks depth-first with bases before the command and does not track visited commands, so a base reached twice is emitted again and a cycle does not terminate.
  - [P3] Bazel reads `@Command` from the live command registry, including commands registered after the cache is constructed, and then reuses that memoized set. Zel walks only the fixed `COMMAND_INHERITANCE` table and has no per-name cache.

### `src/main/java/com/google/devtools/build/lib/runtime/CommandPrecompleteEvent.java`
- Bazel lines: 39
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No event payload stores the command `ExitCode` and exposes it for handlers notified immediately before command completion.

### `src/main/java/com/google/devtools/build/lib/runtime/CommandStartEvent.java`
- Bazel lines: 20
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No event is posted when a command such as clean, build, or test starts.

### `src/main/java/com/google/devtools/build/lib/runtime/CommonCommandOptions.java`
- Bazel lines: 725
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P2] `--experimental_heuristically_drop_nodes` always prints an old-name deprecation warning. Bazel leaves that alias silent (`oldNameWarning = false`).
  - [P0] `--enable_platform_specific_config` is recognized only as the bare flag, `=true`, or `=1` on arguments seen before `--config` expansion, and any earlier match stays on. Bazel parses it as a boolean (last value wins, including `yes`/`y`/`t`) and turns on the host config (`linux`/`macos`/`windows`/`freebsd`/`openbsd`) only after explicit `--config` expansion, so a config can enable it.
  - [P2] `--announce_rc` is announced only for the literals `--announce_rc` and `--announce_rc=true` before `--`. Other boolean spellings, and a later disabling value, do not follow boolean last-wins.
  - [P1] `--generate_json_trace_profile=auto` writes a profile for `build`, `test`, `coverage`, and `run`. Bazel also profiles `query` and every command whose build phase analyzes (`aquery`, `cquery`, `fetch`, `print_action`, `vendor`, `mobile-install`). Raw values `y` and `t` are treated as auto. Bazel treats them as yes.
  - [P1] `--invocation_id` and `--build_request_id` are ignored. The reported id is a time-and-pid substitute, with no use of `BAZEL_INTERNAL_INVOCATION_ID` and no new id on a retry. An empty `--invocation_id` parses as a value. Bazel’s `UUIDConverter` turns empty into null so the flag does not override the environment. `UUIDConverter` and `PrefixedUUIDConverter` also accept hex groups that do not fit in a signed 64-bit `Long.parseLong`, which `UUID.fromString` rejects.
  - [P2] `--repo_env=` and `--repo_env==` are dropped and the command continues. Bazel fails the parse. `--incompatible_repo_env_ignores_action_env=false` still keeps `--action_env` out of the repository environment. Bazel adds those `NAME` and `NAME=VALUE` entries when the flag is false.
  - [P1] `--client_cwd`, `--profile`, and `--memory_profile` keep the raw string. Bazel’s path converter expands a leading `~/` to the user home and normalizes the fragment.
  - [P2] `--experimental_tool_command_line` accepts any base64 payload that is well-formed protobuf wire. Bazel accepts a plain string, or base64 that `CommandLine.parseFrom` accepts, and rejects other decoded bytes.
  - [P2] `--write_command_log` follows only the startup arguments `--nowrite_command_log` and `--write_command_log=false`, and otherwise the log is written. The command option (default false, client default true) is what Bazel’s command log uses, including `=0`/`=no` and a flag placed after the command name. `clean` skips the log in both.
  - [P2] `--notrack_incremental_state`, `--nokeep_state_after_build`, and `--heuristically_drop_nodes` only affect the “without `--nokeep_state_after_build`” warning. Bazel drops incremental Skyframe state, discards in-memory state after the build, and removes `FileState` and `DirectoryListingState` nodes.
  - [P1] These flags are parsed and then unused: `--logging`, `--unconditional_warning`, `--build_metadata`, `--tool_tag`, `--oom_message`, `--startup_time`, `--extract_data_time`, `--command_wait_time`, `--restart_reason`, `--binary_path`, `--profiles_to_retain`, `--starlark_cpu_profile`, `--memory_profile`, `--memory_profile_stable_heap_parameters`, `--heap_dump_on_oom`, `--always_profile_slow_operations`, the install-base and action-cache garbage-collection flags, the thread-dump flags, the profiler collection flags (`worker` data, load average, network, resource estimation, PSI, Skyframe counts), `--redirect_local_instrumentation_output_writes`, `--experimental_allow_project_files`, `--http_timeout_scaling`, `--http_connector_attempts`, `--http_connector_retry_max_timeout`, and `--http_max_parallel_downloads`.

### `src/main/java/com/google/devtools/build/lib/runtime/ConfigExpander.java`
- Bazel lines: 304
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/config_expander.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel expands `--config` instances already parsed on the options parser. Zel first assembles `always` / `common` / inherited rc args plus CLI args, then expands `--config` tokens in that list.
  - [P0] `--enable_platform_specific_config` is the parsed boolean after those explicit config expansions, so a config body can turn it on or off and the last value wins (`true`/`false`/`yes`/`no`/`1`/`0`, `--noenable_platform_specific_config`, or a following value token). Zel enables it only when a pre-expansion token is exactly `--enable_platform_specific_config`, `=true`, or `=1`, so a later disable is ignored and a value that appears only inside a config is missed.
  - [P1] With that flag set and a `<command>:<os>` section present, Bazel expands that section as the expansion of the flag (`enabled by --enable_platform_specific_config`) and does not add a `--config=<os>` value. Zel appends `--config=<os>` and expands it as a normal config, so the platform name is included in cycle checks and the duplicate-config warning.
  - [P2] Nested rc configs must be one `--config=<value>` token; `--config` plus a separate value token is an error. Any arg whose first eight characters are `--config` (including `--configuration` or `--configfoo`) is treated as a config reference. Zel accepts only exact `--config=<value>` and `--config <value>`.
  - [P2] Bazel keeps each `--config=<value>` token in the expansion, parses it as a child of the original option (`expanded from --config=<value>`), and attaches fallback option data to `common` args. Zel drops the `--config` token and returns only the section arguments.
  - [P1] Flags the current command does not support are dropped for every expansion and reported as `Ignored as unsupported by '<command>': ...`. Zel drops unsupported flags only for `common` sections and does not emit that note.
  - [P2] Duplicate configs are warned from the final parsed `configs` list in first-seen order. Zel counts expansions in sorted order, prefixes the warning with `WARNING:`, and counts the synthetic platform config.
  - [P2] A chain of length at least 10 warns once per top-level `--config`, with the chain as `[a, b, ...]`. Zel warns once for the longest chain in the whole invocation, with the chain debug-formatted as quoted strings.
  - [P2] `--` inside a config chunk does not stop expansion. Zel copies everything after `--` through unchanged.
  - [P2] Each rc chunk produces `Found applicable config definition <command>:<config> in file <rc>: <args>`. Zel emits `INFO: Found applicable config definition ...` per contiguous same-file run and returns those notes in the same list as warnings.

### `src/main/java/com/google/devtools/build/lib/runtime/ConfigFlagDefinitions.java`
- Bazel lines: 148
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel value stores per-config flag lists plus rc-file sources, or resolves a name by taking only the first definition.
  - [P2] No lookup that inlines only `--config=<name>` (not `--config <name>`), keeps a non-popped seen-set so a reused config is a cycle, and fails with the missing-config and nested-missing-config errors while returning the expanded flags and the set of rc sources.

### `src/main/java/com/google/devtools/build/lib/runtime/CountingArtifactGroupNamer.java`
- Bazel lines: 91
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/observability.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel maps each `NestedSet` node to one decimal id (`0`, `1`, …, the map size at first insert). `maybeName` returns that name only on the first insert and `null` when the node is already named. Zel `fetch_add`s a new id for every non-empty output group and never keys names by nested-set identity, so the same set is named again.
  - [P2] A second namer of an existing node waits on a one-shot latch until the first namer `close()`s, then returns `null`. Zel returns the new id immediately and does not wait until a prior writer of that set has posted it.
  - [P2] `apply` returns a `NamedSetOfFilesId` for a node that already has a name, and `null` if it does not. Zel has no lookup from a nested-set node to an existing named-set id.

### `src/main/java/com/google/devtools/build/lib/runtime/CrashEvent.java`
- Bazel lines: 22
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No event is posted during crash cleanup immediately before the command-complete event.

### `src/main/java/com/google/devtools/build/lib/runtime/DummyMetricsModule.java`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No metrics module registers on the command event bus and, when the build completes, posts an empty build-metrics event.

### `src/main/java/com/google/devtools/build/lib/runtime/ExecutionGraphModule.java`
- Bazel lines: 994
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P2] The six `ExecutionGraphOptions` flags parse with the same defaults (`false`, path `""`, dep type `none`/`runfiles`/`all`, queue size 32-bit `-1`, file-write edges `true`, change-pruned actions `false`) on `build` and commands that inherit its options. Nothing in the build reads them.
  - [P2] `--experimental_enable_execution_graph_log` does not write a zstd length-delimited `execution_graph.Node` dump. Bazel writes `<output_base>/execution_graph_dump.proto.zst` when the path is blank and BEP streaming uploads are off, writes only to `--experimental_execution_graph_log_path` when that path is non-blank (`String.isBlank`, Unicode whitespace counts as unset), and otherwise uploads a `PERFORMANCE_LOG`. The build tool logs entry is always named `execution_graph_dump.proto.zst` (local file with compression `NONE`, or the upload URI).
  - [P2] A non-blank path while the log is disabled does not fail commands whose build phase executes with `COMMAND_LINE_ERROR` / `BUILD_REPORT_WRITE_FAILED`. Package-path upload failure and dump-file creation failure do not exit with `BUILD_REPORT_UPLOADER_NEEDS_PACKAGE_PATHS` or `BUILD_REPORT_WRITE_FAILED`. An interrupted dump shutdown does not raise the action-dump abrupt exit.
  - [P2] Spawn, action-completion, cache-hit, discovered-input, shared-action, and change-pruned events are not recorded. Bazel emits description, mnemonic, owner target kind and label, runner name and subtype, optional spawn identifier, and metrics (start epoch millis; durations narrowed to signed 32-bit; discover-input time summed onto the first spawn that shares the primary output, then cleared). Unless dep type is `none`, nodes get a dense index. `all` adds direct input edges; `runfiles` and `all` also add runfiles-tree artifacts when input metadata is present; dependency indexes are unique and sorted. File-write actions use the action inputs when the file-write-edges flag is true. `TestRunner` action-completion nodes are skipped once a spawn has recorded the primary output. A later spawn sets `retry_of` when the previous attempt finished at or before this start; with `--experimental_local_lockfree_output`, an overlapping second spawn is emitted without dependency edges or output-map updates; otherwise a non-fatal bug report is sent and edges are still recorded. Change-pruned actions are included only when that flag is set, and only if Skyframe returns an action, using the finish time for both start and end and no runfiles metadata. Shared actions copy node info by pairing the executed and transformed artifact maps in map iteration order, stopping at the shorter map. The writer starts on the first execution-start event, uses an unbounded queue when the size is negative, and sends a bug report if queue puts blocked for more than 100 ms.

### `src/main/java/com/google/devtools/build/lib/runtime/GCStatsRecorder.java`
- Bazel lines: 84
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel type snapshots each `GarbageCollectorMXBean` name, collection count, and collection time at construction, then returns one stat per bean in that iteration order with `long` deltas (`current - snapshot`).
  - [P3] Duplicate collector names fail `ImmutableMap` construction, and a later bean whose name was not snapshotted throws. Zel has neither path.

### `src/main/java/com/google/devtools/build/lib/runtime/GcChurningDetector.java`
- Bazel lines: 158
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No per-invocation handler ignores manual and non-full GCs, clamps each full-GC duration to the invocation wall time, and accumulates that duration.
  - [P3] No fraction series is recorded after at least 1 ms of wall time (`cumulativeFullGcMillis / (int) wallMillis`), and the peak point is never tracked or published.
  - [P3] The threshold is never switched to `--gc_churning_threshold_if_multiple_top_level_targets` when more than one top-level target is parsed (that optional flag otherwise falls back to `--gc_churning_threshold`).
  - [P3] Nothing forces a `CRASH_OOM` / `GC_CHURNING` crash once the fraction is at least the threshold percent and the invocation has run for at least one minute.

### `src/main/java/com/google/devtools/build/lib/runtime/GcThrashingDetector.java`
- Bazel lines: 170
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel never constructs a per-invocation detector. Bazel returns no detector when `gcThrashingLimits` is empty or `gcThrashingThreshold` is 100, and otherwise tracks every configured limit.
  - [P3] Bazel treats tenured occupancy `< threshold` as a reset that clears every limit window. Occupancy at or above the threshold is recorded only for full, non-manual GCs; other events leave the windows unchanged.
  - [P3] Each limit keeps a sliding window of those GC timestamps, drops entries strictly before `now - period`, and on a window length equal to `count` halts via `BugReporter.handleCrash` with `CRASH_OOM` and `OomCauseCategory.GC_THRASHING`. Zel does not run that window or crash.

### `src/main/java/com/google/devtools/build/lib/runtime/GotOptionsEvent.java`
- Bazel lines: 110
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/observability.rs`
- Verdict: partial
- Differences:
  - [P1] `startup_options` and `explicit_startup_options` are always empty. Bazel fills `startup_options` with every non-hidden parsed startup option in canonical form (`--name`, `--name=<unconverted>`, or `--flag`/`--noflag` for booleans; no shell escaping), stably sorted by option priority, and `explicit_startup_options` with the explicit subset whose source is not `"default"` (null source is kept).
  - [P1] `cmd_line` and `explicit_cmd_line` are both the raw command argv, including target residue and the original spellings. Bazel `cmd_line` is the full non-hidden parsed command-option list in that canonical form (priority-sorted, residue omitted, `HIDDEN`/`INTERNAL` dropped, expansions included). `explicit_cmd_line` keeps only explicit options (not expanded, not implicit dependents, not from `PROJECT.scl`) whose source is exactly `"command line options"`, so rc options, invocation-policy options, and launcher-generated options are omitted.
  - [P2] `invocation_policy` is never set. Bazel always sets it to the command's `InvocationPolicy`.
  - [P1] `tool_tag` is always `""`. Bazel sets `CommonCommandOptions.toolTag` (`--tool_tag`, default empty).
  - [P2] The event records no `postedAfter` constraint. Bazel holds it until both the build-started id and the unstructured-command-line id have been posted.
  - [P2] It is emitted only for `build`, `test`, `coverage`, and `run` when BEP is enabled and the target pattern set is non-empty. Bazel posts it from `CommandEnvironment.beforeCommand` for every command that reaches that hook, including an empty pattern set.

### `src/main/java/com/google/devtools/build/lib/runtime/HostJvmStartupOptions.java`
- Bazel lines: 64
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P3] `--host_jvm_debug` expands to one `--host_jvm_args=-agentlib:jdwp=transport=dt_socket,server=y,address=5005`. Zel records that expansion for help but startup parsing does not apply it, so `host_jvm_args` does not gain the JDWP argument.
  - [P3] `--host_jvm_debug=<value>` is a startup-parse error in Zel. Java warns that an expansion option ignores the value and still expands. A bare `--host_jvm_debug` is stored with an empty raw value and forwarded as `--host_jvm_debug=`; Java’s canonical form of a valueless void option is `--host_jvm_debug`.
  - [P3] Each `--host_jvm_args` occurrence is one string, commas included (`StringConverter`, `allowMultiple`, default empty). Zel’s `ListString` conversion splits that occurrence on commas when filling `StartupOptions.host_jvm_args`.
  - [P2] `--server_javabase` defaults to `""` and is an unchecked string on this class. Zel exits with a command-line error when the value is non-empty unless `<path>/bin/java` exists and is executable.

### `src/main/java/com/google/devtools/build/lib/runtime/InfoItem.java`
- Bazel lines: 84
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] `print` on a non-`byte[]` uses `String.valueOf` and emits Latin-1 internal bytes (`StringUnsafe.getInternalStringBytes`), then one `\n`. A UTF-16 string (any code unit above U+00FF, including unpaired surrogates) throws `IllegalArgumentException`. Zel `text` UTF-8-encodes a `&str` and appends `\n`, so U+0080–U+00FF expand to two bytes and higher scalars are encoded instead of rejected.
  - [P3] `print(null)` is the ASCII bytes of `null` plus a newline. `text` only accepts a string.
  - [P3] `print(byte[])` returns that array with no added newline. `text` always appends `\n`; raw bytes are kept only when a caller stores a `Vec<u8>` directly.
  - [P2] `needsSyncPackageLoading()` defaults to false and is what triggers `syncPackageLoading` before `get` when overridden. Zel's `requires_workspace` only drops or errors the key outside a workspace. `get` may also throw `AbruptExitException` or `InterruptedException`; Zel stores a finished `Vec<u8>`.

### `src/main/java/com/google/devtools/build/lib/runtime/InstrumentationOutput.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No instrumentation-output object that opens a write stream and later publishes that output into the build-tool log collection.
  - [P3] Default path is null when an implementation does not override it. Enabled profiles always resolve to a concrete local path, and build-tool logs are posted with no per-output path or URI.

### `src/main/java/com/google/devtools/build/lib/runtime/InstrumentationOutputBuilder.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no instrumentation-output builder. The Java type requires `setName`, `setCreateParent`, and `build`, and the default `setDestination`, `setDestinationRelatedToType`, and `setOptions` are no-ops that return the same builder.

### `src/main/java/com/google/devtools/build/lib/runtime/InstrumentationOutputFactory.java`
- Bazel lines: 245
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No factory builds a local instrumentation output from a name, path, and convenience name (the convenience symlink is created later, in the same directory as that output).
  - [P3] No generic output resolves a path fragment against the output base, the workspace, the working directory, or the temp logging directory.
  - [P2] When `--redirect_local_instrumentation_output_writes` is set and a redirect supplier exists, Bazel returns a non-local output carrying the name, destination, relative-to type, command options, and `createParent`, and does not apply `append` or `internal`. Without a supplier it warns and writes a local file instead. Zel only declares the flag and never selects a destination from it.
  - [P3] The shorter create overload always passes `createParent` as false.
  - [P2] No build-event artifact instrumentation output is built from a name and an uploader.
  - [P3] Construction fails unless both the local and build-event builder suppliers are set. A missing redirect supplier is allowed, and an unset temp logging directory defaults to `java.io.tmpdir`.

### `src/main/java/com/google/devtools/build/lib/runtime/LineBufferedOutputStream.java`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No byte `OutputStream` decorator that buffers up to 1024 bytes by default, flushes on `\n` or a full buffer, and passes writes longer than twice the buffer through after flushing any pending bytes.
  - [P3] `flush` writes a partial buffer then flushes the wrapped stream, and `close` does that and closes it. A flush that throws clears the index first, so those bytes are not written again.

### `src/main/java/com/google/devtools/build/lib/runtime/LoadingPhaseThreadsOption.java`
- Bazel lines: 72
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P2] When `TEST_TMPDIR` or `BAZEL_SHELL_TEST` is set, Bazel clamps the converted count with `Math.min(20, value)` before the `[1, Integer.MAX_VALUE]` check. Zel parses `--loading_phase_threads` with `IntegerSpec::THREADS_MIN1` (`auto` = host CPUs, same bounds) and does not apply that test cap.

### `src/main/java/com/google/devtools/build/lib/runtime/LocalInstrumentationOutput.java`
- Bazel lines: 146
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] `publish` registers the output as an uncompressed `LOG` local file under its name. Zel posts an empty build-tool log list and never attaches this path.
  - [P2] `makeConvenienceLink` does nothing when the convenience name is unset. Otherwise it deletes `<parent>/<convenienceName>` and creates a symlink there whose target is only the output file’s base name. Zel never creates that sibling link; the default profile is the file `<output_base>/command.profile.gz` itself.
  - [P2] `createOutputStream` creates the parent directory only when `createParent` is true (default false). It opens `(append, internal)` only when both are set; if only `append` is set, `internal` is false; if `append` is unset, the stream is truncated and not marked internal, even when `internal` was set. Zel’s profile write always tries to create parents, ignores that error, and always truncates. There is no append open and no unprofiled internal open.
  - [P3] `build()` fails when the name or path is missing. Zel has no builder with those requirements.

### `src/main/java/com/google/devtools/build/lib/runtime/LocationPrinter.java`
- Bazel lines: 80
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs`
- Verdict: partial
- Differences:
  - [P1] With the flag off, Bazel prints the raw file and appends `:line` only when `line != 0` and `:column` only when `column != 0`. Zel always prints `file:line:column`, so a zero line or column is shown as `:0`.
  - [P2] With the flag on, Bazel normalizes the path (`PathFragment.create`) and, if it is absolute, strips a non-null workspace prefix when the path starts with it; otherwise it uses the first package-path root that contains it (an absolute root matches every absolute path and relativize leaves the path unchanged). Zel stores `attempt_to_print_relative_paths` and never rewrites the path.
  - [P2] Package roots start empty and are replaced when the package locator is created. Zel never records those roots, so a later locator cannot change what is printed.
  - [P3] Line and column are signed 32-bit values, so any non-zero value, including a negative one, is printed, and a zero line suppresses the column. Zel stores `usize`, so zero is still printed and a negative coordinate cannot occur.

### `src/main/java/com/google/devtools/build/lib/runtime/MemoryPressureEvent.java`
- Bazel lines: 60
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel value records a JVM GC memory-pressure event (`wasManualGc`, `wasGcLockerInitiatedGc`, `wasFullGc`, tenured used and max bytes, and GC duration). The test builder defaults the three booleans to false.
  - [P3] `percentTenuredSpaceUsed` is absent: Java returns `(int) ((tenuredSpaceUsedBytes * 100L) / tenuredSpaceMaxBytes)` (wrapping `long` multiply, truncating division, `ArithmeticException` when max is 0). OS `system::memory_pressure` and the `MemoryPressureStats` proto do not compute that ratio.

### `src/main/java/com/google/devtools/build/lib/runtime/MemoryPressureListener.java`
- Bazel lines: 195
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No listener registers on tenured garbage-collector beans or rejects startup when none exist.
  - [P3] GC notifications are not turned into memory-pressure events (tenured used/max after GC, skip max of 0, first tenured pool only, manual `System.gc()`, GCLocker, full GC, duration in milliseconds).
  - [P3] Events are not dispatched on a dedicated thread to the thrashing detector, the churn detector, and the current command event bus.
  - [P3] Invocation init, target-parsing completion, stats population, and reset of those handlers are absent.

### `src/main/java/com/google/devtools/build/lib/runtime/MemoryPressureModule.java`
- Bazel lines: 88
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No command-start path creates a GC thrashing detector and a GC churning detector from `MemoryPressureOptions`, installs them on a memory-pressure listener, constructs a high-water-mark limiter from the Skyframe executor and syscall cache, and registers the module and limiter on the command event bus.
  - [P3] No target-parsing-complete subscription passes the parsed target count into the listener.
  - [P3] No after-command path posts `MemoryPressureStats` (limiter stats plus listener stats), resets the listener, and clears the bus and limiter so the same listener can be reused on the next command.
  - [P3] No crash subscription posts those stats only while both the limiter and event bus are still set, skipping the post between commands or when a crash races with after-command cleanup.

### `src/main/java/com/google/devtools/build/lib/runtime/MemoryPressureOptions.java`
- Bazel lines: 190
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P2] `--experimental_oom_more_eagerly_threshold` is accepted as the old name of `--gc_thrashing_threshold`, but `oldNameWarning = false` suppresses the deprecation warning. Zel always emits `Option 'experimental_oom_more_eagerly_threshold' is deprecated: Use --gc_thrashing_threshold instead`.
  - [P3] `GcThrashingLimitsConverter` rejects a zero period (`0`, `0s`, `0ms`, `0ns`, and the other zero units) because `GcThrashingDetector.Limit` requires a positive period. Zel accepts those values.
  - [P3] A period whose length in seconds does not fit in a signed 64-bit `java.time.Duration` (`ofDays` / `ofHours` / `ofMinutes` use `Math.multiplyExact`) fails conversion in Bazel. Zel saturates that multiply into a `u64` duration and accepts the flag.

### `src/main/java/com/google/devtools/build/lib/runtime/NamedArtifactGroup.java`
- Bazel lines: 202
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs`
- Verdict: partial
- Differences:
  - [P1] `BepBuilder::named_sets` builds `NamedSetOfFiles` from a flat `File` list. `targets_completed` in `/workspace/zel-cursor/crates/zel-server/src/observability.rs` fills that list from configured-target output groups. There is no `expandSet`: no `NestedSet` leaf/non-leaf walk, no `STABLE_ORDER` rebuild, and no re-expansion when a single transitive set would be inlined.
  - [P1] Directory, tree, fileset, and runfiles-tree artifacts are not expanded. Bazel `visitArtifacts` skips runfiles trees, expands tree children, and (when fileset expansion is on) emits each fileset symlink as `filesetRootRelative/link.name` with the link target’s metadata. A normal expanded artifact that is still a directory, or a fileset wrapper that is not a fileset, fails `checkState`; any other leaf type throws `IllegalStateException`. Zel emits the artifact exec path as one file.
  - [P1] Each Bazel file uses the artifact root’s exec-path segments as `path_prefix` and the root-relative path as `name`, after `StringEncoding.internalToUnicode` (internal Latin-1 bytes decoded as UTF-8, invalid sequences replaced with U+FFFD). Symlink metadata sets `symlink_target_path`; existing files set lowercase-hex `digest` (only if non-null) and `length`. The context `PathConverter` supplies `uri`, and a proto with no file oneof (`FILE_NOT_SET`) is omitted. Zel always sets a `file://` URI of `{execroot}/{exec_path}` (`file_uri` percent-encodes UTF-8 bytes outside its allowlist; a path that already contains `://` is kept), never sets digest, length, or symlink target, and never drops a file. `path_prefix` is set only when the exec path has more than three segments and starts with `bazel-out`. Source artifacts are removed, and a failed target posts no named set.
  - [P1] The event id is the artifact-group namer’s name, reused so the same set is posted once, and `getChildrenEvents()` is empty (`file_sets` only reference other named sets). Zel uses a process-wide incrementing decimal id with no dedup. A positive `max_entries` (including 1) chunks the list into `{key}-{i}` sets; the parent lists those ids both in `file_sets` and as event children. Bazel splits only in the streamer, and only when `maxNamedSetEntries >= 2`, before this event is built.
  - [P1] `referencedLocalFiles()` returns one `LocalFile` per expanded leaf (resolved path, `LocalFileType` from metadata, and the metadata). The Zel named-set path does not.

### `src/main/java/com/google/devtools/build/lib/runtime/OptionsSupplier.java`
- Bazel lines: 58
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P3] Startup classes, classes for one command name, and classes common to every command are not returned by a supplier. They are the fixed `ALL_STARTUP_OPTIONS`, `COMMAND_OPTION_CLASSES`, and `COMMON_OPTION_CLASSES` tables. A command name is not matched against a supplier result, and a name that is not a key of `COMMAND_OPTION_CLASSES` has no class list.

### `src/main/java/com/google/devtools/build/lib/runtime/OriginalUnstructuredCommandLineEvent.java`
- Bazel lines: 59
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs`
- Verdict: partial
- Differences:
  - [P1] `postedAfter()` is the build-started id, so the streamer holds this event until `BuildStarted` is posted. `unstructured_command_line` leaves `posted_after` empty and emits as soon as it is posted.
  - [P2] For `run` with `--experimental_run_bep_event_include_residue` left at its default `false`, Bazel posts the `REDACTED_UNSTRUCTURED_COMMAND_LINE_EVENT` payload `["REDACTED"]`. Zel always posts the command name plus the real argument vector and never reads that flag.

### `src/main/java/com/google/devtools/build/lib/runtime/PassiveExperimentalEventHandler.java`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] When color is off, Bazel registers this wrapper on the event bus around the spare ANSI-allowing `UiEventHandler`. It forwards only `NoBuildEvent` and `NoBuildRequestFinishedEvent` and does not deliver any other UI events to that handler. Zel has no second handler and no such subscription filter.
  - [P3] `NoBuildEvent` with `showProgress() == true` only sets that handler’s `buildRunning`. Any other `NoBuildEvent`, and every `NoBuildRequestFinishedEvent`, calls `completeBuild`, which is a no-op unless `buildRunning` is already true. Otherwise it clears `buildRunning`, marks the tracker complete, stops the update thread, flushes the terminal and stdout/stderr buffers, and clears then redraws a progress bar if one is showing. Zel’s `UiEventHandler` has no `no_build` or `no_build_finished` path.

### `src/main/java/com/google/devtools/build/lib/runtime/ProcessWrapper.java`
- Bazel lines: 205
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs`
- Verdict: partial
- Differences:
  - [P3] `fromCommandEnvironment` returns a wrapper only when `BinTools` has `process-wrapper` (`process-wrapper.exe` on Windows) and the OS is POSIX (Linux, macOS, FreeBSD, OpenBSD). Zel’s `process_wrapper_supported()` is `cfg!(unix)` and always runs the wrapper in-process, so a missing embedded binary still wraps.
  - [P1] `gracefulSigterm` starts as `--experimental_process_wrapper_graceful_sigterm` (default false). `addExecutionInfo` forces it on when `supports-graceful-termination` is present (value ignored) and leaves an already-true flag set. `build()` appends `--graceful_sigterm`. Zel never reads that option. Spawn sites copy only the execution-info key, and `process_wrapper::run` never reads `graceful_sigterm`: cancel sends SIGKILL immediately, with no kill-delay grace on an external SIGTERM.
  - [P2] With local options present, the kill delay is `Duration.ofSeconds` of the signed 32-bit flag after `Integer.decode` (optional `+`/`-`, decimal, leading-zero octal, `0x`/`#` hex), and `build()` prints `--kill_delay=` plus `toSeconds()`, including 0 and negatives. The flag is omitted only when those options are null. Zel re-parses the raw text as a decimal `u64` (`exec_phase.rs`), so `-1`, `+15`, and `0x10` fail and `010` is 10 seconds. An absent flag still defaults to 15s. Local and sandbox execution pass that duration into `run` as `as_secs_f64()` instead of a whole-second `--kill_delay=` argument. `CommandLineBuilder::build` uses `as_secs()`, which matches `toSeconds()` for a non-negative duration.

### `src/main/java/com/google/devtools/build/lib/runtime/ProjectFile.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel never resolves or caches a project file from the working directory, package path, and path. A missing or unparsable file is not a distinct failure, and a leading `+` residue stays an ordinary target.
  - [P2] Zel does not return a command-line-usable project-file name, or the option strings for a command such as `build` or `test`, and it does not fail an unknown command at that boundary.
  - [P2] Cache invalidation when a package moves between package-path entries is absent. `experimental_allow_project_files` is only a flag-table boolean (default false) and is never read to load a project file.

### `src/main/java/com/google/devtools/build/lib/runtime/ProjectFileSupport.java`
- Bazel lines: 110
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] A non-null project-file provider and a residue whose first token starts with `+` load that token (minus the `+`) as a project file through the package path with a null output base, parse its per-command options at RC-file priority, emit an info event `Using <name>`, and post a project-file event. Zel leaves every `+…` token as ordinary residue and never loads or reparses a project file.
  - [P2] That path rejects more than one residue token with `Cannot handle more than one +<file> argument yet`, and rejects the load when `--experimental_allow_project_files` is false (the default) with `project file support is not enabled. Pass --experimental_allow_project_files to enable.` Zel registers the flag at default false and never checks it or raises either error.
  - [P2] `getTargets` drops only the first residue token when it starts with `+` and a provider is present, without checking the allow flag. Zel keeps the full residue, including a leading `+…` token.

### `src/main/java/com/google/devtools/build/lib/runtime/QueryRuntimeHelper.java`
- Bazel lines: 175
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/query_cmd.rs`
- Verdict: partial
- Differences:
  - [P2] `--output_file` is `Path::join`ed with the client cwd and opened only after the query buffer is complete. Bazel lexically normalizes `workingDirectory.getRelative(outputFile)` (`.` and `..` collapsed, symlinks left unresolved) and truncates the file when the helper is created, before evaluation. A crash during the query leaves the previous file contents in Zel and an already truncated file in Bazel. The open-error path string is the un-normalized joined path.
  - [P2] Bazel leaves an existing output file untouched when it fails before `create` (`query` invalid `--output`, unmet `--experimental_graphless_query`, `cquery`/`aquery` parse failure). Zel still creates or truncates that path and writes the empty stdout buffer. On `aquery --skyframe_state` with a non-proto `--output`, Bazel truncates the file before the format error, and Zel returns with the existing file unchanged.
  - [P1] An unopenable path still prints `ERROR: Could not open query output file <path>` and exits 36 (`QUERY_OUTPUT_WRITE_FAILURE`), after the query has already run; that run's stderr is dropped. Bazel reports the open failure before evaluation.
  - [P2] A `write_all` error is reported as `Could not close query output file <path>` with exit 36. Bazel reports a write or flush failure as `I/O error: ...` or `Failed to flush query results` (exit 36, `OUTPUT_FORMATTER_IO_EXCEPTION` or `QUERY_RESULTS_FLUSH_FAILURE`). A close or flush error after a successful write is ignored when the `File` is dropped; Bazel `close()` fails the command with the close message and `QUERY_OUTPUT_WRITE_FAILURE`.
  - [P1] A relative `--client_cwd` is anchored at the workspace by `getWorkingDirectory()`. Zel opens that `client_cwd` against the process working directory.

### `src/main/java/com/google/devtools/build/lib/runtime/QuiescingExecutorsImpl.java`
- Bazel lines: 166
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] Zel never builds the Skyframe quiescing executors. Evaluation uses one OS-thread pool (`EvaluationOptions.threads`: host parallelism capped at 32, or 4; package loads hardcode 4). It does not take analysis size from `--loading_phase_threads`, execution size from `--jobs`, or globbing size from `--legacy_globbing_threads`, and it does not set those to 0 when the option class is absent so later use fails the `> 0` checks.
  - [P2] No CPU-heavy SkyKey pool. `--experimental_skyframe_cpu_heavy_skykeys_thread_pool_size` is only declared. Bazel adds that second pool when the size is `> 0`, and the merged analysis+execution executor requires analysis, execution, and CPU-heavy sizes all `> 0`.
  - [P3] No third execution lane. With `--experimental_async_execution`, Bazel uses a virtual-thread work-stealing pool of size `max(min(5000, asyncExecutionMaxConcurrentActions), jobs)`; otherwise a pool of size `jobs`. Both are fail-fast with `ParallelEvaluatorErrorClassifier`.
  - [P2] The action scheduler only overlaps the async flags: if async execution is on and the parsed max is `> 0`, worker count becomes `max(jobs, max)` with no cap at 5000. An omitted flag becomes 0, not the default 5000. Those workers are OS threads limited by the action count, not a Skyframe virtual-thread pool.

### `src/main/java/com/google/devtools/build/lib/runtime/RcChunkOfArgs.java`
- Bazel lines: 62
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/rc.rs`
- Verdict: partial
- Differences:
  - [P3] Zel stores each rc option as `(option, source_path)` in `BazelrcConfig.sections` (`BTreeMap<String, Vec<(String, String)>>`), not as one value per uninterrupted file stretch. There is no chunk `equals` (path plus order-sensitive arg list) or `hashCode` (signed 32-bit sum of the Java `String` and `List` hashes).
  - [P2] Command rc notes regroup those pairs in a `BTreeMap` keyed by path, so two stretches of the same file split by an import become one group and notes follow sorted path order. Java keeps each stretch as its own chunk and walks chunks in read order.
  - [P3] Config-definition notes split when the source path changes between consecutive options, so those notes follow chunk boundaries.

### `src/main/java/com/google/devtools/build/lib/runtime/RemoteRepoContentsCache.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] `addToCache` is absent: a fetched repository and its marker are never uploaded to a remote action cache keyed by the predeclared-input hash (and the recorded-input chain).
  - [P2] `lookupCache` is absent: fetch never restores a repository from that remote cache, never returns a hit, and never signals Skyframe `valuesMissing()` so the caller can restart.
  - [P2] `--experimental_remote_repo_contents_cache` is parsed and restart-relevant, but enabling it does not change fetch. The on-disk `RepoContentsCache` is the local contents cache, not this remote cache.

### `src/main/java/com/google/devtools/build/lib/runtime/RepositoryRemoteExecutor.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No repository remote-execution API takes a command argument list, a path-sorted map of remote staging path to host file, execution properties, an environment map, a working directory, and a timeout, and returns an exit code plus raw stdout and stderr bytes.
  - [P3] `ExecutionResult` stores the exit code as a Java `int` and stdout/stderr as unmodified `byte[]`. Zel never builds that remote result.
  - [P2] An empty working directory meaning the remote system chooses the directory is unimplemented. Local `repository_ctx.execute` always runs on the host and defaults the working directory to the repository directory.
  - [P3] `IOException` and `InterruptedException` from this call have no counterpart.

### `src/main/java/com/google/devtools/build/lib/runtime/RepositoryRemoteHelpersFactory.java`
- Bazel lines: 28
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel type supplies an optional repository remote executor and an optional remote repo-contents cache to repository fetch.
  - [P3] Both helpers stay absent on every command. Bazel treats a null factory as both null, and a live factory can return a null executor while still returning a remote repo-contents cache.
  - [P2] `zel-bzlmod`'s on-disk `RepoContentsCache` is the local contents cache, not this remote-helpers factory. `--experimental_remote_repo_contents_cache` is only a flag (default false) and does not build one.

### `src/main/java/com/google/devtools/build/lib/runtime/SafeRequestLogging.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel never builds the bracketed request log (`[arg, arg, …]`) or redacts a leading `--client_env=<name>=` when `<name>` contains `auth`, `pass`, `cookie`, `token`, or `api_key` (ASCII case-insensitive) by keeping only the matched prefix and appending `__private_value_removed__`.
  - [P3] Sensitive terms in the value, non-`--client_env` arguments, and `--client_env` entries with no second `=` stay intact in Bazel; Zel has no equivalent filter. The server log records only the command name and exit code.

### `src/main/java/com/google/devtools/build/lib/runtime/ServerBuilder.java`
- Bazel lines: 188
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No module registration builder accumulates commands, info items, query functions, or query output formatters in registration order, and null elements are not rejected.
  - [P3] An unset query-environment factory is not replaced by a default `QueryEnvironmentFactory` on read. A second `setQueryEnvironmentFactory` is not rejected, and a null factory is not rejected.
  - [P3] `addInvocationPolicy` does not protobuf-`mergeFrom` policies in call order (`flag_policies` append) and does not reject null.
  - [P3] Duplicate info-item names are not failed when the map is built (`ImmutableMap.Builder.buildOrThrow`). The insertion-ordered name-to-item map is absent.
  - [P3] Named build-event artifact uploader factories are not stored by name (same name replaces; the built map iterates in `TreeMap` string order).
  - [P3] The repository remote-helpers factory is not optional, nullable, and last-assignment-wins.
  - [P3] There is no instrumentation-output factory builder to extend or build.

### `src/main/java/com/google/devtools/build/lib/runtime/SkymeldUiStateTracker.java`
- Bazel lines: 252
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs`
- Verdict: partial
- Differences:
  - [P2] Before any phase (`BUILD_NOT_STARTED`), the bar is only an optional timestamp. Zel paints an execution line (` no actions running`) and, unless short, downloads.
  - [P2] `ANALYSIS_AND_EXECUTION` prints the analyzing line, a newline, then execution progress once `executionPhaseStarted` is set. Zel drops the execution section whenever a status string is set, so analysis and execution never share the bar.
  - [P2] `analysisComplete` goes to `ANALYSIS_COMPLETE` only from `CONFIGURATION` (next bar has no phase line) and to `EXECUTION` from every other status. Zel always clears the status and then paints execution progress.
  - [P2] `progressReceiverAvailable` goes to `ANALYSIS_AND_EXECUTION` only from `CONFIGURATION`; from `LOADING_COMPLETE` or any earlier status it jumps to `EXECUTION` and drops the loading/analyzing line. Zel has no such transition.
  - [P2] The execution block is skipped until `executionPhaseStarted`. Zel paints ` no actions running` whenever the status is empty and the build is not finished.
  - [P2] After completion the bar is `INFO:` or `FAILED:` (ok vs fail color) with an empty message, because `buildComplete` clears `additionalMessage` before the next paint. Zel leaves that line off. The separate completion event still matches (`Build completed successfully…` / `Build did NOT complete successfully`).
  - [P2] Post-build `Waiting for remote cache: …` upload/download lines are gated by `buildCompleted()` (`BUILD_COMPLETED`). Zel never emits them.
  - [P2] `loadingComplete` uses the raw `int` size (`10000 targets`). Zel’s `format_count` inserts separators from 10,000 (`10,000 targets`); that string is also what `analysisComplete` returns.
  - [P2] Package and analysis clauses are appended only when the package receiver is non-null. Empty phase messages are concatenated without parentheses (`Loading: 3 packages loaded, 1 target configured`); a non-empty message uses ` (…)` . Zel always parenthesizes, and it appends analysis counts with package progress off.
  - [P2] Those clauses are the receiver strings: ungrouped `N packages loaded` / `currently loading: … (N packages)`, and analysis text that includes ` (N remote cache hits)` for downloaded targets and aspects. Zel groups those counts from 10,000 and omits remote-cache hits.
  - [P2] Multiple open Build Event Protocol transports are walked from a `HashSet` (no announcement order). Zel prints them in announcement order.

### `src/main/java/com/google/devtools/build/lib/runtime/SpawnStats.java`
- Bazel lines: 164
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - [P3] `ProcStats::summary` follows the same row order (`action cache hit` if > 0, then `disk cache hit` / `remote cache hit`, then `internal` when spawn-actions < total, then other runners) but prefixes `INFO: `, which `convertSummaryToString` does not. Total 0 still short-circuits both (`0 processes.` vs `INFO: 0 processes.`), omitting hits and runners.
  - [P2] With total > 0 and no other row, Java returns `N process(es).` Java’s `getSummary` also zeros `disk cache hit` and `remote cache hit` in the multiset, so a later call drops them. Zel always inserts `: ` (`INFO: N process(es): .` when the breakdown is empty) and clones the map, so repeats stay stable.
  - [P2] Other runner names sort by Rust `String` byte order (Unicode scalars). Java uses `String.compareTo` (UTF-16 code units), so a supplementary character sorts before U+E000–U+FFFF in Java and after them in Zel. An unpaired surrogate cannot be a Rust key.
  - [P3] Action, runner, and cache-hit counts are `usize`. Java uses signed 32-bit counters (`AtomicInteger`, multiset, summary `Integer`) that wrap, and `internal` is a wrapping `int` subtraction. Each spawn’s signed `executionWallTimeInMs` is added into a wrapping `long`. Zel never accumulates wall time, and there is no `getExecKindFor` (Java keeps the last `ExecKind.toString()` — `Remote`, `Local`, `Worker`, `Other` — and returns null if that runner was never stored).
  - [P3] A runner named `total`, `internal`, or `action cache hit` makes `buildOrThrow` throw once that key is already in the map. Zel prints a second segment. A zero count left in the non-priority map is printed. Java’s multiset does not keep zeros.
  - [P2] Cache hits are the last `ActionCacheStatistics.getHits()` (`int32`, shown only if > 0), not a sum of actions. Zel adds one `usize` per cache-hit action it saw, plus imputed `RunfilesSupport` extras taken entirely from one representative output (all hits or all internal).
  - [P2] Java counts every `SpawnResult` (the action is non-internal if that list is non-empty), including failed actions that still spawned. Zel’s build phase counts only the last runner of a successful action, once. Any failure is internal, and a failure whose text contains `input file(s) do not exist` is omitted entirely.

### `src/main/java/com/google/devtools/build/lib/runtime/StarlarkOptionsParser.java`
- Bazel lines: 474
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/parser.rs`
- Verdict: partial
- Differences:
  - [P0] `--//` / `--@` values stay raw strings. Bazel loads the target, rejects `flag = False` and non-build-settings (`Unrecognized option`), follows aliases (cycle and `select()` on `actual` fail), and converts with `BUILD_SETTING_CONVERTERS`. Invalid ints, bools, and labels are not rejected with `While parsing option <flag>=<value>: '<value>' is not a <type>`.
  - [P2] Integers use `i64` decimal parse. Bazel `StarlarkInt` accepts `0x`/`0b`/`0o` and values outside `i64`, and rejects leading zeros (`010`). A leading-zero value can compare equal to the default and be dropped.
  - [P1] Booleans accept only exact `true`/`1`/`yes` and `false`/`0`/`no`. Bazel ASCII-casefolds and also accepts `t`/`y`/`f`/`n`, so `TRUE` or `t` equal to the default is still kept.
  - [P0] A bare `--//pkg:flag` is always `true` and `--no//pkg:flag` is always `false`, including non-booleans. Bazel errors with `Expected value after` or `Illegal use of 'no' prefix on non-boolean option`. `--no//pkg:flag=value` is stored as the key `//pkg:flag=value` with value `false`. Bazel only strips `no` when there is no `=`, so the name is `no//pkg:flag`.
  - [P0] Repeated flags and `allow_multiple` / `repeatable` settings do not accumulate. Bazel appends in command order (`LinkedHashSet` drops duplicate set elements; other multi flags keep duplicates). Non-repeatable `string_list` is last-wins as one comma-separated list. Repeatable list flags take each occurrence as a single element. Zel joins every `string_list` occurrence with commas and then splits, so commas inside one occurrence become extra elements.
  - [P1] Spellings that canonicalize to one label (`//pkg:f` vs `@//pkg:f`) are separate `BTreeMap` entries, so sorted key order picks the winner. Bazel merges on the canonical label in command-line order.
  - [P1] Defaults are dropped only when a loaded setting's rendered string matches a string, int, bool, or string-list default. Label defaults and `allow_multiple`'s one-element default list are not compared, so those flags stay set. There is no `includeDefaultValues` path. Canonical output keeps the raw `--flag=value` in encounter order, including defaults. Bazel `canonicalize()` sorts by canonical label, stringifies converted values, omits defaults unless requested, emits one flag per accumulated element, and joins `string_list` / `string_set` with commas.
  - [P2] An explicit `scope` is ignored and not validated (`universal` / `target` / `project`, case-insensitive, original spelling kept). Every Starlark flag is treated as `DEFAULT`.
  - [P1] Flag aliases: `--alias` with no value is stored as `true` even for non-booleans, and `--noalias` is stored as `false`. Bazel rewrites the alias to the label and then applies the boolean-only `no` rules.

### `src/main/java/com/google/devtools/build/lib/runtime/SynchronizedOutputStream.java`
- Bazel lines: 132
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No stdout/stderr byte buffer flushes the build-event streamer once the buffered length reaches `maxBufferedLength` (default 10240). A write into an empty buffer is still taken whole even when it exceeds that limit, and the stream is then flushed.
  - [P2] No read-and-reset decodes the buffered bytes as UTF-8 (invalid sequences become U+FFFD) and splits that Java string into chunks of `max(maxChunkSize, maxBufferedLength)` UTF-16 code units (default chunk size 1048576). An empty buffer yields no chunks.
  - [P2] Writes are not deferred and flushed when stdout/stderr alternation requires a flush. Zel records each stdout/stderr string whole and, when several are pending, concatenates them into one progress payload.
  - [P3] `maxChunkSize <= 0` is not rejected.

### `src/main/java/com/google/devtools/build/lib/runtime/TargetProviderForQueryEnvironment.java`
- Bazel lines: 152
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs`
- Verdict: partial
- Differences:
  - [P2] A package Skyframe node that is only a cycle becomes `error loading package '<id>': Cycle encountered while loading package <id>` (`BuildFileContainsErrorsException`; main-repo `<id>` is the package fragment, not `//…`) and is not reloaded. Zel evaluates `PACKAGE_LOAD` in `/workspace/zel-cursor/crates/zel-skyframe/src/executor.rs` and reports a pure cycle as `cycle in dependency graph`.
  - [P3] A stored graph exception that is not a `NoSuchPackageException` is rethrown if unchecked, and otherwise replaced by `IllegalStateException("Unexpected Exception type from PackageValue for <id>")` with the original exception dropped. Zel returns that failure as a `PackageLoadError` string, or `package loading did not complete` when the node has no value.
  - [P3] The graph is only read. A missing node (no value, exception, or cycle) falls through to `PackageManager`. Zel always evaluates the package key (`keep_going`, 4 threads) and does not have that miss-only fallback.
  - [P2] With `--experimental_lazy_macro_expansion_packages` (default empty), a target in `PackagePiece.ForMacro` loads its BUILD file from the `ForBuildFile` graph node. Any exception or cycle there is `IllegalStateException("Bug in package loading machinery: failed to load package piece for BUILD file of already-loaded target <target>", cause)`. An absent node delegates, which can instead fail with `… unexpected error while retrieving package piece …` if that load recorded events. Zel does not build package pieces; the flag is unused and macros are expanded in the monolithic package. The BUILD file is that package’s BUILD `InputFile`.
  - [P2] `getTarget` on a loaded package is only `Package.getTarget`. Zel `TargetEnv::info` also turns a missing target whose name ends with `.bzl` into an `InputFile`.
  - [P2] Siblings are the package’s `ImmutableSortedMap` values (Java UTF-16 `String` order). Zel walks a `BTreeMap` (UTF-8 byte order), so names containing supplementary-plane characters can sort differently.

### `src/main/java/com/google/devtools/build/lib/runtime/TargetSummaryEvent.java`
- Bazel lines: 123
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel never builds or posts a `TargetSummary` event. Bazel sets `overall_build_success` unconditionally, and sets `overall_test_status` only when the build succeeded, a test summary is expected, and the status is non-null (otherwise the field stays unset).
  - [P2] Bazel orders the event after `targetCompleted` for the original label and configuration id (a null configuration is the `"none"` id), and also after `testSummary` when a test summary is expected. Children are empty, and a non-target-summary id is rejected. Zel does none of this.
  - [P3] `ids::target_summary` in `/workspace/zel-cursor/crates/zel-events/src/bep.rs` only builds a `TargetSummaryId` and is never called, so it does not emit the payload or the order constraint.

### `src/main/java/com/google/devtools/build/lib/runtime/TargetSummaryPublisher.java`
- Bazel lines: 409
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel never posts a BEP `TargetSummary` (`overall_build_success` and `overall_test_status`). `--experimental_bep_target_summary` is not read, so the publisher is never enabled.
  - [P2] No per-target aggregation of target completion, top-level aspect completions, or test status. Missing: the skymeld queue for aspect completions that arrive before the aggregator, the build-complete backstop (unanalyzed aspects expected as `Integer.MAX_VALUE` successes), skipping tests that are not built, and not expecting a test summary for aliases.
  - [P3] `ids::target_summary` in `/workspace/zel-cursor/crates/zel-events/src/bep.rs` only builds a `TargetSummaryId`. It does not publish the summary.

### `src/main/java/com/google/devtools/build/lib/runtime/TerminalTestResultNotifier.java`
- Bazel lines: 347
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - [P2] `FAILED_TO_BUILD` listing stops after 5 targets, then prints `(Skipping other failed to build tests)`. With `--noverbose_test_summary` those target lines are omitted. Zel prints every failed-to-build target in either case. Both trailers still count all of them.
  - [P1] `--test_summary=terse` drops only `PASSED` and `NO_STATUS`. Zel also drops `FLAKY` and `BLAZE_HALTED_BEFORE_TESTING`. `short_uncached` and `detailed_uncached` hide a cached line only for `PASSED` when `!actionRan()`. Zel also hides cached `FLAKY`.
  - [P2] `BLAZE_HALTED_BEFORE_TESTING` produces no per-target line. Zel prints one except under `terse`. Both still count it as skipped.
  - [P2] Duplicate labels are printed with the configuration mnemonic. Zel never adds it. Summary order is cached first, then uncached run count, then status (`PASSED` sorts as -1 and `FLAKY` stays at 2), label, configuration checksum, and case count. Zel sorts `FLAKY` with passes and stops at the label.
  - [P2] Failures with `ranRemotely()` are reported as `N fail(s) remotely`. Zel has no remote bucket and counts them as `fails locally`.
  - [P1] Passing, failing, and skipped counts on the test-case line and both trailers are ANSI-colored when color is on. Zel prints plain text. An empty result list is `Executed N out of M tests: nothing.`; Zel prints no trailer.
  - [P1] Passing cases are `total - failed - unknown - skipped`, so unknown cases are neither passing nor failing. Zel also counts a missing case tree as pass or fail from the target status, and every cached target as one unknown case rather than its XML. Detailed mode skips case lines for cached tests and never prints the partial-shard disclaimer.
  - [P2] `--check_tests_up_to_date` with tests still to run uses the normal filters, so `short` still prints `NO_STATUS` lines, and `detailed` or `testcase` still print the test-case line and the size footer. Zel returns early, drops those `NO_STATUS` lines unless the format starts with `detailed`, and omits the case line and size footer.
  - [P1] Locally cached logs are printed before the summary for `--test_output=all`, or `errors` when the status is not `PASSED` or `FLAKY`, with a status-colored header and each failed or passed log. Zel dumps a log only for `all` and always labels it `PASSED`.
  - [P2] The “specified size is too big” footer follows the max test-process time. Zel uses overall `duration_ms`, so the footer can fire for a different set of passed tests.
  - [P2] `Executed` counts targets whose action ran, including a run that ended `NO_STATUS`. Zel excludes `NO_STATUS` and `BLAZE_HALTED_BEFORE_TESTING`.

### `src/main/java/com/google/devtools/build/lib/runtime/TestResultAggregator.java`
- Bazel lines: 344
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - [P1] Shard and run statuses combine by the higher `BlazeTestStatus` number (`FAILED` replaces an earlier `TIMEOUT`; `REMOTE_FAILURE` outranks both). Zel keeps the first non-passing status, and a stored status value 6 becomes `NO_STATUS`.
  - [P0] `--runs_per_test_detects_flakes` is decided per shard once that shard has `runs` results: `INCOMPLETE` counts as cancelled, `PASSED` and `FLAKY` count as passes, and shard statuses are then max-aggregated, so one passing shard and one failing shard stay `FAILED`. Zel marks the whole target `FLAKY` when any attempt passes and any fails, and it never treats cancelled runs as `INCOMPLETE`.
  - [P2] Unfinished targets become `NO_STATUS` when stopping on the first failure, and `INCOMPLETE` when they already have a status. Targets that never ran become `BLAZE_HALTED_BEFORE_TESTING` on a catastrophe, `NO_STATUS` for `--check_tests_up_to_date` or stop-on-first-failure, and `FAILED_TO_BUILD` otherwise. Events after the summary is posted are ignored. Zel never assigns `INCOMPLETE` or `BLAZE_HALTED_BEFORE_TESTING`; `--notest_keep_going` keeps a finished target's status and marks only targets that have not started `NO_STATUS`; a build failure is `FAILED_TO_BUILD` except under `--check_tests_up_to_date`.
  - [P1] Filtered tests stay in the summary and print as `SKIPPED`. Zel omits them, so they do not appear and do not count as skipped.
  - [P2] The size warning uses the max test-process time of each action while the aggregated status is still `PASSED` (the unreported flag remains if a later shard fails), including cached actions. The band is the symbolic timeout; the eternal fuzzy range extends to one year, and the seconds value is cast to a signed 32-bit int. Zel uses the target's max duration only when the final status is `PASSED`, skips the verbose line on a cache hit, and classifies 60/300/900/3600 seconds or else the size, so a coverage-doubled timeout or `--test_timeout` can select another band and an eternal run warns from 3240s.
  - [P1] Cached versus ran is per action: a local or remote cache hit increments the cached count, a local hit also increments the local-cache count, any other action sets action-ran, and each cached attempt is posted. Zel caches the whole target, leaves the remote-strategy bit false, and posts no per-attempt events for a hit.
  - [P2] Timing keeps the earliest start, the latest stop, and the sum of each action's run duration. Zel reports the max attempt time as the run duration and sums every attempt, including failed flaky attempts.

### `src/main/java/com/google/devtools/build/lib/runtime/TestResultNotifier.java`
- Bazel lines: 30
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No callback that receives the full set of test summaries together with the count of targets that actually ran. Zel writes summaries in the test command and passes `(label, status)` pairs plus a `usize` executed count straight into `format_stats`.

### `src/main/java/com/google/devtools/build/lib/runtime/TestSummary.java`
- Bazel lines: 696
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - [P2] Summary sort uses cached-first, then `testTimes.size() - numCached`, then status (`PASSED` is -1, every other status including `FLAKY` is its proto number), then `Label` order (package, then name), configuration checksum, and total test-case count. Zel’s uncached key is `runs` for an uncached target and 0 when `cached` is set, so sharded and partially cached targets move. `FLAKY` shares `PASSED`’s key because `is_passed` includes it. Equal labels stay in input order.
  - [P1] A `TestSummary` event is posted for targets that never ran. Zel posts a `TestResult` and skips the summary when `attempts` is empty. A halted target that does get a summary is emitted as `NO_STATUS`; Bazel maps `BLAZE_HALTED_BEFORE_TESTING` to `TOOL_HALTED_BEFORE_TESTING`. Zel has no `REMOTE_FAILURE` status.
  - [P1] BEP `total_run_count` is the number of recorded test times (runs × shards × attempts). Zel sends the number of `(run, shard)` units. `attempt_count` is the maximum per-shard sum of those times, so attempts accumulate across runs. Zel uses the longest attempt list inside one `(run, shard)`. `run_count` and `shard_count` are the configured `TestParams` (`shard_count` 1 stays 1). Zel writes `shard_count` 0 when only shard index 0 exists, and derives `run_count` from the observed unit count (divided by shard count when that count is greater than 1).
  - [P1] First start is the minimum action start and last stop is the maximum of `start + duration`. Both the `Timestamp` fields and `first_start_time_millis`, `last_stop_time_millis`, and `total_run_duration_millis` are set. Zel uses one `start_time_ms`, sets last stop to `start + max(attempt duration)`, and leaves those three millis fields at 0. Both sum attempt durations into `total_run_duration`.
  - [P1] Passed and failed logs are every action log, in arrival order, dropped when the path converter returns null, and declared as `SUCCESSFUL_TEST_OUTPUT` / `FAILED_TEST_OUTPUT` local files. The summary is ordered only after `targetCompleted`. Zel keeps the last attempt of each `(run, shard)`, lists them in `BTreeMap` key order, always emits a URI, attaches no local-file types, and orders the summary after each test-result id.
  - [P1] Those Zel URIs are `file://` plus `Path::display()` (invalid UTF-8 becomes U+FFFD, no percent-encoding). Bazel builds a `file` URI from the path string and percent-encodes it with `java.net.URI`.
  - [P1] Leaf collection counts only a childless `TEST_CASE`. Zel also counts a childless node that has a name, so an empty suite or other non-case is a case. An unset case status is `PASSED` (proto2 default) in Bazel and is filed as failed in Zel. `run` defaults to true in both, so only an explicit false is skipped.
  - [P2] `mergeFrom` (alias targets and validation rewrite) rebuilds failed cases through a `TreeMap` keyed by `className + "." + name` (UTF-16 order, later entry wins) and sets failed-case status to `PARTIAL` when the merged statuses differ. Zel keeps every failed leaf in walk order.

### `src/main/java/com/google/devtools/build/lib/runtime/TestSummaryPrinter.java`
- Bazel lines: 324
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`
- Verdict: partial
- Differences:
  - [P2] `print` emits nothing for `BLAZE_HALTED_BEFORE_TESTING`, and nothing for `FAILED_TO_BUILD` when the verbose summary is off. Zel still prints those status lines for short and detailed summaries.
  - [P1] A filtered-out test is printed as `SKIPPED` (warning color) even when the underlying status is `NO_STATUS`. Zel omits size, tag, timeout, and lang filter exclusions instead of printing `SKIPPED`.
  - [P2] The cache prefix is `(cached) ` only when every run was cached, and `(%d/%d cached) ` when only some were. Zel has a single cached flag, so it never prints the fraction. On a cache hit, `attempts` is empty, so the `failed in N out of M` / `in N out of M` clause is dropped for cached flaky, failed, and timeout runs.
  - [P2] `timeInSec` uses `String.format` half-up (`%.1f`). Rust `{:.1}` is half-even, so an exact halfway value such as 1250 ms is `1.3s` in Bazel and `1.2s` in Zel. The same rounding applies to the verbose size warning.
  - [P1] The summary label is `getDisplayForm` (apparent repo name), with ` (configuration mnemonic)` when one label has two configurations. Zel prints `canonical_form` (`@@repo//…`) and never the configuration suffix. Padding uses UTF-16 code units, so a non-BMP character in the label is wider than Rust’s scalar width and shifts the status column.
  - [P1] Passed status is green, flaky and skipped are magenta, and other statuses are red, including each test case. Zel prints plain text. `REMOTE_FAILURE` is shown as `REMOTE FAILURE`; Zel has no such status (cache value 6 becomes `NO STATUS`).
  - [P1] Detailed summaries still list individual cases for cached tests, and only `TEST_CASE` leaves. Zel skips cases when `cached` is set, and also lists a non-case leaf that has a name. The partial-shard line `(some shards did not report details, list of failed test cases incomplete)` is never printed. Zel also prints the size warning in detailed mode; Bazel prints summary warnings only when it is not showing every case.
  - [P1] The non-detailed path prints each existing failed log and every summary warning. Zel prints synthesized paths (reverse run order, every `runs_per_test` log including passing runs, and the primary `test.log` even if it is missing) and only the size warning, not for cached passes. Non-empty coverage files are not listed.
  - [P1] `printCachedOutput` prints `{STATUS}: {canonical label} (see failed logs, then passed logs)`, then `INFO: From Testing …`, then failing logs and/or passing logs (`--test_output=all`, or `errors` for failures). A read error prints `Could not read test output for …`. The size check uses the raw file length, and the log bytes are copied unchanged. Zel reprints cached logs only for `--test_output=all`, always as `PASSED` with a single `test.log`, compares the stripped length, inserts a newline before the footer when the body has none, and skips the read-error line.

### `src/main/java/com/google/devtools/build/lib/runtime/ThreadDumpModule.java`
- Bazel lines: 282
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel only records `--experimental_enable_thread_dump`, `--experimental_thread_dump_interval`, and `--experimental_thread_dump_action_execution_inactivity_duration`. Nothing reads them to dump threads.
  - [P2] Bazel writes no dumps when the enable flag is off, and warns and returns when both durations are zero. Zel never warns or dumps.
  - [P3] With streaming log uploads, Bazel builds a BEP uploader and turns an invalid package-path symlink into `LOCAL_ENVIRONMENTAL_ERROR`. Zel does not.
  - [P3] Bazel deletes and recreates `<output_base>/server/thread_dumps` before the command, with the same abrupt exit on I/O failure. Zel does not.
  - [P3] A non-zero interval schedules dumps at that period in milliseconds. Inactivity dumps run only when now is at or after the last completed action plus the inactivity duration and strictly after the previous dump plus that duration (`lastDumpAt` starts at the epoch). Zel does neither.
  - [P3] Each dump is analyzed into `thread_dump.<pid>.<UTC yyyyMMddHHmmss>.txt` and published as a build-event artifact or an output-base instrumentation file. Dump or analysis I/O failures are logged and the task continues. Zel writes no such files.
  - [P3] Build completion cancels the scheduler, publishes the outputs, and releases the uploader. `afterCommand` shuts the task down without publishing if it is still running. A second live task is rejected. Zel has none of this lifecycle.

### `src/main/java/com/google/devtools/build/lib/runtime/UiEventHandler.java`
- Bazel lines: 1168
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs`
- Verdict: partial
- Differences:
  - [P1] STDOUT/STDERR events are raw `getMessageBytes()` (a `byte[]` message is copied unchanged). Text events are written with `String.getBytes()` (JVM default charset). Rust writes every `message` as UTF-8, so bytes that are not valid UTF-8 cannot pass through.
  - [P2] Debug mode writes `Event.toString()` (`KIND <location or <no location>>: message`, plus `... with STDOUT:` / `... with STDERR:` and the raw bytes) to stdout as ISO-8859-1, truncating code points above U+00FF to the low 8 bits. Rust writes its `Display` form (`KIND: …`, no `<no location>`, no attached process output) as UTF-8 on stderr.
  - [P2] Attached process stdout/stderr is inlined after the event, or replaced by a Latin-1 skip note when larger than `--experimental_ui_max_stdouterr_bytes`. The Rust handler never does this. A later server path prints action output on its own, treats `-1` as unlimited, and uses a different skip note.
  - [P2] Progress-bar wrapping counts UTF-16 code units, so a supplementary character is two columns and can be split into surrogates. Rust counts Unicode scalars and will not split one.
  - [P2] Incomplete event lines and progress-bar newlines use `System.lineSeparator()`. Rust always writes `\n`.
  - [P1] Locations omit a zero line and column, and `--attempt_to_print_relative_paths` rewrites absolute paths against the workspace and package roots. Rust always prints `file:line:column` and never applies that option.
  - [P2] "Current date is …" is a normal INFO event, so `--quiet`, `--ui_event_filters`, and debug mode apply, and phase entry emits it immediately. Rust inserts it from `handle_locked`, skipping those filters, and only on a later handled event.
  - [P1] The handler prints the test scrollback block (extra leading newline, WARNING for FLAKY and ERROR otherwise, skip PASSED / FAILED_TO_BUILD / BLAZE_HALTED_BEFORE_TESTING / NO_STATUS, and skip FAILED when there is exactly one failed log) and redraws the bar. Rust's `test_summary` only updates counters; the server printer uses different status rules, no SGR color, and a leading newline only on multi-attempt FAILED. `test_filtering_complete` does not refresh.
  - [P2] Build completion prints the final INFO/ERROR while the build is still running, redraws the bar under that line, and leaves the update thread running while uploads or fetches remain. Rust always stops the thread and clears the bar. `afterCommand` resets SGR and leaves the bar; Rust clears the bar and does not reset the terminal. `noBuild` / `noBuildFinished` (keep the bar alive, or `setBuildComplete` and redraw once) and the crash posting of in-flight actions are absent.
  - [P1] `refreshSoon` (action progress, completions, upload start) only arms the update thread and waits out `minimalUpdateInterval`. Rust draws on that call once the rate limit has elapsed. Java still starts the thread without curses and drops a draw if `updateLock.tryLock()` fails; Rust starts the thread only with cursor control, so a deferred no-curses update is lost, and it waits on the mutex and applies the draw. `stopUpdateThread` interrupts the sleep; Rust joins only after the current sleep. `loadingComplete` is rate-limited in Java and forced in Rust. `executionPhaseStarted` runs only when `countedInExecutionTime()`; Rust always runs it.
  - [P2] `buildStarted` does not set `buildRunning` or start the thread (progress stays off until main-repo mapping or `noBuild`). Rust `build_started` does both. `enableProgress` can turn the bar on when the option was false; Rust no-ops unless `show_progress` was already true.
  - [P2] The terminal title is `ESC ] 0 ; <full short bar> BEL` in the default charset. Rust sends `ESC ] 0 ; <first line> ESC \` as UTF-8.
  - [P2] Skymeld mode installs `SkymeldUiStateTracker`. Rust always uses `UiStateTracker`. `newStatsSummary` is never set, so the completion line always includes the action count.
  - [P2] The handler always emits SGR; when color is off, an outer stripper removes CSI, including cursor-up and erase-line. Rust omits SGR when color is off but still emits those cursor sequences when curses is on.

### `src/main/java/com/google/devtools/build/lib/runtime/UiOptions.java`
- Bazel lines: 315
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs`
- Verdict: partial
- Differences:
  - [P1] Boolean flags (`show_progress`, `isatty`, `emacs`, `show_timestamps`, `progress_in_terminal_title`, `attempt_to_print_relative_paths`, `experimental_ui_debug_all_events`) are case-insensitive in Java (`true`/`1`/`yes`/`t`/`y` and `false`/`0`/`no`/`f`/`n`). `UiOptions::from_flags` treats only the exact strings `0`, `false`, and `no` as false and every other present string as true, so `False`, `NO`, `f`, and `n` are false in Bazel and true in Zel.
  - [P2] `--emacs` forces color and curses off, so `use_color()` and `use_cursor_control()` are false. Java `useColor()` and `useCursorControl()` ignore `runningInEmacs`.
  - [P2] `--terminal_columns` and `--ui_actions_shown` use `Integer.decode` (leading-zero octal, `0x`/`#` hex, signed 32-bit). Zel parses a decimal `usize`. `010` is 8 in Bazel and 10 in Zel. A hex value such as `0x100` fails the `usize` parse and the default is kept (80 or 8). A negative value is stored in Java and left as that default in Zel.
  - [P2] `--show_progress_rate_limit` uses `Double.parseDouble` (trim of code points `<= ' '`, `f`/`F`/`d`/`D` suffixes, hex floats such as `0x1.0p1`). Zel `f64` parsing rejects those. `from_flags` then fails and the server installs `UiOptions::default()` for every UI flag, while Bazel keeps the parsed double and the other flags.

### `src/main/java/com/google/devtools/build/lib/runtime/UiStateTracker.java`
- Bazel lines: 1426
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs`
- Verdict: partial
- Differences:
  - [P2] Strategy text is only attached in the Caching and Running phases, in global bit order, space-separated, and names past 32 distinct strategies collapse to `unknown`. Zel appends ` (name, …)` in per-action order for any phase that recorded a strategy, with no 32-cap. Cache-check does not record a strategy, and an extra Uploading phase (`[Uploa]`) exists.
  - [P2] Java resets the action timer on each phase advance and creates state for scan, cache, schedule, run, and stop events. Zel leaves `start_nanos` at action start (or 0 if `set_now` was not called) and ignores those events when the id is unknown. Elapsed seconds saturate at 0. Java’s signed difference can go negative.
  - [P2] Several progresses are kept in insertion order and the first one is shown. Zel stores one progress slot, so a second id replaces the first and finishing a different id does not reveal the earlier one.
  - [P2] Test groups are only for labels registered by test filtering or `singleTestAnalyzed`, and the artifact is removed when the action completes. Zel adds every `TestRunner` on start, never removes it, and sorts ids, so `(N actions)` still counts finished actions. There is no flaky-vs-failed distinction and no `; last test:` line (Java also lays that out as ` 1 action` plus a following line).
  - [P2] Unknown action completion throws. Zel ignores it and does not increment the completed count. Scanning completion does not increment Java’s completed count but still notifies the execution receiver. Zel has no separate path and no inflight-mnemonic histogram.
  - [P2] Width uses Java UTF-16 `length`/`substring` (a cut can split a surrogate). Zel’s action and label shortening counts Unicode scalars. The test-group width budget uses UTF-8 byte lengths. Label shortening also differs: Java uses the display form, then a suffix from the first `/`, then the canonical label. Zel keeps the `:name` tail.
  - [P2] Fetches show `FetchProgress` text, seconds only after 3s, and `shortenUrl` (scheme, host, filename). Overflow is a same-line ` ... (N fetches)`. Zel always appends `; Ns`, ignores progress text, ellipsizes the end of the line, and adds ` ... and R more`. Progress before start is dropped.
  - [P2] Post-build `Waiting for remote cache: N uploads` is absent. With unlimited width (`targetWidth == 0`), Java still prints `Waiting for: <name> Ns` because `line.length() <= 0` is false. Zel prints `Waiting for build events upload: …`. Multiple transports follow HashSet order in Java and announcement order in Zel.
  - [P2] `hasActivities` is true for the whole build until completion and open uploads, fetches, and build-event transports are gone. Zel is false unless there is a status, package progress, an active action, the execution phase, a download, or a post-build transport. The time-dependent flag is true during package loading and false while a status string is set. Zel is true only for active actions, downloads, or transports.
  - [P2] The default sample size is 3 versus 8. Overflow is a same-line ` ...` with that width reserved on the last sample. Zel writes ` ... and N more`, and `N` includes actions already folded into a test-group line. Equal start times break by map order and identity hash in Java, and by start sequence in Zel.
  - [P2] Package, analysis, and “N targets” counts are raw `int`/`long` (they wrap). Zel groups at 10,000 and uses `u64`. The analysis fragment omits ` (N remote cache hits)`. The `[completed / total]` bracket is Java’s unique action-lookup set via the execution receiver. Zel turns it on at `execution_phase_started` and bumps a local counter from `action_started`. `loading_started` without package progress stays on `Loading` instead of `Loading: N packages loaded`.

### `src/main/java/com/google/devtools/build/lib/runtime/WorkspaceBuilder.java`
- Bazel lines: 261
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P2] Diff-awareness factories stay in insertion order, and the first factory whose `maybeCreate` returns non-null is used. With `--watchfs` (on Windows only together with `--experimental_windows_watchfs`) that is the OS watcher (WatchService, or FSEvents on macOS); otherwise the view is `EVERYTHING_MODIFIED` and known files are stat-scanned. Turning `--watchfs` off after it was on drops that watcher so the next diff is a full scan. Zel always installs `StatDiffAwareness` (`/workspace/zel-cursor/crates/zel-skyframe/src/executor.rs`, `/workspace/zel-cursor/crates/zel-skyframe/src/diff.rs`) and never reads `--watchfs`.
  - [P3] Duplicate extra `SkyFunction` names throw `IllegalArgumentException` when the immutable map is built. Zel’s `HashMap` `register_function` keeps the last function. In both, extras are applied after the builtins, so an extra can replace a builtin.
  - [P2] If no syscall cache was set, Bazel installs `DefaultSyscallCache` (stat and readdir, including cached `IOException`s, cleared between commands). Its initial capacity is `Runtime.maxMemory() >> 23` as a long, then 32 if that is ≤ 0, or 1024 after a bug report if it is greater than `Integer.MAX_VALUE`, then narrowed to `int`. Zel does not install that cache.
  - [P3] `allowExternalRepositories` defaults to true and the last call wins. When it is false, external-repository files are omitted from the missing-diff scan unless some were already seen. Zel always builds the executor with an external-repo resolver and has no such switch.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/AqueryCommand.java`
- Bazel lines: 201
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/post_analysis_query.rs`
- Verdict: partial
- Differences:
  - [P2] `--nobuild` is injected at computed-default priority, so an rc or command-line `--build` / `--nobuild=false` still executes after the query. Zel never runs the execution phase for `aquery`.
  - [P2] A whitespace-only query is parsed and fails with exit 2. Zel treats a trim-empty query as empty: success, or a Skyframe dump when `--skyframe_state` is set.
  - [P1] `--skyframe_state` does not evaluate the expression. An empty expression still dumps the action graph when `--universe_scope` is set. A non-empty expression with no target patterns only compiles `inputs`/`outputs`/`mnemonic` filters (one argument is allowed) and dumps. Zel rejects any non-empty `--universe_scope` on an empty expression, and evaluates every other non-empty expression instead of dumping.
  - [P1] `--skyframe_state` allows `--output=streamed_proto`. Zel accepts only `proto`, `textproto`, and `jsonproto`.
  - [P2] The "targets with `--skyframe_state`" error quotes the pretty-printed expression (`name(arg, arg)`). Zel quotes the raw query text.
  - [P2] Those quotes and syntax errors truncate at 1000 UTF-16 code units. Zel truncates at 1000 Unicode scalars.
  - [P1] An `inputs`/`outputs`/`mnemonic` call nested under another function, or an invalid filter pattern, fails before the build with exit 1 and `ERROR: <message>` followed by the expression. Zel does not reject nesting; a bad pattern fails later with exit 2 and does not print the expression.
  - [P3] A `StackOverflowError` during the build becomes exit 7 and `ERROR: Aquery output was too large to handle: <query>`. Zel has no such handler.
  - [P2] An interrupt while resolving the main repository mapping is `ERROR: Fetch interrupted: <message>` (exit 8). Zel does not emit that message.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/BuildCommand.java`
- Bazel lines: 112
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P2] The empty-target warning always says `bazel`, ignoring the configured product name.
  - [P1] With no patterns, Zel returns success before loading, analysis, execution, and `BuildStarted`. It prints `Found 0 targets` and a fixed `1 process: 1 internal` / `1 total action` trailer, and skips the analysis summary an empty `BuildTool.processRequest` still runs.
  - [P3] `--target_pattern_file` does not emit the `target_pattern_file` input-file event carrying the file size.
  - [P2] Pattern files are decoded as Latin-1. Java breaks lines on `\n`, `\r\n`, and a lone `\r`, and `String.trim` removes only code units `<= U+0020`. Zel breaks only on `\n` and `\r\n`, so CR-separated patterns stay one pattern with an embedded CR, and `trim` also drops Latin-1 Unicode whitespace `U+0085` and `U+00A0`.
  - [P2] Both pattern-file errors exit 2. Bazel records `TargetPatterns` `TARGET_PATTERN_FILE_WITH_COMMAND_LINE_PATTERN` or `TARGET_PATTERN_FILE_READ_FAILURE`. Zel records `command` / `OPTIONS_PARSE_FAILURE`, and a missing file is `I/O error reading from {path}: No such file or directory` instead of `I/O error reading from {path}: {path} (No such file or directory)`.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/CanonicalizeCommand.java`
- Bazel lines: 283
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/license.rs`
- Verdict: partial
- Differences:
  - [P1] No package-loading sync and no main-repo mapping. Starlark flags are not loaded as build settings: Zel only checks a main-repo `BUILD` or `BUILD.bazel`. A missing target in an existing package is accepted. `--@repo//` is not mapped, and `--no//` skips the package check. Missing-package errors omit the repo-mapping failure, `canonicalization interrupted`, and `Unknown error: ...` paths, and list only that package directory.
  - [P1] `--experimental_include_default_values` (default true) is parsed and ignored. Starlark flags are printed in command-line order with the raw text, including values equal to the build-setting default when the flag is false. Bazel appends them after native flags in `TreeMap` order (UTF-16 code units) using the converted value (`true`/`false`, comma-joined lists, one line per repeatable element).
  - [P1] Implicit-requirement values are included in the canonical lines. Bazel omits a value whose source is an implicit dependent. `--flag_alias` set before `--` is not applied to the residue.
  - [P2] `--canonicalize_policy` prints the filtered policy without expanding expansion flags or implicit requirements onto child flags. String escaping handles only `"`, `\`, and newline, so `'`, other controls, and non-ASCII differ from protobuf TextFormat octal escapes. A `--config` policy error adds "To add a --config to the command line, use config_policies instead." Undefined `SetValue` behavior fails on this path even when the flag is unknown or the policy's commands do not apply.
  - [P2] Policy enforcement rewrites only `SetValue` and `UseDefault` on the argv and does not expand those policies onto child flags. `AllowValues` and `DisallowValues` run later in the options parser, and allow/disallow on an expansion flag is not rejected.
  - [P1] Flags before `--` are recognized only as `--for_command=`, `--invocation_policy=`, and `--canonicalize_policy` / `=true` / `=false` / `--nocanonicalize_policy`. A separate value token and other booleans (`1`, `yes`, `0`, `no`) are ignored, so `--canonicalize_policy=1` prints canonical flags instead of the policy, and a two-token invalid `--for_command` is not rejected.
  - [P3] `FlagClashCanaryOptions` are absent, so `--flag_clash_canary` and its two expanders are unrecognized arguments.
  - [P2] An invalid `--for_command=` always lists `license`. Bazel omits it when no embedded LICENSE is present. Exit code is still 2. `FOR_COMMAND_INVALID`, `OPTIONS_PARSE_FAILURE`, `STARLARK_OPTIONS_PARSE_FAILURE`, `ARGUMENTS_NOT_RECOGNIZED`, `INVOCATION_POLICY_PARSE_FAILURE`, `INVOCATION_POLICY_INVALID`, and the interrupted failure detail are not distinguished.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/CleanCommand.java`
- Bazel lines: 331
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/clean.rs`
- Verdict: partial
- Differences:
  - [P1] Synchronous `--expunge` leaves `output_base/server` and the output-base directory in place, ignores per-entry delete errors, and still returns success. Bazel calls `prepareForAbruptShutdown`, closes stdout/stderr, then `deleteTreesBelow` and `deleteTree` on the whole output base before the command returns, and a failure exits 36 (`OUTPUT_BASE_DELETE_FAILURE` or `OUT_ERR_CLOSE_FAILURE`). Zel removes the remaining tree only after the command response, during server shutdown.
  - [P2] Asynchronous deletion is a `/bin/sh` in a new process group with the server environment and the temp name passed as `./$1`. Bazel runs the embedded `daemonize` helper (`-l /dev/null -p /dev/null -- /bin/sh /bin/sh -c`) with the client environment and the temp basename interpolated unquoted into `find` and `rm`. The info line prints the canonicalized temp path. Failure to start that deletion exits 36; Bazel uses exit 6 (`ASYNC_OUTPUT_BASE_DELETE_FAILURE` or `ASYNC_EXECROOT_DELETE_FAILURE`).
  - [P1] Synchronous non-expunge clean follows a symlinked `execroot` and deletes the target’s children, and it fails when `execroot` exists and is not a directory. Bazel `deleteTreesBelow` does not follow symlinks and leaves a non-directory `execroot` untouched. Individual unreadable directory entries are skipped.
  - [P1] Action-cache cleanup deletes only `action_cache/action_cache.bin` and ignores I/O errors. Bazel removes the `action_cache`, `action_cache.bad`, and `action_cache.tmp` trees and fails the command with exit 36 if that throws. The process-wide computed digest cache is left populated, so a later command in the same server can reuse pre-clean content digests for the same path.
  - [P2] `OutputService.clean()` is skipped. The local and remote-execution services are no-ops in Bazel; `BazelOutputService` sends a `Clean` RPC, and a failure there exits 6.
  - [P1] An empty `--symlink_prefix` is treated as `{product}-`. Bazel keeps the empty prefix, so the links are `bin`, `out`, and so on. A prefix containing `/` only attempts `remove_dir` from the parent downward, so `out/` and `out/dbg/` leave the prefix directory in the workspace; an absolute prefix can remove a directory outside the workspace. Bazel deletes that relative prefix directory and its empty parents, and it refuses an absolute prefix. A non-UTF-8 workspace or output-base name is turned into lossy UTF-8 (`U+FFFD`) for the `{prefix}{workspace}` symlink and the async temp directory name.
  - [P2] Command cancellation does not surface as `clean interrupted` (exit 8). `NoBuildEvent` and `CleanStartingEvent` are not emitted.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/ConfigCommand.java`
- Bazel lines: 468
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/config.rs`
- Verdict: partial
- Differences:
  - [P1] Zel builds the list from `registered_configurations` (hashes recorded by `short_hash`). Each non-exec, non-`core_only` key is emitted twice (untrimmed options and trimmed options), a `NoConfigTransition` config named `{compilation_mode}-noconfig` is appended whenever that registry is non-empty, `core_only` keys are labeled `fastbuild-noconfig`, and equal checksums are dropped. Bazel lists Skyframe’s done `BUILD_CONFIGURATION` nodes, ordered by options checksum, and `ImmutableSortedMap` throws if two keys compare equal on checksum.
  - [P2] A residue matches a hash prefix and also a prefix of `mnemonic` or `display_mnemonic` (` (exec)` / ` (test-trimmed)` included). If more than one config matches and any mnemonic hit is involved, the error is `Multiple configurations match this prefix. Please choose a specific checksum to disambiguate`. Bazel matches only `configHash.startsWith`; several hash hits produce the ambiguous-prefix message that lists those hashes.
  - [P2] Diff `configHash1` / `configHash2` are the resolved full checksums. Bazel forwards the raw residue strings into the diff.
  - [P1] JSON is selected only when the stored `--output` token is exactly `json`. `OutputTypeConverter` accepts any ASCII case (`JSON`, `Json`) via `toUpperCase` and `Enum.valueOf`.
  - [P1] Non-exec id lines use `base_mnemonic()` (no `ST-<hash>`). Exec id lines use `config_display_mnemonic()`. Bazel prints `BuildConfigurationValue.getMnemonic()` for every configuration (the output-path mnemonic, including the `ST-` hash when a transition changes it).
  - [P1] An empty `define` is omitted, and a non-empty `define` stays on the CoreOptions fragment. `user-defined` is filled only from Starlark options. Bazel removes `define` from CoreOptions and records each define as `--define:<name>` under `user-defined`.
  - [P2] Diff fragment names and option keys are ordered by Rust `String` (UTF-8 bytes). Bazel sorts them with `String.compareTo` (UTF-16 code units). BMP names match; a non-BMP character in a Starlark option label sorts differently.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/ConfigCommandOutputFormatter.java`
- Bazel lines: 161
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/config.rs`
- Verdict: partial
- Differences:
  - [P2] Text header `Displaying diff between configs …` and JSON `configHash1` / `configHash2` use the resolved full checksums. Bazel prints the raw ids stored on the diff, so a unique prefix stays unexpanded.
  - [P2] Option maps are walked in Rust `BTreeMap` order (Unicode scalar / UTF-8 byte order). Bazel uses `SortedMap` natural order (`String.compareTo`, UTF-16 code units), so keys that contain supplementary characters sort differently in text lines and JSON objects.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/CoverageCommand.java`
- Bazel lines: 136
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P1] `--collect_code_coverage` is applied at software-requirement priority after the command line, so `--nocollect_code_coverage` cannot turn it off. Zel prepends that flag at normal command-line priority, so a later `--nocollect_code_coverage` wins for the parsed option and for analysis. The test phase still treats the run as coverage because it checks the command name, not the flag.
  - [P1] Unless an rc file or the command line sets `--test_timeout`, coverage installs the computed default `--test_timeout=300,600,1200,3600` (short, moderate, long, eternal). Zel does not, so an unset timeout stays 60, 300, 900, 3600.
  - [P2] `--test_output=streamed` forces `--test_sharding_strategy=disabled` and `--test_strategy=exclusive` at software-requirement priority, replacing any user strategy for analysis and execution. Zel disables sharding and runs those tests exclusively, but leaves `--test_strategy=standalone` or `local` in effect, so the action still gets `no-sandbox` and `no-remote-exec`.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/CqueryCommand.java`
- Bazel lines: 214
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/post_analysis_query.rs`
- Verdict: partial
- Differences:
  - [P2] Computed-default `--nobuild` loses to an rc or command-line `--build`, so Bazel still runs the execution phase after the query. Zel never executes actions for `cquery`.
  - [P2] `--noanalyze` stops after target-pattern parsing and skips query evaluation, exiting 0 when the patterns parse. Zel still loads, analyzes, and evaluates the query.
  - [P1] Computed-default `--build_manual_tests` loses to `--nobuild_manual_tests`, so wildcard roots omit `manual` tests and those targets are absent from the cquery result. Zel always keeps them.
  - [P1] `--show_config_fragments` other than `off` sets `--include_config_fragments_provider` at computed-default priority, so an rc or command-line provider value stays in the configuration. Zel overwrites that option with the `show_config_fragments` value and changes the configuration checksum.
  - [P2] `--nonodep_deps` is rejected after analysis. The filter list omits `ONLY_TARGET_DEPS` and `NO_IMPLICIT_DEPS`, and includes `INCLUDE_ASPECTS` only when aspects are enabled. Exit code is 1, except with `--keep_going`, where the error is printed and the command exits 0. Zel fails before analysis with exit 1, always reports `[NO_NODEP_DEPS, INCLUDE_ASPECTS]`, and ignores `--keep_going`.
  - [P2] With an empty `--universe_scope`, a top-level `somepath` or `allpaths` resolves `PROJECT.scl` (`--scl_config`, `--enforce_project_configs`, Skyfocus) from only the first collected target pattern. Zel does not apply `PROJECT.scl`.
  - [P2] A main-repo mapping interrupt is reported as `Fetch interrupted: ...` and returns that exit code before the query. Zel has no equivalent report.
  - [P2] A syntax error is reported before analysis. The expression is truncated at 1000 UTF-16 code units. Zel can fail on the configuration or analysis-cache check first, and truncates at 1000 Unicode scalars, so a query longer than 1000 with non-BMP characters yields a different message.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/DumpCommand.java`
- Bazel lines: 719
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/dump.rs`
- Verdict: partial
- Differences:
  - [P2] `--packages` dumps done `PACKAGE_LOAD` values. Each header is the canonical package id (`//pkg` or `@@repo//pkg`) and the build-file path. Rules are kind plus label only, and every non-rule target is listed under `Files`. Bazel dumps `PACKAGE` values, uses the repository-less package path (`foo/bar`), prints each rule attribute as `name = value`, and lists only file targets under `Files`. Names sort by Unicode scalar in Zel and by UTF-16 code units in Bazel.
  - [P2] `--rule_classes` prints native classes only, with attribute names in `BTreeMap` order, including `$` attributes stored on the class. Bazel also prints Starlark rules loaded in the server, in UTF-16 string order, skips attributes whose names start with `$`, and uses declaration order.
  - [P2] `--rules` counts rule instances in loaded packages, always prints action count 0, sorts by count descending then name ascending, and prints plain integers with no name truncation (column width at least 4). Bazel counts analyzed configured targets and aspects (aspect labels keep the text after the last `%`), sorts by count descending with hash-map tie order, formats counts with locale grouping (`%,d`), and truncates names to 30 UTF-16 code units with a `...` prefix. With an allocation tracker, Bazel adds `BYTES` and `EACH` (`bytes / count`).
  - [P2] `--action_cache` prints the in-memory map in primary-output order as a 0-based index, digest, discovered inputs, and output-file metadata. A poisoned lock skips the dump and the command still exits 0. Bazel loads the persistent cache, sorts by integer record id, and also prints `packed_len`, timestamp, output-tree metadata, and the string indexer. An `IOException` reports `Cannot dump action cache: ...` and exits 7.
  - [P2] `--skylark_memory` always prints the memory-tracking message and leaves the filesystem untouched. Bazel writes a pprof profile when the allocation tracker is installed. An `IOException` on that path exits 8.
  - [P2] `--memory` accepts the same mode grammar (`shallow|deep|transitive|full`, `summary|count|bytes`, `needle=`, subject type) and then prints per-function node counts for a fixed function list. Bazel measures the Skyframe object graph for that subject, or the whole graph for `full`, and prints a summary line or a by-class count or byte map. A missing node or an ambiguous configuration exits 7.
  - [P2] `--skyframe` is parsed case-insensitively and then matched against the raw token, so `Summary` fails after a successful parse. `active_directories` and `active_directories_frontier_deps` exit 2. Bazel prints those Skyfocus key sets and accepts any case. Summary, count, value, deps, rdeps, and function_graph use a fixed function allowlist. The summary edge total is only that allowlist’s direct deps. Count ties break by function name. Value, deps, and rdeps list every matching key, including nodes that are not done, sorted by key string. Deps and rdeps are a flat sorted list. `function_graph` is a digraph with per-edge counts in function-name order. Bazel walks the whole graph in node-iteration order, skips nodes that are not done, groups deps, emits each function edge once, and on interrupt prints `aborting` and exits 8.
  - [P2] `--skykey_filter` is a custom Unicode-scalar `find` on the raw pattern. Bazel’s `RegexFilter` splits comma-separated inclusions and `-` exclusions and uses `java.util.regex` `find` on UTF-16 code units, where `.` does not match `\r`, U+0085, U+2028, or U+2029. The default `.*` matches every key in both.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/HelpCommand.java`
- Bazel lines: 643
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/help.rs`
- Verdict: partial
- Differences:
  - [P2] `help info-keys` and the completion variable `BAZEL_INFO_KEYS` omit the hardwired key `defaults-package` ("Obsolete. Retained for backwards compatibility."). `help info-keys` also always prints `bazel-bin`, `bazel-genfiles`, and `bazel-testlogs`; Java uses `{product_name}-bin`, `{product_name}-genfiles`, and `{product_name}-testlogs`. Completion variables do use the product name for those three.
  - [P1] Verbosity is not taken from the parsed `--help_verbosity` value. Only `--long`, `-l`, `--short`, and an inline `--help_verbosity=long|short|medium` (exact lowercase) before `--` change it. Java's converter accepts a following argument and any case (`--help_verbosity LONG`, `--help_verbosity=SHORT`), so those stay medium here.
  - [P2] `help flags-as-proto` orders `FlagInfo` messages by reversing each `HashMap` bucket. OpenJDK `HashMap` (Java 8 and later, including 21) links new entries at the tail, so a bucket iterates in insertion order. Field contents still follow the Java builder: startup first, then commands by name, first definition wins.
  - [P2] `help everything-as-html` adds a metadata-tag row for `FULLY_REDACTED_IN_LOGS`. That constant is not in Java's `OptionMetadataTag`.
  - [P2] Unknown command, extra residue, and a bad completion shell still exit 2 with the same `ERROR:` text, but the failure detail is `command` / `OPTIONS_PARSE_FAILURE` instead of `HelpCommand` `COMMAND_NOT_FOUND` or `MISSING_ARGUMENT`.
  - [P2] `help` does not post `NoBuildEvent`, so it does not emit the build-event `BuildStarted` (no command set) and the immediate finished event Java emits for this command.
  - [P3] `help --long` wrapping drops trailing spaces before a wrap and at the end of each paragraph, and the width counts Unicode scalars. Java's line `BreakIterator` keeps the break space on the line and counts UTF-16 code units.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/InfoCommand.java`
- Bazel lines: 354
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P1] A no-argument `info`, and `--show_make_env`, never builds a `BuildConfigurationValue` or syncs package loading. Bazel aborts with that configuration’s detailed exit code, or with `interrupted` and `command interrupted while syncing package loading`. `{product}-bin`, `{product}-genfiles`, and `{product}-testlogs` are therefore not the configuration output roots. `needsSyncPackageLoading()` is ignored, so `build-language` does not load Starlark builtins or fail with `Failed to load Starlark builtins`.
  - [P1] `--show_make_env` adds only `BINDIR`, `COMPILATION_MODE`, `GENDIR`, and `TARGET_CPU`. Bazel adds the full make environment (fragment variables plus last-wins `--define`) into the same sorted map. `BINDIR` and `GENDIR` are `bazel-out/{cpu}-{mode}/bin`. `TARGET_CPU` is rewritten only for `darwin_arm64` to `aarch64` and `darwin_x86_64` to `x86_64`.
  - [P2] Hidden `defaults-package` is absent, so an explicit query is an unknown key. Bazel prints a single newline. It is also absent from the hardwired name list used for `BAZEL_INFO_KEYS`. Help `info-keys` hardcodes `bazel-bin`, `bazel-genfiles`, and `bazel-testlogs` even when the product name is different. Live info keys use `{product}-bin` and the same pattern for genfiles and testlogs.
  - [P2] Stdout unknown keys still exit 2 with `unknown key(s): 'k' (did you mean ...)`, deduped in residue order. The failure detail is `command.OPTIONS_PARSE_FAILURE`. Bazel uses `info_command.KEY_NOT_RECOGNIZED`. Suggestions use Unicode scalar lengths and Unicode `to_lowercase`. Bazel uses UTF-16 `length`/`charAt` and locale-sensitive `toLowerCase`, so non-BMP and locale-sensitive keys can differ. Equal-distance ties follow `BTreeMap` UTF-8 byte order. Bazel walks `TreeMap` UTF-16 order.
  - [P1] `--info_output_type=response_proto` or `-info_output_type=response_proto` anywhere in the raw args forces proto output even when a later value is `stdout`. Unknown keys are omitted and the command still succeeds. Bazel returns exit 2 and still attaches the partial `InfoResponse`. `path_to_replace` is empty.
  - [P2] Stdout key prefixes and text values are UTF-8. Bazel writes internal Latin-1 bytes via `StringUnsafe`. ASCII matches. A non-ASCII make or residue key does not. A non-UTF-8 argument kept as a Latin-1 byte in Bazel can already be U+FFFD before this command.
  - [P2] `client-env` follows `--action_env` and `--test_env` flag order. Bazel prints `TreeMap` order of the allowlisted action env, then the allowlisted test env.
  - [P1] Heap and JVM items differ: used and committed heap and post-GC heap are process RSS, max heap is one quarter of RAM, `gc-time` is `0ms`, `gc-count` is `0`, and `peak-heap-size` is always `unknown`. `java-runtime` and `java-vm` are native Zel strings, `java-home` is `embedded_tools/jdk`, and `character-encoding` forces `file.encoding` and `defaultCharset` to ISO-8859-1 and infers `sun.jnu.encoding` from `LC_ALL`, `LC_CTYPE`, and `LANG`. `build-language` is a reduced proto with only rule name, attribute name, type, and mandatory.
  - [P2] A write error is not `Cannot write info block: ...` with exit 36 `ALL_INFO_WRITE_FAILURE`, and interruption is not the `info interrupted` detailed code. `NoBuildEvent` is not posted.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/LicenseCommand.java`
- Bazel lines: 117
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/license.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel registers `license` only when the classpath resource `license/LICENSE` exists (`isSupported`). Zel always registers the command and embeds `crates/zel-server/resources/LICENSE` at compile time.
  - [P3] The embedded license text is loaded as UTF-8 (invalid sequences become U+FFFD) and then written with Bazel's `file.encoding=ISO-8859-1` (`PrintWriter`); code points outside Latin-1, including U+FFFD, become `?`. Zel writes that text as UTF-8.
  - [P3] JDK/JRE banners in Zel start with an extra newline, so there is one more blank line before `This binary comes with a bundled JDK/JRE...` than `printOutLn` produces.
  - [P2] Bundled license files are walked in directory-iteration order and symlinks are not followed. Zel sorts each directory by file name and follows directory symlinks (`Path::is_dir`), so a symlink cycle can recurse until panic, and a symlink whose name is `LICENSE` (or the other license names) and whose target is a directory is descended into instead of being copied as a file.
  - [P3] Path headers are a Latin-1 round-trip of the raw path bytes on Linux. Zel uses `Path::display`, so non-UTF-8 paths are lossy UTF-8 (U+FFFD). File bodies themselves are copied as raw bytes in both.
  - [P2] A missing embedded license, an unreadable JDK/JRE tree, a bundled path that is not a directory, or an unreadable license file aborts the command (`IllegalStateException` or `UncheckedIOException`). Zel still returns success: `read_dir` failures skip the directory, and a failed file read prints the `path:` header, omits the body and the trailing blank line, and continues.
  - [P3] `exec` posts an empty `NoBuildEvent` (BEP `BuildStarted` with only the tool version and server pid, then build completion). `cmd_license` posts no build event.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/NoTestsFound.java`
- Bazel lines: 28
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs`
- Verdict: partial
- Differences:
  - [P2] `BuildFinished` announces only the build-metrics id. Zel's shared finished event also announces build-tool logs.
  - [P2] The event is built from an `ExitCode` only, so `failure_detail` is omitted. Zel attaches a detail for every non-zero exit, including `test_command` / `NO_TEST_TARGETS` when the exit is 4.
  - [P2] `finish_time` and `finish_time_millis` are both the caller-supplied build stop time. Zel sets only `finish_time`, using the clock at BEP close.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/PathToReplaceUtils.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P2] `bytes` writes ISO-8859-1: U+0000–U+00FF stay one byte, and any character ISO-8859-1 cannot encode is replaced with `?` (0x3F). Zel puts the path strings in as UTF-8 (`as_bytes()`), so U+0080–U+00FF expand and characters above U+00FF are encoded as Unicode scalars instead of `?`.
  - [P1] `BUILD_WORKING_DIRECTORY` is the raw `--client_cwd` text, or the workspace path when that flag is absent. Java uses the normalized working directory (`workspace.getRelative(clientCwd)` inside a workspace, otherwise the filesystem cwd), so a relative client cwd is joined to the workspace before encoding.
  - [P2] The three entries are produced only for `run` with `--portable_paths`. `info` leaves `InfoResponse.path_to_replace` empty, and `mobile-install` never builds the `ExecRequest` that would carry this list or Latin-1-encode its argv, working directory, and `BUILD_*` values.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/PrintActionCommand.java`
- Bazel lines: 406
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/print_action.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel prints protobuf text of `DetailedExtraActionInfo` from `Action.getExtraActionInfo` (expanded command line, real action id, inputs, outputs, environment). Zel writes a fixed text stub: owner label, mnemonic, an id counted from 0, and `SpawnAction` argv only, escaping just `\` and `"`.
  - [P2] With `--compile_one_dependency`, Bazel selects one generating action per requested path by matching inputs, then scheduling dependencies, then rule `hdrs`, and drops each path after the first hit. A package lookup failure while reading `hdrs` reports `Failed to find target to determine output.` and skips that action. Zel never consults the flag and emits every configured-target action.
  - [P2] Without that flag, Bazel walks `FileProvider` files-to-build and keeps actions whose owner label and configuration checksum match the target, skipping non-`Action` metadata and non-rules. A missing target is `Failed to find target to gather actions: …` (`TARGET_NOT_FOUND`). Command-line expansion failure is `Error expanding command line: …` (`COMMAND_LINE_EXPANSION_FAILURE`). Zel prints every entry in `ct.actions`.
  - [P2] Bazel rejects a target only when the `compilation_outputs` group is empty, with `ConfiguredTarget… is not a supported target kind` (`TARGET_KIND_UNSUPPORTED`), and stops before later targets. Zel treats rule class `genrule` or an empty action list as unsupported, emits hardcoded analysis and “1 total action” lines, and uses `ConfiguredTarget(label, checksum)` only on that path. If actions exist but none match `--print_action_mnemonics`, Zel reports `{label} is not a supported target kind`; Bazel reports `no actions to print were found` (`ACTIONS_NOT_FOUND`).
  - [P2] Bazel still prints when the build is unsuccessful and `--keep_going` is set and actual targets are present, then returns that build’s detailed exit code. `actualTargets == null` or a failed build without keep-going is `Build failed when printing actions`. Interrupt is `print_action: action gathering interrupted`. Zel treats any analysis failure as fatal, ignores keep-going, and maps an empty residue or empty resolution to `no actions to print were found` without that build-failure path.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/QueryCommand.java`
- Bazel lines: 255
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/query_cmd.rs`
- Verdict: partial
- Differences:
  - [P2] `--noincompatible_lexicographical_output` is ignored. Bazel lexicographically sorts `--order_output=auto` streamed output only when that flag is true (the default) and the expression is not a top-level `somepath`. When the flag is false, those formatters are not streamed and the order is the digraph label-set iteration order. Zel always lexicographically sorts `auto` streamed results, and still leaves a top-level `somepath` unsorted.
  - [P2] That lexicographic order uses Java UTF-16 `String.compareTo` on the repository name, package path, and target name. Zel compares those strings as UTF-8 bytes, so supplementary characters sort differently. The parse-error truncation counts 1000 UTF-16 code units and can cut a surrogate pair; Zel counts 1000 Unicode scalars and cuts on scalar boundaries.
  - [P1] A syntax error prints the same `Error while parsing '<truncated>': ...` line and exits 2, but the failure detail message is only the raw parser message and the code is `query.SYNTAX_ERROR`. Zel uses `command.OPTIONS_PARSE_FAILURE` and the full formatted line. `verifyCompatible` failures (such as `buildfiles`/`loadfiles` with `--output=location`) keep that exception's `query.*` code; Zel maps exit 2 to `command.OPTIONS_PARSE_FAILURE`. An evaluation `QueryException` forces exit 7 while keeping the exception's `query.*` detail and its inner message; Zel exits 7 with `info_command.INFO_BLOCK_WRITE_FAILURE` and the wrapped stderr line as the detail message.
  - [P1] With `--order_output=no`, targets are written as they are produced, and a later failure still flushes that partial stdout. Zel emits stdout only after a successful evaluation, so a failed unordered query has empty stdout. An interrupt is `ERROR: query interrupted` with `interrupted.INTERRUPTED`, unless the callback already has a non-interrupt I/O error, in which case it is `I/O error: ` plus the interrupt message and `query.OUTPUT_FORMATTER_IO_EXCEPTION`. Other output I/O errors use that same prefix and code. A flush failure is `Failed to flush query results: ` plus the I/O message and `query.QUERY_RESULTS_FLUSH_FAILURE` (exit 36), replacing the query result. Zel reports an `--output_file` write failure as `Could not close query output file <path>` (open failure: `Could not open query output file <path>`), returns before printing the query stderr, and attaches a synthesized exit-36 detail.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/QueryCommandUtils.java`
- Bazel lines: 86
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/post_analysis_query.rs`
- Verdict: partial
- Differences:
  - [P1] An empty aquery expression (`null`) returns `--universe_scope` as-is and does not apply the `--skyframe_state` target check. Zel still rejects a non-empty `--universe_scope` in that case, and a query that fails to parse is treated as having no target patterns, so the same flag produces the skyframe-target error instead of the syntax error.
  - [P2] The rejection text is `Error while parsing '<expr>': Specifying build target(s) [a, b] with --skyframe_state is currently not supported.`, where `<expr>` is the pretty-printed expression truncated by `Ascii.truncate` at 1000 UTF-16 code units. Zel truncates the raw query string at 1000 Unicode scalars, so spacing, quoting, and non-BMP characters diverge. Exit code is 2 in both. Zel attaches `command.OPTIONS_PARSE_FAILURE` instead of `ActionQuery.SKYFRAME_STATE_WITH_COMMAND_LINE_EXPRESSION`.
  - [P0] With an empty `--universe_scope`, targets are the expression's patterns, deduped in encounter order. Words inside `set(...)` that are `$var` references are omitted in Bazel and kept in Zel, so they become top-level targets and can trigger the skyframe rejection.
  - [P2] `resetDeserializedKeysFromRemoteAnalysisCache` deletes evaluator entries whose value is a `DeserializedSkyValue` before aquery and cquery. Zel has no such deletion, so deserialized remote-analysis-cache nodes are not forced to recompute.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/QueryEnvironmentBasedCommand.java`
- Bazel lines: 307
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/query_cmd.rs`
- Verdict: partial
- Differences:
  - [P2] With `--order_output=auto`, Bazel streams only when `--incompatible_lexicographical_output` is true (the default). `--noincompatible_lexicographical_output` forces the ordered digraph environment: `--experimental_graphless_query=yes` fails the streaming prerequisite, and Sky query stays off even with `--universe_scope` or `--infer_universe_scope`. Zel treats `auto` as streaming anyway, so it lexicographically sorts, turns Sky query on when a universe is set, and otherwise runs graphless under `auto`.
  - [P2] Bazel rejects an unknown `--output` before parsing and before the graphless check. Zel parses first, so a syntax error plus an unknown format is reported as a parse error. An unknown format is also treated as streaming, so `--experimental_graphless_query=yes` with `--order_output=full` or `deps` reports the graphless prerequisite error instead of `Invalid output format`.
  - [P1] `--output_file` is opened only after the format and graphless checks, and before evaluation. A failure of either check leaves an existing file untouched; a later parse error truncates it. Zel runs the query first and then creates or truncates the file for those failures too. If the file cannot be opened, Zel returns exit 36 and drops the query stderr. A close failure in Bazel is `Could not close query output file ...` (exit 36) and replaces the query result; Zel ignores a close error after a successful write.
  - [P2] `--consistent_labels` prints with `Label.str` under the effective Starlark semantics. `--noincompatible_unambiguous_label_stringification` prints main-repo labels as `//pkg:name`. Zel always prints those as `@@//pkg:name`.
  - [P2] `syncPackageLoading` `AbruptExitException`s are reported as `Unknown error: <message>` with that failure's exit code, before the query runs. Zel has no equivalent report.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/QueryOptionHelper.java`
- Bazel lines: 75
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/query_cmd.rs`
- Verdict: partial
- Differences:
  - [P2] `--query_file` is `cwd.join`’d and opened by the OS. Bazel lexically normalizes via `Path.getRelative` before `stat`/`read`, so `.`, `..`, repeated slashes, and a trailing slash are collapsed without following symlinks. Zel follows directory symlinks through `..`, and a trailing slash on a regular file fails the read, so the query text or the I/O outcome can differ.
  - [P1] File bytes are decoded as Latin-1 in both. On failure, both leave stderr empty and exit 2, but Bazel’s failure detail is the query message plus `Query.Code` (`QUERY_FILE_WITH_COMMAND_LINE_EXPRESSION`, `QUERY_FILE_READ_FAILURE`, or `COMMAND_LINE_EXPRESSION_MISSING`; the missing-expression text uses the runtime product name). Zel drops `read_query`’s error string and reports `command.OPTIONS_PARSE_FAILURE` with message `command failed`.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/RunCommand.java`
- Bazel lines: 1247
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P2] ExecRequest strings (working directory, argv, env names and values, variables to clear, script path and contents, portable-path values) are ISO-8859-1, with characters above U+00FF replaced by `?`. Zel stores UTF-8.
  - [P1] On Linux the exec argv is always `[--shell_executable, -c, escaped command]`. An empty shell fails with "the \"run\" command needs a shell…". Zel execs the binary and args directly, wraps only a non-label `--run_under` in hardcoded `/bin/bash -c`, and never checks `--shell_executable`.
  - [P0] `--script_path` writes a script under the client working directory and returns success with no ExecRequest. `--emit_script_path_in_exec_request` skips the write and returns the request instead. `--run` does not apply. The script bakes in shell, cleared vars, env, `--run_under`, rule args, and residue. Zel ignores the emit flag, writes the path as given (process cwd if relative) with `#!/bin/bash`, omits `--run_under`, rule args, and residue (`"$@"` only), and still returns an ExecRequest so the client writes the file again. `should_exec` follows `--run`.
  - [P1] `--norun` still returns an ExecRequest with `should_exec` false and posts `ExecRequestEvent`, omitting residue unless `--experimental_run_bep_event_include_residue`. Zel returns no ExecRequest and posts no such event. `--subcommands` uses `CommandFailureUtils.describeCommand` (pretty-print is distinct). Zel prints a fixed env subset and `/bin/bash -c` of the executable plus residue only.
  - [P1] A test uses `TestPolicy.computeTestEnvironment`, `TestRunnerAction.prepare`, and `TestStrategy.getArgs`, ignores `--run_env` and `--run_in_cwd` (cwd is the exec root), and reports setup or expansion failure with the run-command codes. Zel invents a subset of test variables, then still applies `--run_env`, `--test_env`, and the target env, honors `--run_in_cwd`, and runs `test-setup.sh` plus rule args, `--test_arg`, and residue. `--portable_paths` does not add `TEST_LOG_SUBDIR` for tests.
  - [P2] `--batch` overlays the full client environment onto the run environment (client values win). Zel does not.
  - [P0] A pattern that matches nothing is "No targets found to run" (exit 2). Zel says "No target found to run" and exits 1. `test_suite` is not executable. Zel treats it as a test and expands it. Aliases pass the early check and are validated after analysis. Zel rejects them as not executable before the build. With `--keep_going`, extra or non-executable targets warn and the build continues. Zel fails immediately. After a successful build every output must exist and be executable ("Non-existent or non-executable", exit 1; I/O failure exit 36). Zel checks that only for source files, using the workspace path while it runs the execroot path. `--nobuild_runfile_manifests` is rejected (exit 2) when the output service does not stage runfiles. Zel does not check. Runfiles errors are "Error creating runfiles: …" (exit 36). Zel reports "Failed to create runfiles tree" as a build failure.
  - [P1] Non-test cwd is the runfiles directory (workspace prefix when runfiles are enabled), or the client working directory with `--run_in_cwd` or when there is no runfiles directory. Zel falls back to the workspace root when the runfiles directory is missing. `BUILD_ID` is the command id. Zel uses a time-and-pid pseudo-UUID.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/RunCommandLine.java`
- Bazel lines: 424
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P1] On non-Windows, argv is always `[shell, "-c", one escaped command]` and the shell is required even with no `--run_under`. Zel execs the binary and args directly, and uses `/bin/bash -c` only for a command-string `--run_under`. A label `--run_under` is a shell prefix in Bazel and separate argv elements in Zel.
  - [P1] The shell path is the host toolchain (`--shell_executable`); an empty path fails the run, and that path is the `-c` program and the script shebang. Zel hardcodes `/bin/bash` and does not fail that way.
  - [P1] Windows argv and scripts are absent. Bazel leaves the executable unescaped, `windowsEscapeArg`s the rest, flips `/` to `\` on the script executable, and with `--run_under` wraps `sh -c`. The script is a batch file (`@echo off`, `cd /d`, unquoted `SET name=value`, `%*`). Zel always writes a Unix script and Bourne quoting.
  - [P0] `--script_path` bakes shell-escaped args and residue into the script and, when `--run_under` is set, wraps them as `shell -c '<command>'`, then appends `"$@"`. Sorted unset and set blocks are always emitted, with both names and values escaped. Zel’s script is only `#!/bin/bash`, `cd`, `exec env`, and either the absolute executable or `external/bazel_tools/tools/test/test-setup.sh <binary>` plus `"$@"` — run-under, rule args, and residue are left out, and env names are not escaped. Script bytes are ISO-8859-1 (`?` above U+00FF) versus UTF-8.
  - [P2] Pretty args and shown residue words are individually shell-escaped; a label `--run_under` and the executable use `PathPrettyPrinter` (symlink-relative, or the original path when the symlink prefix is `/`). Zel joins those words unescaped, and a label `--run_under` is shown as `{prefix}bin/{basename}`. The ` <args omitted>` suffix matches. Subcommands text is not this argv; Zel quotes the executable plus residue as a single `/bin/bash -c` word.
  - [P1] `ShellEscaper` leaves only ASCII alphanumerics, `@%-_+:,./`, and a non-leading `~` bare (empty becomes `''`) and walks UTF-16 code units. Zel’s run-line quoter also treats `=` as safe, quotes a non-leading `~`, and walks Unicode scalars, so an assignment-shaped command word is a shell assignment inside `sh -c`.
  - [P2] Residue-free argv (`getArgsWithoutResidue`) is not produced; Zel’s exec argv always includes the residue.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/ShutdownCommand.java`
- Bazel lines: 77
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/version.rs`
- Verdict: partial
- Differences:
  - [P2] `--iff_heap_size_greater_than` shuts down only when the limit is 0 or `Runtime.getRuntime().totalMemory()` (current committed JVM heap) is strictly greater than `limit * 1000 * 1000` bytes. Zel uses the same threshold against `getrusage` `ru_maxrss` (peak RSS of the whole process, or 0 if the call fails), so a positive limit can shut down or stay up in different cases, and a later call still shuts down after the peak has crossed the limit even if current usage has dropped. A failed reading never shuts down for a positive limit.
  - [P0] A shutdown that meets the threshold stops the server and leaves the output base in place. Zel sets `shutdown_requested`, and the daemon then deletes the entire output base.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/TargetPatternsHelper.java`
- Bazel lines: 103
- Zel counterpart: `/workspace/zel-cursor/crates/zel-cmdline/src/lib.rs`
- Verdict: partial
- Differences:
  - [P2] Pattern files are decoded as Latin-1 in both, but Java `readLine` splits on `\n`, `\r`, and `\r\n`, while Rust `str::lines` splits only on `\n` and `\r\n`. A bare `\r` that is not trailing whitespace stays inside one pattern.
  - [P2] Java `String.trim` drops leading and trailing code units `<= U+0020`. Rust `trim` drops Unicode `White_Space`. After the Latin-1 mapping, bytes `0x00` and `0x01`–`0x08` and `0x0E`–`0x1F` are removed only by Java, and bytes `0x85` and `0xA0` are removed only by Rust, so empty-line filtering and the pattern text differ.
  - [P2] A missing or unreadable pattern file is still an error, but Java’s message is the VFS text `I/O error reading from <path>: … <path> (No such file or directory)` (JNI prefix and parentheses). Zel reports `I/O error reading from <path>: No such file or directory` or `Permission denied`, and other I/O errors use Rust’s `Display` (including ` (os error N)`).
  - [P2] Java attaches `TargetPatterns.Code.TARGET_PATTERN_FILE_WITH_COMMAND_LINE_PATTERN` or `TARGET_PATTERN_FILE_READ_FAILURE`. Zel still exits 2, and the derived failure detail is `command.OPTIONS_PARSE_FAILURE`.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/TestCommand.java`
- Bazel lines: 284
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs`
- Verdict: partial
- Differences:
  - [P1] `--test_output=streamed` does not rewrite the option set to `--test_sharding_strategy=disabled` and `--test_strategy=exclusive`. Sharding is forced off only while running tests, so `test_unit_grid` still uses the user strategy. An explicit `--test_strategy=standalone` or `local` still adds `no-remote-exec` and `no-sandbox` on those exclusive jobs. Streamed exclusive tests run in reverse target order.
  - [P1] `--noanalyze` is ignored and tests still run. Bazel leaves successful targets unset on an otherwise successful build and returns `test_command.TEST_WITH_NOANALYZE` (exit 1) with "Couldn't start the build. Unable to run tests". Zel has no `TEST_WITH_NOANALYZE` code.
  - [P1] `--nobuild` always prints that couldn't-start error and returns exit 1 after "Found N test targets", without running tests. Bazel prints it only when successful targets were never set; default Skymeld still runs the tests.
  - [P1] With coverage, or `test --collect_code_coverage`, and no explicit `--instrumentation_filter`, Zel prints the heuristic filter and still instruments with the flag default `-/javatests[/:],-/test/java[/:]`. Bazel installs the computed filter (`^//…[/:]`, `javatests` to `java`, `test/java` to `main/java`, trailing `/internal`, `/public`, and `/tests` stripped). `@repo//` labels in a `test_suite`'s `tests` are left out of Zel's heuristic.
  - [P1] `--experimental_use_validation_aspect` does not change reported test status. Bazel passes the targets whose validation aspect succeeded and rewrites other finished tests to `FAILED_TO_BUILD`, or to `NO_STATUS` under `--nokeep_going`.
  - [P2] `--print_relative_test_log_paths` rewrites logs with a lossy UTF-8 replace, so non-UTF-8 path bytes become U+FFFD. Live output drops the execroot prefix, and the summary swaps the absolute testlogs directory for `{symlink_prefix}testlogs/`. Bazel uses convenience symlinks plus `../` for the workspace-relative working directory, keeps the original path when no symlink matches, and leaves paths unchanged when `--symlink_prefix=/`.
  - [P2] "All tests passed but there were other errors during the build." is plain text. With `--color=yes`, Bazel prints it in bold red and then resets the terminal.
  - [P1] If the build failed, the process exit is build failure (1), including when tests failed (3). Bazel keeps the build code when the two detailed codes tie, lets a test result that has a failure detail outrank a build failure that has none, and lets an infrastructure build failure outrank a test failure.
  - [P0] An analysis abort whose log ends in "failed; build aborted" appends "No test targets were found, yet testing was requested" even when tests were requested. Bazel prints that only when the analyzed test set is empty; when successful targets were never set it prints "Couldn't start the build. Unable to run tests" and keeps the build's detailed exit code, so a failed build does not become exit 4. A parsing failure is reported as `BUILD_FAILURE` after that couldn't-start line. Interrupting main-repo mapping does not produce "test command interrupted".

### `src/main/java/com/google/devtools/build/lib/runtime/commands/VersionCommand.java`
- Bazel lines: 98
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/version.rs`
- Verdict: partial
- Differences:
  - [P2] An empty build-data map makes Bazel print `ERROR: Version information not available` and exit 2 with failure detail `VersionCommand.NOT_AVAILABLE`. Zel always prints a five-line summary and exits 0.
  - [P1] `--gnu_format` prints `<productName> <Build label>` when that label is non-empty, otherwise `<productName> no_version`. The product name is the runtime product (`--product_name`, default `bazel`). Zel uses `version_product_prefix()` (`zel` unless argv0 or env heuristics select `bazel`) and `build_label()`, which falls back to `9.3.2`, so an unlabeled build still prints a version.
  - [P1] The default summary is every embedded `build.*` property, `key: value`, sorted. Zel always emits Build label, Build target `@@//crates/zel-client:zel`, Build time, Build timestamp, and Build timestamp as int. Unstamped Bazel data is `Thu Jan 01 00:00:00 1970 (0)` for both time and timestamp and `0` for the int, with no label. Zel fills the label from the fallback above and the timestamps from `ZEL_BUILD_TIMESTAMP` or the binary mtime, formatted as UTC `Dow Mon DD HH:MM:SS YYYY` with a zero-padded day. Stamped Bazel Build time follows workspace `FORMATTED_DATE` (local time, day-of-month not zero-padded).
  - [P2] `--startup_version` is not a version-command option in Bazel (unknown option, exit 2). Zel treats it as GNU format and drops it before the residue check. The client also handles `bazel --version` by injecting that token and printing before the server runs.
  - [P2] Every Bazel invocation posts `NoBuildEvent` (a BEP `BuildStarted` with tool version and server pid, and no command name). Zel posts no build event for `version`.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/events/CleanStartingEvent.java`
- Bazel lines: 30
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No event stores the clean command’s `OptionsParsingResult` and returns that same reference, including null, for subscribers when clean starts.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/BlazeBinInfoItem.java`
- Bazel lines: 40
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P1] Zel prints `realpath({execroot}/bazel-out/{cpu}-{compilation_mode}/bin)` plus a newline. Java prints the main-repo bin root of the command configuration: `{output_base}/execroot/_main/{product}-out/{mnemonic}/bin`. That mnemonic is the output-path mnemonic after platform mapping: the CPU, or the platform short name, `multi-platform`, or `platform-<hash>` when `--experimental_platform_in_output_dir` selects a platform; then the compilation mode; then any `--platform_suffix`, `--cc_output_directory_tag`, Apple split-CPU / minimum OS / distinguisher, and Android distinguisher. With `--enable_bzlmod=false`, Zel uses the `WORKSPACE` name or `__main__` in place of `_main`.
  - [P1] An unset `--cpu` uses `BlazeRuntime::host_cpu`: every non-aarch64 Linux arch becomes `k8`, every Windows arch becomes `x64_windows`, and any other OS becomes `std::env::consts::ARCH`. `AutoCpuConverter` yields `piii`, `ppc`, `arm`, `s390x`, `mips64`, `riscv64`, `arm64_windows`, `freebsd`, `openbsd`, or `unknown` for those hosts.
  - [P1] Zel symlink-resolves the path (`canonicalize`, or the canonical output base plus the relative suffix) and encodes it as UTF-8, so non-UTF-8 bytes become U+FFFD. Java writes the Latin-1 internal bytes of the unresolved VFS path and then a newline.
  - [P2] A product name other than `bazel` still uses a `bazel-out` directory, and `help info-keys` still lists `bazel-bin`. Java uses `{product}-out` and lists `{product}-bin` with the description `Configuration dependent directory for binaries.` Non-ASCII product names are only ASCII-lowercased; Java lowercases them with `Locale.US`.
  - [P2] Java builds that configuration while producing the value and fails the command on `InvalidConfigurationException`. Zel still prints the synthesized path.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/BlazeGenfilesInfoItem.java`
- Bazel lines: 41
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] `--incompatible_merge_genfiles_directory=false` prints the `genfiles` directory in Bazel. Zel always prints the `bin` directory (`bazel_genfiles` in `/workspace/zel-cursor/crates/zel-server/src/lib.rs`).
  - [P1] Bazel prints `{productName}-out/<configuration mnemonic>/bin-or-genfiles`. The mnemonic is the CPU (or a platform short name, `multi-platform`, or `platform-<hash>` when `--experimental_platform_in_output_dir` applies), compilation mode, optional `--platform_suffix`, `--cc_output_directory_tag`, Apple/Android distinguisher pieces, and an `ST-` plus 12-hex hash of options that differ from the baseline and are not marked explicit in the output path. A mnemonic piece containing a path separator fails configuration creation and `info` errors. Zel prints `bazel-out/{cpu}-{compilation_mode}/bin` and still succeeds.
  - [P1] Unset `--cpu` uses `AutoCpuConverter` (`piii`, `ppc`, `arm`, `s390x`, `mips64`, `riscv64`, `arm64_windows`, `freebsd`, `openbsd`, `unknown`, and the usual `k8` / `aarch64` / `darwin_*` / `x64_windows`). Zel uses `BlazeRuntime::host_cpu`: every non-aarch64 Linux CPU is `k8`, every Windows CPU is `x64_windows`, and other OSes are `std::env::consts::ARCH`. An explicit empty `--cpu` is host-detected in Bazel; Zel keeps `""` (`AutoCpuConverter` is not applied), so the segment is `-{compilation_mode}`.
  - [P2] A product name other than `bazel` still uses the directory `bazel-out` in Zel. Bazel uses `{productName}-out`. The info key `{product}-genfiles` matches.
  - [P1] Bazel prints the logical `Root` path. Zel canonicalizes the genfiles path when it exists, otherwise it rewrites the `output_base` prefix to the canonical `output_base`, so symlinks in `output_base` change the printed path.
  - [P2] Java `print` writes the path’s Latin-1 internal bytes plus a newline (raw filesystem bytes). Zel writes UTF-8 of a `String` plus a newline. Unicode paths match after Bazel’s UTF-8-in-Latin-1 internal encoding; non-UTF-8 path bytes do not.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/BlazeTestlogsInfoItem.java`
- Bazel lines: 43
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel prints `execroot/_main/{product}-out/<output-mnemonic>/testlogs` from `getTestLogsDirectory(RepositoryName.MAIN).getRoot()`. The mnemonic is `OutputPathMnemonicComputer`: host `AutoCpuConverter` CPU (empty `--cpu` included), compilation mode, then any `--platform_suffix`, `--cc_output_directory_tag`, Apple split-CPU/min-OS/distinguisher, and Android distinguisher. `--experimental_platform_in_output_dir` replaces the CPU segment with a platform short name, `multi-platform`, or `platform-<hash>`. A no-config configuration is `{mode}-noconfig`.
  - [P1] Zel always prints `execroot/<workspace>/{hardcoded bazel-out}/{cpu}-{compilation_mode}/testlogs`. `--cpu` is the raw flag (`AutoCpuConverter` is not applied, so `--cpu=` yields `-{mode}`); if unset, `BlazeRuntime::host_cpu()` is `k8` for every Linux arch except aarch64, `x64_windows` for every Windows arch, `darwin_x86_64` for every Darwin arch except aarch64, and the raw arch on FreeBSD/OpenBSD. The output directory stays `bazel-out` for a non-`bazel` product name. The execroot segment is `_main` only while `bzlmod_enabled()` is true; otherwise it is the `workspace()` name or `__main__` (`info` does not refresh that flag).
  - [P1] Bazel writes the VFS path as Latin-1 internal bytes plus a newline and does not resolve symlinks. Zel `canonicalize`s the testlogs path when it exists, otherwise rewrites the output-base prefix to the canonical output base, then encodes with `Path::display()` (lossy UTF-8, U+FFFD for non-UTF-8 bytes; OS separators after canonicalize).

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/BuildLanguageInfoItem.java`
- Bazel lines: 213
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P1] Java loads `StarlarkBuiltinsValue` (`keepGoing=false`) and emits builtin rule classes: the native map when `--experimental_builtins_bzl_path` is empty, otherwise predeclared build `RuleFunction`s plus macro-wrapped natives (`includeMacroWrappedRules=true`, `name + "_rule_function"` or the same-named native class). A Skyframe error aborts with `Failed to load Starlark builtins`. Zel always serializes `native_rule_classes()` (no builtins load, no flag, no macro-wrapper heuristic, no that failure) and rejects the key outside a workspace.
  - [P2] Both drop rule names starting with `$` and sort the rest by name. Java includes every attribute, including implicit `$` and `:` attributes, sorted by name. Zel emits only `RuleClass.attrs` (name-sorted) and omits `implicit`.
  - [P1] Zel writes only `name`, type discriminator, and `mandatory`. Java also sets `allow_empty` (`!isNonEmpty()`, so true for ordinary attributes), `allow_single_file`, `configurable` (false for output types and nonconfigurable attributes, otherwise true), and `executable`. Unset proto2 bools read as false, so `allow_empty` and `configurable` disagree for the usual attributes, and `allow_single_file` / `executable` disagree when those flags are set.
  - [P2] Java fills label-typed attributes (`getLabelClass() != NONE`) with `allowed_rule_classes` (policy `ANY`, or `SPECIFIED` plus non-`$` names passing the predicate, in the name-sorted rule order, when strict checking is on and the predicate is not `Predicates.alwaysTrue()`) and `nodep` for `LabelClass.NONDEP_REFERENCE`. Zel leaves both unset.
  - [P2] Java encodes a simple default when it is not null, not a computed/Starlark-computed/late-bound default, and not the type default: dicts in map iteration order (`key.toString()`, recursive values), lists in order, `LICENSE` as an empty value, strings and labels through `StringEncoding.internalToUnicode`, ints via `StarlarkInt.toIntUnchecked()` (throws outside signed 32-bit), bools, and `TriState.toInt()`. Any other type throws `IllegalStateException`. Zel never writes `default`.
  - [P2] Discriminators are collapsed to the stored `AttrType`: `NODEP_LABEL` is `LABEL` (3) not `STRING` (2), `NODEP_LABEL_LIST` is `LABEL_LIST` (6) not `STRING_LIST` (5), `LICENSE` and `DISTRIBUTION_SET` are `STRING_LIST` (5) not 9 and 8, and `LABEL_LIST_DICT` is `STRING_LIST_DICT` (13) not 12. An unknown Java type fails in `getDiscriminatorFromType` instead of becoming `STRING`.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/CharacterEncodingInfoItem.java`
- Bazel lines: 42
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P3] `file.encoding` and `defaultCharset` are the live JVM values (`System.getProperty("file.encoding", "unknown")` and `Charset.defaultCharset().name()`). Zel always prints `ISO-8859-1` for both, which matches the stock client `-Dfile.encoding=ISO-8859-1` and diverges when that property is overridden, set to `COMPAT` or a charset alias (raw property text versus the canonical charset name), or unset (`unknown` versus the real default charset).
  - [P3] `sun.jnu.encoding` is the JVM native-encoding property (fallback `unknown`). Zel builds it from the first non-empty of `LC_ALL`, `LC_CTYPE`, and `LANG`: `UTF-8` if the text contains `utf-8`/`utf8`, `ANSI_X3.4-1968` for empty/`C`/`POSIX`, otherwise the substring after the first `.` or `UTF-8` if there is no `.`. On Linux the Bazel client sets the server `LC_ALL` to the first available of `en_US.ISO-8859-1`, `C.UTF-8`, and `en_US.UTF-8` before the JVM starts, and the JDK reports that locale’s codeset (macOS and the embedded Windows JDK are UTF-8 regardless of `LANG`; OpenBSD is UTF-8). Zel does not apply that choice, so a typical Linux `LANG=*.UTF-8` reports `UTF-8` where Bazel reports `ISO-8859-1` when `en_US.ISO-8859-1` exists, and the locale text is not canonicalized (`en_US` becomes `UTF-8`, `iso8859-1` stays as written, `@euro` is kept, and `c`/`posix` are not treated as the C locale).
  - [P3] Both values are newline-terminated. Java writes the string’s Latin-1 compact bytes and throws if the string is not Latin-1; Zel writes UTF-8. The stock ASCII line matches; a non-ASCII property or locale fragment is Latin-1 or an exception in Bazel and UTF-8 in Zel.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/ClientEnv.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P1] `--action_env` allowlisting is last-op-wins: `NAME` adds, and a later `NAME=value` or `=NAME` removes it. Zel emits every raw value that contains no `=` and does not start with `-`, so an earlier inherit is still printed after a later set or unset, and a repeated inherit is printed once per flag instead of once.
  - [P2] Action-env lines, then test-env lines, are each sorted by `TreeMap` (`String.compareTo`, UTF-16 code units). Zel keeps parsed flag order in each group. `--test_env` still only adds on `NAME` (set and unset do not remove), but repeats are not collapsed.
  - [P2] An inherit name that starts with `-` (`--action_env=-NAME`) is included when that variable is in the client environment. Zel drops any raw value that starts with `-`.
  - [P2] `EnvVarsConverter` rejects the empty string and `=` before this item runs. Zel still builds `client-env`: `=` is ignored, and an empty value is treated as an inherit of the empty name.
  - [P2] `InfoItem.print` writes the string’s Latin-1 internal bytes (the original environment bytes) plus a trailing newline. Zel writes UTF-8 of strings produced with `String::from_utf8_lossy`, so invalid UTF-8 becomes U+FFFD (`EF BF BD`) instead of those bytes. Valid UTF-8 matches.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/CommittedHeapSizeInfoItem.java`
- Bazel lines: 38
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel prints the JVM heap `MemoryMXBean.getHeapMemoryUsage().getCommitted()` byte count (`-1` if undefined, which formats as `-1B`). Zel prints `resident_bytes()` (`getrusage` `ru_maxrss` peak RSS, KiB×1024 except on macOS, or `0` if unavailable), the same figure as `used-heap-size`, not committed heap.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/DefaultPackagePathInfoItem.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] The value is the effective `--package_path` joined with `:`, hidden unless requested, default `%workspace%`, and an explicit empty value prints a blank line. Bazel writes that string as Latin-1 internal bytes, so every input byte is preserved. Zel decodes the flag from the command line and from rc files with lossy UTF-8, so bytes that are not valid UTF-8 become U+FFFD before the value is written as UTF-8. Valid UTF-8, including non-ASCII, matches.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/DefaultsPackageInfoItem.java`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] `defaults-package` is not a registered info key. `info defaults-package` is an unknown-key command-line error. Bazel succeeds and writes a single `\n` (no `defaults-package: ` prefix when it is the only key). The value ignores configuration and environment.
  - [P2] Requested with other keys, Bazel prints `defaults-package: ` plus that newline. Zel rejects the request because the key is unknown.
  - [P2] `--info_output_type=response_proto` includes a `defaults-package` item whose value is that newline byte and sets `print_keys` from the residue size. Zel omits the key and still returns success.
  - [P2] `help info-keys` omits `defaults-package` and the description `Obsolete. Retained for backwards compatibility.`
  - [P2] Bash completion `BAZEL_INFO_KEYS` omits `defaults-package` (a hardwired name, after `character-encoding`).

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/ExecutionRootInfoItem.java`
- Bazel lines: 39
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P1] Zel prints a symlink-resolved path (`canonicalize` of `output_base/execroot/<name>`, or the canonical `output_base` prefix plus the relative suffix if that directory does not exist). Bazel prints the logical path `<output_base>/execroot/_main` with no symlink resolution.
  - [P1] `--enable_bzlmod=false` is a no-op in Bazel 9.3.2, so the directory name stays `_main`. Zel then uses `workspace(name = ...)` from `WORKSPACE.bazel` or `WORKSPACE`, or `__main__` when that name is absent.
  - [P2] Bazel appends a newline to the path’s Latin-1 internal bytes. Zel emits UTF-8, and a canonicalized path goes through `Path::display()`, so non-UTF-8 bytes become U+FFFD.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/GcCountInfoItem.java`
- Bazel lines: 41
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel prints the sum of every `GarbageCollectorMXBean.getCollectionCount()` (a bean may contribute `-1` when its count is undefined). Zel always prints `0`.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/GcTimeInfoItem.java`
- Bazel lines: 41
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel sums `GarbageCollectorMXBean.getCollectionTime()` in milliseconds across every GC MXBean, including a `-1` when a collector's time is undefined, and prints that signed total as `{n}ms`. Zel always prints `0ms`.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/InfoItemHandler.java`
- Bazel lines: 66
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] `response_proto` packs an `InfoResponse` with an empty `path_to_replace`. The handler this factory selects always adds output base, working directory, and workspace directory, with those path strings stored as ISO-8859-1 bytes.
  - [P3] Stdout writes the key as UTF-8, then `": "`, then the value bytes. Bazel writes the key with Latin-1 internal bytes and rejects a key that is not Latin-1, so a code point above U+007F changes the prefix bytes.
  - [P2] Zel forces `response_proto` when any argument is exactly `--info_output_type=response_proto`, including when a later `--info_output_type=stdout` wins and when that token is residue after `--`. Bazel uses only the parsed output type, which defaults to stdout and matches the enum name case-insensitively.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/InfoItemUtils.java`
- Bazel lines: 30
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P3] `getMemoryUsage()` returns the JVM heap `MemoryUsage` (`getUsed()`, `getCommitted()`, and `getMax()` as independent `long`s; `getMax()` is `-1` when no maximum is defined). Zel sets both `used-heap-size` and `committed-heap-size` to `resident_bytes()` (`getrusage` `ru_maxrss`: peak RSS, KiB×1024 on Linux, bytes on macOS, `0` on failure or non-Unix), so used and committed are always the same process high-water mark rather than the current heap pool.
  - [P3] `max-heap-size` is `(host.memory_mb as u64) * 1024 * 1024 / 4` (truncated host RAM in MiB, then one quarter, as `u64`, never `-1`) instead of `MemoryUsage.getMax()`.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/InstallBaseInfoItem.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel prints the stored install-base path (`Path.toString()` as Latin-1 internal bytes) plus a trailing newline and does not consult the filesystem. Zel prints `std::fs::canonicalize` when that path exists (`Path::display()`, lossy UTF-8), so symlinks are resolved, Windows paths can gain a `\\?\` prefix, and non-UTF-8 bytes become U+FFFD.
  - [P2] If canonicalize fails, Zel still rewrites the value when the stored path is under the output base or workspace, substituting the canonical prefix. Bazel always emits the stored path. A null install base prints `null`; Zel always has a path string.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/InstallMd5InfoItem.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] An empty or unset install MD5 is a null `HashCode` in Bazel, so `get` throws on `toString()`. Zel prints the install-base directory basename. `--batch` never records the digest, so that basename is used even when `--install_base` is not named with the digest.
  - [P2] Bazel parses the startup value as a 128-bit MD5 and prints lowercase hex. A non-hex string, an odd length, or any length other than 32 hex digits fails when the server directories are built. Zel prints the raw non-empty `--install_md5` string unchanged, including uppercase hex and non-MD5 text.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/JavaHomeInfoItem.java`
- Bazel lines: 41
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel prints `System.getProperty("java.home")` (the JVM that is actually running the server). Zel always prints `<install_base>/embedded_tools/jdk`, including when that directory does not exist.
  - [P3] If `java.home` is null, Bazel prints `unknown`. Zel has no missing-runtime case.
  - [P2] Bazel normalizes the string through `FileSystem.getPath` (`.` / `..` / duplicate separators, no I/O, symlinks kept). Zel `canonicalize`s when the path exists, so an existing symlink is replaced by its target; only a failed canonicalize keeps the normalized string.
  - [P3] Bazel emits internal-string bytes (`platformToInternal`: raw Latin-1 path bytes when `sun.jnu.encoding` is ISO-8859-1 on Linux, otherwise UTF-8). Zel writes UTF-8 from `Path::display()`, replacing non-UTF-8 bytes with U+FFFD.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/JavaRuntimeInfoItem.java`
- Bazel lines: 38
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel prints `java.runtime.name (build java.runtime.version) by java.vendor`. Zel always prints `Zel native runtime (build <CARGO_PKG_VERSION>) by The Zel Authors`.
  - [P3] Missing properties fall back to `Unknown runtime`, `unknown`, and `unknown`. Zel has no property lookup or those defaults.
  - [P3] Bazel `InfoItem.print` emits Latin-1 bytes and throws if the formatted string is not Latin-1. Zel emits UTF-8. The Zel string is ASCII, so only non-ASCII runtime name, version, or vendor bytes diverge.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/JavaVirtualMachineInfoItem.java`
- Bazel lines: 39
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel prints `java.vm.name (build java.vm.version, java.vm.info) by java.vm.vendor`, using `System.getProperty` defaults `"Unknown VM"`, `"unknown"`, `"unknown"`, and `"unknown"` when a property is unset. Zel always prints `Zel native code (build <CARGO_PKG_VERSION>, <os>-<arch> compiled) by The Zel Authors` and never reads those properties or defaults.
  - [P3] Bazel `InfoItem.print` copies the string's Latin-1 bytes and throws if the formatted value is not Latin-1. Zel writes UTF-8 bytes of its fixed ASCII template plus a trailing newline, so a non-Latin-1 JVM property aborts Bazel and cannot appear in Zel.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/LocalResourcesInfoItem.java`
- Bazel lines: 40
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] RAM is `(int)` truncation of `getMemoryMb()` in Bazel and `memory_mb.ceil() as i32` (`host_ram_mb()`) in Zel, so a non-integral MiB value prints one higher in Zel.
  - [P1] Bazel RAM is `OperatingSystemMXBean.getTotalPhysicalMemorySize()` in bytes divided by 1024² (cgroup/container limit when the JDK is container-aware). Zel uses `/proc/meminfo` `MemTotal` or macOS `hw.memsize`, which ignore the cgroup memory limit, and prints `RAM=1024MB` when that probe fails.
  - [P3] CPU is `Runtime.availableProcessors()` in Bazel and `std::thread::available_parallelism()` (1 on failure) in Zel. Both print that count with one fractional digit, so the number differs when those probes disagree.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/MakeInfoItem.java`
- Bazel lines: 38
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] `get` prints the stored make-variable value with `InfoItem.print` (Latin-1 internal bytes from `StringUnsafe.getInternalStringBytes`, plus a trailing `\n`). Zel’s `--show_make_env` items use `text()`, which writes UTF-8 (`str::as_bytes`) plus `\n`. Code points U+0080–U+00FF are one byte in Bazel and two in Zel. A value that is not Latin-1 makes Bazel `get` throw `IllegalArgumentException`; Zel emits UTF-8.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/MaxHeapSizeInfoItem.java`
- Bazel lines: 37
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel prints `prettyPrintBytes` of `MemoryMXBean.getHeapMemoryUsage().getMax()` (the running JVM heap maximum, i.e. `-Xmx` or the ergonomic max). Zel prints `pretty_print_bytes((host.memory_mb as u64) * 1024 * 1024 / 4)`, one quarter of detected physical RAM (`MemTotal` / `hw.memsize`), so startup heap flags and the real heap limit are ignored.
  - [P3] An undefined JVM max is `-1`, and Bazel prints `-1B` (`-1 < 1E4`). Zel’s `u64` path cannot; if RAM probing fails, `memory_mb` falls back to `1024.0` and the key prints `268MB`.
  - [P3] `memory_mb` is an `f64` megabyte figure; the `as u64` cast drops the fractional megabyte before `* 1024 * 1024 / 4`, so the byte count is not an exact quarter of host RAM. Java passes `getMax()` through as a full `long`.
  - [P3] For the same non-negative count, both formatters use the 10KB / 10MB / 100GB decimal cutovers. Java then does `double` division and casts to `int` (saturates at `Integer.MAX_VALUE` once the unit quotient exceeds 2^31−1, and can diverge from integer division above 2^53). Zel prints the full `u64` quotient.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/OutputBaseInfoItem.java`
- Bazel lines: 41
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel prints the stored output-base path (`Path.toString()`, no symlink resolution) plus a newline. Zel prints `std::fs::canonicalize` of that path when it exists, so symlink components are resolved and a Windows result gains the `\\?\` prefix; only a failed canonicalize falls back to the stored path.
  - [P2] Bazel emits the path as Latin-1 internal bytes (`StringUnsafe.getInternalStringBytes`). Zel’s canonical path is `Path::display().to_string()`, which replaces non-UTF-8 bytes with U+FFFD.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/OutputPathInfoItem.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel prints the logical path `output_base/execroot/<runfiles prefix>/<productName>-out` plus `\n`. The runfiles prefix is fixed `_main`. The last segment is `productName + "-out"`. Bytes are the path’s Latin-1 internal encoding (`StringUnsafe.getInternalStringBytes`), so each path byte is preserved and a non-Latin-1 string throws.
  - [P1] Zel (`output_path` in `info.rs`, path from `BlazeDirectories::output_path` in `/workspace/zel-cursor/crates/zel-server/src/lib.rs`) always appends `bazel-out`, ignoring `--product_name`. The execroot directory is `_main` only when bzlmod is on; with `--enable_bzlmod=false` it is the `workspace(name=...)` value or `__main__`.
  - [P1] Zel then `canonicalize`s that path when it exists (symlink resolution), otherwise rewrites an `output_base` or workspace prefix to the canonical base, and UTF-8-encodes the result plus `\n`. `Path::display` turns non-UTF-8 bytes into U+FFFD.
  - [P1] Outside a workspace Bazel still prints this key (`info` is not `mustRunInWorkspace`, and the item does not read the workspace directory). Zel marks `info` `must_run_in_workspace` and this key `requires_workspace`, so the key is refused.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/PackagePathInfoItem.java`
- Bazel lines: 43
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] A non-UTF-8 `--package_path` byte is emitted as that Latin-1 byte (`InfoItem.print` / `StringUnsafe.getInternalStringBytes`, plus a trailing newline). Zel writes UTF-8 of a lossy `String` (`std::env::args` and `.bazelrc` `from_utf8_lossy`), so that byte is printed as U+FFFD (`EF BF BD`).

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/ReleaseInfoItem.java`
- Bazel lines: 34
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] With no non-empty `Build label`, Bazel prints `development version`. Zel’s `release_name()` (`/workspace/zel-cursor/crates/zel-server/src/lib.rs`) falls back to `BAZEL_COMPAT_VERSION` (`9.3.2`), so the key prints `release 9.3.2`.
  - [P2] Bazel uses only the stamped `Build label`. `BAZEL_DEV_VERSION_OVERRIDE` does not affect this key. Zel uses the first non-empty runtime `ZEL_EMBED_LABEL`, then runtime `BAZEL_EMBED_LABEL`, then those names at compile time, and does not read `build-data.properties`. An empty env var is skipped, so it does not select `development version`.
  - [P3] `InfoItem.print` writes Latin-1 bytes plus `\n` and throws `IllegalArgumentException` if any code point is above U+00FF. Zel writes UTF-8 plus `\n`. Code points U+0080–U+00FF differ in byte length; code points above U+00FF abort the Java item and are emitted by Zel.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/RemoteRequestedInfoItemHandler.java`
- Bazel lines: 63
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] On close, Bazel always appends one `google.protobuf.Any` (`type.googleapis.com/command_server.InfoResponse`) whose `path_to_replace` has three entries, in order: `OUTPUT_BASE` (output base), `BUILD_WORKING_DIRECTORY` (working directory), `BUILD_WORKSPACE_DIRECTORY` (workspace). Each `value` is that path string encoded as ISO-8859-1 (code points above U+00FF become `?`). Zel’s `--info_output_type=response_proto` path packs the same extension, copied item bytes, and `print_keys`, but `path_to_replace` is always empty.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/ServerLogInfoItem.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P2] If the server log is not open yet, Bazel prints an empty line (`getServerLogPath().orElse("")`), even when an old `java.log` symlink exists. Zel prints `output_base/java.log`, or the previous symlink target, and rewrites an `output_base` or workspace prefix to its canonical path when that directory exists.
  - [P2] On `IOException` (no log querier or no queryable handler), Bazel prints `UNKNOWN LOG LOCATION` and logs a warning. Zel always prints a path and has no failure sentinel.
  - [P2] Bazel prints `Path.toAbsolutePath()` of the handler's open log file and does not resolve symlinks. Zel `read_link`s `java.log` once and `canonicalize`s the target, so ancestor symlinks are resolved and the file must exist. If `read_link` fails, Zel reports the `java.log` path itself.
  - [P3] Bazel `InfoItem.print` emits Latin-1 internal bytes plus `\n` (U+0080–U+00FF stay one byte; a code point above U+00FF throws `IllegalArgumentException`). Zel encodes `Path::display` as UTF-8 plus `\n` (invalid path bytes become U+FFFD).

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/StarlarkSemanticsInfoItem.java`
- Bazel lines: 52
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/semantics_info.rs`
- Verdict: partial
- Differences:
  - [P2] Set-valued entries (`incompatible_autoload_externally`, `repositories_without_autoloads`, `incompatible_disable_transitions_on`) are printed from the raw comma split, in input order, with duplicates kept and empty segments dropped. Bazel’s set converter keeps empty segments, then `distinct().sorted()` in Java string order, and an empty element renders as a blank (`[, a]`). A value that is only commas is a non-empty list of empty strings, so Bazel prints `key=[]`; Zel treats that as the empty default and omits the key.
  - [P2] `experimental_starlark_types` (flag `experimental_starlark_types_allowed_paths`) keeps order and duplicates on both sides, but Bazel keeps empty segments (`[a, , b]`, and a lone comma prints `experimental_starlark_types=[]`). Zel drops empty segments and omits the key when nothing is left.
  - [P2] `incompatible_enforce_starlark_utf8` is a bool-or-enum: Bazel prints the enum name, so `true`/`1`/`yes`/`t`/`y` become `ERROR` and `false`/`0`/`no`/`f`/`n` become `OFF`. A bare `--incompatible_enforce_starlark_utf8` is the unconverted value `1` (`ERROR`); `--no…` is `0` (`OFF`). Zel uppercases the raw token, so those spellings print `TRUE`, `1`, `YES`, `FALSE`, `0`, and so on. `off`/`warning`/`error` match.
  - [P2] `max_computation_steps` (`Long.decode`) and `nested_set_depth_limit` (`Integer.decode`) are printed in decimal after octal (leading `0`) and hex (`0x`/`0X`/`#`) decoding. Zel parses a trimmed base-10 `i64` (optional leading `+` only) and otherwise prints the raw token, so `010` is `8` in Bazel and `10` in Zel, and `0x10` is `16` in Bazel and `0x10` in Zel. Values outside the Java `int` or `long` range fail option parsing before this item runs.
  - [P2] Bazel writes the value as Latin-1 internal bytes plus a newline. Zel writes UTF-8 plus a newline. Valid UTF-8 command-line text round-trips to the same bytes. A non-UTF-8 byte is preserved and printed by Bazel; Zel reads arguments with `std::env::args()`, which rejects a non-Unicode argument before this item runs.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/StdoutInfoItemHandler.java`
- Bazel lines: 45
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P3] With keys printed, Bazel writes the key through `StringUnsafe.getInternalStringBytes` (Latin-1 internal bytes) and throws `IllegalArgumentException` if that string is UTF-16. Zel writes `name.as_bytes()` (UTF-8). ASCII and internal strings whose Latin-1 bytes are already UTF-8 match. A Latin-1 key in U+0080–U+00FF is one raw byte in Bazel and a two-byte UTF-8 sequence in Zel. A UTF-16 key aborts in Bazel and is emitted as UTF-8 in Zel. A key that is not valid UTF-8 is written as raw bytes in Bazel and cannot be held in Zel’s `String`.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/UsedHeapSizeAfterGcInfoItem.java`
- Bazel lines: 50
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P3] Java calls `System.gc()` and then prints heap `MemoryMXBean.getHeapMemoryUsage().getUsed()`. Zel prints `getrusage` `ru_maxrss` (peak RSS: bytes on macOS, KiB×1024 on Linux, else 0), the same figure as `used-heap-size`, and does not collect garbage.
  - [P3] Java subtracts the OpenJDK 21+ G1 filler-array size from a `gcClassHistogram`: lines where `Matcher.find` matches `--jvm_heap_histogram_internal_object_pattern` (default `jdk\.internal\.vm\.Filler.+`), summing whitespace field 3. The offset is 0 on older or non-OpenJDK VMs and when the histogram call fails. Zel ignores that option and subtracts nothing.
  - [P3] If used heap minus that offset is negative, Java’s pretty-printer emits a signed value such as `-123B`. Zel’s byte count is a `u64`.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/UsedHeapSizeInfoItem.java`
- Bazel lines: 39
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel prints the current JVM heap occupancy from `MemoryMXBean.getHeapMemoryUsage().getUsed()` (including not-yet-collected garbage). Zel prints `getrusage(RUSAGE_SELF).ru_maxrss` (`/workspace/zel-cursor/crates/zel-server/src/sys.rs`): the process peak RSS since start, in bytes on macOS and KiB×1024 on Linux, or `0` (`0B`) when `getrusage` fails or the OS is not Unix. An undefined Java usage (`-1`) formats as `-1B`.
  - [P3] Both use the same decimal cutoffs (`10000` / `10000000` / `100000000000`) and scales (`1000`, `10^6`, `10^9`) and append a newline. Java divides in `double` and casts the quotient to a signed 32-bit `int` (saturating at `2147483647`); any negative `long` is below `1E4` and is printed as that signed value plus `B`. Zel integer-divides a `u64` and prints the full quotient.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/WorkerMetricsInfoItem.java`
- Bazel lines: 99
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - [P2] With live workers, Bazel inserts `{memoryKb/1024}MB, last action ` (truncating `int` KB/1024), then `{minutes}m ` only when `(nowMillis - lastActionStartTimeInMs) / 1000 / 60 > 0`, then `{remainingSeconds}s ago, ` before `executed`. The collector always attaches one stats entry, so that clause is present. Zel omits memory and last-action time.
  - [P2] Bazel sorts worker ids and prints `id N` for a single id and `ids [a, b]` otherwise, including every multiplex id on that process. Zel always prints one `id {worker_id}` and does not group multiplex ids.
  - [P2] Bazel lists every registered worker that is not killed, including a singleplex worker that is currently executing, and drops a worker that measures 0 KB after it has executed actions. Zel lists only idle processes plus the multiplex shared process, so a borrowed singleplex worker is absent, and any process that is still alive is kept.
  - [P3] Bazel emits lines in `ConcurrentHashMap` pid order. Zel sorts lines by worker id.
  - [P3] The parenthesized hash is `Long.toHexString` of the sign-extended 32-bit `WorkerKey.hashCode()` (negative hashes are 16 hex digits). Zel prints lowercase hex of `DefaultHasher` over its worker key, which also folds in the worker-files hash and leaves out in-memory tracking.
  - [P3] `actions_executed` is a signed 32-bit counter that wraps to a negative value and increments when execution finishes. Zel prints a `u64` incremented when the request is sent.
  - [P3] The process id is the proto `uint32` held in a Java `int` (the collector narrows the pid), so a value above 2147483647 prints negative. Zel prints a `u32`.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/info/WorkspaceInfoItem.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel prints the stored workspace path (`BlazeDirectories.getWorkingDirectory()`, Latin-1 internal bytes plus `\n`) and does not resolve symlinks again. Zel runs `std::fs::canonicalize` and prints UTF-8 (`Path::display()`, U+FFFD for non-UTF-8 bytes) plus `\n`, so a symlink change after startup, a non-canonical `--workspace_directory`, or a non-UTF-8 path differs. If canonicalize fails, Zel falls back to the stored string.
  - [P2] A null workspace prints `null` plus a newline. Zel sets `requires_workspace` and, outside a workspace, omits the key or returns a per-key workspace error instead.

### `src/main/java/com/google/devtools/build/lib/runtime/commands/license/merge_licenses.bzl`
- Bazel lines: 77
- Zel counterpart: `/workspace/zel-cursor/scripts/generate_license.py`
- Verdict: partial
- Differences:
  - [P3] Zel is an offline generator, not the Starlark rule: no `srcs`/`out`/`is_windows`, no `fail`, no `select` on `@bazel_tools//src/conditions:windows`, and no `ctx.actions.run` / `run_shell`. It always writes the bash recipe (`===== {rel} =====\n`, raw file bytes, `\n\n`).
  - [P3] On Windows Bazel runs `cmd.exe /V:ON /E:ON /Q /C` with backslash `OUT`/`SRCS`, a banner after `\`→`/`, and `TYPE` plus `ECHO.`/`ECHO.`. Zel never takes that branch.
  - [P3] Bazel keeps `ctx.files.srcs` order and keeps a file when its path contains `ASSEMBLY_EXCEPTION`, `DISCLAIMER`, `LICENSE`, `license`, or `THIRD_PARTY_README`, using the action exec path in the banner. Zel always starts with repo-root `LICENSE`, walks only `third_party/` (`dirs.sort()`), then sorts matching relative paths and prints those paths. Other srcs are omitted.
  - [P3] Bazel `fail("expected some sources")` when nothing matches. Zel exits 1 only if `LICENSE` is missing; a present `LICENSE` with no third-party matches still succeeds. `--check` exits 1 when the output bytes differ.
  - [P3] Bazel joins paths with spaces and expands unquoted `$SRCS` / `%SRCS%`, so spaces and glob characters change the file list. Zel writes one section per path and does not word-split.

### `src/main/java/com/google/devtools/build/lib/runtime/events/GotProjectFileEvent.java`
- Bazel lines: 36
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No Zel postable event carries a parsed project file's workspace-relative path. Bazel stores the constructor string unchanged, including a null reference, and returns that same string.

### `src/main/java/com/google/devtools/build/lib/runtime/events/InputFileEvent.java`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No postable value with a type string and byte size is constructed or published on the event bus.
  - [P3] `--target_pattern_file` and `--query_file` reads do not report `"target_pattern_file"` or `"query_file"` together with the file length before the file is read.

### `src/main/java/com/google/devtools/build/lib/runtime/mobileinstall/MobileInstallCommand.java`
- Bazel lines: 416
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/print_action.rs`
- Verdict: partial
- Differences:
  - [P2] After a successful build, Bazel launches the deployer (or returns a client exec request). Zel exits 0 and does not run it.
  - [P2] Deployer argv is `<configuration bin fragment>/<label path>_mi/launcher`, then residue after the first target, `--build_id=<command id>`, `--tool_tag=` when non-empty, `--start=`, `--adb=` / `--adb_arg=` / `--device=` when set, `--test_filter=` when the test filter is non-null and non-empty, and `--test_arg=` for each non-empty test arg. The working directory is the parent of the workspace output path.
  - [P2] `--run_in_client` defaults to false and runs that argv as a child with the client environment. A non-zero child status is exit 6 (`NON_ZERO_EXIT`) with `Non-zero return code '<child>' from command: ...`, not the child status. A launch failure is exit 6 (`ERROR_RUNNING_PROGRAM`) with `Error running program: ...`. Interruption is exit 8. True returns an exec request (`shouldExec`, working directory, argv, paths to replace, `BUILD_WORKING_DIRECTORY`, `BUILD_WORKSPACE_DIRECTORY`) instead of spawning. Zel ignores the flag.
  - [P2] A null successful-target set warns `No targets found to run` and exits 0. Any other size than one errors `Can only run a single target. Do not use wildcards that match more than one target` and exits 2 (`MULTIPLE_TARGETS_SPECIFIED`). Zel does not apply that check, so a pattern that builds several targets still exits 0.
  - [P2] The rule check uses the built configured target after `getActual()`. A non-`AbstractConfiguredTarget` fails with `Invalid target` (exit 6). Otherwise it uses `getRuleClassString()` (empty for non-rules). Zel re-resolves only the first pattern, discards resolve errors, and compares `target_kind()` with a trailing ` rule` removed, so an alias is `alias` and a source file is `source file`.
  - [P2] `mobile_install_supported_rules` keeps empty comma-separated segments, so `android_binary,` includes `""` and matches an empty rule class. Zel drops empty segments, which changes both acceptance and the `[...]` error text. A non-empty list that fails the check is exit 6 in both.
  - [P3] Failure to parse the injected `--aspects=<mobile_install_aspect>%MIASPECT` and `--output_groups=mobile_install_INTERNAL_` / `mobile_install_launcher_INTERNAL_` flags is an `IllegalStateException` in Bazel and a command-line error (exit 2) in Zel.

### `src/main/java/com/google/devtools/build/lib/server/CommandManager.java`
- Bazel lines: 288
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/daemon.rs`
- Verdict: partial
- Differences:
  - [P2] Command ids are `{server_pid}-{counter from 1}`. Bazel uses a random UUID per registered command.
  - [P2] Bazel keeps a `HashMap` of every registered command (including those still waiting for the output-base lock), each with its thread, preemptible bit, and terminal-size monitor. Zel tracks cancel flags, one exclusive current command, and a running count. `preemptEligibleCommands` and `interruptInflightCommands` are absent, and `RunRequest.preemptible` is never read, so nothing interrupts only preemptible threads or every in-flight command thread.
  - [P2] Cancel sets that id's flag and `request_global_cancel()`, which kills every in-flight spawn until the next command takes the lock and clears the flag. Bazel interrupts only the matching command thread. A missing id does nothing in both.
  - [P3] Bazel cancel registers and immediately closes a non-preemptible command. If the map was empty that calls `busy()`, ends the idle period, stores idle-task results, notifies waiters, and does not restart idle tasks. Zel cancel only refreshes last activity, which clears the idle-task-done flag once activity is under 1s so the action-cache save can run again after another 10s.
  - [P1] If a cancel flag stays set for 10s, Zel returns exit code INTERRUPTED (`Build interrupted`) while the command thread may still hold the lock. Bazel’s daemon watcher only warns after 10s (`warnAboutSlowInterrupt`) and leaves the command running. A stream reset or dead client also makes Zel set the cancel flag and return INTERRUPTED immediately.
  - [P2] Terminal-size updates are not applied. Bazel updates that command’s monitor (ignored when columns are ≤ 0 or unchanged; a new listener is replayed the last size when columns are > 0).
  - [P3] Idle work is not the registered idle-task list. With idle server tasks enabled and no running commands, Zel writes the action cache once after 10s and exposes no results. Bazel schedules each task the finishing command registered, after that task’s own delay, only when idle tasks are enabled; `busy()` interrupts them and `getIdleTaskResults()` returns those results once (null if none). The constructor and commands that never set tasks start no idle work.
  - [P3] Bazel `wait()` / `wait(timeout)` blocks on the map and wakes on every register and close, throwing `InterruptedException`. Zel waits in 500ms slices while blocked for the lock and polls the command thread every 20ms, waking on cancel and lock release.

### `src/main/java/com/google/devtools/build/lib/server/GcAndInternerShrinkingIdleTask.java`
- Bazel lines: 71
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No idle task calls a full GC and then logs heap used and committed before and after.
  - [P3] Delay is 10 seconds when state is kept after the build and zero otherwise. Nothing selects a GC or shrink delay from `keep_state_after_build`.
  - [P3] When the idle thread is not interrupted and state is not kept, every pooled interner is shrunk. There is no interner shrink, and a set interrupt status does not suppress a later shrink.

### `src/main/java/com/google/devtools/build/lib/server/GrpcServerImpl.java`
- Bazel lines: 787
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/daemon.rs`
- Verdict: partial
- Differences:
  - [P2] Request-cookie checks are constant-time ISO-8859-1 bytes (unmappable characters become `?`). Zel uses UTF-8 string equality. Run args and startup-option bytes are Latin-1 strings in Java; Zel lossy-decodes them as UTF-8 (`U+FFFD`) and drops startup-option `source`. Server cookies are unpadded hex of each random byte plus 128; Zel does not generate them (the client writes zero-padded raw-byte hex).
  - [P3] A bad `Run` cookie is exit 36 / `BAD_COOKIE` with no response cookie. Zel uses that same exit and detail but also sends the response cookie. An empty client description is exit 36 / `NO_CLIENT_DESCRIPTION`; Zel runs the command as `"unknown"`.
  - [P2] Bad-cookie `Ping` succeeds with an empty cookie, and bad-cookie `Cancel` completes with no `CancelResponse`. Zel returns gRPC status 7 for both. `UpdateTerminalSize` is missing (gRPC status 12); Java checks the cookie, updates that command's rows and columns, and returns the response cookie.
  - [P1] An invocation-policy parse error is printed to stderr and returned as exit 2 / `INVOCATION_POLICY_PARSE_FAILURE`. Zel never reads `invocation_policy`. `quiet` and `preemptible` are ignored, so preemptible commands are not interrupted when another command arrives.
  - [P2] Stdout and stderr are streamed in write order in 8192-byte chunks, and command extensions are separate mid-command responses. Zel flushes about every 20ms as stderr chunks then stdout chunks (same chunk size), then the remainder in that order, and puts extensions only on the final response. A failed send is swallowed and the output thread is interrupted; Zel aborts the RPC.
  - [P1] Cancel interrupts that command thread and only warns if it is still running after 10s. Zel sets a process-global cancel and, if the command is still running after 10s, finishes the RPC as exit 8 with message `Build interrupted` and stderr `Build interrupted\n` (dropping `ERROR: spawn cancelled` lines). A stream reset or disconnect takes that path immediately. Java's dispatch interrupt path uses exit 8, message `Command dispatch interrupted`, and an empty command id.
  - [P2] Listen tries `[::1]` (three times on macOS, once otherwise; it fails immediately when epoll is available and IPv6 is not preferred) and then `127.0.0.1`. Bind failure is exit 1 / `SERVER_BIND_FAILURE`. Zel is bound only to `127.0.0.1`, and bind failure is exit 36.
  - [P1] The idle watcher runs only when max idle is positive: shutdown after that many idle seconds, and low-memory shutdown only after 5 minutes idle (Linux: free RAM under 5% and under 1 GiB; otherwise non-normal memory pressure), polled every 5s. Zel can shut down for low memory even when max idle is 0, after 10s idle, when available/total (or the macOS free-page fraction) is under 5%, with no absolute floor. Idle-task results are not passed into the next command; the action cache is saved after 10s idle.
  - [P0] Status files are Latin-1 and deleted from shutdown hooks unless `prepareForAbruptShutdown` disables those hooks and the pid watcher. Zel writes UTF-8, always deletes the status files on exit, and a shutdown request also deletes the output base. Java stops the gRPC server only when the command result requests shutdown; an interrupt of `awaitTermination` is an `IllegalStateException`, while Zel exits cleanly on SIGTERM.

### `src/main/java/com/google/devtools/build/lib/server/IdleTask.java`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No idle-task type with a display name, a default delay of zero, and a run that distinguishes interruption from other failure.
  - [P3] No status of never started, success, failure, or interrupted, and no result carrying the task name, that status, and elapsed running time.

### `src/main/java/com/google/devtools/build/lib/server/IdleTaskException.java`
- Bazel lines: 24
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel defines a checked exception whose only constructor stores an `Exception` cause and sets the message to `cause.toString()` (null when the cause is null), so idle-task failures other than interruption keep that cause. Zel has no such wrapper and does not classify a cause-bearing idle-task failure separately from interruption.

### `src/main/java/com/google/devtools/build/lib/server/IdleTaskManager.java`
- Bazel lines: 158
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/daemon.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel schedules every supplied idle task on a single thread after `delay().toSeconds()` (fractional seconds truncated), in registration order, and may start a later task only after earlier ones finish. Zel runs one hardcoded action-cache `save_to_disk` once `idle_server_tasks` is set and the process has been idle for 10 seconds.
  - [P3] `idle()` and `busy()` are one-shot: the wrong state throws `IllegalStateException`. Zel uses an `idle_tasks_done` flag that is cleared whenever the last activity is under 1 second, so the same process can save again after another 10 seconds of idleness.
  - [P3] `busy()` interrupts the running task, drops tasks that have not started, blocks until the executor terminates (interrupts ignored), and returns one result per task: success, `IdleTaskException` failure, interruption, or not-started (`Duration.ZERO`). Any other exception becomes `IllegalStateException`. Zel does not interrupt or cancel idle work, does not wait out a delay queue, ignores save errors, and returns no status or elapsed time.

### `src/main/java/com/google/devtools/build/lib/server/InstallBaseGarbageCollector.java`
- Bazel lines: 157
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel never walks the install-base parent (symlink-following directory listing), never treats a child as an install base only when it is a directory whose basename is exactly 32 characters in `[0-9a-f]`, and never skips the current install base.
  - [P2] Stale bases are not collected: no exclusive lock on the sibling `<basename>.lock` (held lock or missing directory aborts), no mtime age where `age < maxAge` keeps the base and `age >= maxAge` collects it, no rename to `<random-uuid>.deleted`, no early lock-file delete, and no tree delete of the renamed path.
  - [P2] Incomplete deletions (directory names ending in `.deleted`) are not retried with a tree delete.
  - [P3] A walk is not aborted with an interrupt when the thread is interrupted between entries. Idle server work only persists the action cache; `experimental_install_base_gc_max_age` is unused flag metadata.

### `src/main/java/com/google/devtools/build/lib/server/InstallBaseGarbageCollectorIdleTask.java`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Bazel `create` builds an install-base collector for the install base’s parent directory, the current install base, and the given max age, and `run` executes that collector. Zel never schedules this idle task, so unused sibling install bases are not collected.
  - [P3] An `IOException` from the collector becomes an `IdleTaskException`, while interruption propagates. Zel has no equivalent failure or interrupt path for this task.

### `src/main/java/com/google/devtools/build/lib/server/PidFileWatcher.java`
- Bazel lines: 138
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/daemon.rs`
- Verdict: partial
- Differences:
  - [P3] Java polls a daemon thread every 3s (initial delay 3s). Rust checks only between accept-loop iterations, about every 20ms while idle.
  - [P2] Java reads the file as Latin-1 and treats it as valid only when `Integer.parseInt` of the entire contents equals the expected `int` pid (no trim: trailing newline, empty, non-numeric, or a different pid are invalid). Rust never reads or parses the contents.
  - [P3] An unreadable file is invalid in Java. Rust reacts only when `exists` is false.
  - [P1] If the pid is invalid and shutdown has not been signaled, Java calls `Runtime.halt(37)` (`BLAZE_INTERNAL_ERROR`), skipping shutdown hooks so the pid file is not deleted, then throws if that returns. Rust, and only when no command is running, leaves the serve loop, deletes status files including `server.pid.txt`, and returns success.
  - [P2] An overwritten pid halts Java immediately, including during a command. Rust ignores content changes, and ignores deletion while a command is running until `running == 0`.
  - [P3] `signalShutdown` makes a later invalid read stop the watcher without halting. Rust has no such mode: the shutdown flag exits before the pid check and still deletes status files, and a deleted pid with no shutdown flag still takes the success-and-cleanup path once idle.
  - [P3] `endWatch` stops the watcher and waits before the pid file is deleted. Rust deletes the pid file as part of leaving the loop.

### `src/main/java/com/google/devtools/build/lib/server/ServerWatcherRunnable.java`
- Bazel lines: 205
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/daemon.rs`
- Verdict: partial
- Differences:
  - [P2] The idle deadline is armed only when `maxIdleSeconds > 0`, at the moment the command map is observed empty, and it is rechecked only after `wait` (5s while idle, unbounded while a command is registered), so shutdown can lag by up to 5s. Zel shuts down when `max_idle_secs > 0`, `running == 0`, and time since `last_activity` reaches that limit, polling about every 20ms. `last_activity` is refreshed on every Ping, Cancel, and Run, not only when the command map becomes empty.
  - [P1] The low-memory path runs on that same watcher, so `max_idle_secs == 0` disables it. Zel still shuts down for low memory when `shutdown_on_low_sys_mem` is set and `max_idle_secs` is 0.
  - [P1] The memory check is suppressed until the server has been idle for 5 minutes, then runs on each idle wake. Zel uses a 10-second idle gate and then at most one check every 5 seconds.
  - [P1] On Linux, shutdown requires free/total `< 0.05` and free RAM `< 1<<20` KiB. If `MemAvailable` is absent, free RAM is estimated as `MemTotal - Active - 0.3*Inactive - 0.8*Slab`. A read or missing-key failure does not shut down. Zel shuts down when `MemAvailable/MemTotal < 0.05` only, with no 1 GiB ceiling and no estimate (`None` does not shut down).
  - [P2] On other OSes, Bazel shuts down when the memory-pressure level is not `NORMAL` (macOS events; Windows and other Unix stay `NORMAL`). Zel on macOS uses `(free + speculative pages) / hw.memsize < 0.05`, and elsewhere reads `/proc/meminfo`.
  - [P3] The meminfo values keep the first duplicate key and parse every digit in the field as a signed long. A bad or overflowing field is dropped. Zel parses the first `MemAvailable:` / `MemTotal:` token as `f64`, and invalid UTF-8 fails the whole read.

### `src/main/java/com/google/devtools/build/lib/server/ShutdownHooks.java`
- Bazel lines: 114
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Bazel `createAndRegister` installs a JVM shutdown hook that runs cleanup on process exit. Zel has no exit hook for this; status files are deleted only by an explicit `cleanup_server_status_files` call when a serve loop returns.
  - [P3] `deleteAtExit` appends arbitrary paths and deletes that list in registration order. Zel deletes a fixed set (`command_port`, `request_cookie`, `response_cookie`, `server.pid.txt`, `server_info.rawproto`) and ignores delete errors.
  - [P3] `cleanupPidFile` keeps a single callback (a later call replaces it) that stops the PID watcher (`endWatch`) and then deletes the PID file. IOExceptions are logged at severe with a banner and stack trace. Zel has no watcher-stop-then-delete callback.
  - [P1] `disable()` makes `runHooks` return before any deletion, so a `clean --expunge` shutdown cannot remove a successor server’s files. Zel has no such flag; leaving the daemon, including because the pid file disappeared, still deletes those status paths.

### `src/main/java/com/google/devtools/build/lib/server/TerminalSizeMonitor.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No per-command monitor: a disabled instance drops both listener registration and size updates, and an enabled instance starts at columns 0 and rows 0.
  - [P3] Registering a listener while the stored column count is greater than 0 immediately notifies that listener with the stored columns and rows. Rows are delivered even when they are not positive.
  - [P3] An update with columns less than or equal to 0 is ignored. An update equal to the stored columns and rows does not notify. Any other positive-column update stores both dimensions and notifies the listeners registered at that moment, in registration order, with those new values.

### `src/main/java/com/google/devtools/build/lib/server/signal/InterruptSignalHandler.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Construction installs a SIGINT handler that invokes the subclass `run()` on the JVM signal thread and keeps the previous handler. Zel never installs that SIGINT callback.
  - [P3] `uninstall()` is synchronized, restores that saved handler, then clears it. A second call throws `NullPointerException` (`uninstall() already called`). Zel has no restore and no second-call failure.
