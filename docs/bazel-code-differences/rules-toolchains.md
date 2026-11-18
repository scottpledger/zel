# Bazel vs zel code differences: rules toolchains

Category rule: Native rule implementations, language rules, platforms, configurations, and toolchain logic.

- Failure entries: 158
- Missing: 54
- Partial: 104
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `src/main/java/com/google/devtools/build/lib/platform/SleepPreventionModule.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - For every command, when JNI loaded, a sleep-disable request is pushed in before-command and popped in after-command (including command failure; a crash skips the pop, and the OS drops the request when the process dies). The int result is ignored, so unsupported does not fail the command or emit an event. The command environment is unused. No flag turns this off. If JNI failed to load, both hooks are skipped.
  - On macOS the first push creates an IOPM assertion of type no-idle-sleep named build.bazel at level on. Further pushes only increment a counter, and the assertion is released only when the count returns to 0. Display sleep is not blocked. A failed create or release aborts the process. On Windows the first push sets continuous, system-required, and away-mode-required (system idle and away mode, not display-required). The matching pop back to 0 restores continuous. Both paths return 0.
  - On Linux, FreeBSD, OpenBSD, and an unknown OS the native calls return -1 and do not change power state, so a command does not inhibit sleep. That end state matches on those systems.
  - A command is never bracketed with a power assertion or SetThreadExecutionState. Sleep prevention is recorded as unimplemented. A long command on macOS or Windows can idle-sleep while JNI is available.

### `src/main/java/com/google/devtools/build/lib/platform/SystemCPUSpeedEvent.java`
- Bazel lines: 46
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Nothing accepts a CPU speed, rejects values outside 1 through 100 with an illegal-argument failure, and returns that same value. A signed 32-bit int is stored only after speed is greater than 0 and at most 100 (so 0, negatives including the minimum int, and anything above 100 throw illegal-argument with no message). Valid values are unchanged 32-bit ints. There is no wrap on the success path.
  - Nothing produces the log line `SystemCPUSpeedEvent: %d` (format `%d`, no grouping). For the only values that can be stored (1 through 100) that is a plain decimal.
  - Nothing is a postable build event that reports by posting itself, ignores tags (with-tag returns the same instance), and is not stored for Skyframe replay (store-for-replay is false). Equality is identity, so two events with the same speed are not equal.
  - Cpu-speed-mhz is a different result: optional megahertz from hw.cpufrequency or /proc/cpuinfo cpu MHz, or none. It does not take or check a 1–100 power-limit percent and does not emit this event.

### `src/main/java/com/google/devtools/build/lib/platform/SystemCPUSpeedModule.java`
- Bazel lines: 73
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs` (`system::cpu_speed_mhz`)
- Verdict: partial
- Differences:
  - When JNI is loaded, one process-lifetime monitor is registered and on each command start a speed is read. Values below 100 enter the callback. -1 is ignored. Any other value must be in 1 through 100 or building the CPU-speed event throws. A live reporter is posted that event, and an info line `SystemCPUSpeedEvent: <speed>` is always written. After the command the reporter is cleared, so later callbacks still log and are not posted. Nothing monitors, posts that event, or writes that line. Cpu-speed-mhz is an unused one-shot read and is not tied to command start or a notification.
  - The quantity differs. On macOS the IOPM processor speed-limit percent is read and -1 is returned when the call fails or the key is missing (not-found is not logged). Linux, BSD, and Windows always return -1 and do not monitor, so the callback does nothing. On macOS, hw.cpufrequency divided by 1,000,000 megahertz is returned as u64 (none on sysctl failure, some 0 when the value is 0). On other Unix the first cpu MHz line in /proc/cpuinfo is parsed as f64 and cast to u64, dropping the fraction. A missing file, missing line, or non-numeric field is none. That Linux path yields a frequency that is never surfaced.
  - A signed 32-bit -1 sentinel is used and values at or below 0 and above 100 are rejected with illegal-argument (0 and negatives other than -1 abort the callback). The test notification com.google.bazel.test.cpuspeed delivers an arbitrary state truncated from uint64 to int. Option of u64 is used: no -1, no 1-through-100 check, no exception, and 0 or values above 100 are ordinary megahertz. The u64 division and cast do not wrap like a Java int.
  - If JNI is unavailable, native cpu-speed is still called from before-command and throws unsatisfied-link-error. A second register-JNI posts a singleton assertion and does not install another callback. There is no command lifecycle and no singleton guard. The function exists only on Unix. On Windows -1 is returned and monitoring is skipped.

### `src/main/java/com/google/devtools/build/lib/platform/SystemDiskSpaceEvent.java`
- Bazel lines: 64
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Nothing maps a disk-space level of 0 to the log phrase Low and 1 to Very Low, or formats the event text `SystemDiskSpaceEvent: Low` and `SystemDiskSpaceEvent: Very Low`.
  - Nothing rejects every other signed 32-bit value with illegal-state and the message unknown disk space level plus that value’s decimal form (including negatives). The int is only converted to text. It is not wrapped or widened.
  - Nothing posts this as a reporter event that subscribers can branch on as low versus very low.
  - System disk-space only returns unprivileged free bytes and total bytes for a path (statvfs). It does not classify exhaustion levels or emit this event.

### `src/main/java/com/google/devtools/build/lib/platform/SystemDiskSpaceModule.java`
- Bazel lines: 62
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs` (`system::disk_space`)
- Verdict: partial
- Differences:
  - When constructed with JNI, one module is registered and, on macOS only, subscribed once to kNotifyVFSLowDiskSpace (com.apple.system.lowdiskspace), com.apple.system.verylowdiskspace, and the test names com.google.bazel.test.diskspace.low and .verylow. A notification logs disk space low anomaly or disk space very low anomaly, then calls back with 0 (low) or 1 (very low). Those names are never subscribed to and those lines are never emitted.
  - The callback builds a disk-space event and info-logs `SystemDiskSpaceEvent: Low` or `SystemDiskSpaceEvent: Very Low`. It posts the event only while before-command has stored a reporter. After-command clears it, so notifications outside a command are logged and not posted. Any int other than 0 or 1 throws illegal-state before the log or the post. There is no level, no reporter, and no command-scoped post.
  - Linux and BSD start functions are empty, and the Windows register-JNI is a no-op, so those platforms never fire. A second registration reports singleton already registered and keeps the first instance. Notify-register-dispatch failure aborts. The stock server module list does not construct this module. Only tests do.
  - One (free, total) pair is returned for a path: statvfs bavail and blocks times frsize, or bsize when frsize is 0, or an I/O error if statvfs fails or the path contains a NUL. It is only called from its unit test. On macOS those counts are stored as u32 before the u64 multiply, so volumes above 2^32 blocks truncate. On Linux the u64 product wraps. Byte counts are not reported from this module.

### `src/main/java/com/google/devtools/build/lib/platform/SystemLoadAdvisoryEvent.java`
- Bazel lines: 50
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - A signed 32-bit advisory level and an OS description string (including a Java null description) are stored and exposed unchanged. Log-string is `SystemLoadAdvisoryEvent: %d (%s)`: `%d` is the full int in the default format locale (no grouping; a null description renders as the characters null). As a postable, report-to posts the same instance, with-tag ignores the tag, and store-for-replay is false.
  - No type posts that event, returns that pair, or emits that log line. Load-average is Unix getloadavg floats (1, 5, and 15-minute averages, or the 1-minute average as a profiler counter), not the 0–100 macOS load-advisory level plus description this class carries.

### `src/main/java/com/google/devtools/build/lib/platform/SystemLoadAdvisoryModule.java`
- Bazel lines: 106
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - A command module is installed that, when JNI is present, registers a process-wide monitor, samples an advisory in before-command, drops the reporter in after-command, and accepts later system-load-advisory callbacks. There is no advisory module, no command hooks, and no callback.
  - On macOS the value is an IOSystemLoadAdvisory level: 0 (Great), 25 (OK), or 75 (Bad). Any other native level aborts before Java sees it. The Java label is unknown system load advisory value plus the signed 32-bit value only if some other int is passed in. Those levels or strings are never emitted.
  - On Linux, BSD, and Windows the native side does not monitor and returns 0. Java still describes that as unknown because the Great, OK, and Bad labels are Darwin-only. The initial sample is not logged (value at most 25 and initial), but a load-advisory event is still posted while a reporter is set.
  - Log policy is on the int value: severe if below 0 or above 100, warning if above 50, info if the sample is not the initial one or the value is above 25, otherwise silent. The event is posted whenever the reporter is non-null, including when nothing is logged. After after-command, callbacks still log and do not post. A second register-JNI reports an assertion error and keeps the first instance.
  - The comment points the Darwin load-advisory monitor at load-average, which is getloadavg (1, 5, and 15-minute f64s, or none). The server returns only the one-minute average, or 0.0 on failure, as the profiler counter system load average. That matches the operating-system load-average series, not this 0, 25, 75 advisory, and it does not apply the 25, 50, 100 log bands or post the load-advisory event.

### `src/main/java/com/google/devtools/build/lib/platform/SystemMemoryPressureEvent.java`
- Bazel lines: 40
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No postable event stores a memory-pressure level and returns `SystemMemoryPressureEvent: Normal`, `SystemMemoryPressureEvent: Warning`, or `SystemMemoryPressureEvent: Critical` from log-string.
  - Nothing posts that event on the reporter or event bus, and nothing writes that info log when OS memory pressure changes. In-tree subscribers only see the level through this event.
  - A null level is accepted by the constructor and makes log-string throw null-pointer. There is no value to compare.
  - Memory-pressure only polls a current level. It does not build, post, or log this event.

### `src/main/java/com/google/devtools/build/lib/platform/SystemMemoryPressureModule.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - When this module is installed, before-command stores that command’s reporter on the memory-pressure monitor singleton and after-command clears it to null. A command event sink is never bound or unbound, so memory-pressure notifications are not posted on the active command’s event bus and are not dropped when the command ends.
  - Both hooks call set-reporter, which re-samples the native level. If it is not normal (0), the callback runs immediately: at command start that posts the memory-pressure event and logs warning or critical. At command end the reporter is already null, so the same non-normal sample is only logged. Neither happens.
  - With a reporter set, later native callbacks post that event for levels 0, 1, and 2 (normal, warning, critical) and always log that string. Any other native int throws illegal-state from the sample taken on these hooks. There is no callback path. Memory-pressure and the integer level only return a level and are not called from the command lifecycle. This class is left out of the stock module list, so a default server never runs the hooks unless a test or embedder adds the module.

### `src/main/java/com/google/devtools/build/lib/platform/SystemMemoryPressureMonitor.java`
- Bazel lines: 96
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs` (`system::memory_pressure`; integer form `zel_memory_pressure_level` in `/workspace/zel-cursor/crates/zel-ffi-bridge/src/lib.rs`)
- Verdict: partial
- Differences:
  - A process singleton registers an OS listener and, on each notification, posts a memory-pressure event to the current reporter when one is set and always logs info `SystemMemoryPressureEvent: Normal|Warning|Critical`. Set-reporter stores that reporter and, if the cached level is not normal, immediately posts and logs it. A null reporter skips the post but callbacks still log. Only a poll is done. A level is never posted, logged, or replayed.
  - Level codes 0, 1, and 2 are normal, warning, and critical in both. Any other Java int throws illegal-state before post or log. There is no such failure. An unreadable or unrecognized kernel value becomes normal.
  - On macOS the last DISPATCH_SOURCE_TYPE_MEMORYPRESSURE notification is cached (plus test notify com.google.bazel.test.memorypressurelevel states 0, 1, and 2) and starts at normal, so a query stays normal until the next transition, including when pressure is already elevated. An unknown dispatch flag aborts. kern.memorystatus_vm_pressure_level is read live: 2 is warning, 4 is critical, and every other value or a failed sysctl is normal.
  - On Linux and BSD monitoring never happens and level is always normal. On non-macOS, /proc/pressure/memory some avg10 is read and critical is returned at or above 40, warning at or above 10, and normal otherwise (a missing file or a bad parse is normal). On Windows, register is a no-op and the query returns 0. The module is Unix-only, so that poll is not built.

### `src/main/java/com/google/devtools/build/lib/platform/SystemSuspensionEvent.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - A suspension event is never constructed. JNI reason codes 0 through 3 map to SIGTSTP, SIGCONT, sleep, and wake, and illegal-state (`Unknown suspension reason: ` plus the number) is thrown for every other signed 32-bit value.
  - Log-string is `SystemSuspensionEvent: ` plus `Signal SIGTSTP`, `Signal SIGCONT`, `Computer put to sleep`, or `Computer woken up`. Nothing emits that text.
  - The event implements postable and is posted from the suspension callback. Suspension monitors are recorded as not implemented, so SIGTSTP, SIGCONT, sleep, and wake never surface as an event.

### `src/main/java/com/google/devtools/build/lib/platform/SystemSuspensionModule.java`
- Bazel lines: 63
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - A process-wide JNI suspension callback is registered when JNI is available. A suspension monitor is never installed (it is listed as unimplemented) and SIGTSTP, SIGCONT, sleep, and wake are never observed.
  - On callback reasons 0 through 3, info-logs are `SystemSuspensionEvent: Signal SIGTSTP`, `Signal SIGCONT`, `Computer put to sleep`, or `Computer woken up`. That event is posted on the command reporter only between before-command and after-command. Outside a command it still logs and does not post. Neither the log nor the event is emitted.
  - Any other reason is a Java signed 32-bit int outside 0 through 3 and throws illegal-state before the log or post. There is no reason-code path.

### `src/main/java/com/google/devtools/build/lib/platform/SystemThermalEvent.java`
- Bazel lines: 50
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - There is no thermal-event value: nothing stores a thermal level and an OS description, and nothing formats `SystemThermalEvent: %d (%s)` (a null description prints as the word null, and `%d` is the signed 32-bit level).
  - The event is never posted. The macOS thermal monitor that would emit it is recorded as unimplemented.

### `src/main/java/com/google/devtools/build/lib/platform/SystemThermalModule.java`
- Bazel lines: 111
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - A native thermal monitor is registered when JNI is available, the current thermal load is read at the start of each command, and later native callbacks are accepted. Thermal pressure is never sampled and a monitor is never installed (thermal monitoring is documented as unimplemented, alongside sleep prevention and suspension).
  - On Darwin, 0, 33, 50, 90, and 100 are labeled Nominal, Moderate, Heavy, Trapping, and Sleeping, and every other integer is Unknown. On every other OS the label is always Unknown. There is no thermal value or OS label.
  - The event is logged at severe when the value is outside 0–100, at warning when it is greater than 50, and at info for any other value except an initial 0 (which is silent). The thermal event is posted only while a command reporter is installed, and the reporter is dropped after the command so later callbacks are logged but not posted. Neither the log line nor the event is emitted.

### `src/main/java/com/google/devtools/build/lib/rules/Alias.java`
- Bazel lines: 162
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - An alias wrapper is built: `getLabel()` is the actual target’s label (alias chains are followed), `getOriginalLabel()` is the alias, and `AliasProvider` plus the alias’s own visibility and required config fragments override the actual target. The actual configured target is cloned, the label is set to the alias, `rule_class` is forced to `alias`, and `actions` and `typed_actions` are dropped, so the chain and the actual label are gone.
  - A label whose canonical form starts with `@bazel_tools//platforms` is a rule error. That error is not emitted.
  - For `@bazel_tools//tools:host_platform` and `@@bazel_tools//tools:host_platform`, `actual` is resolved in `target(<cpu>, "fastbuild")` reduced to the core configuration. The alias’s incoming configuration is kept.
  - Toolchain resolution is turned on only when `$config_dependencies` or `target_compatible_with` is non-empty. A `$target_platform` edge is still recorded for every alias whose name is not `host_platform`.
  - Visibility failures on `actual` are discarded (`let _ = check_dependency(...)`). They are reported as rule errors. `testonly` on the alias is then written onto the cloned target, so dependents see the alias attribute. That check is skipped because the rule can have any provider, and dependents are checked against the actual target.
  - `is_test` is copied from `actual`, so an alias of a test is itself a test target. Tests selected via the alias are not run.

### `src/main/java/com/google/devtools/build/lib/rules/AliasConfiguredTarget.java`
- Bazel lines: 261
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - `getLabel()` walks the alias chain and returns the ultimate actual label. `getOriginalLabel()` is the alias label (`actionLookupKey`). `label` is overwritten with the alias rule’s own label and there is no separate original label.
  - `getActual()` follows the chain; `getActualNoFollow()` returns the direct actual. One resolved target is cloned and no actual link is kept.
  - Class provider lookup prefers overrides. `AliasProvider` and the alias `VisibilityProvider` (alias visibility and symbolic-macro bit) always beat both the actual and caller overrides. `RequiredConfigFragmentsProvider` is installed only when the alias’s required fragments are non-null. The actual’s provider map and `visibility` are copied and `rule_class` is set to `"alias"`, with no alias provider, visibility override, or required-config-fragments provider.
  - Starlark `get(String)`, `get(Provider.Key)`, `getIndex`, and `containsKey` forward to the actual and do not see overrides. `getValue` special-cases `label` (actual label) and `files` (depset of `FileProvider.getFilesToBuild()`, so an override wins). Unknown-field error is null; field names come from the actual. There is no Structure split: the cloned label is the alias label and files are the cloned `default_info`.
  - Configuration is the alias rule’s lookup-key configuration, including when `actual` is a source file or self-transitioned. `n.cfg` is used except for `@bazel_tools//tools:host_platform` and `@@bazel_tools//tools:host_platform`, which force `target(cpu, "fastbuild").to_core_only()` for both the result and the actual lookup.
  - Config conditions are the alias rule’s map. They are not stored.
  - The alias lookup key owns no actions; the actual’s actions stay on the actual target. `actions` and `typed_actions` are cleared on the only cloned target, so those actions are not reachable from the alias result.
  - `getProvidersDictForQuery` is the actual’s Starlark dict (insertion order, overrides excluded). The cloned `BTreeMap` (sorted string keys) of the actual’s providers is exposed.
  - `repr` is `<alias target <alias label> of <actual label>>`. There is no alias repr.
  - A null actual, overrides, or config-conditions map fails with `checkNotNull`. `Err` is returned with `missing value for mandatory attribute 'actual' in 'alias' rule` when `actual` is not a label and resolver errors are propagated; `check_dependency` errors are discarded.

### `src/main/java/com/google/devtools/build/lib/rules/LabelBuildSettings.java`
- Bazel lines: 130
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - `label_setting` is not a command-line flag (`BuildSetting.create(false, NODEP_LABEL)`); `label_flag` is. `build_setting` is left false on both and `label_setting` calls `label_flag`, which applies `flag_values`. `--//pkg:label_setting=…` is rejected as an unrecognized option because `isFlag()` is false.
  - `scope` defaults to `universal`, is nonconfigurable, and must be one of `universal`, `target`, or `project`. That default and nonconfigurable string are kept but the set is not restricted.
  - Late-bound `:alias` uses `build_setting_default` when the rule or configuration is null or no Starlark option is set for the rule label. A present option must already be a `Label`, or `Preconditions.checkState` fails. The raw `flag_values` string is `parse_canonical`d (absolute labels only; a relative label is an analysis error). If `build_setting_default` is not a `Label`, analysis returns an empty target instead of failing the mandatory attribute.
  - The aliased target’s visibility check is discarded (`let _ = check_dependency(..., "actual", ...)`), so an invisible actual target is still aliased.
  - A command-line value equal to the default stays in `flag_values` because the native class is not marked a build setting. That option is dropped before this resolver runs, so the configuration key differs while the chosen label does not.

### `src/main/java/com/google/devtools/build/lib/rules/LateBoundAlias.java`
- Bazel lines: 146
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - A null `:alias` prerequisite builds an empty configured target that still provides `RunfilesProvider` of `Runfiles.EMPTY` and `LateBoundAliasProvider`. `cc_libc_top_alias` and `label_flag` treat a missing label as `None` and return `finish()`, which has empty default runfiles and no `LateBoundAliasProvider`.
  - A present prerequisite becomes an `AliasConfiguredTarget`: `getLabel()` is the actual target’s label, visibility and the action lookup key stay on the alias rule, and a class-keyed override map supplies `LateBoundAliasProvider`. For a build setting that map also supplies `BuildSettingProvider` holding the build setting, the typed `build_setting_default`, and the rule label. The actual target’s providers are copied into a `BTreeMap`, `label` is set to the alias rule, and `BuildSettingProvider` is left unset. `label_flag` clears actions and sets `rule_class` to `alias`; `cc_libc_top_alias` keeps the actual target’s actions and rule class. `label_setting` follows `label_flag`.
  - `CommonAliasRule` sets `:alias` to a fragment `LabelLateBoundDefault` (null until the fragment returns a label), requires that fragment, drops `licenses` and `distribs`, and advertises `LateBoundAliasProvider`. The generated `cc_libc_top_alias` schema drops `licenses` and `distribs` and stores `:alias` with default `D::None`; analysis reads `--grte_top` directly and treats an empty string as absent.

### `src/main/java/com/google/devtools/build/lib/rules/ToolchainType.java`
- Bazel lines: 121
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/config.rs`
- Verdict: partial
- Differences:
  - `no_match_error` is stored on `ToolchainTypeInfo` as null when the string is empty and as the string otherwise. The provider has only `type_label`, so a non-empty message is dropped and missing-toolchain errors never append `: <message>`.

### `src/main/java/com/google/devtools/build/lib/rules/android/AndroidCommon.java`
- Bazel lines: 208
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No shared Android resource, manifest, and classpath helper.

### `src/main/java/com/google/devtools/build/lib/rules/android/AndroidConfiguration.java`
- Bazel lines: 1402
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/build_options_table.rs`
- Verdict: partial
- Differences:
  - `--experimental_incremental_dexing_after_proguard` is a Java `int` (`Integer.decode`, signed 32-bit, no wrap). Configuration conversion parses it as `i64` decimal, so values outside the `int` range are accepted and `0x`/`#` hex and leading-zero octal are not decoded. A negative shard count, `--experimental_incremental_dexing_after_proguard_by_default` with a shard count of 0, and `--desugar_java8_libs` while `--desugar_for_android` is off are rejected in the Java constructor and left as stored flags.
  - `ConfigurationDistinguisher.MAIN` has a null output-directory suffix (`getOutputDirectoryName` returns null and the mnemonic skips it). `ApkSigningMethod.signV4()` is null for `V1`, `V2`, and `V1_V2`, and true only for `V4`. An empty `--optimizing_dexer` (`EmptyToNullLabelConverter`) is null. The distinguisher and signing method are stored as enum-name strings and an empty optimizing-dexer string is kept.
  - `--android_platforms` (`LabelOrderedSetConverter`) keeps the first equal `Label` and drops later duplicates, in input order. Every comma-separated entry is kept, including duplicate labels and distinct spellings of the same label. Dexopt comma lists keep empty elements and their order; empty elements are dropped when those lists are split.
  - `useAndroidResourceShrinking()` is the OR of `android_resource_shrinking` and `experimental_android_resource_shrinking`. `checkForMigrationTag()` is the OR of `android_migration_tag_check` and `incompatible_disable_native_android_rules`. `shouldInclude()` is false when `incompatible_remove_ctx_android_fragment` is set. The flags stay independent, and `ctx.fragments.android` is an empty struct, so `getManifestMergerValue()` (`Ascii.toLowerCase` of the enum name) and the V1/V2/V4 accessors are absent.
  - Enum converters match with ASCII case-fold (`Ascii.equalsIgnoreCase`). Configuration conversion rewrites `-` to `_` and uppercases, so a hyphenated token becomes another constant. `AndroidManifestMerger.fromString` uses Unicode `equalsIgnoreCase` and returns null when nothing matches. `--desugar_java8_libs` sets `oldNameWarning = false`; the old name `--experimental_desugar_java8_libs` still emits a deprecation warning. Option-map order is alphabetical on both sides, and the `@Option` defaults match, including the null reference defaults and the persistent-worker expansions.

### `src/main/java/com/google/devtools/build/lib/rules/android/AndroidStarlarkCommon.java`
- Bazel lines: 213
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Starlark `android_common` module for Android providers and resource processing.

### `src/main/java/com/google/devtools/build/lib/rules/android/BazelAndroidConfiguration.java`
- Bazel lines: 70
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/build_options_table.rs`
- Verdict: partial
- Differences:
  - `--merge_android_manifest_permissions` is a boolean defaulting to false in both the options table and `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` (undocumented, affects outputs, `--flag`/`--noflag`, same help). The Starlark fragment `bazel_android` is not built: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs` has no arm for it, so `ctx.fragments.bazel_android` is an empty struct and does not expose `merge_android_manifest_permissions` with the flag value.

### `src/main/java/com/google/devtools/build/lib/rules/android/WriteAdbArgsAction.java`
- Bazel lines: 121
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - `--adb_arg` is a repeatable `List<String>` using the default string converter: each occurrence is one element, so `--adb_arg=a,b` is `["a,b"]` and `--adb_arg=` is `[""]`. The `"null"` default becomes an empty list. `ListString` comma-splits, so `--adb_arg=a,b` is `["a","b"]`, an empty occurrence adds nothing, and the unset default is absent rather than an empty list.
  - `--start` (`StartTypeConverter`) matches `NO`, `COLD`, `WARM`, `DEBUG` with ASCII case-folding and keeps the enum constant (`WARM`). The same names and the same default `NO` are accepted, but the raw spelling is stored, so `warm` and `WARM` remain different strings.
  - `--start_app` and `--debug_app` still expand to `--start=COLD` and `--start=DEBUG`. A supplied value is ignored in both; a warning that the value is ignored is also printed, and that warning is not.

### `src/main/java/com/google/devtools/build/lib/rules/apple/AppleCommandLineOptions.java`
- Bazel lines: 407
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/build_options_table.rs`
- Verdict: partial
- Differences:
  - Dotted-version flags (`ios_sdk_version`, `watchos_sdk_version`, `tvos_sdk_version`, `macos_sdk_version`, `ios_minimum_os`, `watchos_minimum_os`, `tvos_minimum_os`, `macos_minimum_os`, `host_macos_minimum_os`) reject a component whose trailing number does not fit in a signed 32-bit int (`1a2147483648`, `2beta2147483648`). Zel only range-checks the leading digit run, so those values are accepted and kept as the original string.
  - `ios_multi_cpus`, `visionos_cpus`, `watchos_cpus`, `tvos_cpus`, `macos_cpus`, and `catalyst_cpus` keep empty comma-separated segments (`a,,b` is `["a","","b"]`, `a,` is `["a",""]`). Zel drops empty segments. Unset (default `null`) stays null; Zel’s fragment default is an empty list.
  - `getMinimumOsVersion()` returns the minimum-OS flag for the current platform (`ios` and `catalyst` share `ios_minimum_os`; `visionos` is always `1.0`) and throws on an unknown platform. Zel does not select a version that way.

### `src/main/java/com/google/devtools/build/lib/rules/apple/AppleConfiguration.java`
- Bazel lines: 405
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - `single_arch_cpu` is `--cpu` with a leading `darwin_` removed. A non-empty `apple_split_cpu` is used, otherwise the first CPU of that platform’s list, and if the list is null or empty a host default: iOS `sim_arm64` on aarch64 else `x86_64`, visionOS `sim_arm64`, watchOS `arm64` on aarch64 else `x86_64`, tvOS `sim_arm64` on aarch64 else `x86_64`, macOS `arm64` on aarch64 else `x86_64`, Catalyst `x86_64`. Only a leading `sim_` or `device_` is stripped. Any other `apple_platform_type` throws.
  - `single_arch_platform` marks iOS, tvOS, watchOS, and visionOS as simulators only when `--cpu` starts with `ios_sim`, marks every other type a device, and for an unknown type still reports the macOS name and plist. `{platform}_{arch}` (`darwin_{arch}` for macOS) is formed from that effective CPU and the platform is picked from fixed sets (`ios_x86_64` and `ios_i386` are simulators, `watchos_arm64` is a simulator, `watchos_device_arm64` is a device, Catalyst is only `catalyst_x86_64`), and a CPU that is not in a set throws.
  - `apple_cpus` is absent, so there is no `apple_split_cpu` plus those ordered CPU lists with the same null/empty defaults. `apple_platform_type` and `ios_sdk_version_flag`, `macos_sdk_version_flag`, `tvos_sdk_version_flag`, and `watchos_sdk_version_flag` are absent. The minimum-OS fields are the raw option string, or `None` when the option is missing or empty, instead of a `DottedVersion` or null.
  - The output-directory mnemonic is `<cpu>-<mode>[-exec|-<platform_suffix>][-ST-<hash>]`. `platform-splitCpu[-min<version>]` is also appended when `apple_split_cpu` is non-empty (iOS and Catalyst use `ios_minimum_os`, visionOS uses `1.0`, otherwise that platform’s minimum OS), then the distinguisher’s filesystem name (`applebin_ios`, `applebin_visionos`, `applebin_watchos`, `applebin_tvos`, `applebin_macos`, `applebin_catalyst`, `apl`) when it is not `UNKNOWN`.
  - Unset `xcode_config_label` resolves to `@@bazel_tools//tools/cpp:host_xcodes`. The late-bound default on this method is `//tools/objc:host_xcodes` in the tools repository.

### `src/main/java/com/google/devtools/build/lib/rules/apple/ApplePlatform.java`
- Bazel lines: 228
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/builtins/common/objc/apple_platform.bzl`
- Verdict: partial
- Differences:
  - `repr` is `Ascii.toLowerCase` of the enum constant (`ios_device`, `watchos_simulator`, and the other keys). Platforms are structs, so `repr` is `struct(is_device = …, name = …, name_in_plist = …, platform_type = …)` in `BTreeMap` key order and `type()` is `struct`.
  - An unknown CPU throws `IllegalArgumentException` with `"No supported apple platform registered for target cpu " + targetCpu`. `fail` is used with `"No platform found for target CPU %s"`. A null CPU NPEs in `ImmutableSet.contains`; `None` misses every key and fails.
  - `getLowerCaseNameInPlist` returns `nameInPlist.toLowerCase(Locale.US)`. That value is not a field on the struct.
  - `get_target_platform` (catalyst becomes `ios`) and `get_target_environment` (`macabi`, `device`, or `simulator`) are also computed. This Java type does not.

### `src/main/java/com/google/devtools/build/lib/rules/apple/DottedVersion.java`
- Bazel lines: 446
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/natives.rs`
- Verdict: partial
- Differences:
  - Java matches each component with `(?i)(\d+)([a-z0-9]*?)?(\d+)?` and stores a second `int` that defaults to 0. `1.2x` equals `1.2x0` and `1.2x0.0`, and `1.2beta2` < `1.2beta10`. Zel keeps one string tail after the leading digits, so `x` < `x0` and `beta2` > `beta10`. Operator `<`/`>` uses that same split.
  - A component matching `(?i)[a-z]\w*` stops parsing and is dropped, so `10.10.simulator.internal` equals `10.10`. Zel orders that suffix, and a non-empty tail sorts before a missing component, so it is less than `10.10`.
  - `DottedVersion.equals` follows `compareTo` (hash is the parsed numbers), so `1.0` equals `1` and `0.1` equals `0.01`. Zel `==` and dict hashing use the raw host string, so those pairs stay unequal while the comparator reports equal. `DottedVersion.Option` equality is the original string; flag values keep that raw string.
  - When two alpha texts differ, Java `compare_to` returns the `String.compareTo` UTF-16 code-unit delta. Zel returns `-1`, `0`, or `1`. Leading numbers already use a `-1`/`0`/`1` integer compare.
  - `Integer.parseInt` throws when either number is outside signed 32-bit. `apple_common.dotted_version` accepts any string whose first character is an ASCII digit, including `1..3`, `2:3`, `1.-1`, and `2147483648`. The options converter rejects a non-alphanumeric component and an int32-overflowing leading run, and still accepts an overflowing trailing number such as `1a2147483648`. Comparison parses the leading run as `u64` and uses `0` when it does not fit in `u64`.
  - `dotted_version` requires a string. Zel returns `None` for `None` or a missing argument, and it clones an existing dotted-version host. An unset `DottedVersion.Option` whose schema default is `null` stays null. An empty string is an illegal version.
  - Components Java accepts are ASCII, so UTF-16 versus Unicode scalar order does not change them. Zel scalar-compares a non-ASCII tail that the Java pattern rejects.

### `src/main/java/com/google/devtools/build/lib/rules/apple/DottedVersionConverter.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/converters_ext.rs`
- Verdict: partial
- Differences:
  - A component whose leading digit run fits in a signed 32-bit int, but whose trailing digit run (digits after the last ASCII letter) does not, is rejected by Bazel: `Integer.parseInt` on that second group throws `InvalidDottedVersionException`, and `convert` rethrows `OptionsParsingException` with the illegal-version message. Zel only checks the leading run with `i32` and accepts the original string (for example `2beta2147483648`).

### `src/main/java/com/google/devtools/build/lib/rules/config/ConfigFeatureFlag.java`
- Bazel lines: 236
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No `config_feature_flag` rule that exposes a configured string value and trims unused flags.

### `src/main/java/com/google/devtools/build/lib/rules/config/ConfigFeatureFlagProvider.java`
- Bazel lines: 149
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - The Starlark constructor requires a `value` string (positional or named), sets `error` to None, and makes `is_valid_value` always true. Arbitrary kwargs are stored, positional arguments are dropped, and `value` is not required.
  - `value` is None when `error` is a non-empty string; an empty `error` still returns `value`. The stored `value` field is returned and is not hidden based on `error`.
  - `error` is always a field (None from the Starlark constructor). A missing `error` is an error instead of returning None.
  - `is_valid_value` applies the stored predicate. There is no such method.
  - Equality and hashing are by identity. The provider name and fields are compared and hashed, so two instances with the same fields compare equal.
  - Instance repr is `struct(error = None, value = ...)` (struct fields only, sorted). `FeatureFlagInfo(...)` is printed with only stored fields. The constructor prints `<function FeatureFlagInfo>` versus `<provider FeatureFlagInfo>`.

### `src/main/java/com/google/devtools/build/lib/rules/config/ConfigFeatureFlagTaggedTrimmingTransitionFactory.java`
- Bazel lines: 159
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No transition that trims config feature flags to the tagged set on a dependency.

### `src/main/java/com/google/devtools/build/lib/rules/config/ConfigFeatureFlagTransitionFactory.java`
- Bazel lines: 175
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No transition that replaces the config feature-flag map from a rule attribute.

### `src/main/java/com/google/devtools/build/lib/rules/config/ConfigGlobalLibrary.java`
- Bazel lines: 210
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader_host.rs`
- Verdict: partial
- Differences:
  - `transition` stores `implementation`, `inputs`, and `outputs` when they are present and does not cast those sequences to strings. It does not parse non-`//command_line_option:` keys as labels in the calling package, reject an invisible repository, reject a duplicate input or output, or reject native `experimental_*` and `incompatible_*` options (`incompatible_enable_cc_toolchain_resolution` and `incompatible_enable_apple_toolchain_resolution` are still allowed).
  - `implementation`, `inputs`, and `outputs` are required keyword-only arguments. They are also accepted by position and a transition struct is still returned if any are omitted.
  - `exec_transition` is registered in `/workspace/zel-cursor/crates/zel-packages/src/loader.rs` and has no host arm, so the call fails as an unknown native function. It is allowed only from `@_builtins` and, unlike `transition`, allows experimental and incompatible options.
  - `analysis_test_transition` only checks that `settings` is a dict. It does not require string keys, validate those keys as outputs (label syntax and repository visibility; experimental and incompatible options are allowed), or keep the calling module’s label, repository mapping, or `--incompatible_disable_transitions_on` list on the transition.
  - Duplicate detection walks the input sequence and reports the second occurrence in that order. That check is not done. The prefix and flag-name checks are ASCII, so UTF-16 versus Unicode scalars does not change them.

### `src/main/java/com/google/devtools/build/lib/rules/config/ConfigRuleClasses.java`
- Bazel lines: 504
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_class.rs`
- Verdict: partial
- Differences:
  - `config_feature_flag` is absent from `native_rule_classes`, so a BUILD file that calls it fails as an unknown rule class. The unused row in `/workspace/zel-cursor/crates/zel-packages/src/rule_defs_gen.rs` also drops the non-empty check on `allowed_values`, the `scope` allow-set (`universal`, `target`, `project`), the instance allowlist failure (`the config_feature_flag rule is not available in this package`), and the `ConfigFeatureFlagConfiguration` fragment requirement.
  - `:flag_alias_settings` stays an empty label list. Bazel’s late-bound default walks `values` and adds every label that `--flag_alias` maps those keys to, so those targets are prerequisites.
  - `values` and `define_values` are stored in a `BTreeMap`, so keys iterate in sorted order. Bazel’s `STRING_DICT` keeps insertion order (`LinkedHashMap` copied into an `ImmutableMap`), and `FLAG_ALIAS_REFERENCES` emits alias labels in that key order.
  - `flag_values` and `constraint_values` allow any file. Bazel’s empty `allowedFileTypes()` is `FileTypeSet.NO_FILE`, which rejects source files.
  - A `config_setting` with a non-empty `constraint_values` list turns toolchain resolution on. Native analysis of `config_setting` never selects an execution platform for that rule.
  - `setIgnoreLicenses` stores `License.NO_LICENSE` for `config_setting` when `licenses` is set on the rule or inherited from the package. Zel keeps an explicit `licenses` list.
  - When a `values` key names a test option, Bazel skips test-configuration trimming for that `config_setting`. Zel trims the rule under `--trim_test_configuration` unless it is `testonly` or otherwise treated as a test.

### `src/main/java/com/google/devtools/build/lib/rules/config/ConfigRules.java`
- Bazel lines: 60
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_class.rs`
- Verdict: partial
- Differences:
  - `config_setting` is a native rule and is analyzed. `config_feature_flag` is never added to the native rule map (its generated attribute schema in `/workspace/zel-cursor/crates/zel-packages/src/rule_defs_gen.rs` is unused), so the rule cannot be instantiated or analyzed. Bazel registers that rule and, under feature-flag trimming, keeps only that target's own label.
  - Bazel installs `ConfigFeatureFlagTaggedTrimmingTransitionFactory` on `transitive_configs` for every rule and sets the toolchain tagged-trimming transition to the empty flag set. With `--enforce_transitive_configs_for_config_feature_flag` false (the default in both), that patch leaves options unchanged. With it true, Bazel retains only the `transitive_configs` labels whose package is outside `command_line_option` and `command_line_option/…`, also keeps flags written by a `config_feature_flag` transition attribute, gives aliases `NoTransition`, and in legacy output-directory naming records flags changed by that diff as affected by a Starlark transition. An empty keep-set, including the toolchain transition, drops every feature flag. Zel stores the boolean and leaves feature-flag values in place on target and toolchain configurations. `cquery` still prints `ConfigFeatureFlagTaggedTrimmingTransition` for non-alias rules.
  - The Starlark bootstrap is only partly present in `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`. `config_common` has `FeatureFlagInfo` and `toolchain_type` (`mandatory` defaults to true) and has no `config_feature_flag_transition`. A string toolchain-type name is parsed with `Label::parse_canonical`; a parse failure drops the requirement, and a non-label, non-string name is stored and then dropped. Bazel resolves the string with the `.bzl` label converter and errors on a bad label or a wrong type. `config` exposes `int`, `bool`, `string`, `string_list`, `exec`, `target`, and `none`, and has no `string_set`, `BuildSetting`, or `ExecTransitionFactory`. `transition()` stores `implementation`, `inputs`, and `outputs` without rejecting duplicate keys, labels whose repository is not visible, or `//command_line_option:` names that start with `experimental_` or `incompatible_` (Bazel still allows `incompatible_enable_cc_toolchain_resolution` and `incompatible_enable_apple_toolchain_resolution`). `exec_transition` is bound to a host builtin with no dispatcher arm, so a `.bzl` call fails as a BUILD-only function. Bazel builds an exec transition, allows experimental and incompatible option keys, and rejects the call outside builtins.

### `src/main/java/com/google/devtools/build/lib/rules/config/ConfigSetting.java`
- Bazel lines: 853
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/config.rs`
- Verdict: partial
- Differences:
  - Native `values` are compared as raw strings against a comma-split stored value or a filled-in default. Bazel parses `--name=expected` and compares typed values: scalar equality, list `containsAll` (so `b,c` matches when both elements are present), and map last-key-wins. An illegal enum or integer is a non-match here; Bazel reports an attribute parse error. A scalar whose value contains a comma can match one split piece. Expected integers are not decoded with `Integer.decode` (no octal/hex, no signed 32-bit range check).
  - An unset string option with no table default matches `""`. An unset allow-multiple option is `None`: a non-match, and an `unknown option` error only when the name is outside `KNOWN_OPTIONS`. Bazel matches null only to a null expected value, and matches two empty lists. Trimmed test options are not forced to a non-match; a table default can match. The name `c` is treated as `compilation_mode`.
  - `values` and `define_values` are `BTreeMap`s, so settings and errors follow sorted key order. Bazel walks those dicts in insertion order (`LinkedHashMap`). A `values` `define` with no `=` compares the key to `""`. Bazel fails option parsing. Leading `--` is stripped from keys. Flag aliases are left as native names, with no conflict check against `flag_values`. Keys that look like labels (`@`, `//`, `:`) are accepted.
  - Non-configurable options and `disabledSelectOptions` are not rejected. The main-repo deprecation warning for `cpu`, `host_cpu`, and `crosstool_top` is emitted from `/workspace/zel-cursor/crates/zel-rules/src/resolver.rs`.
  - `flag_values` does not collapse aliases, reject duplicate targets, or canonicalize relative labels on label-typed settings. Feature-flag allowed values and deferred provider errors are not applied. Bool expectations match `true`/`True`/`1`/`yes` and `false`/`False`/`0`/`no`. Int expectations match decimal `i64` text (`parse::<i64>`). A list or set matches when any element equals the expected string, or when the elements joined by commas equal it, including multi-value strings that Bazel rejects unless the converted expectation has size 1 and is contained.
  - Duplicate `constraint_values` for one setting are not rejected. With `--platforms` empty, the host platform is used, and a missing setting matches `default_constraint_value`. Bazel fails with `No target platform is present` when the toolchain context is absent, and reports `<unset>` when the collection has no value. The refinement chain is not part of the recorded settings, so `select()` specialization does not see refining constraints. Attribute failures still return a configured target with `matches=false` and an `errors` list. Bazel returns null from `create`.
  - `addRuleImplSpecificRequiredConfigFragments` is not implemented on the rule. `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` records every `values` key as `CoreOptions` (`platforms` as `PlatformOptions`) and does not split a `define` value on `=`.

### `src/main/java/com/google/devtools/build/lib/rules/config/ConfigStarlarkCommon.java`
- Bazel lines: 65
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - `config_feature_flag_transition` is not on `config_common`. One positional string is taken and a rule transition is returned that adds the feature-flag allowlists for that attribute and, when the attribute is explicitly set, replaces the config feature-flag map with that label-keyed string dict (otherwise `NoTransition`, and in legacy output-directory naming updates the flags affected by the transition).
  - `FeatureFlagInfo` is a generic provider. The constructor requires a string `value` (positional or `value=`), sets `error` to None, and `is_valid_value` always returns true; two instances compare by identity. Arbitrary keywords are kept, positional arguments are dropped, there is no `error` or `is_valid_value`, and comparison is by provider name and fields.
  - `toolchain_type` takes a positional-only Label or string `name` and a keyword-only bool `mandatory` (default true), and rejects other types, a missing `name`, extra positionals, and unknown keywords. `name` is also accepted as a keyword, a positional second argument is ignored, a non-bool `mandatory` is stored (later treated as mandatory unless it is exactly false), and a struct with no name is returned when `name` is omitted.
  - A string `name` is parsed immediately against the calling `.bzl` package and repository mapping, failing with `Unable to parse toolchain_type label '%s': %s`. The raw string is stored and converted only in the `.bzl` that calls `rule()`, keeping unparseable strings; those requirements are then dropped. A relative or apparent-repo label built in one `.bzl` and passed to `rule()` in another is resolved against a different package and repository mapping.
  - The value exposes `toolchain_type` (a Label) and `mandatory` (a bool). A struct is returned with `name` (the original value, or a Label only after that later conversion), `mandatory`, and `_toolchain_type=True`, in `BTreeMap` key order.

### `src/main/java/com/google/devtools/build/lib/rules/config/FeatureFlagValue.java`
- Bazel lines: 132
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No value type that distinguishes an unset, set, or invalid config feature flag.

### `src/main/java/com/google/devtools/build/lib/rules/core/CoreRules.java`
- Bazel lines: 43
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No registry that installs the core native rules (`filegroup`, `genrule`, `test_suite`, `alias`, and the other core classes).

### `src/main/java/com/google/devtools/build/lib/rules/core/ValidateTarget.java`
- Bazel lines: 66
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No `validate_target` rule that fails analysis when its `target` dependency is invalid.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/ArtifactCategory.java`
- Bazel lines: 86
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/cc_features.rs`
- Verdict: partial
- Differences:
  - `getAllowedExtensions()` is an ordered list of the default extension followed by the extra extensions. `default_artifact_pattern` keeps only the default prefix and the first extension, so these extras are absent: `STATIC_LIBRARY` `.lib`; `ALWAYSLINK_STATIC_LIBRARY` `.lo.lib`; `DYNAMIC_LIBRARY` `.dylib`, `.dll`, `.pyd`, `.wasm`; `EXECUTABLE` `.exe`, `.wasm`; `INTERFACE_LIBRARY` `.tbd`, `.if.lib`, `.lib`; `OBJECT_FILE` `.obj`; `CPP_MODULE` `.gcm`, `.ifc`; `GENERATED_ASSEMBLY` `.asm`.
  - `getCategoryName()` is `name.toLowerCase()` under the default locale (UTF-16). In a Turkish or Azeri locale, ASCII `I` becomes U+0131, so `STATIC_LIBRARY` becomes `statıc_lıbrary`. That string is not produced: lookup uses the uppercase constant, and configured category names are folded with ASCII-only `to_ascii_uppercase`.
  - An unknown category is `None` from `default_artifact_pattern`. Java `Enum.valueOf` throws. Empty prefix and extension stay `""`, including `EXECUTABLE`.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/BitcodeFiles.java`
- Bazel lines: 95
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No container that pairs LTO bitcode files with their per-object backend artifacts.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CcCommon.java`
- Bazel lines: 336
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_cc/0.2.17/cc/private/toolchain_config/configure_features.bzl`
- Verdict: partial
- Differences:
  - LLVM FDO (`llvm_fdo` or `llvm_cs_fdo`, compilation mode `opt`, `fdo_optimize` not unsupported) also requests `enable_fdo_memprof_optimize` unless `memprof_optimize` is unsupported. The Java method never adds that feature.
  - After configuration, an enabled feature that is unsupported fails for every name in the unsupported set, including `parse_headers` and `module_maps` added inside the function. Java checks only the caller-supplied `unsupportedFeatures`, so a toolchain that unconditionally implies those two names fails only on the Starlark path.
  - Language is a string, default `c++`, with `+` rewritten to `p`. If `requested_features` contains `lang_objc`, the language is forced to `objc` (ObjC action configs, `lang_objc`, and the linkmap and dead-strip features). Java takes a `Language` enum and does not infer ObjC from that feature.
  - `CoptsFilter.passesFilter` drops a flag only when `java.util.regex.Matcher.matches` matches the entire flag. A null pattern (`alwaysPasses`, empty `nocopts`) keeps every flag. An invalid pattern is an error. Zel compile actions do not read `copts_filter`, and `cpp.disable_nocopts()` is hardcoded true, so a non-empty `nocopts` fails in `cc_helper.bzl` instead of compiling a Java regex.
  - `fdo_instrument()` and `cs_fdo_instrument()` on the cpp fragment always return None, so those features are never requested. Java adds `fdo_instrument` when that string is non-null, otherwise `cs_fdo_instrument` when that string is non-null, unless `fdo_instrument` is unsupported.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CcCompilationContext.java`
- Bazel lines: 925
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/cc_internal.rs`
- Verdict: partial
- Differences:
  - `HeaderInfo.create` moves `.inc` public and private headers into textual headers and dedupes public, private, and textual lists in first-seen order. Zel keeps the input sequences as tuples, including duplicates, and leaves `.inc` files in the modular lists.
  - `create_header_info_with_deps` rebuilds one `HeaderInfo` and folds `merged_deps` modular public, private, and textual headers into those lists. Zel only stores `deps` and `merged_deps` and leaves the header tuples unchanged, so exported headers never appear on `modular_*` or `textual_headers`.
  - A separate module with no main module (PIC and non-PIC checked independently) throws in Bazel. Zel accepts `None` for the main module and still records the separate module.
  - Bazel `HeaderInfo` equality and transitive dedup use a fresh identity token, so two builds with the same headers are not equal. Zel struct equality is field-wise on a `BTreeMap`, with no identity, and struct field order is sorted by name.
  - Transitive header order is self, then a depth-first walk of deps that are not already in the largest dep, then that largest dep’s memoized order. Zel does not flatten deps.
  - Include-scan legal paths are a `CompactHashMap` from exec path to artifact (later path overwrites; tree artifacts expand from Skyframe and a missing value returns null). Modular headers omit textual headers and this target’s own public, private, textual, and separate headers. Zel has no such map.
  - `computeUsedModules` and `getDirectModules` choose the PIC or non-PIC main and separate modules, drop this target’s own module, and emit direct modules in `CompactHashSet` insertion order wrapped as a stable nested set. `getHeaderModuleSrcs` returns either the separate-module list or a deduped public-then-private-then-textual-then-separate union. Zel does not implement these.
  - Omitted module arguments default to null/`None` and omitted header sequences to an empty list in both. Bazel rejects unknown `create_header_info` keywords and non-artifacts. Zel ignores unknown keywords and stores any sequence elements.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CcLibcTopAlias.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - Bazel aliases `CppConfiguration.getLibcTopLabel()` (`--grte_top` after `LibcTopLabelConverter`). `null`, including `--grte_top=default`, yields an empty target. Zel reads `BuildConfigurationKey.options["grte_top"]` and treats only a missing or empty string as absent. That map is filled from `CONFIG_OPTIONS`, which omits `grte_top`, so a normal command-line `--grte_top` stays unset and the target is empty. A present `"default"` string is parsed as a label and fails.
  - A set label is rewritten to the target named `everything` in that package (`//pkg:name` and `@repo//pkg:name` become `//pkg:everything` / `@repo//pkg:everything`) before the late-bound `:alias` is resolved. Zel resolves the stored string as written.
  - A non-null actual is an `AliasConfiguredTarget`: `getLabel()` is the actual target's label, visibility is the alias rule's, and `LateBoundAliasProvider` is advertised on both the alias and the empty target. Zel clones the configured target, replaces `label` with the alias rule's label, and keeps the actual's actions. The empty path does not advertise the late-bound alias provider.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CcModule.java`
- Bazel lines: 216
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/natives.rs`
- Verdict: partial
- Differences:
  - Bazel `isCalledFromStarlarkCcCommon` runs on every method except `add_go_exec_groups_to_binary_rules` and fails unless the innermost `.bzl` canonical label ends with `_builtins//:common/cc/cc_common.bzl` or `_builtins//:common/cc/cc_common_bazel.bzl`, or contains `_builtins//:common/cc/compile`, `_builtins//:common/cc/link`, or `_builtins//:common/cc/toolchain_config` (`cc_common_internal can only be used by cc_common.bzl in builtins, please use cc_common instead.`). Zel invokes these natives with no caller check. `checkPrivateStarlarkificationAllowlist` fails outside the default private-API allowlist (`file '%s' cannot use private API`); Zel has no such gate.
  - `check_experimental_cc_shared_library` reads `--experimental_cc_shared_library` (default false). Zel always returns true. `incompatible_disable_objc_library_transition` reads `--incompatible_disable_objc_library_transition` (default true). Zel always returns false. `add_go_exec_groups_to_binary_rules` reads `--add_go_exec_groups_to_binary_rules` (default false). Zel always returns false.
  - `legacy_cc_flags_make_variable_do_not_use` returns the toolchain provider field `_legacy_cc_flags_make_variable`. Zel fails with `cc_common.legacy_cc_flags_make_variable_do_not_use is not implemented yet`.
  - `get_execution_requirements`, `get_tool_requirement_for_action`, and `get_memory_inefficient_command_line` return immutable lists. Zel returns tuples. `get_environment_variables` returns an immutable insertion-ordered dict. Zel returns a mutable insertion-ordered dict. These parameters are keyword-only in Bazel. Zel also binds them by position.
  - A duplicate environment key makes Bazel `ImmutableMap.Builder.buildOrThrow` fail the call. Zel overwrites the earlier value and still returns a dict.
  - Command-line and environment expansion use `PathMapper.NOOP`. Zel resolves `File` values with `artifact_path` (exec path); a missing resolver becomes a struct variable. Zel command-line failures append `(while expanding action '...'; variables: [...])`, and those keys come from a `BTreeMap` (sorted). Bazel reports the expansion error text alone.
  - `nullIfNone` maps only `Starlark.NONE` to null and class-casts every other value, including unbound. Zel has no equivalent helper.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CcStarlarkInternal.java`
- Bazel lines: 1298
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/cc_internal.rs`
- Verdict: partial
- Differences:
  - `check_private_api` always succeeds. Bazel walks `depth` (default 1; `StarlarkInt.toIntUnchecked`, so it must fit in a signed 32-bit int) to the enclosing module, returns immediately when that module is null, and fails when the module is outside the `(package, location)` allowlist.
  - `cc_toolchain_variables` turns a non-dict into an empty map. Bazel rejects it. Both drop `None` bindings and store keys in sorted order.
  - `combine_cc_toolchain_variables` flattens each argument, including its parent chain, into one `BTreeMap` and lets later keys overwrite earlier ones. Bazel copies only each value’s own keys, errors if those keys intersect, and leaves nested parents out of the chain so a nested parent cannot hide the combine parent. A list is unwrapped; Bazel requires each extra positional to already be `CcToolchainVariables`. `None` is accepted as “no parent”; Bazel’s parameter type is not optional.
  - `intern_string_sequence_variable_value` returns the same object. Bazel returns an interned immutable list, so a tuple becomes a list.
  - `intern_seq` returns the same sequence. Bazel returns a tuple of interned elements, so a list becomes a tuple.
  - `freeze` copies and freezes only a dict or a list. Bazel turns any other iterable (including a tuple) into an immutable list and leaves non-iterables as-is. Dict entry order is preserved on both sides.
  - `exec_os` ignores `ctx` and returns `osx`, `windows`, or `linux` from the process target. Bazel returns `OS.name()` for the execution platform’s OS constraint (`DARWIN`, `LINUX`, `WINDOWS`, `FREEBSD`, `OPENBSD`, or `UNKNOWN`), and uses the host OS only when that constraint is absent.
  - `is_tree_artifact` is the struct field `is_directory` being truthy, otherwise false. Bazel uses `Artifact.isTreeArtifact()`.
  - `get_artifact_name_for_category` splits on the last `/` and concatenates prefix and extension. Bazel normalizes the path (`PathFragment`: `.`, `..`, repeated separators, and OS-specific absolute forms) before applying the category pattern. Unknown categories become a Starlark error; `ArtifactCategory.valueOf` is an unchecked invalid-enum failure. Default prefix and first extension match the enum.
  - `cc_toolchain_features` treats a string tool path as crosstool-relative unless it starts with `/`. Bazel keeps `path_origin` and rejects a non-absolute `FILESYSTEM_ROOT` path or an absolute `WORKSPACE_ROOT` path via `PathFragment.isAbsolute`.
  - `create_header_info` stores the sequences as tuples, including duplicates, and leaves `.inc` files in the public or private list. Bazel returns immutable lists, drops duplicates, moves `.inc` files into textual headers, and errors when a separate module is set without the corresponding main module. Equality is structural, including a synthetic `_zel_native` field. Bazel equality is the fresh identity token, so two calls with the same contents are not equal.
  - `create_header_info_with_deps` only replaces the `deps` and `merged_deps` fields, both of which are visible. Bazel has no Starlark `deps` or `merged_deps` fields; `merged_deps` are unioned into the modular and textual header lists, and `None` for `header_info` is a null dereference rather than a typed error.
  - These methods return “not implemented” instead of running: `solib_symlink_action`, `dynamic_library_symlink`, `dynamic_library_symlink2`, `dynamic_library_soname`, `aspect_class` (`None` when there is no aspect; if the name contains `%`, the second `split("%", -1)` segment), package hdrs-check reads for rules and aspects, `stl_computed_default` (null when tags contain `__CC_STL__`, otherwise `@//third_party/stl`), `absolute_symlink`, `get_link_args` (a string `parameter_file_type` selects `ParameterFileType` and splits the command line; `None` does not), `wrap_link_actions` (`build_configuration` default `None` uses the rule configuration; `sharable_artifacts` default false), `actions2ctx_cheat`, `rule_class`, `collect_per_file_lto_backend_opts`, `compute_output_name_prefix_dir` (`arc` / `non_arc` / `""` from the `_objc_arc` suffix), `create_cc_compile_action` and `create_cc_compile_action_template` (empty or `None` copts filter always passes; otherwise `java.util.regex.Pattern`, with an invalid pattern failing as `invalid regular expression`), `check_toplevel`, `per_file_copts`, `declare_compile_output_file`, `declare_other_output_file`, both LTO backend action constructors, and `expand_and_tokenize` (empty flags returned unchanged; otherwise location expansion over `srcs`, `non_arc_srcs`, `hdrs`, `data`, and `additional_linker_inputs`, then shell tokenization).
  - Zel also exposes `maybe_hash_preserve_extension`, which this type does not define.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CcToolchainAliasRule.java`
- Bazel lines: 42
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_class.rs`
- Verdict: partial
- Differences:
  - With builtins injection, Bazel replaces both the BUILD symbol and `native.cc_toolchain_alias` with the removed-rule `fail()`. Zel replaces only `native.cc_toolchain_alias`, so a BUILD `cc_toolchain_alias()` still instantiates the native class and fails at analysis with no configured target.
  - When that native factory runs, Bazel rule-errors `Rule 'cc_toolchain_alias' is unimplemented.` (`$bzl_load_label` is null, so the `load()` hint is skipped) and returns null. Zel has no analysis arm and returns an error string instead.
  - `:action_listener` is a late-bound label list from the configuration (null when unset). Zel stores default `None` and does not add those labels.
  - The `licenses` default `License.NONE` is dropped (`D::License` becomes no default), and a package `licenses()` value is not copied onto the rule.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CcToolchainConfigInfo.java`
- Bazel lines: 127
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/cc_features.rs`
- Verdict: partial
- Differences:
  - `get` returns null when the target has neither the builtins provider (`@_builtins//:common/cc/toolchain_config/cc_toolchain_config_info.bzl`) nor the rules_cc provider (`@rules_cc+//cc/private/toolchain_config:cc_toolchain_config_info.bzl`), and the builtins instance wins when both are present. Zel addresses providers as `<bzl>%CcToolchainConfigInfo` and `cc_toolchain_features` only reads the value it is given. A missing info or a missing `_features_DO_NOT_USE`, `_action_configs_DO_NOT_USE`, or `_artifact_name_patterns_DO_NOT_USE` becomes an empty list (`items` treats `None` and non-sequences as empty) where `Sequence.cast` throws.
  - `getActionConfigs` passes `_exec_os_DO_NOT_USE` through `OS.valueOf`, which accepts only `DARWIN`, `FREEBSD`, `OPENBSD`, `LINUX`, `WINDOWS`, and `UNKNOWN`, then parses tool paths with that OS (`PathFragment.createForOs`, so Windows drive letters and backslashes are absolute). Zel leaves `_exec_os_DO_NOT_USE` unread and treats a path as crosstool-relative unless it starts with `/`.
  - Action names must match `^[_a-z0-9+\-\.]*$` and must be nonempty. Zel keeps any string, and a missing `action_name` becomes `""`. A missing `enabled` boolean fails in Bazel. Zel treats a missing value as false and any other truthy value as enabled.
  - Artifact `category_name` must equal the lowercase category (`static_library`). Unknown categories and extensions outside the allow-list fail, and a duplicate category fails in `ImmutableMap.Builder.buildOrThrow`. An override equal to the category default prefix and extension is omitted. Zel stores `category_name.to_ascii_uppercase()` in a `HashMap` (a later duplicate overwrites), skips a pattern with no `category_name`, and does not check the extension allow-list while building, so a lowercase category misses its override and falls through to the default pattern.
  - Feature and action-config lists keep Starlark sequence order (`ImmutableList` and `Vec`).

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CcToolchainFeatures.java`
- Bazel lines: 1352
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/cc_features.rs`
- Verdict: partial
- Differences:
  - `getToolPathForAction` and `getToolRequirementsForAction` resolve any action config registered for that action name, including one feature selection left disabled. Zel errors unless that action config is enabled.
  - `getEnvironmentVariables` fails when two expanded entries share a key. Zel keeps the later value in the first key's slot.
  - Crosstool-relative tool paths use `PathFragment.getRelative` and `getSafePathString` (`.` / `..` / repeated separators normalized; an empty fragment is `"."`). Zel joins `tools_directory` and the path with `/`, treats only a leading `/` as absolute (a Windows drive path is joined onto the toolchain directory), and leaves `.` and `..` in the string.
  - `getArtifactNameForCategory` normalizes `output_name` as a `PathFragment` and replaces the basename in the parent. An empty path or filesystem root has a null parent and throws. Zel splits on the last `/` only, so `.`, `..`, `//`, and a trailing slash stay in the result.
  - `getPerFeatureExpansions` returns the action-config flags and each enabled feature's flags as separate buckets, which is how `unfiltered_compile_flags` skips the copts filter. Zel only builds one concatenated command line.
  - If several `provides` symbols each have multiple enabled providers, Bazel reports the symbol in registration order. Zel reports the lexicographically smallest symbol (`BTreeMap`). Provider names within one symbol follow feature definition order in both.
  - Duplicate action names are reported before duplicate feature or action-config names. Zel reports a repeated feature name while scanning features, and for one action config it reports a repeated config name before a repeated action name.
  - `configure_features` rejects a non-string `requested_features` element and rejects callers outside the private Starlark allowlist. Zel drops non-strings and does not check the allowlist.
  - Flag and env expansion pass every string variable, artifact path, and path fragment through `PathMapper`. Zel copies string values as stored and does not apply that mapping.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CcToolchainFeaturesLib.java`
- Bazel lines: 559
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/cc_features.rs`
- Verdict: partial
- Differences:
  - Java rejects a feature or action_config name unless `String.matches` (`java.util.regex`) fully matches `^[_a-z0-9+\-.]*$`. An empty feature name is rejected unless `enabled` is true. Rust keeps any string name, including empty, with no regex. A missing or `None` `enabled` is an error (unboxing `null`) in Java and defaults to false via `is_truthy` in Rust.
  - Missing mandatory strings (`iterate_over`, `expand_if_available`, `expand_if_not_available`, `expand_if_true`, `expand_if_false`, `expand_if_equal`, env `key`/`value`, `action_name`) error in Java; `None` becomes null. Rust treats missing and `None` as `Option::None` or `""`. A flag group with both `flags` and `flag_groups`, or with neither, errors in Java. Rust expands both, or expands nothing. An action_config flag set with a nonempty `actions` list errors in Java. Rust ignores that list and always applies the set to the action. A feature flag set or env set with empty `actions` errors in Java. Rust keeps it and it matches no action.
  - Flag and env strings go through `StringValueParser` (`charAt` / `indexOf`, UTF-16 code units). `%{path:rel}` is a relative exec-path chunk; an empty or absolute path after `path:` fails, and `%{}` fails. Rust `parse_chunks` walks Unicode scalars and stores `%{path:…}` and `%{}` as ordinary variables. `%%` and `%{var}` expand to the same text when the delimiters are BMP characters.
  - Tool `path` uses `PathFragment.createForOs(execOs)`: absolute when the drive length is greater than 0 (Windows `C:\…`), with `.` / `..` normalization. An empty path, or both `tool` and `path`, errors. Rust treats a path as crosstool-relative unless it starts with `/`, does not normalize, prefers `tool` when both are set, and otherwise keeps an empty relative path.
  - Java `ImmutableSet` drops duplicate actions, with-feature names, and execution requirements, keeping first-seen order. Rust `Vec` keeps duplicates. A repeated artifact category throws from `ImmutableMap.Builder.buildOrThrow`. Rust `HashMap` keeps the last prefix and extension. The category must equal the lowercased enum name and the extension must be in that category’s allow-list (`.lib`, `.dll`, `.exe`, `.obj`, and the other extras). Rust stores `to_ascii_uppercase(category_name)`, accepts any extension, and accepts unknown categories. A pattern equal to the built-in default is omitted in Java; lookup still yields that same default prefix and extension.
  - Java sets both the selectable name and the action name from `action_name`. Rust uses a string `config_name` when that field is present. Java checks `type_name` and field classes (`None` becomes null). Rust does not check `type_name`, drops non-strings in lists, and turns a non-string `expand_if_equal` into no constraint. `make_variable` and `tool_path` (nonempty name and value) have no Rust parser.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CcToolchainProvider.java`
- Bazel lines: 506
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No `CcToolchainInfo` provider that exposes the C++ toolchain files, features, and builtin include paths.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CcToolchainVariables.java`
- Bazel lines: 1064
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/cc_features.rs`
- Verdict: partial
- Differences:
  - Flag parsing walks Java UTF-16 code units and reports `current` as that index (`expected '{'` / `expected variable name` at a position). Zel walks Unicode scalars and reports no index (`expected '%%' or '%{'` / `unterminated variable reference`). An unclosed `%{` throws `StringIndexOutOfBoundsException` in Bazel; Zel returns a Starlark error. `%{}` is rejected in Bazel and becomes an empty variable name in Zel.
  - `%{path:…}` is a relative exec-path chunk in Bazel (Linux separators, empty and absolute paths rejected, expanded with `PathMapper.map`). Zel treats the whole text as a variable name.
  - String expansion applies `PathMapper.mapHeuristically`; artifacts use `getMappedExecPathString`; path fragments use `getSafePathString` (empty path becomes `"."`) and are always truthy. Zel returns the stored string or a single `path_of` result, with no mapper and no `"."` rewrite, so an empty path is an empty string and falsy.
  - Booleans expand to `"1"` / `"0"`. Zel rejects them in string expansion (`expected string, found boolean`).
  - `None` is an absent binding in both. A missing field or a field access on a non-structure is a structure error in Bazel (`doesn't have a field named` / `expected structure`, type name from `Starlark.type` / `Structure.truth()`). Zel reports `Cannot find variable named '…'` and treats every non-string, non-bool, non-sequence value as a structure that is always truthy.
  - Dicts and other iterables become sequences (dict iteration yields keys). Ints are not variable values, so expansion throws `ClassCastException`. Zel turns int, float, dict, and range into structures, so they are not sequences and string expansion fails with `found structure`. A structure field that is a list of tree artifacts is replaced by the children's mapped exec paths (a tree with no metadata is dropped). Zel keeps the list unchanged.
  - `combine_cc_toolchain_variables` copies only each value's own keys and throws `Cannot overwrite existing variables` on overlap. Zel `flattened()` also copies ancestor bindings, so those keys override the new parent, and a later binding silently replaces an earlier one.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CompileCommandLine.java`
- Bazel lines: 228
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/cc_native.rs`
- Verdict: partial
- Differences:
  - With `compiler_param_file` enabled, Bazel’s argv is the tool path plus one argument `@` concatenated with `parameterFilePath.getSafePathString()`. Zel’s compile spawn always inlines the expanded flags (`param_file: None`).
  - Bazel copies every flag from the feature `unfiltered_compile_flags` and copies any other flag only when `CoptsFilter.passesFilter` is true (`java.util.regex` `Matcher.matches` on the whole string). Zel appends every flag from the action config and the enabled features.
  - When `overwrittenVariables` is non-null, Bazel expands a child variable set built with `addAllNonTransitive` (that set’s own keys only; a key already inserted on the builder fails). Zel expands the analysis-time `compile_build_variables` alone, so a later `module_files` overlay is absent.
  - A non-noop `PathMapper` rewrites the tool path (`PathFragment.create`, then `getSafePathString`) and the expanded options. Zel uses the toolchain tool path and analysis-time artifact exec paths.
  - `getEnvironment` returns the feature-configuration map in feature insertion order, and a duplicate key fails `ImmutableMap.Builder`. Zel stores those pairs in `Action.env` as a `BTreeMap` (sorted by key; a later value replaces an earlier one).
  - `getCopts` returns the path-mapped `user_compile_flags` string list when that variable is available, and an empty list otherwise (a failed expansion throws `IllegalStateException`). Zel has no separate copts list.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppActionNames.java`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No module defines this C++ action-name constant catalog.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppCompileAction.java`
- Bazel lines: 2125
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/cc_native.rs`
- Verdict: partial
- Differences:
  - Effective environment is a `LinkedHashMap` (configuration env, then `PWD=/proc/self/cwd` unless execution info contains `requires-darwin`, then feature env overwrites in place). Zel stores the feature env in a `BTreeMap` (sorted keys) over the configuration env and does not inject `PWD`.
  - Execution info is an insertion-ordered map merged over the base action map. Zel keeps only toolchain tool requirements as a `BTreeMap` of keys to empty strings, so it never sets `remote-execution-inline-outputs` for an in-memory `.d` or `differentiate-workspace-cache` for `/showIncludes`.
  - `.d` bytes are decoded as Latin-1 (`ISO_8859_1`) in `DependencySet.process`. The exec path in `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`parse_depfile` on `String::from_utf8_lossy`) replaces invalid UTF-8 with U+FFFD. Modmap lines are also read as Latin-1; that path is absent.
  - `.d` parsing requires a trailing newline, treats only space (not tab) as a separator, unescapes `$$` to `$`, and treats `:` as the target separator only when the next byte is space, CR, or LF. Zel accepts a missing newline, splits on tab, leaves `$$` intact, and cuts at the first colon except a drive letter at index 1. Java keeps duplicate deps in file order; Zel dedups, then sorts discovered paths.
  - When `compiler_param_file` is enabled, the param path defaults to `<output dir>/<output filename>.params` (GCC-quoted) and argv is rewritten to pass it. Zel always leaves `param_file` unset. Starlark `action.args` uses a GCC-quoted param file only when `useArgsParamsFile` is true (default false); that switch is absent.
  - Declared output order is primary, `.gcno`, additional outputs, `.d`, diagnostics, `.dwo`, LTO index. Zel orders `output_file`, `.d`, diagnostics, `.gcno`, `.dwo`, LTO index, then additional outputs. A null `.gcno` means every output is mandatory; a non-null `.gcno` is optional and an empty file is created if the compiler omits it. Zel fails the action if any declared output is missing.
  - Include-scan results are sorted by exec path before a stable nested set. With scanning off, declared includes and prunable headers are still returned as discovered inputs (not mandatory) and later replaced by `.d` or `/showIncludes` deps. Zel puts those headers on the spawn inputs up front and only records `.d` paths for the action cache.
  - Mnemonic defaults to `CppCompile`, but ObjC compiles use `ObjcCompile`, linkstamp uses `CppLinkstampCompile`, header parsing uses `CppCompileHeader` or `ObjcCompileHeader` when `useCppCompileHeaderMnemonic` is set, header analysis uses `CppHeaderAnalysis`, and module dep scanning uses `CppDepsScanning`. Zel always registers `CppCompile`. The progress text defaults to `Compiling <source>`; header analysis and dep scanning use `Header analysis for ` and `Deps scanning for `. Zel defaults the prefix to `Compiling` plus the exec path.
  - Local resource estimate defaults to 250 MB and 1 CPU. With the experimental estimate enabled, `CppCompile` on Linux or Darwin is `80 + 0.7 * inputCount` MB and `ObjcCompile` on Darwin is `80 + 0.2 * inputCount` MB (`inputCount` is a Java `int`). Zel always uses the 250 MB / 1 CPU default.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppCompileActionBuilder.java`
- Bazel lines: 664
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/cc_native.rs`
- Verdict: partial
- Differences:
  - Unset action name: Bazel matches the full exec path in order (`.cppmap` → `c++-module-compile`, headers → `c++-header-parsing` only if `parse_headers` is enabled, otherwise throw, `.c` → `c-compile`, `.cc`/`.cpp`/`.cxx`/`.c++`/`.C`/`.cu`/`.cl` → `c++-compile`, `.m` → `objc-compile`, `.mm` → `objc++-compile`, `.s` except `.pic.s` and `.asm` → `assemble`, `.S` → `preprocess-assemble`, `.ipb` → `clif-match`, `.pcm`/`.gcm`/`.ifc` → `c++-module-codegen`, anything else throws). Zel uses only the substring after the last `.`: `.c`/`.s`/`.S` match those three names, `.h`/`.hh`/`.hpp`/`.hxx`/`.inc`/`.tcc`/`.H` always become `c++-header-parsing` (no `parse_headers` gate; `.ipp`/`.h++`/`.inl`/`.tlh`/`.tli` become `c++-compile`), and every other extension including `.m`, `.mm`, `.asm`, `.pic.s`, `.ipb`, `.cppmap`, and module files becomes `c++-compile`.
  - Include scanning stays a nullable boolean: unset means scan only when the config flag, the `cc_include_scanning` feature, a non-`c++-module-deps-scanning` action, and a source that is neither assembler nor a C++ module all hold, then that boolean is cached. Zel never computes it.
  - Mandatory toolchain files follow that flag: scanning uses `compiler_files_without_includes` (falling back to `compiler_files` if empty); otherwise `use_specific_tool_files` and a non-tree source use `as_files` for `assemble` and `compiler_files` otherwise; else `all_files`. Zel always appends `_compiler_files`, `compiler_files`, and `_builtin_include_files`.
  - With scanning, `compiler_files` are prunable rather than mandatory, and declared includes plus additional prunable headers are mandatory only when scanning is off, there is no `.d` file, and `parse_showincludes` is off. Transitive modules and the separate header module (unless it is the output) are added only for header-module use or load when not scanning. `grep_includes` is added when non-null, and a modmap requires its modmap input. Zel always flattens prunable headers, context headers, module maps, and module files into one input list, with no separate-module-versus-output exclusion.
  - `cache_key_inputs` are merged into the invalidation inputs in stable nested-set order and kept out of the spawn inputs. Zel has no separate cache-key set. Builtin includes are concatenated after the toolchain builtins and before build-info headers. Zel appends build-info headers with the other inputs.
  - Execution requirements are a `LinkedHashMap`: caller entries first, then each tool requirement mapped to `""`, then `modifyExecutionInfo` for the mnemonic (`ObjcCompile`, `CppLinkstampCompile`, `CppCompile`/`CppCompileHeader` or `ObjcCompile`/`ObjcCompileHeader` for header parsing, `CppHeaderAnalysis`, `CppDepsScanning`, otherwise `CppCompile`). Zel stores only tool requirements in a `BTreeMap` (key-sorted), skips caller info and the modifier, and always uses the mnemonic `CppCompile`. Feature environment pairs are also collected into that `BTreeMap`, and `use_default_shell_env` is forced true instead of the configuration action environment.
  - A missing action config throws `Expected action_config for '<name>' to be configured` before inputs are built. Zel still expands feature flag sets, then fails later in tool lookup, and treats a missing tool's requirements as an empty list. A null feature configuration or source is an NPE/`IllegalStateException`; Zel returns a Starlark error. A second `setActionName` or a second non-tree source is rejected; a tree source may replace a non-tree source. Zel takes one source and one optional action name.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppCompileActionTemplate.java`
- Bazel lines: 408
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No template that expands a tree artifact of C++ sources into one compile action per file.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppConfiguration.java`
- Bazel lines: 968
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - `--fdo_optimize`, `--cs_fdo_absolute_path`, and both propeller absolute profiles are not parsed. Bazel treats a `//` `--fdo_optimize` value as a label and any other value as a path, and it throws unless `--enable_remaining_fdo_absolute_paths` is on (the default) and the path is absolute with a basename that is not empty, `.`, `..`, or slash-containing. Zel leaves `fdo_optimize`, `fdo_path`, `cs_fdo_path`, both propeller paths, `fdo_instrument`, and `cs_fdo_instrument` as `None`.
  - `propeller_optimize` is null when `--fdo_instrument` or `--cs_fdo_instrument` is set, and `xbinary_fdo` is null when `--fdo_optimize`, `--fdo_instrument`, `--fdo_profile`, or coverage collection is set. Zel returns those labels whenever they are set. The FDO zipper follows the resolved values (including an absolute `fdo_path`) and is `@bazel_tools//tools/zip:unzip_fdo`; Zel keys off the raw option strings and uses `@@bazel_tools//tools/zip:unzip_fdo`, so a suppressed `xbinary_fdo` still selects a zipper.
  - `--experimental_omitfp` appends `-Wl,--eh-frame-hdr` to linkopts. The copts prefix matches: `-fomit-frame-pointer`, `-fasynchronous-unwind-tables`, `-DNO_FRAME_POINTER`, then the user `--copt` values.
  - `objc_should_strip_binary` is objc stripping and compilation mode `opt`. Zel uses objc stripping and `should_strip_binaries` (`strip` equals `always`, or `sometimes` and fastbuild). With the default `strip=sometimes`, Zel is true in fastbuild and false in opt.
  - `strip` and fission tokens are case-sensitive raw strings. Bazel’s enum converter is ASCII case-insensitive, and fission is exactly `yes` (all modes), exactly `no` (none), or a comma-separated mode set; any other token is a parse error. Zel treats any comma-piece `yes` as on and any piece `no` as off, so `yes,opt` is active and `Sometimes` does not strip.
  - Repeated list flags are comma-joined and split again, and empty pieces are dropped. One `--copt`, `--linkopt`, `--stripopt`, or LTO value that itself contains a comma becomes multiple flags. Bazel keeps a single list element, in order.
  - `--grte_top` (`libc_top`): `default` is null, a value that does not start with `//` is an error, and the target name is rewritten to `everything`. Zel parses it as a label or yields `None`, and keeps the original target name.
  - `include_scanning` is always false, so both include-scanning flags are ignored. `incompatible_use_specific_tool_files`, `disable_nocopts`, and `experimental_cc_implementation_deps` are always true.
  - Bazel reports an error when `--fdo_optimize` and `--fdo_profile` are both set, when instrumentation is combined with FDO optimize/profile, and when `--fdo_instrument` is set without `--copt=-Wno-error`. Strip plus `-g` in `--copt` or `--per_file_copt` is a warning. Zel does not report these. Per-file copt and per-file LTO backend options, which are Java regex filters, are not applied.
  - A non-empty `--cc_output_directory_tag` is not added to the output-path mnemonic.
  - An empty `minimum_os_version` is `None` rather than `""`. `proto_profile_path` is a string or `None`, not a label. Fragment fields are a `BTreeMap` (sorted keys), and the Starlark methods do not enforce the private or expanded API allowlists, which throw in Bazel.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppFileTypes.java`
- Bazel lines: 307
- Zel counterpart: `/workspace/zel-cursor/third_party/bcr/rules_cc/0.2.17/cc/common/cc_helper_internal.bzl`
- Verdict: partial
- Differences:
  - `FileType.of` matchers compare case-insensitively when the host OS is Windows. Zel’s `endswith` and `File.extension` checks stay case-sensitive, so suffixes such as `.H`, `.LIB`, `.OBJ`, `.ASM`, `.PCM`, and `.SO` match in Bazel on Windows and fail in Zel. The custom `.c` / `.C`, `.cc`/`.cpp`/`.cxx`/`.c++`/`.cu`/`.cl`, and `.s` / `.S` matchers are case-sensitive on every OS in both.
  - Zel does not keep the disjoint suffixes. `endswith(".o")` accepts `.pic.o` and `.indexing.o`, `endswith(".a")` accepts `.pic.a` and `.dll.a`, `endswith(".lo")` accepts `.pic.lo`, and `endswith(".lib")` accepts `.if.lib` and `.lo.lib`. Bazel `OBJECT_FILE` rejects `.pic.o`, `ARCHIVE` rejects `.pic.a`, `.if.lib`, and always-link `.lo`/`.lo.lib`, `ALWAYS_LINK_LIBRARY` rejects `.pic.lo`, `ASSEMBLER` rejects `.pic.s`, `PREPROCESSED_C` rejects `.pic.i`, and `PREPROCESSED_CPP` rejects `.pic.ii`.
  - `File.extension` is only the segment after the last dot. Checks of `"." + file.extension` miss `.dll.a`, `.pic.o`, `.pic.a`, `.lo.lib`, `.indexing.o`, `.thinlto.bc`, and `module.modulemap`. Bazel matches those suffixes on the full path.
  - `extensions.SHARED_LIBRARY` is `.so`, `.dylib`, `.dll`, `.pyd`, `.wasm`. Bazel `SHARED_LIBRARY` also includes `.tgt` and `.vpi`. Zel’s `is_shared_library` adds `.tgt` and `.vpi` by last segment only. There is no `.so`/`.dylib`-only set corresponding to `UNIX_SHARED_LIBRARY`.
  - Versioned libraries use `rpartition` on the last `.so.` and then `.dylib.`, with Unicode `is_alphanumeric` and scalar indexing. Bazel uses the ASCII regex `^.+\.((so)|(dylib))(\.\d\w*)+$` on the whole string. `lib.so.1.dylib.2` matches Bazel and fails Zel. `lib.so.1é` matches Zel and fails Bazel. `lib.so.2.` is a non-match in Bazel and an index error in Zel.
  - These Bazel matchers have no Zel predicate: `.bc`, `OBJC_HEADER` (every C++ header plus `.pch`), preprocessed `.i`/`.ii`/`.pic.i`/`.pic.ii`, `.imports`, `.thinlto.bc`, `.gcno`, `.afdo`, `.xfdo`, `.profdata`, `.profraw`, every `.zip`, and `module.modulemap`. `MODULE_MAP_HEADER` is absent: Bazel accepts every tree artifact and otherwise only a `.h` filename. `headerDiscoveryRequired` is only approximated by `_use_dotd_file`, which skips discovery when the last extension is `.s`, `.asm`, `.pcm`, `.gcm`, or `.ifc`.
  - `.rlib` is accepted as a static-library suffix. Bazel `RUST_RLIB` matches `.rlib` and `ARCHIVE` does not. `CC_AND_OBJC` adds headers and assembler suffixes and omits `.ipb`; Bazel `ALL_C_CLASS_SOURCE` is `.cc`/`.cpp`/`.cxx`/`.c++`/`.C`/`.cu`/`.cl`, `.c`, `.m`, `.mm`, and `.ipb`.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppHelper.java`
- Bazel lines: 94
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/cc_native.rs`
- Verdict: partial
- Differences:
  - `getCompileOutputArtifact` uses `config.isSiblingRepositoryLayout()` and `config.getBinDirectory(label.getRepository())`. With sibling layout, the relative path is `<pkg>/_objs/<name>/<output>` under that repository’s own bin root. Otherwise an external label is `external/<repo>/<pkg>/_objs/<name>/<output>` under the shared bin root. Zel ignores the `configuration` argument, hardcodes sibling layout off, and always writes `<pkg>/_objs/<name>/<output>` under the rule configuration’s `bazel-out/<mnemonic>/bin`, prefixing `external/<repo>/` for every non-main owner. The flag defaults to false, so the default layout matches. A non-rule configuration or sibling layout does not.
  - An absolute `output_name` replaces the object directory in `PathFragment.getRelative` (Unix: a leading `/`). Zel concatenates and then normalizes, so `/abs.o` becomes `<pkg>/_objs/<name>/abs.o`. On Unix, `\` stays inside a path segment. Zel turns `\` into `/` before splitting.
  - A missing or non-string `output_name` is a Starlark error. A missing `label` defaults to the rule label. Java requires both and throws on null.
  - `getToolchain` is absent. Java treats a null toolchain as missing: it tries `//tools/cpp:toolchain_type`, then `@bazel_tools//tools/cpp:toolchain_type`, and always unwraps the `cc` field, failing the rule if either is missing. Zel’s C++ rules use Starlark `find_cpp_toolchain` on a single `@@<repo>//tools/cpp:toolchain_type` already present in `ctx.toolchains`, return `None` when `mandatory` is false, and return `toolchain.cc` only when both `cc` and `cc_provider_in_toolchain` are set. Otherwise they return the toolchain info itself.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppIncludeExtractionContext.java`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No include-extraction action context that supplies an artifact resolver.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppIncludeScanningContext.java`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No action-context hook that, for a C++ compile, discovers extra inputs by include scanning and returns that artifact list, or signals a Skyframe restart when the scan cannot finish.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppLinkActionBuilder.java`
- Bazel lines: 100
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/cc_native.rs`
- Verdict: partial
- Differences:
  - `wrap_link_actions` returns the original actions object and drops `build_configuration` and `sharable_artifacts`. `LinkActionConstruction` roots every `declare_shareable_artifact` and `declare_shareable_directory` in `config.getBinDirectory(actionOwnerLabel.repository)`, using the passed `BuildConfigurationValue` when that argument is one and the rule configuration otherwise. Split-config links (Apple/Android `build_config`) therefore land in the rule configuration’s `bazel-out/<mnemonic>/bin`.
  - With `sharable_artifacts` false (the default), `create` and `createTreeArtifact` call `getDerivedArtifact` and `getTreeArtifact`, which require the path to start with the package directory (`external/<repo>/<pkg>` for a non-main owner when sibling layout is off) and throw `IllegalStateException` otherwise. They do not prepend the package. Zel’s `declare_shareable_artifact` and `declare_shareable_directory` always take the path as bin-root-relative and do not apply that check, so a path outside the package is created.
  - With `sharable_artifacts` true, Bazel stores the path unchanged under that bin root (a normal artifact or a tree artifact). For an external owner, Zel’s `derived` still prefixes `external/<repo>/` when the path does not already start with it, so the same call produces `bazel-out/<mnemonic>/bin/external/<repo>/<path>` rather than `bazel-out/<mnemonic>/bin/<path>`.
  - `getBinDirectory(repository)` is a different root only when sibling repository layout is on (`bazel-out/<repo>/<mnemonic>/bin`, and the package path omits `external/<repo>/`). Zel reports `is_sibling_repository_layout` as false and always uses the single `bazel-out/<mnemonic>/bin` root plus the external prefix above.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppModuleMap.java`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No module-map holder that pairs a module name with its generated map artifact.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppOptions.java`
- Bazel lines: 1026
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - Repeatable string flags (`--copt`, `--cxxopt`, `--conlyopt`, `--objccopt`, `--linkopt`, `--ltoindexopt`, `--ltobackendopt`, `--stripopt`, and the `host_*` variants) are `List<String>` with one element per occurrence. Zel types them as comma-separated lists and the configuration map joins occurrences with commas and splits again, so `--linkopt=-Wl,--start-group` becomes two options and an empty `--copt=` is dropped.
  - `--grte_top` and `--host_grte_top`: `default` is a null label in Bazel. Zel keeps the string `default`. Any other accepted label is rewritten to that package’s `:everything` target. Zel keeps the original target name, and `cc_libc_top_alias` parses that raw string, so `default` fails to parse and `//pkg:name` aliases `:name`.
  - `--fission` accepts the same `yes` / `no` / mode tokens, but the cpp fragment keeps the raw text, splits on commas, and compares case-sensitively to `fastbuild`, `dbg`, and `opt`. `--fission=OPT` parses and does not enable opt. Bazel stores the `CompilationMode` set (`yes` is all three modes, `no` is empty, duplicates removed).
  - `--per_file_copt`, `--host_per_file_copt`, and `--per_file_ltobackendopt` split on the first `@`, escaped or not, and only the filter is a regex. Zel validates the filter before the last unescaped `@`, so `.*@-O0@(` is rejected by Zel and accepted by Bazel, while `foo\@bar@-O0` is accepted by Zel and rejected by Bazel.
  - Label flags (`--custom_malloc`, `--fdo_profile`, `--xbinary_fdo`, `--fdo_prefetch_hints`, `--cs_fdo_profile`, `--propeller_optimize`, `--memprof_profile`, `--proto_profile_path`) accept a relative value by parsing `//` plus that text, then store the original text. The fragment parses the original text and yields none. The configuration canonicalizer instead builds `//{client package}:{text}`, not `//{text}`.
  - `--strip` maps any case of `always` / `sometimes` / `never` to `StripMode`. The fragment compares the raw spelling to lowercase `always` and `sometimes`, so `--strip=Always` parses and does not strip.
  - The cpp fragment drops non-default values for `--fdo_optimize`, `--fdo_instrument`, `--cs_fdo_instrument`, both propeller absolute-profile flags, both include-scanning flags, `--incompatible_use_specific_tool_files`, `--experimental_cc_implementation_deps`, and `--incompatible_disable_nocopts`, returning none or the default boolean instead.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppRuleClasses.java`
- Bazel lines: 319
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No counterpart for the C++ toolchain feature-name constants (`lang_objc`, `unfiltered_compile_flags`, `parse_headers`, `module_maps`, `thin_lto`, `fdo_optimize`, `coverage`, `cpp_link`, and the rest).

### `src/main/java/com/google/devtools/build/lib/rules/cpp/CppSemantics.java`
- Bazel lines: 65
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No `validate_layering_check_features` Starlark entry (rule context and aspect descriptor, `CcToolchainProvider` wrap, `unsupported_features` cast to a string set, `RuleErrorException` surfaced as `EvalException`) and no `@rules_cc+//` label prefix check.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/DebugPackageProvider.java`
- Bazel lines: 99
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No wrapper that resolves `DebugPackageInfo` from `@rules_cc+//cc/private:debug_package_info.bzl` and reads `target_label` as a required label, `unstripped_file` as a required file, and `stripped_file` and `dwp_file` as files that are null when the field is absent or `None`.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/FdoContext.java`
- Bazel lines: 112
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No reader of the `fdo_context` struct: `branch_fdo_mode` equality for `auto_fdo`, `xbinary_fdo`, `llvm_fdo`, and `llvm_cs_fdo`; noneable `profile_artifact`, `prefetch_hints_artifact`, `propeller_optimize_info`, `memprof_profile_artifact`, and `proto_profile_artifact`. `hasArtifacts` treats a present branch profile, prefetch hints, propeller info, or memprof artifact as set and ignores `proto_profile_artifact`.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/FeatureConfigurationForStarlark.java`
- Bazel lines: 91
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/cc_internal.rs`
- Verdict: partial
- Differences:
  - `str()` is `<FeatureConfiguration(ENABLED:` plus enabled feature names joined by `", "` in enabled-feature insertion order, then `;REQUESTED:` plus requested names joined by `", "` in request order with duplicates dropped, then `)>`. Zel `str()` is `<FeatureConfiguration <registry index>>` and does not list either set.
  - `print` uses `debugPrint`, which is `<FeatureConfiguration(` plus the same enabled names joined by `", "`, then `)>`, with no requested names. Zel `print` uses the same host string as `str`.
  - `repr()` is the default `<unknown object com.google.devtools.build.lib.rules.cpp.FeatureConfigurationForStarlark>`. Zel `repr()` is the same `<FeatureConfiguration <registry index>>` string as `str`.
  - `type()` is `FeatureConfiguration`. Zel `type()` is `host_object`.
  - `is_enabled` and `is_requested` fail with `file '<label>' cannot use private API` unless the caller is builtins or on the internal Starlark API allowlist. Zel runs both with no allowlist check.
  - A missing or non-string `feature` argument is a Starlark argument/type error. Zel treats that argument as `""` and returns a bool.
  - Given a string, both methods are exact membership: `is_enabled` in the enabled feature-name set, `is_requested` in the requested-name set, including names absent from the toolchain.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/FeatureSelection.java`
- Bazel lines: 301
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/cc_features.rs`
- Verdict: partial
- Differences:
  - When more than one provide-symbol is implemented by multiple enabled selectables, Bazel reports the symbol that appears first in toolchain order. Zel walks those symbols in `BTreeMap` order and reports the lexicographically smallest one. The message text and the definition-order list of provider names for a single symbol match. Enabled-feature order, implication closure, and requirement pruning match.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/HeaderDiscovery.java`
- Bazel lines: 306
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs`
- Verdict: partial
- Differences:
  - Bazel returns a stable-order nested set of non-mandatory artifacts. Each dependency path is resolved to the first allowed derived artifact whose `pathMapper`-mapped exec path matches (`HashMap.putIfAbsent`), otherwise to a source artifact in the package discovered from that exec path, otherwise to an enclosing tree artifact. Zel only keeps relative path strings from `.d` outputs that are absent from the declared input list, and uses them solely as action-cache discovered inputs.
  - Bazel drops absolute paths under the permitted system-include prefixes (ASCII case-insensitive when the execution platform is Windows), rewrites exec-root paths to exec-root-relative, and with sibling repository layout rewrites paths under the exec-root parent to the `../` external prefix (including the same-name `local_repository` / `ignoreMainRepository` exception). Remaining absolute paths fail the action when inclusion validation is on. Zel drops only strings that start with `/`, strips a literal `execroot + "/"` prefix, and never fails the action.
  - Unresolved paths with extension `cppmap` are ignored. Any other unresolved path is an undeclared-inclusion failure when validation is on. Zel records the path and does not special-case `cppmap`.
  - On Windows, source lookup is ASCII case-insensitive. If several artifacts match and any of them is already a source input, only those overlapping inputs are kept; if none overlap, every match is kept. The compile source itself is removed by artifact equality. Zel compares exact path strings and has no case folding or multi-match rule.
  - A header whose parent directory is a tree artifact, or whose parent is under any tree artifact's mapped exec path, adds that tree artifact (`findAny` if several trees match). Zel does not treat tree artifacts as covering nested headers.
  - Discovered artifacts follow dependency iteration order. Zel sorts the path strings and deduplicates them.
  - Depfile bytes reach Bazel discovery as Latin-1 path strings (`ISO-8859-1`, one char per byte). Zel decodes the `.d` file with lossy UTF-8, so invalid sequences become U+FFFD before the path is kept or dropped.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/HeaderInfoCodec.java`
- Bazel lines: 169
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No codec that serializes C++ `HeaderInfo` for Skyframe.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/IncludeProblems.java`
- Bazel lines: 55
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No collector of include-scanning problems that is later formatted into an action failure.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/IncludeScannable.java`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No interface that exposes the sources, declared includes, and quote/system include dirs an include scanner reads.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/IncludeScanner.java`
- Bazel lines: 279
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No include scanner that walks quote and system includes and returns the discovered artifacts.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/Link.java`
- Bazel lines: 275
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No link-type catalog (`STATIC_LIBRARY`, `DYNAMIC_LIBRARY`, `EXECUTABLE`, and the PIC and interface-library variants) with the matching linker output categories.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/LinkCommandLine.java`
- Bazel lines: 229
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/cc_native.rs`
- Verdict: partial
- Differences:
  - A param file is attached only when `splitCommandLine` is set and some expanded argument contains `LINKER_PARAM_FILE_PLACEHOLDER`. Zel attaches one whenever `parameter_file_type` is a string, and if that placeholder is absent it still spills with `use_always` and flag format `@%s`.
  - Arguments containing the `linker_param_file` value are dropped whenever that variable is available, including when the command line is not split. Zel drops them only in the param-file branch.
  - The filter string is `getStringValue` (string, boolean `"1"`/`"0"`, artifact exec path, or path fragment), and a non-string value fails expansion. Zel filters only a `File` or string and leaves every other type in the argv with no error.
  - `UNQUOTED` is one raw argument per line. `GCC_QUOTED` and `WINDOWS` keep their own escapers. Zel maps every type other than `UNQUOTED`, including those two, to shell quoting. An unknown type name throws in Bazel and is shell-quoted in Zel.
  - With no forced tool path, the linker executable is the feature-config tool for `actionName`, and a missing action config is an error. That executable is a separate leading command-line segment. Zel returns only the expanded flag args.
  - A null variable map becomes an empty map, so nothing is filtered. Zel rejects a missing or invalid `build_variables` value.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/LtoBackendAction.java`
- Bazel lines: 324
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No LTO backend action that compiles one bitcode file into an object file.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/LtoBackendActionTemplate.java`
- Bazel lines: 460
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No template that expands a tree of bitcode files into one LTO backend action per file.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/LtoBackendArtifacts.java`
- Bazel lines: 249
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bazel-tools/builtins/common/cc/link/lto_backends.bzl`
- Verdict: partial
- Differences:
  - `cc_internal.create_lto_backend_action` and `cc_internal.create_lto_backend_action_template` fail as unimplemented, so no LTO backend action is registered. Bazel’s command line is the `lto-backend` tool path, then the feature-expanded arguments, then `-fPIC` when `usePic` is true, appended last so it cannot be overridden.
  - Starlark still fills `thinlto_index` (`/dev/null` when the index is absent), `thinlto_output_object_file`, `thinlto_input_bitcode_file`, and, when a dwo file is present, `per_object_debug_info_file` plus empty `is_using_fission`. Those values are never expanded, so Bazel’s `PathMapper` rewrite of the exec paths does not run.
  - A directory bitcode input requires a separate per-file path string in `thinlto_input_bitcode_file` (the file inside the tree). The non-template call passes no path and rejects a directory; the template native that would supply per-file paths is unimplemented. Java also skips the tree-artifact checks for the template fingerprint action (`isDummyAction`); that dummy action is absent.
  - Imports stay on the action inputs so they survive `--discard_orphaned_artifacts` and feed input discovery with `allBitcodeFiles`. With no registered action, that discovery does not run.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/PropellerOptimizeInputFile.java`
- Bazel lines: 37
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No holder for the Propeller cc and ld profile artifacts.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/ShowIncludesFilter.java`
- Bazel lines: 210
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No filter that reads MSVC `/showIncludes` stdout and turns those lines into discovered headers.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/SolibSymlinkAction.java`
- Bazel lines: 336
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/cc_native.rs`
- Verdict: partial
- Differences:
  - Long-name hashing trips at Java `String.length()` (UTF-16 code units; Bazel internal paths are Latin-1, one unit per raw byte). Zel uses `str::len()` (UTF-8 bytes), so non-ASCII names cross 255 on a different side. The digest is SHA-256 of `hashString(..., UTF_8)`, which UTF-8-encodes those Latin-1 units (bytes ≥ 0x80 expand to two UTF-8 bytes). Zel hashes the string’s UTF-8 bytes. Non-UTF-8 filesystem names are turned into U+FFFD by `to_string_lossy` before mangling.
  - A name longer than 255 whose final `.` is the last character becomes the bare 64-char hash (`Files.getFileExtension` is empty). Zel keeps the trailing dot (`{hash}.`).
  - Bazel rejects a library, and for the explicit-path overload the link basename, unless it matches `Link.SHARED_LIBRARY_FILETYPES` (fixed suffixes, no `.` after the last slash, or the versioned `.so`/`.dylib` pattern `^.+\\.((so)|(dylib))(\\.\\d\\w*)+$`). Zel registers the action for any artifact and never runs that regex.
  - `PathFragment.getRelative` leaves an empty solib directory or an absolute link path unchanged and normalizes `.`, `..`, and repeated separators. Zel concatenates `{solib_dir}/{rest}` as raw text, so an empty directory becomes a leading `/` and `.` / `..` / `//` stay in the soname and the output path. `getDynamicLibrarySoname` sees the normalized fragment; Zel uses the caller string.
  - Omitted `preserve_name` and `prefix_consumer` default to false, and a missing or non-string `runtime_solib_dir_base` falls back to `solib_directory`. Bazel requires those Starlark arguments; a Java `null` runtime dir selects the toolchain directory. Non-booleans are accepted through `is_truthy()`.
  - `computeKey` fingerprints the symlink exec path and the primary input exec path and no GUID. Zel’s aquery key for this mnemonic is the `Symlink` GUID only. The empty execution platform and empty exec properties match.
  - Execution creates the symlink and fails the action on I/O (`LINK_CREATION_IO_EXCEPTION`, not catastrophic), including spawn-log I/O (`LINK_LOG_IO_EXCEPTION`), then copies the input’s file metadata onto the output. Zel deletes the output first, and if `symlink` fails it copies the target bytes. There is no spawn-log failure and no metadata injection. `getProgressMessage()` is null; Zel’s symlink execution path reports `Creating symlink …`. `mayInsensitivelyPropagateInputs()` is true, and Zel has no case-insensitive rewind propagation.

### `src/main/java/com/google/devtools/build/lib/rules/cpp/SwigIncludeScanningContext.java`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No action context that include-scans a SWIG source and returns the extra inputs.

### `src/main/java/com/google/devtools/build/lib/rules/extra/ActionListener.java`
- Bazel lines: 88
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - Bazel copies `mnemonics` into a `HashSet` (duplicate mnemonics collapse) and publishes `ExtraActionMapProvider` as an `ImmutableSortedKeyListMultimap`: keys are sorted, and each mnemonic maps to the same `ExtraActionSpec` list in prerequisite order. Zel’s `extra_action_like` only resolves `extra_actions` and returns empty files and empty runfiles, so `mnemonics` are never read and that provider is never attached.
  - A prerequisite whose `ExtraActionSpec` is null is skipped and recorded as an `extra_actions` attribute error (`target <label> is not an extra_action rule`); `RuleConfiguredTargetBuilder.build()` then returns null when the rule has errors. Zel accepts any configured dependency and still succeeds.
  - An empty collected spec list emits the `extra_actions` attribute warning `No extra_action is specified for this version of bazel.` and still yields a target whose map is empty. Zel emits no warning.

### `src/main/java/com/google/devtools/build/lib/rules/extra/ActionListenerRule.java`
- Bazel lines: 119
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_defs_gen.rs`
- Verdict: partial
- Differences:
  - `extra_actions` is a mandatory `LABEL_LIST` in both, but Bazel allows only the `extra_action` rule class and no files (`allowedRuleClasses("extra_action")`, `allowedFileTypes()` → `FileTypeSet.NO_FILE`). Any other rule fails analysis, and an existing source file fails the file-type check. The generated attribute leaves `allowed_rule_classes` unset and `allowed_files` as any file, so Zel accepts other rules and source files.
  - Inherited `exec_compatible_with`, `exec_group_compatible_with`, and `target_compatible_with` are also `allowedFileTypes()` / no-file (and `target_compatible_with` requires constraint providers). The schema keeps type, nonconfigurability, and the tool-dep flag, and drops those predicates, so those prerequisites are not rejected.
  - `deps`, `data`, and `:action_listener` are removed in both. `mnemonics` stays a mandatory, order-preserving `STRING_LIST` (empty list allowed). `exec_properties` defaults to an empty dict, `exec_compatible_with` to an empty list, and `exec_group_compatible_with` to an empty dict, with the last two nonconfigurable tool deps.

### `src/main/java/com/google/devtools/build/lib/rules/extra/ExtraActionFactory.java`
- Bazel lines: 94
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - Bazel builds an `ExtraActionSpec` from `CommandHelper` tools, `data` prerequisite artifacts, `out_templates`, the rule label, `requires_action_output` (default false), and `TargetUtils.getExecutionInfo` (a `HashMap` of legal tags mapped to `""`, then copied), and adds that provider plus an empty runfiles provider. Zel only resolves `tools` in the exec configuration and returns an empty target, so `data`, templates, the label spec, the boolean, and the execution-info map are dropped.
  - Bazel reads `cmd` and does a literal `String.replace` (not `java.util.regex`) of `$(EXTRA_ACTION_FILE)`, `$(ACTION_ID)`, `$(OWNER_LABEL_DIGEST)`, and `$(output ` to a doubled `$`, then expands the rest with the configuration make-variable context and data exec locations. Zel never reads `cmd`, so those owner-time variables are not preserved and no make variables are expanded.
  - If the rule context already has errors, Bazel returns null and produces no target. Zel returns an error only when `tools` resolution fails; otherwise it returns an empty configured target.

### `src/main/java/com/google/devtools/build/lib/rules/extra/ExtraActionRule.java`
- Bazel lines: 167
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_defs_gen.rs`
- Verdict: partial
- Differences:
  - `tools` is a label list with `allowedFileTypes()` (`FileTypeSet.NO_FILE` and strict label checking) under the exec transition. A source file is rejected: a missing label is reported as a nonexistent rule or target, and an existing file is misplaced. Zel records `tools` as a configurable `LABEL_LIST` with `tool: true` (analysis resolves it in the exec configuration) and leaves `allowed_files` as `Any`, so a source file in `tools` is kept as a dependency.
  - `cmd` (mandatory string), `out_templates` (string list, empty when unset), `requires_action_output` (boolean, default false, `0`/`1` accepted), and the removal of `deps` and `:action_listener` match the generated schema.

### `src/main/java/com/google/devtools/build/lib/rules/filegroup/Filegroup.java`
- Bazel lines: 135
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - `output_group` values ending in `_INTERNAL_` are rejected with an attribute error in Bazel. Zel still selects that group.
  - A non-empty `output_group` that a `srcs` dep does not provide is an empty stable-order set in Bazel (deps with no `OutputGroupInfo` are skipped). Zel fails analysis if the name is absent from `output_groups`.
  - An empty `output_group` forwards `srcs` files in both. Bazel keeps stable nested-set order. Zel dedups into a `BTreeSet` and sorts by exec path.
  - `--incompatible_filegroup_runfiles_for_data` defaults to true. When it is false, Bazel data runfiles are the built files plus `data` deps only. Zel always also merges each `srcs` dep’s data runfiles.
  - Data-dep files are added to runfiles only when `--incompatible_always_include_files_in_data` is true (the default). Zel always merges those files, and it appends them after the dep’s data runfiles. Bazel inserts them before the merged runfiles.
  - With coverage enabled, Bazel forwards `InstrumentedFilesInfo` from `srcs` and `data` and does not treat those attributes as local sources. Zel leaves instrumented files unset.
  - Bazel’s built target exposes default runfiles as `_hidden_top_level_INTERNAL_` and forwards validation output groups. Zel publishes `default` (the built files) and, when there is exactly one file, `compilation_outputs`. A later `filegroup` with `output_group = "default"` or `"compilation_outputs"` therefore sees those files in Zel and an empty set in Bazel.

### `src/main/java/com/google/devtools/build/lib/rules/filegroup/FilegroupRule.java`
- Bazel lines: 147
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_class.rs`
- Verdict: partial
- Differences:
  - Bazel skips environment-constraint checks on `data` (`dontCheckConstraints()`) and still checks `srcs`. Zel stores no skip-constraints flag and does not run those checks, so an environment-incompatible `srcs` dependency is rejected by Bazel and accepted by Zel.
  - The `NativeBuildRule` ancestor this rule attaches restricts `compatible_with`, `restricted_to`, `package_metadata`, `aspect_hints`, and `target_compatible_with` to `FileTypeSet.NO_FILE` (`compatible_with` and `restricted_to` only allow `environment` rules; `target_compatible_with` requires `ConfigMatchingProvider` and `ConstraintValueInfo`). Zel leaves those attributes at `FileTypes::Any` with no allowed-rule-class or provider requirement, so source files and other rule kinds are accepted.
  - `native.existing_rule()` lists this rule’s attributes from a `BTreeSet` (alphabetical). Bazel returns them in rule-class declaration order (`srcs`, `output_group`, `data`, `output_licenses` after the common attributes).

### `src/main/java/com/google/devtools/build/lib/rules/genquery/GenQuery.java`
- Bazel lines: 522
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - `scope` and `strict` are dropped. Bazel loads only the scope’s transitive closure and, with `strict` (default true), fails when the query leaves it; `strict = false` warns and skips that path. Zel queries the whole loader.
  - Streamed formatters (`label` and the other `StreamedFormatter`s, including the default) run graphless with `--order_output=no` and a lexicographic aggregator, except a top-level `somepath`, which keeps walk order. `minrank`, `maxrank`, and `graph` use the graph environment and full order. Zel always uses the graph environment and full topological order.
  - Query patterns that are filenames (`couldn't determine target from filename`) or recursive (`recursive target patterns are not permitted`) are rejected here. Zel does not apply those checks.
  - `compressed_output` (default false) writes a gzip file when true; otherwise the file is the raw query bytes (gzip above 1 MiB is only the in-memory form and is decompressed on write). Zel never reads the attribute and stores `String::from_utf8_lossy`, so non-UTF-8 output (proto, gzip) is replaced with U+FFFD.
  - Default runfiles contain the output file, and an empty `validation_transitive` output group is registered. Zel’s default and data runfiles are empty, and that group is absent.
  - Opts parsing is Bazel `QueryOptions`: `--keep_going`, a non-empty `--universe_scope`, `--order_results`, `--noorder_results`, `--order_output`, and `--experimental_graphless_query` are `opts` attribute errors; `--nokeep_going` and an empty `--universe_scope` are allowed; other query options keep their command-line defaults, with `relative_locations` forced true and `nodep_deps` forced false unless set. Zel allows only `--implicit_deps`, `--tool_deps`, `--nodep_deps`, `--relative_locations`, and `--output=`; `--nokeep_going` and any `--universe_scope` are rejected, and every other option is an unrecognized-option error on the rule rather than on `opts`.
  - Syntax and eval failures are `query syntax error:` / `query failed:` rule errors, and a missing Skyframe dep returns no target without an error. Zel surfaces the query command text (`Error while parsing '…':` and the eval error) and has no silent restart.

### `src/main/java/com/google/devtools/build/lib/rules/genquery/GenQueryOutputStream.java`
- Bazel lines: 228
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No stream that keeps a genquery result in memory until 1 MiB and then spills a gzip file.

### `src/main/java/com/google/devtools/build/lib/rules/genquery/GenQueryPackageProvider.java`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No package provider that answers genquery lookups only from the scope’s loaded packages.

### `src/main/java/com/google/devtools/build/lib/rules/genquery/GenQueryPackageProviderFactory.java`
- Bazel lines: 427
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No factory that loads the genquery scope closure and builds the scoped package provider.

### `src/main/java/com/google/devtools/build/lib/rules/genquery/GenQueryRule.java`
- Bazel lines: 136
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_class.rs`
- Verdict: partial
- Differences:
  - `scope` is `GENQUERY_SCOPE_TYPE_LIST` (`LabelClass.GENQUERY_SCOPE_REFERENCE`): the labels are query-visible dependencies and are not configured. Zel stores `scope` as a normal `LabelList` with `nodep: false`, the same dependency class as `deps` and `data`.
  - Bazel only marks `scope` mandatory, so `scope = []` is accepted. Zel keeps `non_empty` from the hand-written attribute through `apply_gen`, and loading rejects an empty `scope`.
  - Inherited label attributes that Bazel sets to `FileTypeSet.NO_FILE` (`compatible_with`, `restricted_to`, `target_compatible_with`, `exec_compatible_with`, `exec_group_compatible_with`, `aspect_hints`, `package_metadata`) stay `FileTypes::Any`, so source-file labels are accepted where Bazel rejects them. `scope` itself is any file type on both sides (`legacyAllowAnyFileType()`).

### `src/main/java/com/google/devtools/build/lib/rules/genrule/GenRuleBase.java`
- Bazel lines: 402
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - Zel returns on the first `outs`, `executable`, or `cmd` error. Bazel records those attribute errors and still resolves `srcs` and tools before failing the target.
  - On a Windows exec platform Bazel selects an explicitly set `cmd_ps` (Powershell, `.genrule_script.ps1`) or `cmd_bat` (batch, `.genrule_script.bat`, and `/` replaced by `\` in make-variable values). Zel only chooses `cmd_bash` then `cmd`, and always runs bash.
  - Zel always prepends `source external/bazel_tools/tools/genrule/genrule-setup.sh;` and adds that hardcoded path as an input. Bazel prepends `source` only for bash, using the `$genrule_setup` prerequisite exec path and its files-to-build.
  - The shell is `shell_executable` or `/bin/bash` with argv `["-c", command]`. Bazel uses `ShToolchain` (`/bin/bash` when `requires-darwin` is set) and, above the command-length limit, writes `<name>.genrule_script.sh` (`#!/bin/bash` plus the command) and runs `[shell, script]`.
  - An empty `message` leaves the progress message unset. Bazel uses `Executing genrule <label>`.
  - `stamp` is ignored. Bazel adds the stable and volatile workspace-status artifacts when `stamp` is yes, or auto and `stampBinaries()` is set.
  - Execution requirements are a `BTreeMap` of `local`, `requires-*`, and only `no-sandbox`, `no-cache`, `no-remote`, `local`, and `block-network`. Bazel copies every legal tag (`block-*`, `requires-*`, `no-*`, `supports-*`, `disable-*`, `cpu:*`, `local`, worker mnemonic, `resources:`) through a `LinkedHashMap` and then applies `modifyExecutionInfo` for mnemonic `Genrule`.
  - Toolchains (the `toolchains` attribute and the toolchain prerequisites) are not tool inputs, so their `$(location)` values and template make variables are absent. Make-variable lookup is only `SRCS`, `OUTS`, `<`, `@`, `RULEDIR`, `@D`, `BINDIR`, `GENDIR`, `COMPILATION_MODE`, and `TARGET_CPU`.
  - `output_to_bindir` is ignored: outputs, `$(RULEDIR)`, `$(BINDIR)`, and `$(GENDIR)` are always under `bazel-out/<mnemonic>/bin`. Bazel uses the genfiles root unless `output_to_bindir` is set or genfiles are merged into bin.
  - `heuristic_label_expansion` is ignored. `$(locations)` paths are sorted in a `BTreeSet`. A single-segment `$(@D)` becomes an empty string. Bazel keeps stable nested-set order, and a single-segment `$(@D)` throws.
  - `$<` / `$@` are detected by scanning the string after deleting every `$$`, and a failed lookup expands to `""`. `$$(<)` is reported as a use of `$<`. Bazel expands `$$` to `$` during expansion and errors only when the variable is actually expanded.
  - Single-character make variables are scanned as Unicode scalars. Bazel’s expander takes one UTF-16 code unit.
  - Tool inputs are the tool’s files plus individual default runfiles. Bazel uses each tool’s files-to-build, and for an executable the runfiles-tree artifact and that executable.
  - No `InstrumentedFilesInfo` is produced for `srcs`.

### `src/main/java/com/google/devtools/build/lib/rules/genrule/GenRuleBaseRule.java`
- Bazel lines: 349
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_class.rs`
- Verdict: partial
- Differences:
  - `$is_executable` is a nonconfigurable computed default: true only when `executable` is true and `outs` has exactly one entry. Zel stores a constant `false` on the implicit attribute, and `RuleInstance::is_binary_rule` treats the target as executable whenever the public `executable` attribute is true.
  - `outs` is a mandatory `OUTPUT_LIST`. Zel also sets `non_empty`, so an explicit empty list fails while the package is loading.
  - `toolchains` allows no files, requires `TemplateVariableInfo` or `ToolchainTypeInfo`, skips constraint checking, and is nonconfigurable. Zel keeps a nonconfigurable label list with `FileTypes::Any` and an empty provider list, and genrule analysis never resolves those labels, so they are not prerequisites and do not contribute Make variables.

### `src/main/java/com/google/devtools/build/lib/rules/java/BootClassPathInfo.java`
- Bazel lines: 101
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - `isEmpty` is true only when `bootclasspath`, `_auxiliary`, `_system_inputs`, and `_system_path` are all empty (`None`/null nested sets become empty stable-order sets). `boot_class_path` keeps the provider only when `bootclasspath` has artifacts or `_system_path` is present, and otherwise substitutes the toolchain `_bootclasspath_info`, so an auxiliary-only or system-inputs-only provider falls back.
  - `_system_path` is an `Optional` of a string (`Starlark.NONE`/null is empty; a non-string fails the rule). Rust yields `Some` only for a string and `None` for any other value, including a present non-string, with no error. A missing provider becomes that toolchain fallback, or four empty defaults when the fallback is also absent.
  - A non-depset field fails in `Depset.noneableCast`. Rust `items` accepts a list, tuple, or set and drops non-artifact elements, so the same field becomes an empty or shorter artifact list.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaCompilationHelper.java`
- Bazel lines: 590
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - `enable_jspecify` defaults to true and is ignored. Bazel, when the toolchain JSpecify info matches the label, appends implicit deps to the transitive classpath only, merges the JSpecify processor, and appends JSpecify javac opts after the existing opts.
  - `--experimental_turbine_annotation_processing` is ignored. Bazel then runs a header compile with the direct header compiler disabled, emits `-turbine-resources.jar`, `-turbine-apt.jar`, `-turbine-apt.jdeps`, and a gensrc jar, adds that gensrc jar to the javac source jars, and passes an empty processor-class set with the processor classpath and data still set. Zel always runs processors in the javac action.
  - A separate resource jar is named with `appendWithoutExtension` (`dotIndex > 0`) on the sibling-layout output-dir-relative path. Zel inserts `-class` before the last `.` of `root_relative_path`, including a leading dot.
  - Bootclasspath fallback treats a provider as present when boot jars are non-empty or `_system_path` is set. Bazel uses it whenever boot jars, auxiliary jars, system inputs, or system path is non-empty, so auxiliary-only or system-input-only providers fall through to the toolchain in Zel.
  - `--experimental_fix_deps_tool` is fixed as `add_dep`. Bazel passes the configured value.
  - `LC_CTYPE` is `en_US.UTF-8` when a platform string contains `osx` or `macos`, or when the Zel process is macOS. Bazel chooses that only when the execution platform's constraint OS is Darwin.
  - Execution info is a sorted `BTreeMap` with path-mapping and worker bits only. Bazel keeps insertion order, applies `--modify_execution_info`, then overlays rule-tag requirements with later keys winning.
  - Coverage does not write `-paths-for-coverage.txt` or pass `--post_processor jacoco`. Bazel does when coverage is on, the target has sources, and the instrumentation filter includes it.
  - `--experimental_local_java_optimizations` does not rename the compile output to `-pre-optimization.jar` or register the optimizer spawn (`-runtype LOCAL_ONLY`, `-libraryjars` = bootclasspath plus classpath, unquoted param file, mnemonic from the optimizer label).
  - Package-configuration `data()` files that match the label are not compile inputs.
  - GenClass requires the toolchain jar. Bazel uses `$genclass` when that is null, and that spawn has an empty environment. Zel turns on the configuration action environment.
  - Header compilation drops compile-time `.jdeps` and the configured classpath mode on the annotation-processing path. `--builtin_processors` follows processor-list order. Bazel intersects sets.
  - Javac opts are retokenized with Unicode scalars and Unicode whitespace, and a backslash inside double quotes is always dropped. Bazel walks UTF-16 code units, splits only on space and tab, and keeps the backslash unless the next unit is `\` or `"`.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaCompileAction.java`
- Bazel lines: 904
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs`
- Verdict: partial
- Differences:
  - Reduced-classpath membership stores `getExecPathString()` as Bazel internal Latin-1 (one `char` per raw byte) and `Deps.Dependency.getPath()` as a protobuf Unicode string in the same `HashSet`. Zel compares Rust Unicode strings and decodes `.jdeps` paths with `String::from_utf8_lossy` (invalid UTF-8 becomes U+FFFD), so non-ASCII and non-UTF-8 jars are kept or dropped differently. A `Dependency` with no path is `""` in Java and a parse error in Zel.
  - Unquoted param files are the Latin-1 internal bytes of each argument (`StringUnsafe.getInternalStringBytes`). Zel writes the classpath param file as UTF-8.
  - Protobuf `parseFrom` reads tags and string lengths as signed 32-bit varints and rejects a length that does not fit in a non-negative `int`. Zel accumulates a `u64` and casts the length to `usize`.
  - `computeKey` adds the UUID `e423747c-2827-49e6-b961-f6c08c10bb51`, `classpathMode.ordinal()` via `addInt` (`OFF`=0, `JAVABUILDER`=1, `BAZEL`=2, `BAZEL_NO_FALLBACK`=3), and `executionInfo` plus the fixed environment in `ImmutableMap` insertion order. Zel fingerprints a `BTreeMap` environment (sorted keys) and, only for a reduced-classpath spec, the mode name and the full classpath.
  - `createFullOutputDeps` is absent. Java builds a `HashMap` from mapped exec path to original path and, on a collision, keeps the earlier path, then rewrites the on-disk `.jdeps`. Java prefers `SpawnResult.getInMemoryOutput` when it is non-null (`--experimental_inmemory_jdeps_files` defaults to true) and reads the file only when that is null. Zel only reads the file and does not rewrite paths.
  - A null `outputDepsProto` with `BAZEL` or `BAZEL_NO_FALLBACK` fails action construction. Zel forces the mode to `OFF` and runs the full classpath. Any tree-artifact output throws in Java and is accepted in Zel.
  - Param-file suffixes count only command lines that use a param file, starting at `<output>-0.params`. Zel uses the argument-vector index, so a `java -jar …` executable shifts the flag and classpath files (`-0`/`-1` versus `-1`/`-2`).
  - The Java spawn reserves 750 MB and 1 CPU. Zel acquires the default action resources, 250 MB and 1 CPU.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaCompileActionBuilder.java`
- Bazel lines: 518
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - Zel always appends `--compress_jar`. Bazel appends it only when `compressJar` is true, and that field defaults to false.
  - Zel always passes `--experimental_fix_deps_tool add_dep`. Bazel passes the configured `fixDepsTool` string, whose default is `add_dep`.
  - For a non-main repository, Zel passes `--target_label` as `@` plus the canonical label (`@@@repo//pkg:name`). Bazel prefixes `@` to `Label.getDisplayForm(mainRepoMapping)`, so a main-repo apparent name is `@@apparent//pkg:name` and the canonical fallback is `@@@repo//pkg:name`.
  - Zel’s fixed environment is only `LC_CTYPE` (`C.UTF-8`, or `en_US.UTF-8` when a configured platform string contains `osx` or `macos`, or the host OS is macOS), and the inherited set is empty. Bazel merges the supplied UTF-8 map into the configuration action environment, using `en_US.UTF-8` when the execution platform’s OS constraint is Darwin and falling back to the host OS only when that constraint is absent.
  - Zel stores environment and execution requirements in `BTreeMap`s, so fingerprint iteration is sorted by key. Bazel keeps insertion-ordered `ImmutableMap`s. Zel also omits `--modify_execution_info` adjustments and rule-tag execution requirements.
  - Zel omits the classpath param-file argument when the classpath is empty and the mode is `OFF`. Bazel always appends `getFullClasspathLine()` as a use-always param file.
  - Mandatory inputs are a first-seen `Vec` deduped by exec path (plugins, sources, runtime `files`, bootclasspath, sourcepath, extra inputs, system inputs, then tool runfiles, then toolchain tools, then classpath, then `.jdeps`). Bazel builds a stable-order nested set that already includes compile-order JavaBuilder inputs (data and the executable only; runfiles are excluded) plus `javaBaseInputs()`, and that set also includes per-package `extraData` and the coverage artifact.
  - Zel never adds a coverage artifact input or `--post_processor jacoco <artifact>`.
  - Zel does not attach the extra-action `JavaCompileInfo` (source files, classpath, bootclasspath, source jars as sourcepath, javac opts, processors, processor path, output jar, optional `--system`, and the expanded arguments).
  - Zel keeps duplicate outputs and accepts tree-artifact outputs. Bazel builds an insertion-ordered set and rejects tree artifacts.
  - The same function also rewrites the javac output to a `-class` jar and registers GenClass and resource-jar actions. This builder only constructs the `Javac` action.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaCompileActionContext.java`
- Bazel lines: 50
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/java_compile.rs`
- Verdict: partial
- Differences:
  - Bazel keeps a `ConcurrentHashMap` of parsed `Deps.Dependencies`. A hit returns the stored message and does not open the file. A miss parses, `putIfAbsent` stores the first writer, and the caller still returns its own parse. `insertDependencies` overwrites the entry, so a later lookup returns that object even when the file is missing or differs (header compile seeds path-mapped in-memory `.jdeps` this way). Zel reads and `parse_jdeps`s on every call and has no insert path.
  - `Dependencies.parseFrom` rejects invalid UTF-8 in string fields with `InvalidProtocolBufferException`. Zel decodes each dependency `path` with `String::from_utf8_lossy`, so illegal bytes become U+FFFD and the path is still kept.
  - Protobuf length prefixes are 32-bit (`readRawVarint32`); a length whose low 32 bits have the sign bit set is negative and the parse fails. Zel accumulates a `u64` and casts it to `usize`, so those bits are a large unsigned length and fail only when they run past the buffer.
  - Proto2 `parseFrom` fails the message when a `Dependency` is missing required `path` or `kind`. Zel errors only when `path` is absent and accepts a dependency with no `kind`.
  - Java’s parser skips unknown group wire types 3 and 4. Zel returns an error for those wire types.
  - `parseFrom` retains `kind`, source locations, `rule_label`, `success`, and `contained_package`. Zel keeps dependency paths, in order, and `requires_reduced_classpath_fallback` (default false, any non-zero varint true).

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaCompileOutputs.java`
- Bazel lines: 76
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - `manifestProto()` is required and non-null: `build()` fails if it was never set, and a null setter argument is rejected. Zel stores `manifest_proto` as `Option`; a missing argument, `None`, or a non-File value is absent, the Javac action is still registered, and `--output_manifest_proto` is omitted. The missing manifest is an error only when `gen_class` is set.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaConfiguration.java`
- Bazel lines: 418
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - `--javacopt` (`default_javac_flags` / `default_javac_flags_depset`): Bazel shell-tokenizes each occurrence (the original string is kept if tokenization fails), shell-escapes the tokens, and stores them as one `NAIVE_LINK_ORDER` (`preorder`) nested-set element, then tokenizes that element again for the list. Zel comma-splits the raw value into a stable (`default`) depset. A comma inside one flag becomes another flag; a quoted multi-token flag stays one Starlark string; several flags reverse against each other when `tokenize_javacopts` reverses a multi-element depset. Nesting that depset into the preorder javacopts depset is an order mismatch in Bazel and is accepted here because the child order is `default`.
  - `--jvmopt` (`default_jvm_opts`): Bazel keeps each occurrence as one string. Zel comma-splits, so a comma inside a flag becomes another flag and empty pieces are dropped.
  - `generate_java_deps` is `java_deps || experimental_java_classpath != OFF`. With the default classpath `bazel`, `--java_deps=false` still generates deps. Zel uses only `java_deps` (default true).
  - `multi_release_deploy_jars` is hardcoded true, so `--incompatible_multi_release_deploy_jars=false` is ignored.
  - `auto_create_java_test_deploy_jars` is hardcoded false, so `--experimental_java_test_auto_create_deploy_jar=true` is ignored.
  - `--java_launcher` (configuration field `launcher`) is that label, or null when unset or empty. Zel reads an option named `launcher`, so the flag never applies.
  - `--bytecode_optimizers` must be exactly one mapping; a null label is allowed only for the mnemonic `Proguard`; `--experimental_local_java_optimizations` without an optimizer label is rejected. Zel does not check this. `java_toolchain_bytecode_optimizer` and `local_java_optimization_configuration` are those labels only when local optimizations are enabled, and null otherwise. Zel's late-bound lookup always leaves both unset. `bytecode_optimizer_mnemonic` is absent (default `Proguard`).
  - `--experimental_fix_deps_tool` (default `add_dep`) is what `getFixDepsTool` returns. The compile action always passes `add_dep`.
  - These values are not applied, so only the defaults match: `experimental_add_test_support_to_compile_time_deps` (true; Starlark `add_test_support_to_compile_deps` is absent), `split_bytecode_optimization_pass` (false), `bytecode_optimization_pass_actions` (int 1), `enforce_proguard_file_extension` (false), `experimental_enable_jspecify` (true), `experimental_turbine_annotation_processing` (false).
  - `plugins` is the comma-split raw `--plugin` text, reparsed with `parse_canonical`, not the `LabelListConverter` labels (options-context repository mapping).

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaHeaderCompileAction.java`
- Bazel lines: 667
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - Direct-classpath args use an `UNQUOTED` param file only past the command-line limit, written as Latin-1 internal bytes. Zel always spills that line (`use_always`) and writes UTF-8.
  - The annotation-processing path registers `JavaCompileAction` (`JavacTurbine`, 750 MB): reduced classpath (`BAZEL` / `BAZEL_NO_FALLBACK`, forced `OFF` when a processor is in the reduced-classpath incompatible set), full-classpath fallback, and `.jdeps` dependency insertion. Zel puts the full classpath on the command line with no `--reduce_classpath_mode`, no fallback, and a 250 MB spawn.
  - `LC_CTYPE` is the execution platform’s UTF-8 locale (`en_US.UTF-8` on Darwin, otherwise `C.UTF-8`), merged over the configuration action environment. Zel sets `C.UTF-8` unless a platform string contains `osx` or `macos`, or the binary itself targets macOS.
  - Javac options are tokenized on UTF-16 code units, and a backslash inside double quotes is kept unless the next unit is `\` or `"`. Zel tokenizes Unicode scalars and always drops that backslash.
  - Execution requirements are an insertion-ordered map (rule tags, then `modifiedExecutionInfo` for `Javac` and `Turbine` / `JavacTurbine`; sorted only when a modifier applies). Zel stores a `BTreeMap` with `supports-path-mapping` and, by default, `internal-inline-outputs` only.
  - With header-compilation direct jars, the full compile classpath is kept for `.jdeps` path unmapping, and `afterExecute` rewrites deps when the classpath mode is `BAZEL` or `BAZEL_NO_FALLBACK` (a null Java compile context or an `IOException` skips that). Zel does not.
  - A non-main `--target_label` is `@` plus the main-repo display form. Zel uses `@` plus the canonical form (`@@@repo//pkg:name`) with no repository mapping.
  - Builder defaults that Zel does not share: strict deps `OFF` (Zel uses `ERROR` when the argument is omitted), and the turbine annotation-processing outputs `--gensrc_output`, `--resource_output`, `--output_manifest_proto`, and `additional_outputs`, with header-compiler direct forced off.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaHelper.java`
- Bazel lines: 128
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - `tokenizeJavaOptions` walks Latin-1/UTF-16 code units and splits only on space and tab. Zel’s `shell_tokenize` walks Unicode scalars and splits on `char::is_whitespace` (newlines, vertical tab, form feed, and Unicode spaces such as U+00A0), so `"a\nb"` stays one token in Bazel and becomes two in Zel.
  - Inside double quotes, Bazel keeps a backslash unless the next code unit is `\` or `"`. Zel drops the backslash and keeps only the next scalar, so `"\n"` is `\n` in Bazel and `n` in Zel. A backslash before a non-BMP character keeps the backslash plus the high surrogate in Bazel and drops the backslash in Zel.
  - `detokenizeJavaOptions` has no Rust body. The live helper `/workspace/zel-cursor/third_party/bcr/rules_java/9.1.0/java/common/rules/impl/java_helper.bzl` (`_detokenize_javacopts`) turns an empty collection into a preorder depset of one empty string. Bazel returns an empty `NAIVE_LINK_ORDER` set.
  - `getJavaResourcePath` strips the rule repo with `external/<repo>` or, under sibling repository layout, `../<repo>`, using segment `PathFragment` prefix checks. Zel only strips the string `external/<repo>/` and never `../<repo>`.
  - An explicitly set empty `resource_strip_prefix` is an empty `PathFragment` prefix in Bazel, so the repo-stripped path is returned unchanged. Zel treats `""` as unset and runs the default Java resource-path heuristics.
  - A strip prefix is normalized (`.` / `..`, repeated slashes) and matched by path segment. Zel string-matches `prefix/` after trimming trailing slashes. A resource path equal to the prefix becomes an empty relative path in Bazel. Zel fails the call. A prefix mismatch records an attribute error and still returns the unstripped path in Bazel. Zel returns an error and does not use that path.
  - With no strip prefix, Bazel’s `getJavaPath` has no `third_party/java` case. Zel runs the source-root scan on the segments after `third_party/java`, so `third_party/java/src/main/java/com/foo/A.java` stays `src/main/java/com/foo/A.java` in Bazel and becomes `com/foo/A.java` in Zel.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaOptions.java`
- Bazel lines: 440
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - `--bytecode_optimization_pass_actions` is a signed 32-bit `Integer.decode` value (decimal, leading-zero octal, `0x`/`#` hex). The configuration value is a trimmed decimal `i64`. `010` becomes 10 rather than 8. `0x10` and `#10` fail that conversion and the flag stays at the default `1`.
  - `--bytecode_optimizers` (`LabelMapConverter`) trims each entry and stores an empty value as a null label. The configuration map does not trim, so whitespace after a comma is part of the key, and `name=` is stored as `name=` rather than `name=null`.
  - `--javacopt`, `--host_javacopt`, `--jvmopt`, and `--host_jvmopt` are repeatable plain strings: each occurrence is one element, commas and empty strings included. The parser types them as comma-separated lists, so the converted value splits on `,` and an empty occurrence adds nothing. The configuration list is taken from the raw occurrences and does not split. The Java fragment then reads a comma-joined options map: `javacopt` is in that map, so embedded commas are split and empty entries are dropped; `jvmopt` is not copied into that map, so `default_jvm_opts` stays empty.
  - `--plugin` is a repeatable comma-separated label list (empty pieces omitted, each piece parsed as a label). The fragment keeps the raw comma-split strings, so a value such as `pkg:name` is not turned into the parsed label `//pkg:name`.
  - `--incompatible_multi_release_deploy_jars=false` and `--experimental_java_test_auto_create_deploy_jar=true` parse with the declared defaults (`true` and `false`), but the Java fragment always reports `multi_release_deploy_jars` as true and `auto_create_java_test_deploy_jars` as false.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaOutput.java`
- Bazel lines: 269
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No `JavaOutput` record, `wrapSequence`, `fromStarlarkJavaOutput`, `nullIfNone`, or builder (`addSourceJar` null-skip, stable-order source jars, `fromJavaCompileOutputs` jdeps flag, `getSrcJar` single-element-or-null).

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaPackageConfigurationProvider.java`
- Bazel lines: 140
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No provider that wraps `JavaPackageConfigurationInfo`, matches a label’s package against `package_specs`, or exposes tokenized `javac_opts` and the `data` depset.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaPluginInfo.java`
- Bazel lines: 298
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - `java_plugins_flag_alias` rebuilds `JavaPluginInfo` with empty `java_outputs` and transitive depsets of `processor_classes`, `processor_jars`, and `processor_data`. `wrap`, `fromStarlarkJavaInfo`, `hasProcessors`, `isEmpty`, `JavaPluginData.create`/`empty`, and the `//java/private:java_info.bzl` `RulesJavaProvider` key have no implementation.
  - Those merged depsets use stable (`default`) order. `JavaPluginData.merge` builds `NAIVE_LINK_ORDER` (`preorder`). A direct-empty flatten follows the children either way; a preorder set is incompatible with postorder and topological parents, while a stable set is compatible with every order. A postorder or topological child fails Java `addTransitive` and is accepted under the stable parent.
  - Empty plugin data is still stored as a transitive child. `mergeWithoutJavaOutputs` omits a side whose `plugins()` or `apiGeneratingPlugins()` is empty, and `addTransitive` drops an empty subset after the order check.
  - Pairwise `mergeWithoutJavaOutputs(a, b)` returns `b` unchanged, including `b`'s `java_outputs`, when `a.plugins()` is empty, even if `a.apiGeneratingPlugins()` is not (`isEmpty` looks only at `plugins`). The alias always allocates a new provider, always clears `java_outputs`, and still merges `api_generating_plugins`.
  - A missing `plugins` / `api_generating_plugins` value, or a `processor_*` value that is not a depset, is skipped. `wrap` and `JavaPluginData.wrap` raise `RuleErrorException` (`got element of type …, want JavaPluginInfo`, or an unexpected nested type); `Depset.cast` requires `String` processor classes and `Artifact` jars and data.
  - `empty(providerType)` returns the `EMPTY` singleton for `@@rules_java+//java/private:java_info.bzl` unless the provider is exactly `RULES_JAVA_PROVIDER`. An all-empty merge in Zel still inserts a fresh `@@rules_java+` provider.
  - `hasProcessors` is true only when `plugins().processorClasses()` is non-empty. `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs` `plugin_data` turns a missing value into empty lists and drops non-string processor-class elements.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaPluginsFlagAliasRule.java`
- Bazel lines: 107
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - Plugins come from splitting the `--plugin` option and loading each label in the exec configuration. They are not the late-bound `:java_plugins` prerequisites, so apparent repository names are parsed with an empty repo mapping, and visibility and `testonly` are not checked on that edge.
  - Bazel uses prerequisites that provide `@@rules_java+//java/private:java_info.bzl` `JavaPluginInfo`, and only if that list is empty the ones that provide `//java/private:java_info.bzl`. Zel requires every plugin to have the `@@rules_java+` provider.
  - Bazel returns the merge as both of those provider types, with empty Java outputs. Zel returns only `@@rules_java+//java/private:java_info.bzl%JavaPluginInfo`.
  - Merged `processor_classes`, `processor_jars`, and `processor_data` use default depset order. Bazel merges them in naive link order (`preorder`).

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaRuntimeInfo.java`
- Bazel lines: 162
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No provider that exposes the Java runtime’s `java` binary, files, and hermetic flag.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaSemantics.java`
- Bazel lines: 69
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - `utf8Environment` sets `LC_CTYPE` from the execution platform’s `@platforms//os` constraint: `osx` or `macos` yields `en_US.UTF-8`, any other known or unknown OS yields `C.UTF-8`, and a missing OS constraint falls back to the host OS. Zel sets `en_US.UTF-8` when any target `--platforms` label contains the substring `osx` or `macos`, or when the zel binary itself was built for macOS, and otherwise sets `C.UTF-8`.
  - The default resource path strips the first `src/<segment>/resources` prefix, then uses the Java source root (`java`, `javatests`, `src`, `testsrc`, including `src/(main|test)/(java|resources)`). Zel also, for any path whose text starts with `third_party/java` (including `third_party/javascript/...`), searches for a source root after that prefix. `third_party/java/src/main/java/com/Foo.txt` stays `src/main/java/com/Foo.txt` in Bazel and becomes `com/Foo.txt` in zel.
  - An absolute path that misses the `src/*/resources` prefix makes Bazel’s Java-root lookup throw. Zel still returns a relative string.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaStarlarkCommon.java`
- Bazel lines: 437
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - `_check_java_toolchain_is_declared_on_rule` always succeeds. Bazel errors when the rule did not declare the Java toolchain, and both compile entry points call that check before registering an action.
  - `target_kind` returns the stored rule class. An alias stays `"alias"` (Bazel follows the alias to the actual rule class). Source and generated files return `"source file"` or `"generated file"` (Bazel returns `""`).
  - `collect_native_deps_dirs` keeps `.dll.a` as an error, rejects `.pyd`, `.wasm`, `.tgt`, `.vpi`, and versioned `.dylib` names, and accepts any name containing `.so.` (Bazel skips interface libraries including `.dll.a`, accepts those shared suffixes, and accepts only versioned `.so`/`.dylib` names matching a version suffix). Directory strings come from the root-relative parent, not the runfiles parent.
  - `get_runtime_classpath_for_archive` only treats file elements as exclusions. A non-empty exclusion depset with no files is ignored and the original depset is returned. Non-file classpath elements are kept. Bazel rebuilds a stable-order depset whenever the exclusion depset is non-empty and rejects a classpath that is not files.
  - `expand_java_opts` reads only `srcs`, `deps`, `data`, `tools`, and `resources`, and still includes `data` when `exec_paths` is true. Bazel also adds rule outputs and `implementation_deps`, omits `data` for exec paths, and uses the executable file of a `deps`/`tools` target when it has one. A missing or non-list attribute becomes an empty list. Expansion or tokenization failure aborts the call. Bazel requires a string-list attribute, reports an attribute error, and on a make-variable failure returns the original string.
  - `_google_legacy_api_enabled`, `_incompatible_java_info_merge_runtime_module_flags`, and `incompatible_disable_non_executable_java_binary` are fixed `false` (`/workspace/zel-cursor/crates/zel-builtins/src/natives.rs`). Bazel reads those semantics flags from the thread. The private-API allowlist checked on the first two, and on `target_kind`, `collect_native_deps_dirs`, `get_runtime_classpath_for_archive`, and `check_provider_instances`, is not enforced.
  - `tokenize_javacopts` is not implemented.
  - `strict_deps_mode` is uppercased and passed through. Bazel maps `DEFAULT` to `ERROR` and rejects any value other than `OFF`, `ERROR`, `WARN`, and `DEFAULT`.
  - `create_header_compilation_action` drops a boot classpath that has only auxiliary jars or system inputs, ignores `compile_time_java_deps`, and, when annotation processors force the non-direct compiler, emits a plain `--classpath` line instead of a reduced-classpath compile action. The direct Turbine action does not keep the full classpath for path mapping or downstream `.jdeps` insertion.
  - `create_compilation_action` ignores `enable_jspecify`, so JSpecify implicit deps, the processor, and javacopts are never added. It also skips turbine annotation processing, coverage path files, local bytecode optimization, and per-package toolchain data inputs. `--experimental_fix_deps_tool` is always `add_dep`. `LC_CTYPE` follows the target platform string or the host OS, not the execution platform. A `resource_strip_prefix` that does not match the resource fails the call. Bazel records an attribute error and keeps the unstripped path, and it strips only when that attribute was set explicitly.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaTargetAttributes.java`
- Bazel lines: 506
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - Source files kept for compilation are filenames ending in `.java` (ASCII case-insensitive on Windows), deduplicated in first-seen order. Zel passes every `sources` / `source_files` artifact through to the header and javac actions, including other extensions and duplicates.
  - Resources are stored one artifact per exec path, and a later add with the same path replaces the earlier artifact. Zel packages every resource, so two artifacts with the same jar path are both written into the resource jar.
  - `build()` places the direct jars in front of the compile-time classpath in naive link order, unless the classpath was supplied already prepended; after that, adding direct jars throws and the given classpath is kept. Zel never joins `direct_jars` onto `compile_time_classpath`.
  - `appendAdditionalTransitiveClassPathEntries` returns a copy whose compile-time classpath includes another nested set while the direct jars stay unchanged. Zel has no such update.
  - A single runtime classpath entry whose filename does not end in `.jar` throws; a transitive runtime-classpath add does not check names. Zel does not validate runtime classpath jar names.
  - Additional outputs are a set, so duplicate artifacts collapse. Zel appends `additional_outputs` as given.
  - A second plugin is merged without Java outputs (processor classes, processor classpath, and data only). The boot classpath must be non-empty and can be set once, a non-empty source path can be set once, and every mutator throws after `build()`. Zel does not merge plugins in the compile action and does not enforce those constraints.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaToolchainProvider.java`
- Bazel lines: 344
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - Bazel resolves the toolchain from the `$java_toolchain_type` prerequisite’s `ToolchainInfo.java` and accepts `JavaToolchainInfo` from both `//java/common/rules:java_toolchain.bzl` and `@@rules_java+//java/common/rules:java_toolchain.bzl`. A bad read reports “There was an error reading the Java toolchain: …”, then “The selected Java toolchain is not a JavaToolchainProvider”, and returns null; a wrong provider fails with “expected JavaToolchainInfo” or “want 'JavaToolchainInfo'”. Zel takes a caller-supplied `java_toolchain` value, errors with “missing java_toolchain” when it is absent, and does not check the provider key.
  - Equality is label-only for `hashCode` and full underlying struct for `equals`. Zel has no toolchain-provider identity.
  - `None` is null for the header compilers, Android linter, JSpecify info, bytecode optimizer, and timezone data. Other getters fail the rule when the field is missing or the wrong type (`label`, bootclasspath, tools, JavaBuilder, builtin and reduced-classpath processor sets, SingleJar, GenClass, ijar, `jvm_opt`, the four worker booleans, package configuration, local optimization config, JaCoCo runner, Java runtime). Zel treats missing `tools`, `jvm_opt`, processor sets, and worker flags as empty or false, and never reads `label`, ijar, JaCoCo, Android lint, JSpecify, bytecode optimizer, timezone data, package configuration, or local optimization config.
  - `JspecifyInfo` parses a struct into a non-null processor, implicit deps, javacopts, and package specs (`None` is absent; any other type is “expected JspecifyInfo”) and matches a label when any package specification contains that package. Zel does not parse or match it.
  - Bootclasspath, Java runtime, and toolchain tools are wrapped as `BootClassPathInfo`, `JavaRuntimeInfo`, and `JavaToolchainTool`. Zel reads the raw Starlark fields and only fails later if a specific action needs a missing tool. Worker flags must be booleans; Zel uses truthiness and omits the execution requirement when the field is missing or false.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaToolchainTool.java`
- Bazel lines: 146
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - `addInputs` adds `data` and the executable, plus the Java runtime `files` when the executable extension is `jar`. It does not add the tool's runfiles. `java_tool_command` also adds the `FilesToRunProvider` runfiles (artifacts, symlinks, and root symlinks).
  - `withAdditionalJvmFlags` appends flags onto `jvm_opts` in a stable-order nested set, so a flag already present is not repeated. Those flags then sit after the toolchain `jvm_opt` values and before `-jar` for a jar, or after the executable for a non-jar. Zel only splices `javabuilder_jvm_flags` on the javac action, inserting them at the first argv entry equal to `-jar` (or at the end if there is none), and repeats flags that were already present.
  - `fromStarlark` requires `tool` to be a `FilesToRunProvider` and `data` / `jvm_opts` to be depsets of artifacts and strings (or `None`), and fails the rule on a bad type. Zel drops non-artifact `data` entries and non-string `jvm_opts` entries, and accepts lists.

### `src/main/java/com/google/devtools/build/lib/rules/java/JavaUtil.java`
- Bazel lines: 129
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - `getJavaPath` rejects absolute paths with `IllegalArgumentException`. `java_path` splits on `/` and still returns the suffix after a source root (`/java/com/Foo.java` → `com/Foo.java`; a Windows drive prefix is not treated as absolute).
  - `java_path` special-cases strings starting with `third_party/java` (so also `third_party/javatests/...`): it searches only after the first two segments and, if that finds a root, uses it. Bazel always uses the first `java`/`javatests`/`src`/`testsrc` in the whole path and does not scan for a nested root when that hit is `java` or `javatests` past segment 0. `third_party/java/src/main/java/com/Foo.java` is `src/main/java/com/Foo.java` in Bazel and `com/Foo.java` in zel.
  - Bazel normalizes the `PathFragment` first (collapse `.`, `..`, repeated `/`, and a trailing `/`). `java_path` splits the raw string, so those forms change the segments and the returned path.

### `src/main/java/com/google/devtools/build/lib/rules/java/ResourceJarActionBuilder.java`
- Bazel lines: 190
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - Bazel spills the singlejar arguments to a shell-quoted `@params` file once the argument length exceeds `--min_param_file_size` (default 32768, minus a 512-byte estimate). Zel spills only above a fixed 131072-byte threshold, so command lines between those sizes stay inline.
  - With `--incompatible_allow_tags_propagation` (default true), Bazel adds the target's legal execution-requirement tags (`no-*`, `requires-*`, `block-*`, `supports-*`, `disable-*`, `local`, `cpu:*`) to the action, which already has `supports-path-mapping=1`. A tag equal to `supports-path-mapping` aborts registration. Zel records only `supports-path-mapping=1`.
  - Bazel runs the action in the passed exec group: the Java toolchain group when `--incompatible_auto_exec_groups` is set, otherwise `default-exec-group`. Zel never sets an exec group, so the action stays on the rule's default execution platform.
  - Bazel refuses to register the action when the toolchain has no Java runtime. Zel registers it whenever `single_jar` is a `FilesToRunProvider`.
  - `setTranslations` adds those artifacts as inputs and as `--resources` entries under the default Java resource path (including when the resources map is empty, and after any explicit resource paths). Zel has no translations input or `--resources` entries for them. The only in-tree caller leaves translations empty.

### `src/main/java/com/google/devtools/build/lib/rules/java/StarlarkInfoWrapper.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No wrapper that reads a Java Starlark info struct and returns null for a missing or `None` field.

### `src/main/java/com/google/devtools/build/lib/rules/objc/AppleStarlarkCommon.java`
- Bazel lines: 58
- Zel counterpart: `/workspace/zel-cursor/crates/zel-builtins/src/natives.rs`
- Verdict: partial
- Differences:
  - `dotted_version` accepts any string that starts with an ASCII digit and otherwise returns the raw string. Bazel requires every component to match `\d+([a-z0-9]*?)?(\d+)?` (case-insensitive), stops at a trailing descriptive component matching `[a-z]\w*`, and rejects the rest, including `1.`, `1..2`, and a component whose leading integer does not fit in a Java `int`. The error text also differs.
  - A missing argument or `None` returns `None`, and an existing dotted-version value is returned unchanged. Bazel requires a string.
  - Ordering treats the non-digit tail of a component as one string, so `1.0beta2` > `1.0beta10`. Bazel splits that tail into an alpha sequence plus a trailing integer, so `1.0beta2` < `1.0beta10`. A descriptive suffix such as `experimental` is dropped from Bazel comparison (`1.2.experimental` equals `1.2`) and is compared in Zel.
  - `==` is raw-string identity, so `1.2` != `1.2.0` and `1.2.experimental` != `1.2`. Bazel equality follows `compareTo` after dropping trailing zero components and descriptive suffixes, so both pairs are equal. Zel `<` and `>` still treat a missing component as `0`.
  - The native `platform` struct (the `ios_device`, `ios_simulator`, `macos`, `tvos_*`, `visionos_*`, `watchos_*`, and `catalyst` values) is not exposed on Zel’s internal `apple_common`.

### `src/main/java/com/google/devtools/build/lib/rules/objc/CompilationSupport.java`
- Bazel lines: 73
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No helper that registers the ObjC compile and archive actions from a compilation context.

### `src/main/java/com/google/devtools/build/lib/rules/objc/J2ObjcCommandLineOptions.java`
- Bazel lines: 53
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - `--j2objc_translation_flags` is a repeatable comma-separated list that preserves empty entries (`a,,b` and a trailing comma stay as separate elements). Zel splits each occurrence on commas and discards empty pieces.
  - `--j2objc_dead_code_report` stores any syntactically valid label: a value without a leading `@` or `/` is an absolute `//` label, and the target does not have to exist. Zel trims the value, resolves `:name` and other non-`@`/`//` values against the client-relative package, and leaves the option at its null default when that package or target is missing.

### `src/main/java/com/google/devtools/build/lib/rules/objc/J2ObjcConfiguration.java`
- Bazel lines: 119
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No fragment builds the j2objc invocation flags as `-g:relative`, then the `--j2objc_translation_flags` values, then always-on `-encoding`, `UTF-8`, `--doc-comments`, `-XcombineJars`, and `-XDinjectLogSites=true`.
  - No `translation_flags` value exposes that list, and no `dead_code_report` configuration field returns the `--j2objc_dead_code_report` label, or none when unset.
  - No invalid-options check reports an error when any composed flag equals `--prefixes`, `--prefix`, or `-x`, with the message `J2Objc translation flags: <comma-joined flags> not supported. Unsupported flags are: --prefixes,--prefix,-x`.

### `src/main/java/com/google/devtools/build/lib/rules/objc/ObjcCommandLineOptions.java`
- Bazel lines: 174
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - All 13 flags match Bazel on name, type, default, converter, effect tags, metadata tags, and `allowMultiple`. `--experimental_objc_fastbuild_options` still diverges: the options parser leaves `CommaSeparatedOptionListConverter` as one string, and configuration conversion splits on commas while dropping empty segments, so `-O0,,-g` becomes `[-O0, -g]` instead of `[-O0, , -g]`.

### `src/main/java/com/google/devtools/build/lib/rules/objc/ObjcConfiguration.java`
- Bazel lines: 202
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - `copts_for_current_compilation_mode` is always empty. Bazel returns `--experimental_objc_fastbuild_options` in fastbuild (default `-O0`, `-DDEBUG=1`); in dbg it adds `-O0`, `-DDEBUG=1`, `-fstack-protector`, `-fstack-protector-all`, `-g` unless `--incompatible_avoid_hardcoded_objc_compilation_flags` (default true), then adds `-D_GLIBCXX_DEBUG`, `-D_GLIBCXX_DEBUG_PEDANTIC`, `-D_GLIBCPP_CONCEPT_CHECKS` when `--objc_debug_with_GLIBCXX`; in opt it returns `-Os`, `-DNDEBUG=1`, `-Wno-unused-variable`, `-Winit-self`, `-Wno-extra` unless that incompatible flag is set. Any other mode throws.
  - `target_should_alwayslink` ignores the rule context and always returns `--incompatible_objc_alwayslink_by_default`. Bazel returns an explicitly set `alwayslink` boolean, otherwise that default, and fails when called outside the builtin allowlist.
  - These fields are absent: `ios_simulator_device`; `ios_simulator_version` (unwrapped dotted version of `--ios_simulator_version`, null if unset); `run_memleaks` (`--ios_memleaks`, default false); `signing_certificate_name` (null if unset); `uses_device_debug_entitlements` (`--device_debug_entitlements`, default true, and only when the compilation mode is not opt).

### `src/main/java/com/google/devtools/build/lib/rules/platform/ConstraintSetting.java`
- Bazel lines: 121
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/config.rs`
- Verdict: partial
- Differences:
  - A `default_constraint_value` in another package is accepted. Bazel rejects it.
  - A `default_constraint_value` that is not a target in the setting's package is accepted. Bazel rejects it by looking up the already-loaded package, with no dependency edge.
  - `refines_constraint_value` is ignored: the prerequisite is not required to provide `ConstraintValueInfo`, and the alias-resolved refined value is not stored on `ConstraintSettingInfo`.

### `src/main/java/com/google/devtools/build/lib/rules/platform/ConstraintSettingRule.java`
- Bazel lines: 113
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_class.rs`
- Verdict: partial
- Differences:
  - `refines_constraint_value` is a nonconfigurable label, but any label is accepted. Bazel allows only a `constraint_value` rule, rejects source files, requires `ConstraintValueInfo`, and configures that dependency. Zel stores the label and does not analyze it, so those checks and the visibility and testonly checks on that edge do not run.

### `src/main/java/com/google/devtools/build/lib/rules/platform/ConstraintValue.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/config.rs`
- Verdict: partial
- Differences:
  - Zel fails analysis when `constraint_setting` is missing or the dependency has no `ConstraintSettingInfo`. This factory still builds the target and passes that setting through, including a null setting when the provider is absent.
  - Zel reanalyzes `constraint_setting` under a new core-only configuration (`cpu` taken from the current configuration, compilation mode `fastbuild`). This factory reads the prerequisite the analysis engine already configured for that attribute.
  - Both attach `ConstraintValueInfo` (that setting plus this target’s label) and empty file and runfiles data.

### `src/main/java/com/google/devtools/build/lib/rules/platform/ConstraintValueRule.java`
- Bazel lines: 94
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_defs_gen.rs`
- Verdict: partial
- Differences:
  - `constraint_setting` accepts a `constraint_setting` rule or any target that provides `ConstraintSettingInfo`, and it allows no files. Zel only requires `ConstraintSettingInfo`. A `constraint_setting` without that provider is rejected. An existing source file, or a missing prerequisite whose name has no dot, is materialized as a source file and then fails the provider check instead of being rejected as a disallowed file or a missing rule.
  - `package_metadata`, `compatible_with`, and `restricted_to` are removed from the rule, so package defaults for them do not apply. Zel still writes those package defaults onto the target. `package_metadata` is then visible to query attribute output.

### `src/main/java/com/google/devtools/build/lib/rules/platform/Platform.java`
- Bazel lines: 184
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/config.rs`
- Verdict: partial
- Differences:
  - `parents` must be a single platform. Zel merges every parent, with later parents overriding earlier constraints and exec properties.
  - Inherited constraints are flattened into the provider. An unset setting does not fall back to that setting’s default constraint value, and the parent platform is not retained.
  - `remote_execution_properties` is ignored: no parent inheritance, no `{PARENT_REMOTE_EXECUTION_PROPERTIES}` substitution, and no error when it is combined with `exec_properties` on this platform or its parent.
  - An empty `exec_properties` value is stored. Bazel uses an empty value to remove that key from the parent.
  - `flags` are ignored, including prepending the parent’s flags.
  - `required_settings` are ignored.
  - `check_toolchain_types` and `allowed_toolchain_types` are ignored.
  - `missing_toolchain_error` is ignored.
  - A direct non-default `constraint_value` whose setting refines another value is not rejected when the platform omits that refined value or sets a conflicting one.

### `src/main/java/com/google/devtools/build/lib/rules/platform/PlatformCommon.java`
- Bazel lines: 52
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - `platform_common` still exposes `PlatformInfo`, `ConstraintSettingInfo`, `ConstraintValueInfo`, `TemplateVariableInfo`, and `ToolchainInfo` as provider keys.
  - Bazel `PlatformInfo`, `ConstraintSettingInfo`, and `ConstraintValueInfo` are not constructors. Zel treats every provider as callable and builds an instance from whatever keywords are passed.
  - `TemplateVariableInfo` takes one `vars` argument, positional or named: `None` or a `dict` of string to string, exposed as `.variables`. Zel drops a positional dict and stores keyword names as fields, so `TemplateVariableInfo({'FOO': 'bar'})` does not set `variables`, and non-string values are accepted.
  - `ToolchainInfo(**kwargs)` stores those keywords as fields in both. Extra positional arguments are rejected in Bazel and ignored in Zel.
  - `PlatformInfo.constraints` is a constraint collection: indexing and `in` take a `ConstraintSettingInfo`, and `get`, `has`, `has_constraint_value`, and `constraint_settings` follow parents and the setting’s default. Zel stores a flat dict of setting label to value label and also exposes `exec_properties`, which the Bazel provider does not.
  - `ConstraintSettingInfo.default_constraint_value` is a `ConstraintValueInfo` or `None`, with a separate `has_default_constraint_value` field. Zel stores the default as a label or `None` and has no `has_default_constraint_value` field.
  - `label`, `constraints`, `constraint`, and `default_constraint_value` are readable only with `--experimental_platforms_api` (`has_default_constraint_value` is not gated). Zel reads those fields with the flag off.

### `src/main/java/com/google/devtools/build/lib/rules/platform/PlatformRule.java`
- Bazel lines: 414
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_class.rs`
- Verdict: partial
- Differences:
  - `required_settings` accepts any label. Bazel allows only `config_setting` rules and rejects files, and those settings must match before the platform is an execution platform. Zel never reads the attribute, so it does not filter execution platforms.
  - `allowed_toolchain_types` is a non-dependency label list. Zel still loads those labels as rule dependencies, so a missing target fails loading.
  - `constraint_values` and `parents` do not reject files on the attribute. Analysis still rejects a dependency that does not provide `ConstraintValueInfo` or `PlatformInfo`.

### `src/main/java/com/google/devtools/build/lib/rules/platform/Toolchain.java`
- Bazel lines: 110
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/config.rs`
- Verdict: partial
- Differences:
  - `use_target_platform_constraints` is ignored. Bazel fails the target when it is true and `exec_compatible_with` or `target_compatible_with` is non-empty. When it is true and both lists are empty, the toolchain matches every target platform and its execution constraints are the target platform's constraints. Zel always keeps the two lists, so the flag does not change selection and the illegal combination still analyzes.
  - Two different values of the same constraint setting on `exec_compatible_with` or `target_compatible_with` fail the Bazel target, with an attribute error on each list that conflicts. Zel keeps every label and does not fail analysis; those toolchains only fail to match during resolution.
  - Bazel records `ToolchainTypeInfo` from the configured `toolchain_type` prerequisite. Zel stores the attribute label and does not require `ToolchainTypeInfo`, so an alias or a target that is not a toolchain type is recorded and compared as that label.
  - Exec and target constraints are `ConstraintValueInfo` values, and prerequisites that lack that provider are dropped. `target_settings` are the `ConfigMatchingProvider` results from analyzing those prerequisites with the toolchain. Zel stores the raw labels, re-checks settings later in the requesting configuration, and keeps a constraint label that has no `ConstraintValueInfo`, which then fails to match.

### `src/main/java/com/google/devtools/build/lib/rules/platform/ToolchainRule.java`
- Bazel lines: 130
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_class.rs`
- Verdict: partial
- Differences:
  - `toolchain_type` must be a `toolchain_type` or provide `ToolchainTypeInfo`. `exec_compatible_with` and `target_compatible_with` must provide `ConstraintValueInfo`. `target_settings` must be a `config_setting`. All four reject files. Zel records the labels and does not configure them or check rule class, provider, or file type, so invalid targets are accepted.
  - Those four attributes are configured prerequisites. `toolchain` is a configurable non-dependency label and is not. Zel does not configure the four prerequisites when analyzing the toolchain target, and it still records `$target_platform` and `$exec_platform` dependencies even though this rule disables toolchain resolution and is exempt from constraint checking.

### `src/main/java/com/google/devtools/build/lib/rules/proto/BazelProtoCommon.java`
- Bazel lines: 67
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - `incompatible_enable_proto_toolchain_resolution` always returns true (`loader_host.rs`). Bazel returns the configured `--incompatible_enable_proto_toolchain_resolution` semantics flag (default true).
  - `external_proto_infos` is absent. Bazel returns an immutable list of three exported `ProtoInfo` providers for `@com_google_protobuf//bazel/private:proto_info.bzl`, `@com_google_protobuf+//bazel/private:proto_info.bzl`, and `@@protobuf+//bazel/private:proto_info.bzl`.
  - Both methods fail unless the caller is in the builtins repository (empty extra allowlist). Zel does not enforce that.

### `src/main/java/com/google/devtools/build/lib/rules/proto/ProtoConfiguration.java`
- Bazel lines: 254
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - `experimental_proto_descriptorsets_include_source_info()` is absent. Bazel returns the boolean flag (default false) and rejects callers outside the default allowlist.
  - `strict_proto_deps()` and `strict_public_imports()` are allowlisted methods that return the enum name (`ERROR`, `OFF`, `WARN`, `STRICT`, `DEFAULT`). Zel exposes lowercase strings (`error`, `off`, or the raw flag text) as struct fields, with no allowlist check. `strict_public_imports` also accepts values the strict-deps converter rejects.
  - `experimental_protoc_opts` splits each `--protocopt` value on commas and drops empty pieces. Bazel keeps every repeated flag as one string, including commas and empty values.
  - Header and source suffix lists are comma-separated sets (sorted, duplicates removed, empty entries kept). Zel splits on commas, drops empty entries, and if the option is absent—including an explicit empty value that is not stored—substitutes `.pb.h` and `.pb.cc`.
  - Unset `proto_toolchain_for_j2objc` resolves to no label. Bazel uses `@bazel_tools//tools/j2objc:j2objc_proto_toolchain`. An empty `--proto_toolchain_for_java` or `--proto_toolchain_for_cc` is null in Bazel and is replaced by the default toolchain label in Zel. `configuration_field` also accepts `proto_toolchain_for_javalite`, which Bazel does not.

### `src/main/java/com/google/devtools/build/lib/rules/proto/ProtoConstants.java`
- Bazel lines: 73
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - Zel does not publish the three external `ProtoInfo` provider keys Bazel loads with `keyForBuild`: `@com_google_protobuf//bazel/private:proto_info.bzl`, `@com_google_protobuf+//bazel/private:proto_info.bzl`, and `@@protobuf+//bazel/private:proto_info.bzl`. `proto_common` is an empty struct, so `external_proto_infos` is absent.
  - Zel does not register a `ProtoLangToolchainInfo` provider key for `@@com_google_protobuf+//bazel/common:proto_lang_toolchain_info.bzl`.
  - Unset `proto_compiler`, `proto_toolchain_for_java`, `proto_toolchain_for_java_lite` / `proto_toolchain_for_javalite`, and `proto_toolchain_for_cc` still resolve to `@bazel_tools//tools/proto:protoc`, `:java_toolchain`, `:javalite_toolchain`, and `:cc_toolchain`.
  - Unset `proto_toolchain_for_j2objc` resolves to no label. Bazel uses `@bazel_tools//tools/j2objc:j2objc_proto_toolchain`.
  - Zel never builds the strict-deps compiler argument `--direct_dependencies_violation_msg=%s is imported, but <proto_library label> doesn't directly depend on a proto_library that 'srcs' it.`

### `src/main/java/com/google/devtools/build/lib/rules/proto/ProtoInfo.java`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No wrapper reads a Starlark `ProtoInfo` and exposes `transitive_sources` (cached `NestedSet` of artifacts), `direct_sources`, `transitive_proto_path`, `check_deps_sources`, `direct_descriptor_set`, or `transitive_descriptor_sets`, and it does not turn field type or evaluation failures into rule errors.

### `src/main/java/com/google/devtools/build/lib/rules/python/PrecompilePythonMode.java`
- Bazel lines: 33
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No enum for Python precompile mode (`none`, `all`, `auto`).

### `src/main/java/com/google/devtools/build/lib/rules/python/PyBuiltins.java`
- Bazel lines: 420
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/py_native.rs`
- Verdict: partial
- Differences:
  - `regex_match` accepts only `(?U)\w+` and `\w+`, and treats both as one or more Unicode alphanumeric or `_` characters. Bazel uses Java `String.matches` (the pattern is fully anchored). Other patterns fail in Zel, and ASCII `\w` is not distinguished from `(?U)\w`.
  - `get_current_os_name` is fixed at compile time to `osx`, `windows`, or `linux`. Bazel reports the host OS canonical name (`freebsd`, `openbsd`, and `unknown` included) and honors `blaze.os`.
  - `expand_location_and_make_variables` builds its location map only from the `targets` argument's default files. Bazel also expands `srcs`, `deps`, `implementation_deps`, `data`, `tools`, and rule outputs, prefers a `FilesToRun` executable over the files-to-build, and records alias labels. `$(location)` of an external file becomes `external/<repo>/...` rather than the runfiles path `../<repo>/...`. Expanded paths are not shell-escaped. Make variables are substituted once and not rescanned. A single-character `$x` form is accepted. An expansion error fails the call. Bazel reports an attribute error and returns the original expression.
  - `create_repo_mapping_manifest` takes source repositories from the target repository plus repositories inferred from runfiles path prefixes. Bazel uses every transitively loaded package. Repositories that appear in runfiles come from the first path segment. Bazel uses artifact owner repositories and the first segment of root symlinks. Compact grouping compares mapping contents. Bazel merges only mappings that are the same instance.
  - `merge_runfiles_with_generated_inits_empty_files_supplier` still rejects an empty runfiles object, but it inserts missing `__init__.py` paths immediately. Bazel attaches an empty-file supplier that runs when the final runfiles manifest is built, so later merges that add `.py`, `.so`, or `.pyc` files still gain parent `__init__.py` entries.
  - `declare_constant_metadata_file` ignores `root` and returns an ordinary derived file under the rule output root. Bazel declares the file under the given artifact root and marks it constant-metadata so its digest always reports unchanged.
  - `create_sources_only_manifest` writes one sorted runfiles path per line and does not add the main-workspace `.runfile` placeholder when every entry is external. It does not report nested runfiles trees or prefix conflicts, and a duplicate path keeps the first entry.
  - `copy_without_caching` registers a `/bin/cp` spawn tagged `no-cache`. Bazel copies the input bytes with an action that always executes and is volatile, so a constant-metadata source is recopied even when its reported digest is unchanged. Zel's local action cache can still skip that spawn.
  - `are_action_listeners_enabled` is always false. Bazel is true when the configuration's action-listener list is non-empty.
  - `add_py_extra_pseudo_action` returns `None` and does not register the Python extra-action pseudo action (rule `srcs`, transitive Python sources, and a dummy output).

### `src/main/java/com/google/devtools/build/lib/rules/python/PyCommon.java`
- Bazel lines: 103
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/py_native.rs`
- Verdict: partial
- Differences:
  - Bazel registers a non-executable pseudo action with mnemonic `Python`. Its inputs are the rule’s unfiltered `srcs` artifacts plus the given transitive dependency Python sources, in stable order. Its only output is a genfiles dummy artifact named `<target name>.extra_action_dummy`. Zel’s `add_py_extra_pseudo_action` returns without registering an action.
  - The action’s extra-action info is `PythonInfo`: `source_file` holds the exec paths of those unfiltered `srcs`, and `dep_file` holds the exec paths of the dependency sources only. The rule’s own `srcs` are not repeated as dep files. Zel records no source or dependency paths for extra actions.
  - The action key is the fixed UUID `8d720129-bc1a-481f-8c4c-dbe11dcef319` plus the `PythonInfo` bytes. Executing the action fails instead of writing the dummy output. Zel has no corresponding action.

### `src/main/java/com/google/devtools/build/lib/rules/python/PythonConfiguration.java`
- Bazel lines: 163
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/fragments.rs`
- Verdict: partial
- Differences:
  - Bazel omits `ctx.fragments.py` when `--incompatible_remove_ctx_py_fragment` is true (the default) and both `build_python_zip` and `incompatible_default_to_explicit_init_py` are present in the command-line flag aliases. Zel always supplies the `py` struct for a rule that declares that fragment.
  - `build_python_zip` is TriState: yes is true, no is false, and auto is true only when the process host OS is Windows. Zel reads `cfg.options["build_python_zip"]` and treats any value other than true/yes/1 or false/no/0 as the Windows default. Command-line Python flags are stored on the native option map, not in `cfg.options`, so an explicit `--build_python_zip=yes` or `no` leaves the field at the auto default. That auto check is the compile-time target (`cfg!(windows)`), not the runtime host OS.
  - `default_to_explicit_init_py` follows `--incompatible_default_to_explicit_init_py`. Zel only sees that flag if a transition wrote it into `cfg.options`; otherwise the field stays false.
  - The Starlark fields `use_toolchains` (always true), `disallow_native_rules` (`--incompatible_python_disallow_native_rules`), `native_rules_allowlist` (`--python_native_rules_allowlist`, a label or None, also a configuration field), and `include_label_in_linkstamp` (`--experimental_py_binaries_include_label`) are absent.
  - A late-bound default for `py.native_rules_allowlist` looks up an option named `native_rules_allowlist` on `cfg.options`, so it stays unset instead of returning `--python_native_rules_allowlist`.

### `src/main/java/com/google/devtools/build/lib/rules/python/PythonOptions.java`
- Bazel lines: 108
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/build_options_table.rs`
- Verdict: partial
- Differences:
  - `getPythonVersion()` always returns `PY3` and ignores every flag. Zel stores the same six options (TriState `build_python_zip` default `auto` with the `--no` deprecation warning, booleans defaulting to `false`/`false`/`false`/`true`, and a null `python_native_rules_allowlist` label) but has no accessor that reports that major version.

### `src/main/java/com/google/devtools/build/lib/rules/python/PythonUtils.java`
- Bazel lines: 114
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/py_native.rs`
- Verdict: partial
- Differences:
  - Ancestor `__init__.py` paths are chosen immediately from the runfiles passed to `merge_runfiles_with_generated_inits_empty_files_supplier` and stored as concrete empty filenames. Bazel attaches `GetInitPyFiles` and runs it on the manifest at runfiles-input construction, so `.py`, `.so`, and `.pyc` files merged in later still receive ancestor `__init__.py` files.
  - Zel walks artifacts, symlinks, root symlinks, and existing empty filenames. Bazel walks only artifact runfiles paths and ordinary symlinks, after dropping paths obscured by an ancestor symlink, so a root symlink can add extra `__init__.py` files in Zel and an obscured `.py` still does.
  - Zel treats a directory as `__pycache__` when its path string ends with `__pycache__`. Bazel skips only a path whose last segment is exactly `__pycache__`, so a directory such as `pkg/not__pycache__` still gets an `__init__.py`.
  - `.py`, `.so`, and `.pyc` checks are case-sensitive. Bazel `FileType` matches those extensions case-insensitively when the host OS is Windows.

### `src/main/java/com/google/devtools/build/lib/rules/python/PythonVersion.java`
- Bazel lines: 170
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Python version enum (`PY2`, `PY3`, `PY2AND3`, `PY3ONLY`) with the flag converter and default-version lookup.

### `src/main/java/com/google/devtools/build/lib/rules/repository/RepoRecordedInput.java`
- Bazel lines: 940
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/lockfile.rs`
- Verdict: partial
- Differences:
  - `FILE` is `DIR`, `ENOENT`, or the lowercase hex digest (fast digest when the filesystem has one, otherwise the configured content digest). An I/O error is `failed to stat <path>: <message>`. Zel stores `DIR`, `ENOENT`, or SHA-256 of the bytes it reads; a read error becomes an empty string and the reason `file info or contents of <path> changed`.
  - Inside-workspace ids are `@@repo//rel` and `@@//rel` for the main repo, and parse also accepts `@repo//rel` and `//rel` as the main repo. Zel rewrites only a string that contains both `@@` and `//`; every other string is a filesystem path. External repos resolve as `<external>/<repo>/<rel>` on both sides.
  - `DIRENTS` is a fingerprint of the sorted entry names, and it is requested only after earlier inputs are still current. Zel uses that same fingerprint when `read_dir` succeeds. When the listing fails, Zel fingerprints an empty name list and reports `directory entries of <path> changed` if that differs.
  - `DIRTREE` is the hex digest of the sorted child names, each child's type ordinal and file digest, and each subdirectory tree digest. A change reports `directory tree at <path> changed`. Zel `watch_tree` records only a `FILE` value of `DIR`, and the up-to-date check skips `DIRTREE` lines, so nested content, names, and types stay current.
  - `ENV` is the repo-environment value from `RepoEnvironmentFunction` (the `--repo_env` view), compared null-safely. Zel compares the client environment map. The reason text `environment variable <name> changed: 'old' -> 'new'`, with `<unset>` for null, has the same shape.
  - `REPO_MAPPING` stores `sourceName,apparent` and the canonical name, or null when the apparent name is absent. A missing source repo yields `source repo <repo> doesn't exist anymore`. A different canonical name yields `canonical name for @<apparent> in <source> changed: <old> -> <new>`, with `<doesn't exist>` for null. Zel records those lines for extensions, and `recorded_inputs_outdated` never reads them.
  - `DIRENTS`, `DIRTREE`, and a `FILE` in an external repo each start a new batch and are requested only after every earlier batch is current. Zel checks every `FILE` and `DIRENTS` line in one pass, including lines after an input that is already outdated.
  - `WithValue.parse` requires a space and a known prefix (`FILE`, `DIRENTS`, `DIRTREE`, `ENV`, `REPO_MAPPING`). Any other line is dropped. A bad prefix, or a bad `FILE`, `DIRENTS`, `DIRTREE`, or `REPO_MAPPING` body, is the always-outdated sentinel `malformed marker file entry encountered`. Zel treats a missing value as `""`, ignores every prefix other than `ENV`, `FILE`, and `DIRENTS`, and returns `malformed recorded input` only when the input token unescapes to null or an `@@` path has no `//`.
  - Repository-rule markers written by Zel contain only the predeclared hash. Inputs recorded while the rule runs are discarded, so they cannot invalidate that repo. Extension lock entries store the recorded lines and check `ENV`, `FILE`, and `DIRENTS`.

### `src/main/java/com/google/devtools/build/lib/rules/repository/RepositoryDirectoryValue.java`
- Bazel lines: 105
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No SkyValue for a fetched repository. `RepositoryDirectoryValue` extends `NotComparableSkyValue`, so `DirtyBuildingState.unchangedFromLastBuild` treats every fresh value as changed and dependents are rebuilt after a refetch even when the path and metadata match. Zel has no `REPOSITORY_DIRECTORY` SkyFunction or key (`SkyKey` in `/workspace/zel-cursor/crates/zel-skyframe/src/lib.rs` stops at file, directory, package, configured-target, action, and precomputed keys). A refetch does not dirty dependents through that graph edge.
  - `Success` stores a `Root` (the repo directory) and `excludeFromVendoring`. `Failure` stores only `errorMsg`. Nothing in zel is that pair of values. `FetchRuntime::fetch_repo` in `/workspace/zel-cursor/crates/zel-bzlmod/src/fetch.rs` returns `Result<PathBuf, String>` and records the name in an in-memory `fetched` set. Vendoring exclusion is decided later from the rule name (`local_repository` / `new_local_repository`) and `VENDOR.bazel` `ignored_repos`, not from a flag on the fetch result.
  - `Key` is interned on `RepositoryName` and its function name is `SkyFunctions.REPOSITORY_DIRECTORY`. Zel never builds that key, so repository lookup is not a memoized Skyframe node keyed by repository name.
  - The precomputed injections `fetch_disabled` (`Boolean`), `dependency_for_force_fetching_repository` (`String`, with `FORCE_FETCH_DISABLED` equal to `""`), `dependency_for_force_fetching_configure_repositories` (`String`), `is_vendor_command` (`Boolean`), and `vendor_directory` (`Optional<Path>`) are absent. Zel keeps `nofetch`, `force`, and `vendor_dir` on fetch options. Changing them does not invalidate a `REPOSITORY_DIRECTORY` node, because that node does not exist.

### `src/main/java/com/google/devtools/build/lib/rules/starlarkdocextract/StarlarkDocExtract.java`
- Bazel lines: 362
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - Analysis produces an empty configured target. It does not load `src` as a Starlark module, report load failures on `src`, reject a generated file, or restart when the module is not yet available.
  - Transitive loads are not checked against source files in `deps` files-to-build. Missing `bzl_library` coverage and the generated-file note are not reported, and `src` and `deps` are not resolved as prerequisites.
  - No `ModuleInfo` is extracted. `symbol_names` does not filter symbols, `allow_unused_doc_comments` is ignored, and `render_main_repo_name` does not select the main-repo module name or workspace name for label rendering. A main-repository mapping failure is not a rule error.
  - `%{name}.binaryproto` and `%{name}.textproto` are declared as implicit outputs, but no non-executable write actions are registered. Files-to-build stays empty, so the binary proto is not the only default output and the text proto is not an on-demand output.
  - Default and data runfiles stay empty. Bazel adds the rule's default runfiles plus the binary proto.

### `src/main/java/com/google/devtools/build/lib/rules/starlarkdocextract/StarlarkDocExtractRule.java`
- Bazel lines: 157
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_class.rs`
- Verdict: partial
- Differences:
  - `src` is a mandatory label limited to a single `.bzl` or `.scl` file. Zel records a mandatory `LABEL` with any file type and `allow_single_file` false, so other extensions and multi-file targets are accepted.
  - `deps` replaces the ancestor's any-file label list with `.bzl` and `.scl` only. Zel keeps `deps` as an unrestricted `LABEL_LIST`.
  - The configured-target factory is `StarlarkDocExtract`. Zel analyses the rule with `extra_action_like`, which does not read `src`, `deps`, `symbol_names`, `render_main_repo_name`, or `allow_unused_doc_comments` and returns no files, so the declared `%{name}.binaryproto` and `%{name}.textproto` outputs are not produced.

### `src/main/java/com/google/devtools/build/lib/rules/test/StarlarkTestingModule.java`
- Bazel lines: 181
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - `testing.ExecutionInfo` does not validate `requirements` as a string-to-string dict (default `{}`) or default `exec_group` to `"test"`. Zel stores the supplied fields, maps only the first two positional arguments to `requirements` and `exec_group`, later drops non-string requirement entries, and never uses `exec_group` when scheduling the test.
  - `testing.TestEnvironment(environment, inherited_environment=[])` does not build a `RunEnvironmentInfo`. Bazel checks both arguments (string-to-string dict and sequence of strings) and, on a non-executable non-test rule, warns that returning it has no effect. Zel records a `TestEnvironment` provider with no defaults or construction checks, and analysis only applies `RunEnvironmentInfo`, so the call neither sets the run environment nor warns.
  - `testing.analysis_test` is not implemented. Bazel accepts it only in a BUILD thread, requires `name` to match `[A-Za-z_][A-Za-z0-9_]*`, rejects `name` in `attr_values`, defines one non-executable test rule with `analysis_test` set (implementation, attrs, fragments, and toolchains from the call; no parent, initializer, outputs, cfg, exec groups, or build setting), keys that rule class by package name, target name, and the transitive .bzl digest, exports it as `{name}_test` (export errors become `Errors in exporting {name}: ...`), and instantiates a single target with `name` plus `attr_values`.

### `src/main/java/com/google/devtools/build/lib/rules/test/TestSuite.java`
- Bazel lines: 120
- Zel counterpart: `/workspace/zel-cursor/crates/zel-rules/src/native.rs`
- Verdict: partial
- Differences:
  - The suite configured target does not take data runfiles from kept members. Bazel publishes empty default runfiles and data runfiles built from each kept dependency’s data runfiles (`alwaysIncludeFilesToBuildInData` included). Zel publishes empty default and data runfiles.
  - Direct tests are not filtered by the suite’s `tags`. Bazel drops a direct test whose test tags fail the filter: a literal `manual` tag is not required, `-tag` excludes, `+tag` is a required tag with the `+` removed, and every other tag is required; taggable attributes such as `size` count. Nested suites are kept without that filter. Zel does not apply this filter.
  - An empty `tests` list does not add `$implicit_tests`. Bazel still analyzes those package tests and can place them in the suite runfiles. Zel only walks the explicit `tests` labels.
  - Membership is decided by rule-class name (`*_test` or `test_suite`), not by a test-tags provider or a transitive-tests marker. Bazel accepts any `tests` prerequisite that has either provider, so an alias of a test or suite is accepted, and the suite itself provides the marker. Zel rejects an alias of a test or suite and does not provide that marker.
  - The suite target is marked as a test and gets an empty `compilation_outputs` group. Bazel does not treat `test_suite` as a test rule, so it gets neither test tags nor a test action.

### `src/main/java/com/google/devtools/build/lib/rules/test/TestSuiteRule.java`
- Bazel lines: 164
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/rule_class.rs`
- Verdict: partial
- Differences:
  - `testonly` is a constant default of true, overriding the ancestor's package `default_testonly` computed default. Zel records a suite as testonly only when the attribute is written or `package(default_testonly)` is set, so an ordinary `test_suite` is not testonly and a non-test rule may depend on it. `package(default_testonly = False)` also forces the suite not testonly.
  - `tests` allows no files. Zel leaves that label list accepting any file, so a file label is not rejected as a disallowed file type.

### `src/main/java/com/google/devtools/build/lib/rules/test/TestingSupportRules.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-packages/src/loader.rs`
- Verdict: partial
- Differences:
  - Bazel installs `testing`, `coverage_common`, `InstrumentedFilesInfo`, `AnalysisFailureInfo`, and `AnalysisTestResultInfo` on the configured Starlark environment, including BUILD evaluation. Zel installs those names only as `.bzl` predeclared globals, so a BUILD file cannot call `testing`.
  - Bazel `testing` provides `ExecutionInfo`, `TestEnvironment(environment, inherited_environment=[])` (a `RunEnvironmentInfo` that does not error when the rule is not executable), and `analysis_test`. `analysis_test` runs only in a BUILD thread, requires an identifier `name`, rejects `name` in `attr_values`, exports one analysis-test rule as `{name}_test`, and instantiates a single target of that rule. Zel `testing` only has generic `ExecutionInfo` and `TestEnvironment` providers: there is no `testing.analysis_test`, and `TestEnvironment(...)` does not build `RunEnvironmentInfo`.
  - Bazel `ExecutionInfo` defaults `requirements` to `{}` and `exec_group` to `"test"` and requires a string-to-string dict. Zel stores positional `requirements` and `exec_group` with no defaults and no dict type check.
  - Bazel `AnalysisTestResultInfo` requires a bool `success` and a string `message` at construction. Zel accepts any fields and checks those types only when a rule created with `analysis_test=True` returns the provider. `AnalysisFailureInfo` is not the Bazel provider whose `causes` field is a depset of analysis failures.

### `src/main/java/com/google/devtools/build/lib/rules/test/TransitiveTestsProvider.java`
- Bazel lines: 25
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No empty immutable marker provider is defined or attached so one `test_suite` can recognize another by provider lookup.
