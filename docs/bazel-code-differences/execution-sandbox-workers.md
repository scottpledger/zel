# Bazel vs zel code differences: execution sandbox workers

Category rule: Local action execution, spawn strategies, sandboxing, workers, dynamic execution, shell execution, and include scanning.

- Failure entries: 161
- Missing: 75
- Partial: 86
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `src/main/java/com/google/devtools/build/lib/dynamic/Branch.java`
- Bazel lines: 249
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`Branch`, `Race`, `ExecRuntime::exec_dynamic`)
- Verdict: partial
- Differences:
  - Call points the branch at sibling logs with a `.LOCAL` or `.REMOTE` suffix on the stdout and stderr basenames. After the implementation returns or throws an execution exception, each file that exists is moved onto the action's real stdout and stderr. An interrupt skips that move, so the suffixed files remain and the destination is unchanged. Zel never creates those paths and only keeps the winning result's in-memory stdout and stderr, so the on-disk action logs and leftover suffixed files diverge, including a last-move-wins race where the files can be the other branch's output.
  - The move overwrites the destination. A cross-filesystem copy keeps mode and modification time, and if deleting the source then fails it deletes the destination too. Symlinks are recreated as links. A directory throws, which is logged and swallowed. A stdout move failure skips stderr entirely. A missing source leaves that destination untouched, so old stdout and new stderr can mix. Zel replaces both buffers from the single winner.
  - Getting the suffixed logs throws when either log path has no parent, before the branch runs. A null result throws after the move, fails the future with a non-interrupt, and the listener therefore cancels the other branch. Zel has no null result and does not consult those paths. Bazel path strings are Latin-1. The suffix is Java string concatenation on that basename.
  - Preparing the future cancels the other branch with an interrupt only when this future succeeded or failed with something other than an interrupt. Cancelled and interrupted futures do not cancel the other. That listener runs after call has already moved logs. If both futures are already successful, cancel is a no-op and both stay not-cancelled, so waiting throws an assertion that neither branch cancelled the other. Zel cancels only when this branch wins the lock. The first winner is returned, and neither completed is an error naming the mnemonic.
  - Bazel's done semaphore is released by the listener when the branch is still starting, so a waiter can observe cleanup after an early cancel, and getting results is interruptible. Zel waits for the local thread only when the remote branch wins the lock. A local win sets a flag and does not wait for the remote thread. The local delay loop polls cancellation every 5 milliseconds instead of interrupting sleep, and the remote thread is detached. The debug spawn scheduler is never read. Bazel's debug listener calls get on the completing thread, which clears a pending interrupt on that worker.

### `src/main/java/com/google/devtools/build/lib/dynamic/DynamicExecutionFinishedEvent.java`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - This is a postable with four fields and getters only. The constructor stores the mnemonic, the local branch name, the winner branch name as the remote branch name, and the winner branch type by reference. There is no null check, copy, trim, or encoding change. The getters return those references. There is no event type, post, or subscriber.
  - Bazel posts this from the dynamic spawn strategy: the mnemonic plus the local and remote strategy names, with the winner's name replaced by the result runner name, and the mode local or remote. The metrics collector keys a concurrent map on the mnemonic and the two names and increments a 32-bit local-wins or remote-wins count. The metrics proto emits those race statistics onto build metrics. Zel's dynamic execution never builds that payload. Zel posts an empty build-metrics message, so those race counts stay absent. The int increment wrap is unreachable here because the counter is never updated.
  - The nearest race state stores only a local or remote winner. It does not store the mnemonic or the strategy names, and it is not posted. The mode is a nullable reference on this event. Zel's branch has no those names and uses an option for no winner. Nothing reads an equivalent event, so that null-versus-option split does not show up in output.
  - This file does not index strings, so UTF-16 versus Unicode scalars does not change a result by itself. The three name strings are stored as Java strings and can hold unpaired surrogates. Zel has no field that would reject them. There is no integer arithmetic, regex, map iteration, Latin-1 conversion, or JSON in this file.

### `src/main/java/com/google/devtools/build/lib/dynamic/DynamicExecutionModule.java`
- Bazel lines: 289
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/strategy.rs`, with the race in `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` and flag wiring in `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - A cyclic dynamic local or remote strategy is the same text, but Bazel exits 36 as a local environmental error. Zel reports it as a command-line error, exit 2.
  - The string-to-list converter rejects a strategy list that contains an empty piece when there is more than one piece, including a trailing comma. A single empty value becomes an empty list and replaces that mnemonic, including the empty-string fallback. Zel splits on commas and drops empty ids, so a trailing comma is accepted as one strategy and a comma-only value silently becomes no strategies.
  - With verbose failures, an ignored local signal posts an info event naming the action owner and the signal. Zel drops that local win and emits no event. The ignore rule itself matches: local only, and only when the exit code is non-zero and the exit code minus 128 is in the signal set.
  - Signal parsing differs. Java trim strips only code points at or below U+0020. Rust trim strips all Unicode whitespace, so a leading non-breaking space makes Bazel fail the flag and Zel accept the number. The integer parser rejects a leading plus. Rust accepts it. Both require a positive 32-bit int. The subtraction is 32-bit. Java always wraps, while a debug Rust build panics if the subtraction overflows. Normal 0–255 wait statuses do not.
  - This module passes the full dynamic options plus the truncated CPU count and jobs into the strategy. Zel never applies the exclude-tools flag, default true, so tool-configuration actions that can run both ways are raced instead of forced remote-only. It also ignores the local load factor. Bazel limits concurrent local branches to the CPU count when the factor is positive, and when the factor is strictly between 0 and 1 it shrinks permits with queue depth using 32-bit float division, never below 1. A truncated CPU of 0 grants 0 permits, so local dynamic branches never start. Zel starts every local branch immediately, aside from the post-cache-hit delay.
  - The slow-remote time is unused. Bazel, once local branches are queued on that semaphore, runs the oldest waiter ahead of the others after this duration.
  - The local execution delay is a Java int decoded with integer decode, so a leading 0 is octal. A negative value is accepted and sleep then throws, failing the local branch. Zel parses a decimal 32-bit int and clamps negatives to 0.
  - Default local strategies are worker, then sandboxed only if that identifier is already registered, then standalone when local lockfree output is set. Last mnemonic wins, and the remote fallback is remote only when the remote strategy flag is unset. That matches on Unix. On Windows, Bazel registers sandboxed for the Windows sandbox. Zel's process-wrapper support is Unix-only and it never registers the Windows sandbox, so the default local list stays worker, plus standalone if lockfree.
  - Branch threads differ in a way that can change which side wins. Bazel runs them on the pool created before the command and shuts that pool down afterward, interrupting on interrupt. Zel detaches an OS thread per remote branch and does not interrupt it at the end of the command.

### `src/main/java/com/google/devtools/build/lib/dynamic/DynamicExecutionOptions.java`
- Bazel lines: 207
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`, `/workspace/zel-cursor/crates/zel-options/src/converters_ext.rs`, and `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - Excluding tools defaults to true and is never read. Bazel runs tool-configuration spawns remote-only. Zel still races local and remote for those spawns.
  - The local load factor is never read. Bazel sizes the local-branch semaphore to jobs when the factor is at most 0 and to the CPU count when it is positive, and a factor strictly between 0 and 1 shrinks that cap as the local queue grows. Zel starts every local dynamic branch immediately.
  - The slow-remote time is never read. When the duration is positive, Bazel dequeues the oldest waiting local job once its age exceeds that duration, and otherwise runs the queue last-in-first-out. Zel has no such queue.
  - That duration uses saturating multiplication. Durations that overflow Java's duration abort Bazel with an arithmetic exception. Zel accepts them. A digit string that does not fit in a Java long is a number-format exception in Bazel and an illegal-duration error in Zel.
  - The local execution delay is parsed again with a decimal 32-bit parse clamped at 0, not integer decode. A leading-zero value sleeps 8 milliseconds in Bazel and 10 in Zel. Hex and hash prefixes are valid in Bazel and rejected by Zel. A negative value makes Bazel sleep throw. Zel clamps it to 0.
  - The internal spawn scheduler treats only the raw strings 0, false, and no as false. Bazel also treats f, n, and any case variant as false, so `False` disables dynamic execution in Bazel and leaves it enabled in Zel.
  - The signal-list converter trims with Java trim, which strips only code units at or below U+0020. Zel trims all Unicode whitespace. A leading NUL is signal 15 in Bazel and a parse error in Zel. A leading non-breaking space, next-line, or em space is a parse error in Bazel and signal 15 in Zel. Exact null is an empty set on both sides. Duplicates are a set versus a vector, but only membership of the exit code minus 128 is tested.
  - The old-name warning flag is not honored. The experimental local-delay and debug-scheduler names print a deprecation warning in Zel and are silent in Bazel.
  - The debug spawn scheduler is stored and never read, so Zel does not print the scheduler debug logs Bazel prints when the flag is true.

### `src/main/java/com/google/devtools/build/lib/dynamic/DynamicInterruptedException.java`
- Bazel lines: 22
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/lib.rs` (`cancelled_error`)
- Verdict: partial
- Differences:
  - Bazel is a distinct interrupt subclass whose only state is the constructor message. Zel has no such type. The cancelled error is a plain I/O error with the interrupted kind and the fixed message "spawn cancelled."
  - That subclass is what the local and remote branches catch separately from a plain interrupt. On this exception they cancel before rethrowing so the branch future does not cross-cancel the winner. A failed future whose cause is an interrupt, including this subclass, does not cancel the sibling. Waiting turns it into a null or cancelled branch. A non-interrupt failure still cancels the sibling and fails the action. Zel uses one interrupted kind for a lost race, a pre-start cancel, and any other interrupt. The race drops every interrupted error and does not publish it, while any other I/O error can still win and cancel the other branch. An interrupted error that is a real I/O interrupt is discarded the same way as a lost race, so that branch cannot become the action failure.
  - Caller messages are not preserved. Bazel records which strategy lost and why. Those strings, the class name, and the Java stack are what the debug spawn scheduler logs. Zel always reports "spawn cancelled" and carries no class name or stack.

### `src/main/java/com/google/devtools/build/lib/dynamic/DynamicSpawnStrategy.java`
- Bazel lines: 764
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`ExecRuntime::exec_dynamic`, `Race`) and `/workspace/zel-cursor/crates/zel-exec/src/strategy.rs`
- Verdict: partial
- Differences:
  - Excluding tools defaults to true. Bazel then runs tool-configuration spawns remote-only. Zel always races them when both branches can run.
  - Bazel treats a spawn as locally executable only if the extra local spawn is absent or that post-processing spawn can also run locally, and the local branch appends its results after a successful local run. Zel never runs a post-processing spawn, so local winners omit those outputs and a local-incapable post-processing spawn does not force remote-only execution or the combined failure text.
  - If neither branch can run, Bazel throws a user execution exception whose failure detail is no usable strategy found, and whose message can include both the spawn and the post-processing spawn. Zel returns that the spawn is not executable locally, with no failure-detail code and no post-processing clause.
  - The local and remote strategy lookups walk the dynamic contexts, which drop strategies the execution platform does not allow. Zel's first-match scan does not apply that filter, so a different branch strategy is chosen or a branch stays enabled.
  - Local branches are queued behind a shrinkable semaphore: CPU-count permits when the load factor is positive, otherwise jobs, and permits shrink via a float load ratio when the factor is strictly between 0 and 1. The oldest job is started only when its age exceeds the slow-remote time. Otherwise the newest is started. Zel starts every local branch immediately, so concurrency and which branch wins change whenever the load factor or slow-remote window would have queued work.
  - The local delay starts false and is set when any cache hit comes back from the remote branch, including remote-only execution. Later local branches sleep that many milliseconds, and the sleep is interruptible. Zel arms the delay only inside the raced remote thread, and only when the runner name contains "cache hit," then polls the cancel flag every 5 milliseconds. A remote-only or tool-action cache hit never delays later local runs, and a hit whose runner name lacks that substring does not either.
  - Stopping a branch always cancels the other branch and waits on its done semaphore before the winner returns. If cancel fails and that branch's future is already successful, it throws the dynamic interrupt so the canceller does not keep its result. Zel's local win only sets remote-cancelled when cancelling the remote branch on a local win is true, and does not interrupt the remote call or wait for it. Dynamic execution then returns the first kept result while that remote thread can still be running. A lost race against an already-successful branch does not discard the canceller's result.
  - Waiting joins the local future first. A local execution exception, runtime exception, or interrupt cancels the remote branch and fails the spawn even if remote already succeeded. An interrupt of the waiting thread cancels the branch and is propagated. Zel keeps the first stored winner, so a later local error does not replace a remote success, and a condition-variable wait does not observe thread interruption.
  - Ignorable failures go through the ignore check before stopping the branch. Stock Bazel ignores only a local death by signal by throwing the dynamic interrupt, and with verbose failures posts an info event. Zel applies that check only as the exit code minus 128 inside the race lock, publishes no such event, and has no remote or custom ignore callback.
  - If both branches return a result, Bazel throws an assertion that neither branch cancelled the other. If both return null, it throws that neither branch completed, naming the primary output or the mnemonic plus owner when the primary output is null. Zel returns the first kept result in the first case, and neither branch of the mnemonic completed in the second.
  - After a non-empty win, Bazel posts a finished event with the mnemonic, each branch's strategy name, and the winning side replaced by the result runner name. The metrics collector records that. Zel posts no event, so those metrics stay empty.
  - With the debug spawn scheduler, Bazel emits info events for a null branch result, cancellation, an interrupt wrapped in an execution exception, and local-exception cancellation. Zel stores the flag and does not emit them.
  - The raced remote thread runs on the default local filesystem and ignores the filesystem passed into dynamic execution. The local branch uses that filesystem. On a non-default filesystem the two branches see different inputs and outputs.

### `src/main/java/com/google/devtools/build/lib/dynamic/LocalBranch.java`
- Bazel lines: 216
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`ExecRuntime::exec_dynamic` local thread and `Race::lock`)
- Verdict: partial
- Differences:
  - While the local branch is racing, Bazel passes a non-null stop callback, so the abstract strategy drops the remote spawn cache: the speculative local run neither looks up nor uploads the remote or disk cache. Zel's local thread calls the ordinary strategy, which on a cache-enabled runtime looks up the disk cache and then the remote cache, locks with that exit code, and on a miss uploads a successful local result. A cache hit taken by the local thread also does not set the local delay. Bazel sets the delay flag only from a cache hit on the remote branch, so later local branches stay undelayed whenever the local branch consumes the hit.
  - The local run runs an extra spawn only after every primary result is success, then runs that spawn with a null stop callback and appends its results. Zel never runs a second spawn. In this tree the post-processing spawn is always empty, so a stock build does not hit this path.
  - Ignoring a failure runs only from the stop callback, and only when the local future is not already cancelled. Exit code 0 returns immediately. Otherwise the exit code minus 128 is tested against the ignored-signal set. A match throws the dynamic interrupt before stopping the branch, the local future is cancelled, and the remote branch is left to finish. Zel uses the same subtraction inside the race lock, but the local worker strategy locks with 0 before the worker runs, and standalone local execution also locks with 0 before the process starts. That claims the race and, by default, cancels remote before the real exit code exists, so a later ignorable signal still fails the action. Bazel's worker runner locks with the worker's exit code after the response, and with lockfree output the standalone runner locks with the real non-zero exit code only after the process exits.
  - The local execution delay is a Java int decoded with integer decode, so a leading 0 is octal and a hex prefix is hex, and it is passed to sleep. A negative value throws from the local branch. That failure is not an interrupt, so the branch listener cancels remote and the dynamic action fails. Zel parses a decimal 32-bit int, clamps negatives to 0, and treats 0 as no delay, so a negative value runs immediately, an octal-looking value sleeps the decimal count, and a hex value is rejected at flag parse.
  - Age is the age of the queued local branch. The strategy uses it, when the slow-remote time is greater than zero, to start the oldest waiting local job ahead of the usual last-in-first-out queue, and the load factor limits how many local branches run. Zel starts the local thread immediately and has no age, queue, or load-factor cap.
  - If the chosen local strategy returns a null result list, Bazel logs a warning and returns null, and the branch then throws. If every dynamic local strategy cannot execute after the precondition, Bazel throws an assertion. Zel's result path has no null result, and the strategy is fixed before the race.

### `src/main/java/com/google/devtools/build/lib/dynamic/RemoteBranch.java`
- Bazel lines: 181
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`ExecRuntime::exec_dynamic` remote thread and `Race::lock`)
- Verdict: partial
- Differences:
  - Running remotely latches the local-execution delay on any cache hit, including the remote-only call when local cannot run and when tools are excluded, which is the default. Zel's remote-only arm returns the strategy with default hooks and never stores the delay, and tool actions still race a local branch. Later local branches therefore are not delayed by the default one-second delay after a cache hit that Bazel would have counted, which changes who wins.
  - Bazel sets that flag only from a cache hit after the strategy returns. A lost race throws inside execution, so the flag stays false. Zel sets it when the runner name contains "cache hit." The remote runner treats a cached non-zero exit or a missing mandatory output as a miss before locking outputs. Zel locks with that exit code and, unless the status is an execution failure, returns a non-zero exit named as a disk or remote cache hit, so the failed cache entry wins the race and latches the delay.
  - Getting the dynamic contexts applies the execution-platform allowlist, keeping order, and the dynamic remote policy, and the registry rejects a non-sandboxed dynamic strategy. Zel takes the first executable kind for that mnemonic, or empty, with no platform allowlist and no remote policy. Whether a spawn can execute can also disagree because Zel does not see the execution context.
  - A null result list posts a warning and then throws in the loop, failing the branch as an unchecked exception so the listener cancels local. Zel has no null result list. If no strategy can execute, Bazel throws an assertion that can-exec passed but no usable remote strategy exists. Zel does not re-check at the start of the remote thread. A strategy error becomes an I/O error, and the race locks remote with exit code 1, so that error can be the action result.
  - On the dynamic interrupt, the implementation cancels the future so the branch listener does not cross-cancel local. Any other thrown failure is not an interrupt, and that listener still cancels local even if local already holds the output lock. Zel sets the cancel hook only for the local branch. Remote cancellation only makes the lock return false, and the remote call still runs to completion, so cache uploads can land. A non-interrupted remote error is kept only when remote is still the winner, so a late remote failure does not cancel a local winner.
  - The remote thread uses a default local filesystem and ignores the filesystem passed into dynamic execution. Bazel runs the strategy on the action execution context's filesystem, so a non-local filesystem view changes the remote branch's inputs and outputs.

### `src/main/java/com/google/devtools/build/lib/dynamic/ShrinkableSemaphore.java`
- Bazel lines: 91
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No shrinkable permit pool exists. Bazel uses this semaphore as the dynamic strategy's thread limiter: it updates the load from the waiting local jobs, then tries to acquire before a local dynamic branch starts. Zel starts the local branch immediately on its own thread, with no acquire, wait queue, or release, so every dynamic action runs locally at once. That changes how many local processes run and which branch wins.
  - The local load factor is only a flag definition, default 0. The execution phase never reads it into dynamic options. Bazel shrinks only when the load factor is strictly between 0 and 1. The shrink factor is one minus the load factor, times the load divided by the maximum load. The division is 32-bit float, so integer loads above 2^24 are not exact, and is then widened to double. The int cast truncates toward zero. The cap keeps at least one permit. A negative load throws. A load factor at or below 0, at or above 1, or NaN leaves the permit count unchanged. None of this runs.
  - A zero maximum load is not guarded. A positive load makes the float ratio infinity, so the shrink becomes the total permits minus one. A zero load makes the ratio NaN, and the int cast of NaN is 0, so the shrink is cleared. Zel never applies that.
  - Shrink updates are a single compare-and-set with no retry. A lost race drops that reduction or release entirely, so a concurrent load update can leave the shrink amount stale. Reducing permits may drive the available count negative while more permits are held than the new limit. Try-acquire then fails until releases catch up. Raising the limit releases the negative of the reduction, which is a zero release when unchanged. Zel has no permit counter, so queue growth never withholds local starts and a negative load is not rejected.
  - Even with the default load factor 0, the parent still constructs a non-fair semaphore whose initial permits cap in-flight local branches: jobs when the factor is 0, and the CPU count when the factor is positive. Zel has no equivalent ceiling.

### `src/main/java/com/google/devtools/build/lib/exec/AbstractSpawnStrategy.java`
- Bazel lines: 410
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`ExecRuntime::run_strategy`) with the cache half in `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs` and the context half in `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs`
- Verdict: partial
- Differences:
  - Timeout comes from integer parse of the timeout requirement, or zero if absent. Fractions, exponents, trailing junk, and surrounding whitespace throw, so the action never runs. A negative integer becomes a negative duration. Zel trims and parses a float, keeps fractional seconds, and treats unparseable or non-positive values as no timeout, so the action runs.
  - On any non-success result with an empty failure message, Bazel builds a spawn-execution exception from the command-failure description: original argv, spawn environment, exec-root working directory, the configuration checksum, and the execution-platform label, with no runner. The detail prefixes the exit, timeout, or out-of-memory status, appends the wall-time timeout sentence from the int wall time, and appends the remote out-of-memory sentence. A non-empty failure message replaces the command description. A spawn-execution exception thrown by the runner is logged and rethrown unchanged. Zel has no failure-message field and no out-of-memory, execution-denied, catastrophic, or remote-cache-failed status. Non-zero exit and execution failure share one exit text. Timeout never adds the wall-time sentence and instead appends the requirement, not the wall time. Sandbox failures use the sandbox working directory when the runner name ends with the sandbox suffix, and the platform is hard-coded. Catastrophic versus keep-going is lost because those statuses collapse.
  - Cache selection skips the context cache when the runner handles caching, when the context cache is null, or when speculating and the cache is not useful in dynamic execution. The local branch of dynamic execution therefore does not read or write that cache. Zel's dynamic local branch still does a disk-then-remote lookup and may publish a cache hit. An interrupted I/O exception becomes an interrupt. Any other I/O exception from lookup, exec, or store fails the spawn. Zel treats remote lookup errors as warnings and misses, and cache upload errors as warnings, so the action still succeeds. Store runs for every result the runner returns without throwing, including non-success. Zel uploads only success with exit code 0. There is no cache-handle close wait.
  - The executed event is posted only on a cache miss, with the start time from the context clock and the spawn identifier set to the result digest hash. The execution log runs for both hits and misses. An I/O exception there fails the action immediately even after a successful spawn. Zel logs declared inputs with empty hashes, a zero timeout, no action digest, and a status of empty or non-zero exit only. Log write failures are deferred to command shutdown and do not fail that action.
  - The subcommand is reported before lookup, including cache hits and spawns that later fail with missing inputs, and it uses the real execution platform. Zel announces only after a result, skips missing inputs, and prints the result runner name rather than the strategy name.
  - The input mapping is a tree map ordered by the hierarchical path comparator. Slash sorts before any other differing byte, so `a/b` sorts before `a-b`. It expands runfiles, fileset symlinks, and tree artifacts, keeps empty trees, applies the path mapper, and prefixes the base directory. The map is memoized only when it will be accessed repeatedly, and the same instance is returned. Zel spawn inputs are a vector of UTF-8 path fragments with no expander, no path mapper, no base directory, and no memoized map. Directory walks use lossy conversion. Sorted-map byte order sorts the hyphen before the slash, so `a-b` sorts before `a/b`.
  - Prefetch runs only when local prefetch is not disabled, over the expanded mapping. A bulk-transfer exception with lost artifacts becomes a lost-inputs exception. Otherwise it is an environmental exception. Zel has no prefetch flag and no lost-input rewind on this path. Download errors become an execution failure. A missing input fails with unexpected lost inputs only when rewind is off. When it is on, the missing input is ignored and the spawn still runs.
  - The spawn id is a process-wide atomic int taken in the context constructor for every strategy and every exec, including cache hits. It wraps to negative after 2^31−1. Zel's id is an unsigned 64-bit counter that increments only inside the sandbox runner, so sandbox directory ids diverge as soon as a non-sandbox spawn or a cache hit occurs. Setting the digest rejects null and a second different digest. Zel does not keep that digest on the context.
  - Locking outputs forwards the exit code, the error message, and the output streams. Zel's lock passes only the exit code. Report returns without posting when the owner or the primary output is null. Zel still reports those actions.

### `src/main/java/com/google/devtools/build/lib/exec/BinTools.java`
- Bazel lines: 250
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never scans the embedded-binaries root. Production walks it without following symlinks, keeps only regular files, and drops any relative path that starts with `embedded_tools/`. Names are internal strings: Latin-1 raw bytes on Linux with that locale, otherwise platform bytes reencoded through the string encoding. Lookup is case-sensitive code-unit equality with no Unicode normalization. A duplicate relative path throws at startup. Zel has no such list, so those names are absent.
  - The embedded path and the action input return null when the name is not in that filtered list, even if the file exists on disk, and they do not stat. A null action-input name throws. A null embedded-path name returns null. Callers that then ask for the path string throw. That membership test is what enables the process wrapper, including the Windows executable extension, the Linux sandbox, daemonize, and the Xcode locator. Zel does not consult an install-base tool. Process-wrapper support is only Unix, so an in-process wrapper still runs on POSIX when the binary was never extracted, and Windows never uses the executable. Strategy detection always reports that the Linux sandbox binary is not installed, so that strategy is never registered. Async clean starts a shell in its own process group instead of executing embedded daemonize. macOS local environment uses xcode-select and xcrun and lossy UTF-8 instead of the embedded locator, so the developer directory and SDK root differ or are left unset.
  - Each kept tool is a path action input whose exec path is `_bin` plus the embedded path. An absolute embedded path replaces `_bin`. Parent segments are normalized and can escape `_bin`. When that input is on a spawn, the local runner copies the raw bytes onto the execroot, creates parents, and sets the executable bit twice. Zel never materializes those tools, so the file, its executable bit, and the digest handed back to the sandbox input map are missing. The local and sandboxed command line uses the absolute install-base path as the first argument of the wrapper. Zel runs the child directly.
  - Atomic write caches the digest and skips the copy when a digest is already cached and the output exists, without comparing contents, so a different pre-existing file is left in place. The lock is a per-instance reentrant lock, not a temp file plus atomic rename. Metadata hashes the source with the filesystem digest and a 1024-byte buffer, stopping on a non-positive read, so a zero-length read ends the hash before end of file. The size is a Java int, so a file of at least 2 GiB wraps to a negative size. That digest and the write-path digest can diverge if the source changes. The cached metadata is not invalidated. Zel records neither value, so action-cache and remote metadata for these tools do not match.

### `src/main/java/com/google/devtools/build/lib/exec/BlazeExecutor.java`
- Bazel lines: 121
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` (`ExecConfig`)
- Verdict: partial
- Differences:
  - There is no blaze executor. Zel never snapshots a show-subcommands value or holds a clock, a bug reporter, and an action-context registry on one object. The execution phase builds a config and a strategy registry and passes the exec root, filesystem, and parsed options through separately.
  - Construction requires the execution-options class and throws when that class is missing. Zel never looks the class up. A missing subcommands flag is false and execution continues.
  - Reporting subcommands returns the enum captured at construction: true shows them, pretty-print shows them pretty-printed, and false hides them. The converter matches the enum names case-insensitively, then boolean spellings map to true or false. Zel keeps a bool and pretty-prints only when the last raw flag text is exactly pretty_print. Pretty-print in any other case uses the single-line form.
  - That pretty-print bit is a process-global atomic bool, not a per-executor field. The test runner announces test subcommands from the bool alone and always calls the non-pretty description. Bazel pretty-prints those spawns when the enum is pretty-print.
  - The clock is the injected clock. Spawn start times and the standalone test start time use it. Zel always calls the system clock and casts milliseconds to an unsigned 64-bit value, then to a signed 64-bit value in the test cache status. A substituted clock is ignored. A pre-epoch time is a negative long in Bazel and 0 in Zel. With the default wall clock both are epoch milliseconds.
  - Getting a context returns null when that class was not registered. The abstract strategy treats a null spawn cache as no cache. Zel has no class-to-instance registry on the executor, so that null fallback does not exist.
  - The constructor logs the spawn strategies and action contexts at info. Zel does not emit those lines. It also skips the used notifications. In this tree the only override throws if the dynamic strategy registry is missing and otherwise notifies strategies whose used-context is empty, so a stock registry does not change spawn results.

### `src/main/java/com/google/devtools/build/lib/exec/CheckUpToDateFilter.java`
- Bazel lines: 69
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`execute_action_with` when `check_up_to_date`) and `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - Bazel allows a non-test action only when neither up-to-date check is set, and a test-runner action only when the test check is not set. Zel's build gate fails every cache miss solely when check-up-to-date is true. Checking tests up to date still forbids build actions in Bazel when the build check is false. Zel only gets that by the implicit check-up-to-date, applied as a computed default, so an explicit no-check lets stale build actions run.
  - With only the build check, Bazel still executes test-runner actions. Zel does not special-case that type inside the action executor. Tests still run because the test phase ignores the build check. Test-runner actions are synthesized for aquery only.
  - When tests are forbidden, Bazel suppresses unconditional test execution and accepts an action-cache hit, so no cache, external, and more than one run under automatic stay up to date if the action cache matches. Zel treats no cache and the external and no-cache tags as not cached, and otherwise trusts the test action key plus the on-disk test status. The same test can be out of date in one and up to date in the other. Zel's error names the test label. Bazel's names the action progress message.
  - If either flag is set and the stable-status file is missing, Zel aborts before other actions and, for test, also prints that no test targets were found even when tests were found. Bazel reports only the workspace-status action that actually needs to run, and does not add that line.
  - Denied build-action text is not the action's pretty print, including the tool suffix. Zel uses a writing-file message for every file write, while Bazel uses writing-script when the output is executable. Expanding a template has no tool suffix. Creating a symlink uses the root-relative path instead of the action's progress message. For spawns it uses the stored progress message or the mnemonic and owner rather than the mnemonic and primary output.
  - This class has no string-encoding, integer-width, Latin-1, regex, map-iteration, or JSON difference. Applying null is not a test-runner action and returns the build-action bit without throwing. Zel has no null action.

### `src/main/java/com/google/devtools/build/lib/exec/CompactSpawnLogContext.java`
- Bazel lines: 810
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/exec_log.rs` (`ExecutionLogWriter::log_compact`)
- Verdict: partial
- Differences:
  - The on-disk compact log is a real compressed stream written as spawns complete. Zel keeps plaintext in memory and, on close, writes one compressed frame of uncompressed raw blocks. The same logical bytes therefore do not match, and a crash before close leaves Bazel a partial frame and Zel no compact file. Discard deletes the compact path. Bazel does not.
  - Bazel always publishes the compact file to the build-tool log after close. Zel never publishes it.
  - The invocation entry uses the digest function's string form, the real workspace name, sibling repository layout, and a UUID, all converted from internal bytes to Unicode. Zel writes the hash-function name from the digest-function flag uppercased, hardcodes the workspace runfiles directory to `_main`, and never sets sibling repository layout.
  - Spawn, invocation, and symlink-action entries are written without an id. Zel assigns an id to every spawn entry, so later file and input-set ids shift.
  - Bazel always sets spawn metrics from the result, including an all-zero metrics message. Zel's compact path never writes metrics. The spawn-metrics flag affects only the JSON total time.
  - The platform comes from exec properties plus remote default properties, converted to Unicode and sorted. Zel's compact spawn never sets the platform.
  - Timeout milliseconds are always written, and zero is omitted by proto3. The observability record leaves timeout at 0, so Zel omits it whenever Bazel had a nonzero timeout.
  - The digest is the spawn result digest with the hash-function name cleared. Zel adds the hash-function name whenever the action digest is set, and the caller leaves it unset, so the field is absent.
  - Status is set only when it is not success, using the enum name. Zel sets status from the record. The caller stores non-zero exit only when the exit code is non-zero, and an empty status is omitted.
  - Remotable, cacheable, and remote-cacheable follow whether the spawn may be executed remotely, cached, and cached remotely. The caller forces all three true. The runner is the result runner name. The caller uses remote cache hit, the result runner, or local. The target label is set only when the label is non-null, as the canonical form after the Unicode conversion. Zel always sets the target label from the action owner string.
  - Inputs keep nested-set shape: transitive set ids then leaf input ids, postorder. Shared sets are interned. The top-level non-tool set uses a null key and is not interned. Tool files are a subset of input files, so they appear in both sets. A test-runner mnemonic changes runfiles interning. Zel flattens inputs, sorts by path, and puts each file in either the input set or the tool set. Input sets are never interned. The caller never sets the tool bit, so the tool set is always empty and every path is an input.
  - File entries are interned by the internal exec-path string, except parameter files, which are logged every time. The first digest for that path is reused even if the bytes change. Zel interns on path and hash, including parameter files, and a new hash is a new entry. An empty virtual input throws in Bazel.
  - Digests prefer input metadata, then virtual-input bytes, then a filesystem digest, without the hash-function name. Zel's compact file digest includes the hash-function name when the hash is non-empty, and omits the digest when the hash is empty. The caller sets no hashes, so compact file entries are path-only.
  - Directories, filesets, unresolved symlinks, and runfiles trees are separate entry types. Tree artifacts expand from metadata. Otherwise a walk follows links, records only non-directories, and sorts with UTF-16 compare. Filesets add the directory id twice. Unresolved symlink targets come from metadata or a symbolic-link read. Runfiles trees store the canonical input set, symlink sets, empty files taken from the path string without the Unicode conversion, and a repo-mapping manifest that has a digest and no path. A workspace-name mismatch throws. Zel has none of these. Symlink targets on a spawn file are not written in the compact file entry.
  - Logging a symlink action writes a symlink action with no id, and returns immediately when the primary input is null. Zel never writes a symlink action.
  - Each spawn output is classified against the filesystem in spawn order: regular file, directory, or unresolved symlink, otherwise an invalid output path. The first I/O exception sets a process-wide flag, logs the mnemonic and the first output, and close emits that the compact log is incomplete. Later outputs in that spawn also become invalid paths. Zel sorts actual outputs by path and emits them as file ids. Listed paths missing from that list become invalid in listed order. On success the caller copies listed outputs into actual outputs with empty digests and no type check, so directories and symlinks are logged as files. On failure it logs every listed path as invalid and does not warn.
  - Every logged string except runfiles empty files goes through the internal-to-Unicode conversion. Environment order is the order of those internal strings before decoding, which matches UTF-8 order for valid UTF-8. Zel stores and sorts already-decoded UTF-8 and does not do that conversion.

### `src/main/java/com/google/devtools/build/lib/exec/ExecutionOptions.java`
- Bazel lines: 661
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`, with strategy parsing in `/workspace/zel-cursor/crates/zel-exec/src/strategy.rs` and resources in `/workspace/zel-cursor/crates/zel-exec/src/resources.rs`
- Verdict: partial
- Differences:
  - The platform strategy allowlist is parsed and then never read. Bazel installs those label filters, so a platform allow-list that drops every strategy fails the spawn in Bazel and is ignored in Zel.
  - An empty spawn-strategy flag is an empty list in Bazel and therefore the default remote, worker, sandboxed, local, with remote only when an executor is configured. Zel splits the empty string into one empty piece and errors that the flag must contain at least one non-empty strategy.
  - A strategy assignment with no equals sign is a valid value with an empty mnemonic. Zel requires name equals value and rejects it.
  - Strategy regexps are matched with a Java find on the progress message, using UTF-16 code units and an atomic-group union. Zel compiles each piece with the bzlmod engine as a wrapped pattern and requires a full match on Unicode scalars. Case-insensitive flags, word boundaries, lookaround, backreferences, possessive quantifiers, and a newline in the description therefore select a different strategy or fail registration.
  - Flaky-test attempts walk the list from the last flag, apply a real regex filter to the label string, and treat regex-at-default as 1 attempt, or 3 when the test is flaky. Zel returns on the first hit, so an earlier regex beats a later catch-all. It only does a literal or trailing-star prefix check, so an exclusion pattern does not exclude the later package. A leading plus parses in the converter but fails the unsigned parse and the spec is dropped. Regex-at-default is skipped instead of restoring the 1 or 3 default. The converter also splits on the last unescaped at-sign, while the per-label converter uses the first, so a label with two at-signs is accepted by Zel and rejected by Bazel.
  - Streamed test output makes Bazel inject disabled sharding and an exclusive test strategy at software-requirement priority, overriding a user test strategy. Zel disables sharding and runs those tests one at a time, but leaves the test strategy in place, so streamed output plus a sandboxed test strategy is a command-line error in Zel and a local exclusive run in Bazel.
  - The maximum test-output size is integer-decoded, so hex and octal prefixes are accepted. Zel re-reads the raw token as a signed 64-bit decimal, so a leading-zero value is decimal and a hex value fails the parse and becomes unlimited. The cached all-output path compares the limit to the log after stripping the test-setup header. The live path compares the raw file length.
  - Local test jobs are applied on the resource manager, including auto, host-CPU expressions, and the 32-bit range. The test thread pool re-parses as an unsigned size and, on failure or 0, uses jobs. Notest-keep-going forces that pool to 1, so later tests never start. Bazel still admits up to the job and resource limit until a failure is observed.
  - Local resources use the same 0.67 times RAM and last-wins overrides, but the host numbers differ. Bazel uses the operating-system bean and cgroup-aware processor count. Zel uses meminfo and available parallelism, which is affinity rather than the CFS quota. Default CPU and memory caps therefore differ inside a container.
  - Disallowing one action when a resource is unavailable is ignored. Zel hardcodes allowing one, so an action that asks for more CPU or RAM than the machine has still runs. Bazel throws not-enough-local-resource when that flag is off.
  - CPU-load scheduling does not use OS load or the window size. Zel only refuses another action while the sum of estimated CPU is already at the cap.
  - The computed-file-digest cache size is stored and never consulted. Zel always hashes file contents, so a content change that leaves stat metadata unchanged changes action and remote-cache keys.
  - Sorting the execution log defaults to true for the binary and JSON logs and is never read. Zel appends spawns in execution order. If more than one of compact, binary, and JSON is set, Bazel writes only compact, else binary, else JSON. Zel writes every file that is set. The workspace placeholder is not rewritten, and a relative path is opened against the process working directory rather than the client working directory. The compact file is a compressed frame of raw stored blocks, not a compressed stream.
  - Remote-cache eviction retries are not implemented. An evicted result is not retried with a new invocation id.
  - The stats summary and critical-path profiling flags are not read. The modern summary never replaces the default summary, and critical-path profiling cannot be turned off.
  - Materializing parameter files uses the same condition, but Zel only creates parent directories for paths that look like parameter files. The parameter-file bytes are not written, except the Java classpath parameters path. Writing them directly is unused on both sides.

### `src/main/java/com/google/devtools/build/lib/exec/ExecutionPolicy.java`
- Bazel lines: 61
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/strategy.rs` (`dynamic_can_exec_local`, `dynamic_can_exec_remote`)
- Verdict: partial
- Differences:
  - The only producer returns local-only as soon as the spawn may not be executed remotely, and only then checks no-local. Can-run-locally is true unless the locality is remote-only, so a spawn that is banned from both sides still can run locally and cannot run remotely. Zel treats no-local as cannot run locally on its own and a remote ban as cannot run remotely, so both branches are absent and dynamic execution is false.
  - That changes the outcome. The local runner only rejects path mapping, and sandbox can-exec is whether the spawn may be sandboxed, not no-local. With no-local plus a remote ban, Bazel dynamic execution still runs a local strategy. Zel rejects the spawn because it cannot be executed with any available strategy.
  - The failure text also differs when both sides are refused. Bazel cannot observe a policy where both sides are false, so this case is that no usable dynamic local strategy was found and remote execution is disabled. Zel's reason would say neither local nor remote execution is allowed. The public execute path usually fails earlier with the strategy-list error instead.
  - Can-run-remotely matches whether the spawn may be executed remotely. Single-tag cases match: a remote ban only allows local, and no-local only forces remote. The only-local, only-remote, and string-form helpers have no production callers, so their absence does not change results. Key presence is exact string equality on ASCII requirement names. Map order and UTF-8 versus UTF-16 do not change these lookups.

### `src/main/java/com/google/devtools/build/lib/exec/ExecutorBuilder.java`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - There is no executor-lifecycle listener registry. Bazel stores listeners in an insertion-ordered set, keeps the first equal instance, and returns an immutable copy in that order. The execution tool notifies that order. Include scanning registers its lifecycle manager here. Executor-created builds the include-scan pool, and execution-phase start loads include hints and initializes the include parser. Zel never collects or invokes these listeners, so that setup does not run. Adding null is accepted by the set and then copying to an immutable set throws.
  - There is no single action-input prefetcher slot. The field starts null and the getter returns none when unset. A second set throws even for the same instance. Null throws and is not stored. The remote module is the only production setter and installs the remote fetcher only when a combined cache exists. Zel has no such slot, so that remote input prefetch is not installed through this builder.
  - There is no action-execution salt slot. The field starts null and the getter returns an empty string. An explicit empty string is stored and matches unset on get, but a second set, including empty, throws because the field is then non-null. Null throws before assignment. The value is a raw Java string, so unpaired surrogates are kept. The execution tool passes it to the salt setter, which drops all action-execution nodes when the salt changes, and into the persistent action-cache digest. Zel has no such salt, so a salt change does not invalidate skyframe action nodes or the on-disk action cache. The remote module's salt, including remote default exec properties, therefore cannot invalidate prior executions through this builder. Zel's remote cache salt is a different remote-execution field, not this string.
  - Two modules setting the prefetcher or the salt abort executor construction. Zel has no equivalent fail-fast because both values are absent.

### `src/main/java/com/google/devtools/build/lib/exec/ExecutorLifecycleListener.java`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - There is no executor-lifecycle listener. Nothing registers or runs the documented sequence of executor created, then execution phase starting, then execution phase ending.
  - Execution-phase start is specified to take a nullable action graph, null in the merged analysis mode, a nullable supplier of top-level target and aspect outputs, also null in that mode and evaluated only when a listener calls it, and a nullable ephemeral check of whether an output was consumed. Zel never supplies those values at execution-phase start, so those nulls versus a real graph, lazy top-level artifact discovery, and the consumed-output check are unavailable to execution setup.
  - Executor created may abort via a checked abrupt exit. Execution-phase start may abort via an abrupt exit or an interrupt. Execution-phase ending declares no checked exception. Zel has no listener-driven abrupt exit or interrupt at executor creation or at phase start or end.
  - This file has no strings, integers, maps, regexes, or JSON. The nullable references have no option counterpart because the API is absent.

### `src/main/java/com/google/devtools/build/lib/exec/ExpandedSpawnLogContext.java`
- Bazel lines: 385
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/exec_log.rs`
- Verdict: partial
- Differences:
  - Inputs are the sorted spawn input map, including runfiles, filesets, tree files, and virtual inputs. Zel logs declared action input exec paths only, with an empty hash and size, the tool bit always false, no symlink target, and no directory expansion.
  - An empty action input is path plus the tool bit and no digest. Other files get a digest from metadata or from the file. Input symlinks store the unresolved target and no digest. Directories are read without following links, names are sorted with UTF-16 compare, the directory node is omitted, and non-directory children are content-digested. The tool bit is true for tool files, tree files whose parent is a tool, or paths under a tool runfiles tree. Zel writes none of this. A digest I/O error keeps the partial entry and still writes the message. A symlink whose metadata is not a symlink fails the spawn. Zel never hashes.
  - Listed outputs are every output exec path, sorted. Actual outputs are only files, directories, and symlinks that exist, in output-file order, including failed spawns. Zel sorts listed paths and, only on success, emits every declared output with no digest. Failure yields no actual outputs. Zel then sorts actual outputs by path, so order differs from declaration order.
  - Remotable, cacheable, and remote-cacheable follow the no-cache, local, no-remote, no-remote-cache, and no-remote-exec requirements. Zel hardcodes all three true. A non-zero timeout in milliseconds is recorded as a 64-bit value, and overflow throws. Zel never sets it. Platform properties are copied in proto order. Zel leaves the platform unset. The result digest is set when non-null. Zel never sets the action digest, and its writer would label a digest with the uppercased digest function, SHA256 rather than SHA-256.
  - Status is omitted on success and otherwise the enum name, including timeout, out of memory, execution failed, catastrophic failure, execution denied, and remote cache failed. Zel writes an empty string when the exit code is 0 and non-zero exit otherwise. A cache-hit runner is the result name, disk or remote. Zel uses remote cache hit for every cache hit, else the runner or local.
  - Metrics are always the spawn metrics: each non-zero duration and the byte, file, and memory counters, plus start time when present. Zel sets only total time from wall time, and only when the spawn-metrics flag is not false, 0, or no. A zero wall time still emits 0 seconds. Bazel omits a zero total time.
  - Sorting the execution log defaults to true. JSON or sorted logs are unsorted length-delimited binary in a compressed file under the output base, the destination is deleted up front, and close runs a stable sort. The queue is a priority queue, so equal keys are not stable. Zel never reads the flag and writes completion order straight into the final file. Bazel also refuses more than one of binary, JSON, and compact before constructing this class. Zel writes every format that is set.
  - JSON is the protobuf printer that always prints fields with no presence: pretty-printed, proto3 defaults included, messages concatenated with no extra newline. Zel's JSON is compact, omits proto3 defaults, and adds a newline per spawn. Binary framing is length-delimited in both. Missing digests, a true tool bit, platform, timeout, status, and metrics change the bytes. This path is protobuf JSON, not Gson.
  - Environment keys and values pass through the internal-to-Unicode conversion after sorted-map order. Paths, command arguments, listed outputs, and symlink targets are not decoded, so bytes 0x80 through 0xFF are logged as U+0080 through U+00FF. Zel uses Unicode strings and sorts with string order. Valid UTF-8 environment text and bytewise environment order match. Non-ASCII path and argument text does not. A Java string already in UTF-16 sorts by surrogate code units, which differs from Rust scalar order. Environment ties in Zel also compare the value.
  - A conversion or write error fails the command. Deleting the temp file ignores I/O errors. Zel flushes each spawn into the truncated destination, reports write errors as events, and deletes the files if analysis never ran. Logging a symlink action is a no-op on both sides, and the expanded log is not published to the build-event protocol on both sides.

### `src/main/java/com/google/devtools/build/lib/exec/FileWriteStrategy.java`
- Bazel lines: 71
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`execute_action_with`, file-write arm)
- Verdict: partial
- Differences:
  - Bazel writes the deterministic-writer bytes unchanged. Zel, when the exec path ends with the runfiles-manifest suffix, decodes with lossy UTF-8, so invalid UTF-8 becomes U+FFFD, splits on newline and CRLF only, and rewrites each line to end in a newline. A relative value after the first space is prefixed with the execroot. A missing final newline is added. Bytes, CRLF, and non-manifest files that merely use that suffix all change.
  - Bazel opens the input path with truncate and does not create parents or unlink first. A file-output failure is an environmental exception with a file-write I/O detail, exit 36. Zel creates parents, ignores delete errors, then writes the file. A missing parent or an empty directory succeeds in Zel and fails in Bazel. A symlink is replaced by a regular file in Zel. Bazel follows it and truncates the target. A non-writable file in a writable directory is replaced in Zel and fails to open in Bazel. The standard write does not fail the action on a close error. Bazel fails a close failure the same way as a write error. Zel surfaces the I/O error as an error line and a build failure, exit 1, with no file-write detail.
  - Make-executable ORs the executable bits onto the existing mode. A chmod failure fails the action. A new file is the usual create mode, then those bits: umask 022 becomes 0755, umask 002 becomes 0775, and umask 077 becomes 0711. An existing inode keeps its other bits, including setuid and group-write. Zel forces mode 0755, ignores metadata and permission errors so the action still succeeds and the file may stay non-executable, and the block is Unix-only, so non-Unix builds never set the bit.
  - With make-executable false, Bazel truncates in place and leaves the old mode. Zel deletes and recreates, so the mode becomes the usual create mode, typically 0644. A previous 0755 output stays executable in Bazel and is not executable in Zel.
  - The executable bit is applied with the host filesystem on the path string, not through the filesystem that just wrote the bytes, so an in-memory or other non-host filesystem never records it.

### `src/main/java/com/google/devtools/build/lib/exec/ModuleActionContextRegistry.java`
- Bazel lines: 214
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/lib.rs` (the test-strategy rejection)
- Verdict: partial
- Differences:
  - Build keeps, per identifying class, the last registration whose identifier list contains the restriction string. An empty restriction matches every registration. Getting a context is an exact class lookup and returns null when that key was never stored. Zel never builds this map, so it cannot select exclusive versus standalone test strategy by identifier. The test-strategy flag only changes later tag injection.
  - An explicit empty test strategy is a match-all restriction, so the last test-action context wins and build does not throw. Zel treats an empty value as present and not standalone or exclusive, so it prints the unmatched-restriction error and exits with command-line error.
  - The error text is generated. For each unmatched restriction type that has at least one non-empty identifier list, Bazel appends the class simple name, the restriction, and every identifier in registration order. Empty lists do not create a key, so those types omit this line. It then appends unused plus the set difference, whose class strings look like interface or class. Both the per-type lines and that set follow hash-map bucket order, not registration order and not sorted order. Zel always prints one hardcoded test-action-context line with exclusive and standalone, and only when the flag is present and not exactly those two strings.
  - Bazel raises an abrupt exit with the unmatched-restriction code after context registration, so earlier loading or analysis output can already have been produced. The couldn't-start-the-build trailer is added by the test command when the build produced no successful targets. Zel returns that trailer for every command that reaches the check, including build and run, and it returns before analysis with zero processes.
  - A later restriction overwrites the previous restriction for the same class, including resetting it with an empty string. Zel has no such per-type state.
  - A null context, identifying type, or identifier list fails in the record constructor. A null restriction is stored and then throws on emptiness. Identifier match is Java string equality, with no trim and no case fold. Getting a context uses null for a missing type. Zel only has an option for the flag string and compares valid UTF-8 strings, so an unpaired-surrogate restriction cannot occur.

### `src/main/java/com/google/devtools/build/lib/exec/RegexFilterAssignmentConverter.java`
- Bazel lines: 33
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/converters_ext.rs` (`RegexFilterAssignmentConverter`)
- Verdict: partial
- Differences:
  - A character class containing a backspace escape is a valid Java class. The regex filter rejects it because the Java-regex port reports an illegal escape for a word-boundary escape inside a class.
  - A counted repeat above the maximum 32-bit int is an illegal Java repetition. The port parses the bound as an unsigned size and accepts it.
  - Zel deletes a backslash-at before the syntax check and again when converting. Java only rewrites a backslash-comma to a comma, so a key whose text is two backslashes then at matches a backslash followed by at in Bazel and at in Zel.
  - The options converter returns the raw string. Matching uses a full match of a wrapped piece, one piece at a time. Bazel compiles one pattern of atomic alternatives per side and tests find.
  - That engine changes match results. A dot matches carriage return and the Unicode line separators. A subject that contains a newline outside the piece fails the full match while Java find still succeeds. Dollar matches only at the end of the string, while Java dollar also matches before a final line terminator. Word-boundary escapes are the letters themselves. Hex, Unicode, property, and several anchors are literals. A backreference is the digit. Intersection and nested classes are ordinary characters. Java accepts inline flags, lookahead, lookbehind, atomic groups, and possessive quantifiers, and the filter accepts them too, but the download regex then fails or matches a different string. Quoted spans are quoting in Java and literal text in Zel.
  - Syntax errors quote a Unicode-scalar index. The Java exception quotes a UTF-16 code-unit index, so a supplementary character before the failure changes the index and the caret column. The checked text is the wrapped union.

### `src/main/java/com/google/devtools/build/lib/exec/RemoteLocalFallbackRegistry.java`
- Bazel lines: 34
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/strategy.rs` (`SpawnStrategyRegistry::remote_local_fallback`)
- Verdict: partial
- Differences:
  - Getting the fallback filters the registered fallback through the platform filter on the spawn's execution-platform label. If that platform's allowlist does not contain the fallback strategy, the method returns null and local fallback fails because a fallback must be set. Zel ignores the spawn and the allowlist and always runs the configured strategy, so a platform that disallows local still executes it.
  - Every query requires a non-null execution-platform label, even when no platform filter is configured. Zel never reads the spawn, so a spawn with no execution platform still falls back.
  - The interface returns null when no identifier was registered. Zel always stores a strategy kind. An empty fallback flag is not null in Bazel, so registry build fails because no strategy with that identifier was registered. Zel treats empty as local and continues.
  - Bazel accepts the fallback only if it is an abstract spawn strategy. Dynamic is not a subclass, so requesting dynamic aborts setup. Zel accepts dynamic and remote. Fallback execution fails only when the fallback runs, because those strategies cannot execute spawns in this build. Bazel's local execution calls the returned strategy's runner, so a remote fallback re-enters the remote runner.
  - Bazel can return the docker and windows-sandbox strategies. Zel has no strategy kind for them, so those identifiers fail registry build as unknown instead of running in Docker or the Windows sandbox.

### `src/main/java/com/google/devtools/build/lib/exec/RunfilesTreeUpdater.java`
- Bazel lines: 170
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/runfiles_tree.rs` (`build_runfiles_tree`)
- Verdict: partial
- Differences:
  - Bazel runs this from the local spawn runner for every runfiles-tree spawn input, and returns immediately when build-runfile-links is true. Zel never updates runfiles inputs of local actions. It rewrites a tree only for a top-level build executable, test and coverage, and run, and it ignores build-runfile-links.
  - The same exec path is updated once per updater. A finished attempt, including the missing-manifest return, is permanent. Waiters rethrow and do not retry. An error outside the catch leaves the future incomplete, so later joins block. Zel runs the body on every call, with no cross-spawn lock, so two callers can interleave writes and a later call can succeed after an earlier failure.
  - If the runfiles manifest is missing, Bazel returns and does not create the directory. Exists follows symlinks, so a dangling manifest symlink is treated as missing. Zel does not look at that file. It always rewrites the manifest from in-memory entries.
  - In create mode Bazel skips the rebuild when the manifest is not a symlink and the digests match, including an xattr fast digest. An I/O error during that check is discarded and the tree is rebuilt. On non-Windows a matching regular manifest is enough even if the symlink tree is empty. On Windows it also requires the directory to be populated. Zel has no digest or xattr check and always reconciles the tree, so a stale xattr match leaves Bazel's symlinks stale while Zel replaces them.
  - That Windows check reads the output manifest as ISO-8859-1, one code unit per byte, drops newline forms, then splits on space and takes the first field. A leading space yields an empty field, and a relative empty path is the runfiles directory itself. An absolute first field is stated outside the tree. Exists without following links counts a dangling symlink as present. An empty file makes the read return null and throws, which is not caught. Zel never parses the manifest.
  - Runfile symlink mode is create for enable-runfiles yes, or auto when the OS is not Windows, and skip otherwise. Zel treats the flag as false only for 0, false, and no. Auto and an unset flag are true on every OS, so Windows defaults to a full tree in Zel and a minimal tree in Bazel.
  - Skip keeps the runfiles directory, deletes trees below it, symlinks the manifest at the input manifest, and creates the workspace subdirectory. Zel's disabled path writes the manifest file and then removes the runfiles directory, and does not recreate the directory, the workspace subdirectory, or a manifest symlink.
  - Create deletes the output manifest and creates a symbolic link to the input manifest, so the manifest is a symlink and those bytes stay the source-manifest text: ISO-8859-1, absolute targets, and an escape when a path contains a space or newline. Zel writes both manifests as regular files, one path, space, target, newline line per sorted-map entry, with no escaping. A space or newline in a path splits differently, and overwriting the input manifest replaces an action output. Symlink versus regular file also changes readlink, the digest, and the Windows copy check.
  - Symlink targets in Bazel are the artifact path, or for a symlink artifact the unresolved link text, which is often relative. Equality goes through path-fragment normalization. Zel uses the entry if it starts with a slash, otherwise joins the exec root. Lossy conversion puts U+FFFD into the link text when the exec root is not UTF-8. A Windows absolute path does not start with a slash and is joined onto the exec root. Path equality is byte-wise, so a path with a dot segment is rewritten.
  - Bazel path and manifest bytes are ISO-8859-1. Zel writes UTF-8. The byte 0xE9 stays one byte in Bazel and is the two-byte sequence C3 A9 in Zel. A non-UTF-8 path cannot be a Rust string.
  - An empty mapping value becomes an empty file, and an existing regular file is left untruncated in both. Bazel deletes a symlink, directory, or special node first and then creates the empty file. Zel removes a symlink, then writes only when the path does not exist. A directory or a node that remove-file cannot delete is left as it is.
  - Bazel replaces a non-directory that should be a directory, chmods 0755 when permissions are unset, and otherwise ORs in 0700. Stale entries are deleted as trees, and an I/O failure fails the spawn. Zel create-dir-all fails when a parent component is a file, does not chmod, and ignores remove errors, so a non-empty stale directory can remain. The Unix-only symlink omits links on Windows, so non-empty entries are never linked there.
  - Bazel rejects an empty or absolute mapping key and caches that failure. Zel's join of an absolute key drops the runfiles directory and writes that absolute path. An empty key refers to the directory itself.
  - Zel adds a legacy external path when that option is set and the key does not start with the workspace or an underscore, keeping an existing value. The updater only materializes the tree mapping. Zel always writes the repo mapping, including an empty file, as source, app, canonical lines, after any symlink at that name, so the path is a regular file. Bazel has the repo mapping only when the mapping contains it, as a symlink to the repo-mapping artifact.
  - The existence, symlink, and digest checks use the exec-root path, which can be an action-filesystem overlay. The helper then writes through the host filesystem. Zel only sees the host filesystem.

### `src/main/java/com/google/devtools/build/lib/exec/SingleBuildFileCache.java`
- Bazel lines: 189
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`path_digest`)
- Verdict: partial
- Differences:
  - Bazel keeps one unbounded cache entry per exec path for the whole command and replays the first file value or the same I/O exception. Zel's computed digest cache stays unset, so the path digest re-stats and re-reads every call. Invalidating it does not change results. A source file that appears, disappears, or changes mid-build stays frozen in Bazel and is observed live in Zel.
  - Derived artifacts throw before any I/O. Non-artifacts whose exec path starts with the relative output path, at a segment boundary, throw that the cache does not support an action input in the output tree. Zel hashes those paths like any other input.
  - A followed stat that is not a regular file is stored as a directory exception and never given a digest or modification time. Zel hashes directory trees and reads special files, which can yield bytes, nothing, or a blocking read.
  - The directory walk's relative path is a strip of the base path with no extra separator, so a child is recorded with a leading slash. Those bytes are what Zel puts in the metadata digest. Bazel contributes no directory digest at all.
  - Missing, dangling, or unreadable paths throw a cached I/O exception. The action-cache checker turns that into null metadata, exec path only, no digest bytes. Zel returns nothing, and the metadata digest records the literal missing, so the action-cache digest differs and the lookup does not fail.
  - Readable files use a fast digest when present, otherwise a content hash. An unreadable file can still return a filesystem or xattr digest. Zel always hashes the file bytes and never consults xattrs, so a stale or privileged fast digest and a chmod-0 file diverge. Bazel then fingerprints the raw digest bytes. Zel fingerprints the hex text.
  - Artifacts are stated at the artifact path, the source root, not the exec root plus the exec path. Other inputs use the exec root relative to the exec path. Zel always digests execroot-relative paths, so a source that is not linked into the execroot is real in Bazel and missing in Zel.
  - Tree, enclosing-tree, fileset, and runfiles metadata are always null, and the fileset and runfiles collections are empty. This cache never expands trees, filesets, or runfiles. Zel has no such provider, so a directory path is content-hashed instead.
  - Getting an input returns the first action input that filled that exec path, or null if never looked up. Two inputs with the same exec path share one stat and one exception. Zel keys the absolute path string it was given and does not preserve the input object.
  - Cache keys and the output-tree check use Bazel's internal path string, Latin-1 bytes on Linux. Zel path fragments are UTF-8 strings, and directory names from readdir go through lossy conversion, so non-UTF-8 paths change the key and the directory digest. Name order is UTF-8 bytes, not UTF-16 code units.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnCache.java`
- Bazel lines: 181
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs` (`lookup_cache`, `upload_result`)
- Verdict: partial
- Differences:
  - There is no spawn cache or cache handle. The no-result-no-store handle has no result, store returns without doing anything, and getting the result throws. The success handle has a result, will not store, and store throws. The type comment says a miss throws. Zel returns a hit as the spawn result and does not upload it. Uploading returns with no error when the write policy is off, so a store after a hit does not fail the action.
  - Useful in dynamic execution defaults to true. Callers replace the cache with no cache when a speculative spawn sees false, which is the remote cache. Zel has no switch. The strategy still looks up and uploads on the dynamic local branch whenever a cache is configured, so that branch can win on a disk or remote hit and can upload outputs Bazel will not touch while speculating.
  - Lookup and store may throw an execution exception, an I/O exception, or an interrupt. An interrupted store must return quickly with an interrupt. The abstract strategy turns an I/O exception into an exec I/O exception and aborts on interrupt. Zel warns on a remote lookup error and treats it as a miss, then runs locally. Upload failures are warnings and the local success still stands. Neither path checks cancellation, so an interrupted build can still execute and upload.
  - A successful lookup must write output files, stdout, and stderr only after locking outputs. Zel locks with the exit code, then writes output files when applying the result. Stdout and stderr stay on the spawn result and are not written to the action streams during lookup, so those logs can be missing or attached later.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnCheckingCacheEvent.java`
- Bazel lines: 38
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs` (`UiStateTracker::checking_action_cache`)
- Verdict: partial
- Differences:
  - Posting posts a caching action event, and setting caching always ORs the name into the process-wide strategy bitmap, then sets the caching phase only when the current phase is earlier. The progress suffix while caching or running is a semicolon, seconds, and the name, with the time shown even at 0 seconds because a strategy is present. Zel drops the strategy and only advances the phase when it is still below caching, so the line never gains that suffix from a cache check.
  - No caller passes the caching phase. Bazel reports remote, disk-cache, and remote-cache before those lookups. Zel already sets running with local or internal when the action starts, and a later cache check would not move the phase backward and would not record the cache strategy.
  - Entering caching resets the start time to the event time. Zel leaves the start time at action start, so the elapsed seconds differ.
  - A null name throws before post. An empty string is kept and shown. A string reference cannot be null, and the caching path does not validate or store the name.
  - Distinct names share a 32-bit bitmap whose bit 0 is the unknown fallback. The 32nd id shifts into the sign bit, and formatting names then spins because the mask shift becomes 0. Later names collapse to unknown. Zel would keep separate strings in insertion order, but the caching path never appends them.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnExecException.java`
- Bazel lines: 80
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (non-success spawn status)
- Verdict: partial
- Differences:
  - The three-argument constructor sets catastrophe from the result, which is true for catastrophic execution failure or denial. That flag is copied into the spawn-action exception and aborts the build even with keep-going. Test execution rethrows it instead of recording an attempt. Zel's status has no catastrophic status, so the same failures are ordinary action failures and keep-going continues. The four-argument constructor can force catastrophe independently of status. Zel cannot.
  - The failure detail ignores the action description and returns the result's failure detail unchanged. The detailed exit code then uses that proto's metadata exit code: non-zero exit, timeout, and out of memory are process exit 1, execution failed and remote cache failed are 34, remote cache evicted is 39, the I/O exceptions are 36, and spawn unknown is 37. A null detail throws whose message is this exception's string form. Zel keeps the raw spawn exit code on the action outcome and the build-event result, and always builds a non-zero-exit detail, including timeouts, with a message that names the mnemonic and the exit code when no error string is set. The exec-log status is non-zero exit whenever that code is non-zero. The command exit is then forced to build failure, so Bazel 34, 36, and 39 spawn failures become 1. The proto spawn exit code and catastrophic fields are not set.
  - The message for the action exception uses the result's detail message. The simple result ignores the catastrophe argument. The status token is a termination status: timeout is timeout, a normal exit is exit N, and a non-timeout code in 129 through 191 is a signal name, so 142 is alarm clock rather than exit 142. Zel prints timeout only for the timeout status and exit N for both non-zero exit and execution failed, including 129 through 191.
  - A null or empty message omits the colon separator, leaving only the status token. Zel always appends the tool-failed sentence.
  - On timeout, Bazel appends a sentence with the wall time when the wall time is non-zero, formatting a 32-bit millisecond value divided by 1000 with a US locale and half-up to two decimals, or a sentence without the duration when the wall time is 0. That int overflows to a negative duration after about 24.8 days. Zel's timeout line has neither sentence. Its wall time is an unsigned 64-bit value and is not interpolated.
  - Out of memory appends that the remote action was terminated due to out of memory. Zel has no out-of-memory status, so those results are non-zero or execution-failed exits without that sentence.
  - When the spawn was forcibly run remotely and the status is not timeout, Bazel appends that an action tagged as local was forcibly run remotely. The only production constructor call passes false. Zel has no such flag.
  - Timed out is the timeout status. Tests use that for the timeout test status versus failed, and a setup that is not success, non-zero exit, timeout, or out of memory is rethrown so the attempt is not a failed test. Zel sets timed out from the timeout status and also when the log contains the bytes of a truncated "action timed out" prefix, and it drops log lines that start with that sentence after a lossy UTF-8 decode. Missing-input and execution-failed spawns become test attempts instead of a setup failure.
  - The exception message is a Java string, so an unpaired surrogate survives in the exception text. A protobuf failure-detail string is UTF-8 and replaces unpaired surrogates when serialized. Zel builds the text as a UTF-8 string. The default failure decoder uses lossy UTF-8, so invalid UTF-8 becomes U+FFFD before the detail message is chosen. The public constructor rejects success. The test constructor does not. Zel never wraps a success result as a failure.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnExecutingEvent.java`
- Bazel lines: 44
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/observability.rs` (`ActionObserver::action_started`)
- Verdict: partial
- Differences:
  - Posting posts a running action only when a spawn runner is about to execute: after resource acquire for the local and sandbox runners, after the worker is borrowed, and for remote only once the execute operation reaches executing. Zel marks running inside action start, before cache lookup and before a runner is chosen, including action-cache hits that never spawn.
  - The strategy string is that runner name. Zel always passes local when the action is a spawn and internal otherwise, so the progress strategy does not follow sandbox, remote, or worker execution. The final runner name on the spawn result is a different field and is not fed back into this phase.
  - A second runner posts another name. Bazel ORs names into a process-wide 32-bit strategy bitmap and prints them in first-seen id order. The id 0 slot is unknown. The 32nd distinct name and every name after it collapse to unknown. Zel appends a strategy only if that exact string is not already on the action, and nothing calls running again, so only one name is ever shown and there is no 32-bit overflow.
  - Entering running replaces the start time, so the progress elapsed seconds start at this event. Zel stamps the start at action start and running does not reset it, so the shown seconds include queue and cache time.
  - Reporting skips the post when the owner or the primary output is null. Zel still emits running. The tracker id is the first output path, or empty when there are no outputs, so those actions appear and can share one slot.
  - The compact constructor rejects a null name. An empty string is allowed and would be posted unchanged. Zel's string cannot be null, and the call site only passes local or internal.
  - The inactivity status reporter prints running plus the strategy from this event. Zel has no equivalent dump. Progress text also differs: Bazel appends the strategy with no parentheses, and Zel renders it in parentheses.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnInputExpander.java`
- Bazel lines: 247
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (spawn inputs) and `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs` (`expand_dir`)
- Verdict: partial
- Differences:
  - The input mapping is a tree map keyed by the hierarchical path comparator, so a slash mismatch sorts the path that contains a slash first. Zel keeps a vector of path fragments in analysis-input order. Action-cache keys sort those strings with UTF-8 byte order, so a hyphen sorts before a slash. The hierarchical compare in the virtual filesystem matches Bazel and is not used for this map. Remote Merkle children are sorted by filename per directory, not as one hierarchical flat map.
  - Non-empty tree artifacts are replaced with declared children only. Each child is keyed as the mapped parent exec path plus the parent-relative path, which is not the same as mapping the child's own exec path when the mapper is not a pure prefix rewrite. Zel puts the tree's exec path in the spawn inputs unchanged. The sandbox symlinks that one path, so the sandbox sees the whole directory, including undeclared files. Remote expansion walks the filesystem following links, sorts by OS string, turns non-UTF-8 names into U+FFFD, skips children whose metadata fails, and if reading the directory fails adds nothing.
  - With empty trees kept, a tree whose metadata is null or whose children are empty is kept so consumers create the directory. Parents that already contributed a tree-file input are removed, then appended, so a colliding path overwrites. Zel treats a missing path as a missing input and does not insert an empty-directory input. Remote emits an empty directory only when the directory read succeeds and is empty.
  - Expanding archived tree artifacts applies only inside runfiles mappings. When it is false and an archived artifact exists, the archived artifact is stored at the unmapped root plus the mapping key and children are not expanded. The default constructor sets the flag true, so archived artifacts are ignored. Zel never substitutes an archived tree. The mnemonic filter is only a config option.
  - Runfiles trees expand the sorted mapping. A null artifact becomes an empty marker. Nested runfiles artifacts throw. Tree artifacts and filesets inside the mapping are expanded as above. The map includes every runfile and does not add the manifest. Zel does not expand runfiles into spawn inputs. It materializes a tree on disk, and the test runner copies exec inputs only, so sandbox and remote input sets omit runfiles and empty-file markers.
  - Mapping for runfiles returns the exec path when the path mapper is a no-op. Otherwise the runfiles directory basename must end with `.runfiles`. The sibling executable is the basename minus the 9-unit suffix, mapped, then the original basename is restored and the entry is appended. The suffix is ASCII, so UTF-16 length matches Unicode scalars. Replacing the name is null when the runfiles path has no parent, and the following call throws. Archived trees and filesets are not path-mapped. Zel has no path mapper on spawn inputs.
  - Filesets become one entry per output symlink, with no path mapping. Top-level filesets use the fileset exec path as the location. Zel has no fileset input expansion.
  - Every key is the base directory plus the target. An absolute target or an absolute runfiles root fails before insertion. Zel path fragments accept absolute paths, rewrite backslashes, drop dot and empty segments, and pop parent segments.
  - Duplicate keys keep the last write. Runfiles entries are visited in path-fragment compare order, which is lexicographic normalized string, not hierarchical. Top-level inputs follow nested-set order, then empty trees. Zel keeps duplicate paths in the vector. The sandbox skips a path whose destination already exists, so the first wins. Remote Merkle errors when the same path has different file or symlink nodes and ignores identical duplicates.
  - The Bazel map never stores null. Non-artifact inputs pass through artifact expansion and are keyed with the mapped exec path. Zel paths are plain strings. There is no empty-marker input distinct from a real zero-byte file.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnLogContext.java`
- Bazel lines: 272
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/exec_log.rs`
- Verdict: partial
- Differences:
  - Environment variables are sorted by the internal Latin-1 key and then decoded as UTF-8, replacing malformed sequences with U+FFFD. Zel sorts the already-decoded pairs by UTF-8 byte order and writes them unchanged. Valid UTF-8 keys match in order. Invalid UTF-8, replacement collisions, and duplicate names do not, because Bazel orders by the pre-conversion key only.
  - The platform copies the platform proto: properties sorted by name with UTF-16 compare, names and values passed through the Unicode conversion, and a user execution exception if the remote execution properties text fails to parse. A null or empty platform is omitted. Zel copies a caller-supplied vector with no sort and no re-encode, and the only call site leaves it empty, so exec properties never appear and a bad platform does not fail the log.
  - An input is a directory when the input says so, not for symlinks even when the target is a directory, not for every virtual input, and for a source artifact only when metadata is non-null and the type is a directory. Anything else, including a null metadata result, is a file. Zel drops the directory bit on inputs and never stats, so a directory is a path-only file with no children and no digest.
  - The digest uses a virtual-input stream hash, else a cached metadata digest, and on an I/O or illegal-state exception falls through to a stat plus an xattr fast digest or a content hash. The hash is lowercase hex, the size is the metadata or stat length, and the hash-function name is set only when requested, from the digest function's string form. Zel never hashes inputs or outputs, so the file hash stays empty and the digest message is omitted. When a digest is built, the name is the raw digest-function flag uppercased, default SHA256, which is not the canonical SHA-256.
  - Spawn metrics emit a duration only when the 32-bit milliseconds are non-zero, via a duration that truncates toward zero. It always sets the byte, file, memory, and limit counters, and sets the start time from a non-null instant. Zel logs only total time, from an unsigned 64-bit wall time cast to signed, and only when total time is present. Non-negative values that fit in a Java int match. A zero still becomes an empty duration submessage. A negative value uses Euclidean division, so minus 1500 milliseconds is seconds minus 2 and nanos 500000000 instead of seconds minus 1 and nanos minus 500000000. Times above the maximum 32-bit int wrap in Bazel's int before this method. Parse, network, fetch, queue, setup, upload, execution-wall, process-outputs, retry, time-limit, start time, and the size and memory counters are absent. Turning spawn metrics off drops them entirely. This method always returns them.
  - Logging a symlink action, close, and whether to publish are abstract. Zel logs only spawn actions and never writes a symlink-action entry.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnLogReconstructor.java`
- Bazel lines: 570
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No reader turns a compressed, varint-delimited execution-log stream into expanded spawn messages. The nearest writer emits JSON and binary spawn records from in-memory records and a separate compact log. It never reads a compact log back, and there is no parser or converter.
  - Read returns null at end of file. An unset or unknown entry throws with the type number. A symlink action is skipped and produces no spawn record. Zel does not implement that filter.
  - Decompression is a full compressed stream, including compressed blocks, checksums, and multi-frame streams. Zel's store and unstore only round-trip a single-segment raw-block frame with an 8-byte content size and no checksum. Unstore rejects non-raw blocks, so a Bazel compact log does not decode.
  - Input, directory, unresolved-symlink, runfiles, input-set, and symlink-entry-set entries must use consecutive 1-based ids. Id 0 is not stored. A missing or wrong-typed entry throws. A 32-bit id above 2^31−1 is a negative Java int, so getting it throws. Spawn and symlink-action entries are not inserted into the id map. Zel assigns an id to every compact entry, including each spawn, so any entry after the first spawn breaks this consecutive-id check.
  - Stored file values are rewrapped on read: an empty symlink target is a file, and any non-empty target is a symlink. Digest and size are not part of that test.
  - Spawn inputs are the postorder walk of the input set only, inserted into a tree map keyed by path with UTF-16 compare. The same walk of the tool set only records paths in a hash set. The tool bit is set when a path is in both. A tool that is only in the tool set is dropped. Zel puts tool files only in the tool set and omits them from the input set, so this reconstruction would drop them, while Zel's own expanded log keeps them and sets the tool bit.
  - Visiting an input set and symlink entries is a two-visit postorder. A set id already present is not queued again. On the second visit, a repeated direct input id is skipped. Zel's compact input set is one flat id list with no transitive set ids.
  - Listed outputs are a tree set of each output file or symlink path, each directory's own path, and each invalid path, in UTF-16 order. Actual outputs follow the output list, and a directory is replaced in place by its children in stored order, not sorted. Zel sorts both lists by UTF-8 byte order and records a listed path that is not an actual output as invalid. It never expands a directory or runfiles tree.
  - Arguments, environment, label, mnemonic, exit code, status, runner, cache flags, and timeout are copied in proto order, including a zero timeout. Platform and metrics are copied only when present. Zel sorts the environment before writing either format, omits a zero timeout, writes no platform and no metrics on the compact spawn, and on the expanded log synthesizes only total time from integer milliseconds when spawn metrics are enabled.
  - Digests on reconstructed files and on the spawn digest get the hash-function name from the latest invocation, overwriting any name already on the digest. The invocation also sets the workspace runfiles directory and sibling layout for every later runfiles tree. Zel writes one invocation up front, hardcodes the workspace runfiles directory to `_main`, never sets sibling layout, and stamps the hash name at write time from the digest-function flag.
  - Runfiles trees are rebuilt into an insertion-ordered map. Re-putting a path replaces the file and keeps the original key position. Override order is symlinks, then canonical artifacts, then empty files, then root symlinks, then the repo mapping, then the runfile marker when nothing has yet established the workspace runfiles directory. Zel has no runfiles-tree, symlink-entry-set, or directory compact entries and does not rebuild this listing.
  - Non-root symlink paths that start with `../` drop three UTF-16 code units. Other non-root paths are prefixed with the workspace runfiles directory. Root symlink paths are used as-is. A target id of 0 becomes an empty file. A directory target rewrites each child by UTF-16 length, which throws when the directory path is longer than the child path.
  - Canonical artifact paths come from regex extraction. Sibling layout tries two patterns, then the default layout tries two more. Matching is a full match. Named groups are UTF-16 code units because the patterns are not compiled with the Unicode character class. A dot does not match the line terminators. A path that matches neither pattern throws. A null repo group yields the workspace directory plus the path. A non-null repo yields only the repo plus the path.
  - The repo-mapping entry is stored under the key `_repo_mapping`, while the file path appends that name. Its digest is copied without the hash-function name. The runfile marker is an empty file under the workspace runfiles directory.
  - Path prefix tests, substring, tree-map and tree-set order, and regex matching are code-unit operations. This file does not convert Latin-1 and does not parse JSON. Unpaired surrogates sort and match as single units. Zel string ordering is UTF-8 bytes and cannot hold unpaired surrogates. For well-formed BMP text the orders match.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnProgressEvent.java`
- Bazel lines: 46
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs` (`UiStateTracker::action_progress`)
- Verdict: partial
- Differences:
  - There is no event value and no post. Bazel builds a progress event and posts it from the abstract strategy and the remote download reporter. Zel only has the UI sink, and no crate calls action progress, so spawn and download progress text is never applied.
  - Concurrent progress ids: Bazel keeps every unfinished id in an insertion-ordered map and shows the oldest. A finish removes only that id, so the next oldest and its last message become visible. Zel stores one optional pair. A later unfinished event overwrites the visible text. Finished clears the slot only when the id matches, and drops any id that was overwritten, so finishing the visible id clears the suffix while other downloads are still running.
  - The compact constructor throws if the progress id or the progress text is null, and the action progress event throws if the action is null. Zel takes string references and leaves state unchanged when the id is not active. Empty strings are kept on both sides, and an empty progress string is displayed as the progress id.
  - The suffix width uses string length and substring, which are UTF-16 code units, and the cut can split a surrogate pair. Zel counts scalar values and takes that many characters. Non-BMP progress text can truncate at a different point or not truncate. BMP text matches. Both truncate only when the remaining width is at least 7.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnRunner.java`
- Bazel lines: 318
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs` (`SpawnRunner`)
- Verdict: partial
- Differences:
  - The Rust trait has only name, can-exec, and exec. There is no handles-caching and no speculating. Bazel turns the outer spawn cache off when the runner handles caching, and again when speculating and the cache is not useful for dynamic execution, and it acquires local resources at dynamic-standalone priority while speculating. Zel cannot express either switch, so a dynamic local branch is not cache-bypassed or reprioritized that way.
  - The local and remote can-exec methods always return true. Bazel local rejects path-mapped spawns, and remote can-exec is whether the spawn may be executed remotely. A direct trait caller therefore runs those spawns here, and remote then falls back inside exec. The path resolver defaulting to identity has no override, so mapped output paths are written at the exec-root path.
  - Exec must throw an interrupt, an I/O exception, or an execution exception, and must return a spawn result for a natural non-zero exit. Zel returns an I/O result and does not watch the thread interrupt flag, only the cancel hook. Missing inputs, an empty argument vector, and remote materialize failures are success with a missing-inputs or execution-failed status and exit code 1, so failure detail, rewind, and build status change. Prefetch is absent. Bazel waits under the remote-download profile, rethrows a cause that is an I/O exception, an execution exception, an interrupt, or a runtime exception, wraps any other cause in an I/O exception, and on interrupt cancels the future before rethrowing. Zel materialize errors become that execution-failed result and nothing cancels an in-flight prefetch.
  - Locking outputs takes the exit code, the error message, and the output streams, and throws an interrupt for every caller but one. The hook takes only a 32-bit exit code and returns a bool. The stock signal check is replicated, but stderr and error text cannot force a yield. Standalone local execution locks with exit code 0 before the process starts, so a later ignorable signal cannot drop the lock. Sandbox execution locks after exit with the real code. The subtraction panics in a debug build if it overflows.
  - The id is a 32-bit atomic int that wraps to negative after 2^31−1. Zel sandbox ids are an unsigned 64-bit counter. After that many spawns in one server, Bazel sandbox directory names go negative and Zel's do not.
  - Setting and getting the digest, and the spawn-result builder, are absent. The builder sets the runner name and the nullable cache digest, and a second call with a different digest throws. Zel's result has a runner name and no digest, and runner names are local, remote, remote-cache, disk cache hit, and remote cache hit rather than the runner name plus a separate cache-hit flag. Spawn logs and digest consumers therefore see no action digest.
  - Timeout is not on the context. Runners trim, parse a float, and keep only a positive value. Bazel fills the context from an integer parse with no trim, and a number-format exception becomes an invalid-timeout user exception. A fractional, exponential, or padded value fails the spawn in Bazel and becomes a timeout in Zel. A negative value is a negative duration in Bazel and no timeout in Zel. A value above the maximum 32-bit int throws in Bazel and can panic when building a duration. The interface also requires an illegal-argument exception when the timeout exceeds an implementation maximum, rather than clamping.
  - The input mapping returns a sorted map ordered by the hierarchical path comparator, optionally memoized, and expands tree artifacts, runfiles, and filesets under the base directory. Zel passes a vector of declared exec paths. Collecting inputs walks directories and reads nested lazy outputs from a hash map. Merkle siblings are re-sorted by UTF-8 byte order, which is the opposite of the slash rule for paths such as `a/b` versus `a-b`. Directory names go through lossy conversion, so non-UTF-8 bytes become U+FFFD and can collide, while Bazel path strings hold those bytes. The client environment is an immutable map. Zel stores it in a sorted map. UTF-8 byte order and UTF-16 compare differ for non-BMP keys, which changes remote command environment order and the action digest.
  - Rewinding and the lost-input check are absent. The context method throws a lost-inputs exception when rewinding is on and lost inputs were detected. Zel only checks declared inputs up front: if rewinding is off it returns a normal action failure, and if it is on it does not throw and still executes. The action filesystem and the context lookup have no counterparts, so lost-input checks on an action filesystem are not run from the runner. Progress status is absent, so a cache-miss progress event cannot start the other dynamic branch. Zel uses a fixed local delay instead, which changes which branch publishes outputs.
  - Cleaning the sandbox base defaults to a no-op and, when overridden, must only remove entries this build may have generated and must throw. The trait has no method. The sandbox runner removes every child of the runner directory, then removes that directory and the sandbox base when empty, and ignores per-entry errors. A cleanup I/O error fails the Bazel command and does not fail the Zel command, and an empty sandbox base is removed.
  - The environment contract is only the spawn's variables, plus an optional absolute temporary directory when unset. Rewriting the local environment drops any spawn temporary directory and sets the client value or `/tmp` even when that value is relative. Local execution inserts a default path when path is absent. Both change the process environment and therefore outputs.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnSchedulingEvent.java`
- Bazel lines: 39
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs`
- Verdict: partial
- Differences:
  - This is a progress-status record of one strategy name. The compact constructor throws when the name is null. Create uses that constructor. An empty string is stored and posted. Posting always posts a scheduling action and does not check the action. Zel's scheduling update only updates an id that action start already inserted, ignores any other id, and appends an empty strategy.
  - Spawn runners report this before they wait on resources. Local and sandbox runners pass their name. Remote posts the shared remote event after input upload and before remote execution. Workers post multiplex-worker when multiplex and not speculating, otherwise worker. Reporting drops the post when the action owner or the primary output is null. Zel's spawn runner has no progress hook. Action start sets running immediately with local for a spawn and internal otherwise, so the progress line never enters scheduling and never records those runner names.
  - On delivery, Bazel ORs the name into the strategy bitmap even when a later phase blocks the change, and it updates the start time only when the phase actually advances to scheduling. The scheduling line is the status truncated to 5 UTF-16 code units, with no strategy in the postfix. The stored name appears later, joined in low-to-high bit order, and only while caching or running. The 33rd distinct name collapses to unknown. Zel advances only while the phase is below scheduling, leaves the start time unchanged, and appends the name when string equality fails. The action description then prints that name in parentheses on every non-caching, non-running line, in append order, with no 32-name cap.
  - Strategy identity is Java string equality. Zel compares UTF-8 strings. The runner names above are ASCII, so those comparisons match. A name that contains an unpaired surrogate can be posted here and cannot be a Rust string. When a non-ASCII name is rendered, Bazel measures width with UTF-16 length and Zel with Unicode scalar values.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnStrategyPolicy.java`
- Bazel lines: 93
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never builds or applies this filter. Building the strategy registry uses the spawn strategy, strategy, genrule strategy, strategy regexp, and the dynamic strategy lists as given. Bazel runs three independent policies from the invocation policy: the mnemonic policy on mnemonic lists, explicit defaults, and the reversed registration-order implicit defaults, plus the dynamic local and remote policies on those dynamic lists. A regexp match is filtered again per mnemonic at lookup time. An empty post-filter list falls through to the mnemonic or default list. Zel resolves regexp strategies once, with no per-mnemonic allowlist.
  - An empty mnemonic policy is a no-op: both apply overloads copy the input, keeping order and duplicates. Any other policy, including one that matches the default on known fields but has unknown fields, becomes a filter. With both allowlists empty that filter drops every strategy. Zel ignores the policy, so those strategies still run.
  - The strategy allowlist is a mnemonic-to-set map. A repeated mnemonic keeps the last strategies-for-mnemonic entirely, not a union. An empty strategy list allows nothing for that mnemonic and does not fall through to the default allowlist. Unlisted mnemonics, and apply with no mnemonic, use only the default allowlist. A policy that lists only genrule and leaves the default allowlist empty therefore leaves every other mnemonic with no strategies. Zel does not do this.
  - Filtering is set membership on the requested list: exact string equality, input order and duplicate requested names preserved, allowlist order irrelevant, and names present only in the allowlist are not added. Proto strings are well-formed UTF-8, so this is not a UTF-16 versus UTF-8 split for policy data. Bazel removes disallowed names before can-exec, which can select a later strategy or fail the spawn, while Zel still attempts the full list.
  - The policy is not stored on Zel's invocation policy. Binary decode skips invocation-policy field 2. Text parse type-checks the strategy policy and then discards the message, so a valid default allowlist does not change execution.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnStrategyRegistry.java`
- Bazel lines: 768
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/strategy.rs` (`SpawnStrategyRegistry`)
- Verdict: partial
- Differences:
  - Test-runner is exempt from the strategy regexp. Zel applies description filters to every mnemonic, so a matching filter changes the test strategy.
  - A null progress message skips regex selection. Zel rewrites an empty progress message to the mnemonic plus the owner and matches that synthetic string.
  - The invocation strategy policy is not applied. Bazel filters mnemonic, regexp, dynamic-local, dynamic-remote, and default identifier lists, and a regexp hit that sanitizes to an empty list falls through to the mnemonic or default. Zel keeps the raw flag lists, so disallowed strategies still run and a policy-emptied regexp does not fall through.
  - The platform allowlist is stored and never consulted. Bazel intersects the chosen list with the platform allowlist and preserves candidate order, including dynamic branch lists and the remote-local fallback. A platform with no overlap yields no strategy. The same label registered twice keeps the last list. A null execution platform throws. Zel does not.
  - Remote-local fallback is always a strategy kind, and an empty flag means local. Bazel leaves it null unless the remote module sets it, applies the platform filter, and rejects a non-abstract spawn strategy such as dynamic. Zel accepts dynamic as the fallback.
  - On Linux, sandboxed is the last registered sandbox, normally linux-sandbox, when that runner works. Detection always reports linux-sandbox unsupported, so it is never registered and sandboxed is the process-wrapper sandbox. Docker and windows-sandbox are absent, so those identifiers fail lookup. On Windows, Bazel's default list omits sandboxed even if a Windows sandbox is registered. Zel adds sandboxed whenever any sandbox kind was registered.
  - The strategy regexp uses a Java regex filter: each piece is an atomic group, the union is an unanchored find on UTF-16 code units, and a dot does not match a newline. Zel compiles a wrapped pattern and full-matches Unicode scalars with its own engine. Results change when the description contains a newline, when a dot or anchors count a non-BMP character as one scalar instead of two UTF-16 units, when dollar should match before a final newline, or when a pattern needs backtracking out of a greedy quantifier. Java word boundaries, Unicode escapes, backreferences, lookahead, and possessive quantifiers are real operators. Zel treats unknown escapes as literals or rejects the flag. Unknown-strategy errors name the wrapped filter in Bazel and the raw flag text in Zel.
  - Resolve's failure text lists Zel names. Bazel's resolver prints the strategy objects, except dynamic, and, when the spawn uses path mapping, inserts a clause that the action requires sandboxing. Zel's worker can-exec is only the supports-workers requirements. Bazel's worker runner also requires a non-empty tool-file set and membership in the worker mnemonic allowlist, so Zel can select worker where Bazel skips it.

### `src/main/java/com/google/devtools/build/lib/exec/SpawnStrategyResolver.java`
- Bazel lines: 100
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/strategy.rs` (`SpawnStrategyRegistry::resolve`)
- Verdict: partial
- Differences:
  - The no-usable-strategy failure message is a Java text block, so it ends with a newline. Zel's string does not. Bazel also inserts a clause that the mnemonic requires sandboxing due to path mapping when the path mapper is not a no-op. Zel has no path mapper and never inserts that clause.
  - The candidate list is the string form of the unfiltered strategies. Dynamic prints dynamic. Zel joins the strategy-kind name, and dynamic prints DynamicSpawnStrategy. Worker, remote, standalone, and the Unix sandbox names match. Docker and windows-sandbox can be chosen in Bazel. Zel has no such kinds.
  - Bazel throws a user execution exception with the no-usable-strategy code. Zel returns a string error, and the exec loop reports an error line with no spawn failure-detail code.
  - A matching strategy regexp posts a progress event with the description and the context identifiers inside getting strategies, before can-exec. If every candidate then fails can-exec, that progress line is already emitted. Zel computes the same progress in resolve but drops it on error, so the failure path has no progress line.
  - Resolve applies the platform allowlist and throws if the execution-platform label is null. Zel never reads that allowlist, so strategies that flag would remove still run, and a platform filter that would empty the list does not produce this error.
  - Bazel skips the strategy regexp for the test-runner mnemonic, and skips it when the progress message is null. Zel applies regexp filters to test-runner and to whatever description string it is given, including empty.
  - The can-exec filter that picks the first strategy differs. Standalone is not-path-mapped in Bazel and always true in Zel, so a path-mapped spawn can run unsandboxed. Worker in Bazel also needs non-empty tool files and, when the worker allowlist is non-empty, the worker-key mnemonic on that list. Zel only checks the supports-workers requirements, and action spawns are built with tool inputs empty, so worker is still selected. Remote in Bazel is false unless the cache is a remote execution cache, an executor exists, the spawn may be executed remotely, and the spawn is not scrubbed. Zel is remote execution plus the no-remote, no-remote-exec, and local tags, so a scrubbed spawn can be sent to remote execution. Sandbox filtering for the no-sandbox and local tags matches.

### `src/main/java/com/google/devtools/build/lib/exec/StableSort.java`
- Bazel lines: 165
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The default execution-log sort buffers every binary and JSON spawn record and rewrites the file on close. A producer sorts before a consumer when an actual-output path equals an input path. Otherwise the order is the priority-queue key. Zel appends and flushes each spawn immediately in execution order. JSON and binary logs differ whenever execution order is not that order. Compact logs match: Bazel never sorts them, and Zel does not either.
  - Ready spawns with a listed output use a key of `1_` plus the first listed output, in proto order, not sorted. Spawns with no listed outputs use `2_` plus the protobuf text of a message that contains only command arguments, environment, platform, inputs, and mnemonic, in logged order. Any spawn with a listed output precedes any spawn without one unless an edge forces the reverse. Fields left out of that stripped message do not affect the key. Zel never builds either key.
  - Comparison is UTF-16 code units. A non-BMP path sorts as surrogate units, which is a different order from UTF-8 scalar order. Protobuf strings are valid UTF-8, so unpaired surrogates do not appear. Zel never compares these keys.
  - The priority queue does not break ties. Initially ready spawns with equal keys follow heap order from input order. Spawns unblocked later also depend on identity-hash-set iteration, which is not stable across JVM runs. Equal first listed outputs, or equal stripped text, can therefore swap. Zel keeps execution order instead.
  - Edges are exact path-string matches. Every producer of an input path blocks the consumer, so test retries require every attempt to be emitted first. Identity equality is used, so two value-equal messages are distinct nodes. A spawn that inputs one of its own actual outputs blocks itself and is never written, and any spawn that stays blocked on an unemitted spawn is silently dropped. Zel writes every logged spawn.
  - The output-producer map is a hash-key, list-value multimap. Lookups are exact string equality with no path normalization. Value order is stream order and then collapses in an identity set, so it does not by itself change order beyond the tie behavior above.

### `src/main/java/com/google/devtools/build/lib/exec/StandaloneTestResult.java`
- Bazel lines: 85
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`AttemptResult`)
- Verdict: partial
- Differences:
  - The result is passed only when the test status is passed. An unset proto status is no-status. Flaky, timeout, failed, incomplete, remote failure, failed to build, and halted before testing are all failed-can-retry. This type never returns permanent failed, so can-retry is true for every non-pass and the fallback-runner branch is not taken. Zel has no status read: it stops retries only when the exit code is 0, the attempt did not time out, and sharding is supported.
  - A zero exit with the premature-exit file present is failed on the builder, so the result is failed-can-retry and the attempt is retried. Zel sets that file and never checks whether the test exited prematurely, so exit 0 ends the attempt as a pass.
  - A coverage post-processing spawn that times out or fails rewrites the same builder to timeout or failed before this object is built, so the result retries the attempt. Zel copies coverage only after a pass and does not flip that attempt.
  - Sharding without the shard-status file throws and does not build this result, so it is not retried. Zel sets exit code 1 and sharding-unsupported and still loops up to the flaky attempts, so a later attempt can pass.
  - Spawn failures that are catastrophic or did not set up successfully are thrown before this object exists. Zel's error path stores exit code 1, not timed out, and an empty runner, then retries it like a normal test failure.
  - The result re-reads the live builder on every call. Passed plus earlier failures is changed to flaky, and a missing renamed test log to incomplete, on that same builder after the retry decision. A second result call would then be failed-can-retry. Zel fixes the classification on the attempt and does not reclassify from a proto status. Per-attempt build-event status is only timeout, passed, or failed, so flaky and incomplete are not posted on the attempt.
  - The inherited primary system failure uses only the first spawn result: an empty list, success, or a user-error status yields null, otherwise that spawn's detailed exit code. Under no-keep-going a non-null value aborts with the system-failure message. Production builds of this object only store setup-success spawns at index 0, so the method is null there. Zel has no spawn-result list and notest-keep-going always reports that the test failed and the build is aborting.
  - Execution info keeps the primary spawn's exit code, remotely cached on a cache hit, the runner name, hostname, and a fixed timing tree from 32-bit milliseconds. Zel's child has only the strategy and the exit code. An empty runner is rewritten to local. Writing the cache status forces the remote-strategy bit false and leaves remotely cached unset. Timeout exit is forced to 142 even when the spawn reported another code. Bazel stores the spawn exit code. There is no hostname and no timing children.
  - The compact constructor requires all three fields. Build reassigns the same list and does not copy it. Order is insertion order, and index 0 is the test spawn with XML-generator results appended after. Zel keeps one runner string and whether XML was generated, and does not retain later spawn statuses. Map iteration, UTF-16 versus Unicode scalars, Latin-1, regex, and JSON do not apply in this file.

### `src/main/java/com/google/devtools/build/lib/exec/StandaloneTestStrategy.java`
- Bazel lines: 903
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`
- Verdict: partial
- Differences:
  - Bazel passes exec-root-relative runfiles and, when it is under the exec root, a relative temporary directory. Zel puts absolute paths into the source, Java, Python, and runfiles directories and the test temporary directory. The test-setup script absolutizes some of these, so the spawn environment and any cache key hashed from it still differ.
  - The temporary-directory name fingerprints the executable exec path plus shard and run with protobuf varints and keeps 32 hex characters. Zel fingerprints the absolute executable path with a little-endian length prefix and little-endian shard and run. The temporary directory differs.
  - Zel's policy map sets home to that temporary directory. Bazel does not. The test-setup script fills home only when it is unset or not absolute. The action environment differs whenever the user did not set home.
  - If the resolved environment already contains the test name, Bazel throws and does not run the test. Zel does not check.
  - Bazel copies execution requirements into a tree map in UTF-16 order and always sets timeout to the duration in seconds. If a cached result should not be accepted, it also sets no-cache. Zel uses a sorted map in UTF-8 order, so non-BMP keys sort differently, and does not add no-cache for those cache-policy cases, so the spawn can still be remote-cached. With local test jobs, Bazel reserves only a local-test slot. Zel still acquires size-based CPU and RAM.
  - The test spawn's mandatory outputs are empty, so a missing test log or XML does not fail the spawn. XML and coverage spawns pass a null mandatory set, which makes every output mandatory. Zel lists the test log, XML, shard, infrastructure failure, and outputs, then treats missing outputs with exit code 0 as a pass.
  - If deleting the premature-exit file succeeds and the test had passed, Bazel forces failed, not cachable, and appends a UTF-8 line to stderr. Zel exports the variable and never checks the file, so that run stays passed and cachable.
  - When the test passed, is sharded, the check is on, and the shard-status file is missing, Bazel throws and does not record a test result. Zel appends that sentence to the log, forces exit code 1, and records no-status, then still writes the XML and the cache status.
  - Spawn failures that did not set up successfully or are catastrophic are rethrown, with no retry and no test result. An interrupt is rethrown. Other executed failures become failed or timeout, with cachable from whether the status is a user error. Zel turns a runner error into exit code 1 and failed. Timeout forces exit code 142, drops log lines that start with the timeout sentence, and round-trips the log through lossy UTF-8. Bazel keeps the spawn's exit code and raw log bytes.
  - Duration prefers the wall time in milliseconds, a 32-bit int used whenever it is not 0, so a real 0 falls back and a negative value is kept, over the injected clock interval. Zel always uses local elapsed time truncated to 64 bits and ignores spawn wall time. The start time is the action clock in Bazel and the system clock in Zel.
  - On flaky reruns, the aggregated times are the last attempt's time first, then each failed attempt's first time only. Zel stores attempts in execution order. Passed plus any failed attempt becomes flaky in both.
  - Failed-attempt files move every mapped output into the attempts directory, with the test log going to attempt N and every other path replacing the first occurrence of test. Zel writes the captured buffer to attempt N and copies the XML only. Undeclared outputs of earlier attempts are deleted and replaced. The shard file is not kept.
  - If the outputs map has no test log, Bazel sets incomplete and does not add a passed or failed log. Two test-log entries throw. Zel does not do either check. Cancellation builds an incomplete, not cachable, not passed result. Zel has no cancelled-attempt result.
  - Stderr is appended to the test log only when the stderr file exists and its size is greater than 0. The source is then deleted, and an existing destination is made writable first. An empty test log is touched when the spawn recorded no output. An I/O error here becomes an environmental exception. Zel ignores most of those I/O errors.
  - Streamed test output tails the log while the test runs. Zel prints the log after the attempt finishes.
  - A missing XML while the log exists runs the XML generator as a second spawn, with the wall time in seconds as integer division toward zero and the primary exit code. Any failure is fatal. The spawn does not get the test timeout and uses default resources with tag overrides ignored. Zel writes an in-process XML document with zero duration, the target name only, and lossy UTF-8 log text, and ignores write errors.
  - Parsing the test result runs only for detailed, detailed-uncached, and testcase summaries and stores a test case. Parse errors become a null case. Zel always parses when it can, but writing the cache status does not store the case. A cache hit forces one run, empty attempts, and failed attempts equal to the time count minus one, so a cached flaky test loses the failed-in-N-out-of-M summary. Bazel's cached result keeps the cached proto and marks it cached.
  - Execution info records the exit code, remotely cached on a cache hit, the strategy name, remote only when the runner name is remote, the hostname, and a timing breakdown. Zel's cache proto sets the remote-strategy bit false and does not store cache-hit, hostname, or timings.
  - Split coverage runs the coverage collector as another spawn, flips a user-level failure to failed or timeout, rethrows a catastrophic or setup failure, and appends coverage logs into the test log. If the coverage tree is null it throws the fetch-all prerequisite even when the test already failed. Zel always exports split coverage and the coverage-spawn flag as 0. The execution phase only rewrites a passing result to failed-to-build when split is on and fetch-all is off, and does not run the post-processing spawn when both flags are on.

### `src/main/java/com/google/devtools/build/lib/exec/StreamedTestOutput.java`
- Bazel lines: 77
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` (`emit_test_live`)
- Verdict: partial
- Differences:
  - Bazel tails the live log on a watcher thread, polling every 100 milliseconds and reading 1024-byte chunks, and writes post-header bytes to stdout while the attempt runs. Zel reads the log once after the test returns and appends that block. A header-bearing log is therefore interleaved with other reporter output in Bazel, and a watcher that stops early keeps only the bytes already pumped.
  - The 77-dash delimiter must be an entire newline-terminated line. Zel also treats a trailing carriage return as optional and treats end of file as a line end. A delimiter followed by a carriage return and a body, or a final delimiter with no newline, is copied whole by Bazel and emitted as only the suffix by Zel.
  - If the delimiter is never seen, close copies the raw file once. Zel does the same, then if the bytes are non-empty and do not end in a newline it appends one. A body without a trailing newline stays unchanged in Bazel and gains a newline in Zel. Empty input stays empty on both.
  - Log bytes are copied raw on both sides. The watcher diagnostic is a platform-charset string and is emitted only when the header was already seen. Before that the filter drops it and close dumps the file. Zel has no diagnostic line: a read failure emits nothing and does not fail the test. An I/O error on close becomes an environmental test error. If the watcher is still alive after the 30-second join, an assertion throws and the fallback copy is skipped.
  - One streamed output is created per attempt, so each flaky attempt is filtered on its own. Zel prints a single strip of the final test log. For one run that is only the last attempt. For more than one run the file is the concatenation of every attempt and only the first delimiter is removed, so later setup headers remain in stdout.
  - Streamed tests are marked exclusive and run in reverse configured order, so multi-test stdout follows that reversed completion order. This class does not reorder tests.

### `src/main/java/com/google/devtools/build/lib/exec/SymlinkTreeHelper.java`
- Bazel lines: 286
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/runfiles_tree.rs` (`build_runfiles_tree`)
- Verdict: partial
- Differences:
  - Fileset farms are missing. Bazel prefixes every name with the workspace directory and stores the target path as the link text, and duplicate names throw. Zel has no fileset symlink tree.
  - Disabling runfiles does not match the minimal directory. Bazel keeps the runfiles directory, symlinks the output manifest at the input manifest, and creates the workspace subdirectory. Zel writes the executable manifest and removes the runfiles directory, so the directory and workspace subdirectory are gone. Remove errors are ignored and the function still returns success.
  - The output manifest is a different file. Bazel deletes the runfiles manifest and symlinks it to the input manifest, whose bytes were already written as ISO-8859-1, including space and newline escaping. Zel never reads an input manifest. It writes both manifests as regular files of key, space, absolute target, newline in sorted-map order, with no escaping. Writing follows an existing symlink, so a previous manifest symlink overwrites the input manifest.
  - Unresolved symlinks are not planted textually. For a symlink artifact Bazel stores the unresolved link text. Other artifacts store the path. A null value is an empty regular file, and an empty path fragment is still a symlink. Zel treats every empty string as an empty file and every other value as the exec root joined with the value, so the runfiles link points at the artifact inode.
  - In-place repair differs. Bazel rejects empty and absolute link paths, normalizes dot and parent segments before creating names, and compares link text with normalized path-fragment equality. It deletes a directory or other non-symlink where a link or empty file should be, then recreates it. An existing regular file used as an empty placeholder is left untruncated. Zel's join of an absolute key drops the runfiles root, and parent segments are not normalized, so a link can be created outside the tree. Path equality does not treat a parent segment as the collapsed path. A directory where a file or symlink should be is not removed. Stale directories are removed only if empty, and failures are ignored.
  - When one path is both a symlink and a directory prefix, Bazel's directory pass runs after the symlink pass and replaces the symlink with a real directory. Zel's sorted map creates the shorter symlink first, then creating directories through that symlink fails or writes through it.
  - Directory modes differ. Bazel follows links and, if permissions are unavailable, chmods 0755, otherwise ORs in 0700 when the owner lacks read, write, and execute. Zel never changes mode, so a non-writable existing directory stays non-writable and its children are not updated.
  - Bazel builds the tree on the host filesystem so overlay or snapshot filesystems do not hide on-disk parents or freeze symlink targets. Zel uses the standard filesystem on the path it was given.
  - Bazel path strings are Latin-1 raw bytes and symlink targets go through the internal-to-platform encoding. Zel keys and targets are UTF-8 strings, and lossy conversion replaces non-UTF-8 exec-root bytes with U+FFFD. A code point in U+0080 through U+00FF is one manifest byte in ISO-8859-1 and two bytes in Zel's UTF-8 file. Sorted-map order is UTF-8 byte order. Bazel's manifest tree map uses UTF-16 compare, which diverges for supplementary characters.
  - With legacy external runfiles, Zel adds external links for keys that do not start with the workspace or an underscore, keeping the first on collision. In this Bazel revision that flag does not change the map this helper receives.
  - Zel always writes the repo-mapping file, possibly empty, as a regular file after the symlink pass, replacing a mapped repo-mapping symlink. Bazel only has that path when it is in the symlink map, as a symlink to the repo-mapping artifact.
  - On non-Unix, Zel's symlink call is compiled out. A mismatched existing path is removed and the runfile is left missing while the function still succeeds.

### `src/main/java/com/google/devtools/build/lib/exec/SymlinkTreeStrategy.java`
- Bazel lines: 125
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/runfiles_tree.rs` (`build_runfiles_tree`)
- Verdict: partial
- Differences:
  - Fileset trees are absent. Bazel plants fileset links and throws on duplicate names. Zel never materializes a fileset symlink farm.
  - The tree manifest is a symlink to the input manifest, created last. Zel writes the executable manifest first and the runfiles manifest later as regular files, so a crash leaves a manifest whose tree is stale, and readlink and stat disagree. Zel also overwrites the source manifest path the strategy never writes.
  - Manifest bytes differ. Bazel's linked manifest is the source manifest: ISO-8859-1, sorted by the Java path string, with escaping when a path has a space or newline. Zel writes UTF-8 lines from a sorted map with no escaping. Non-UTF-8 paths become U+FFFD. On Linux Bazel keeps the raw bytes, so both the manifest and the symlink targets differ.
  - Link text uses the unresolved symlink text or the artifact path. Zel joins the exec root when the value does not start with a slash, so parent segments are not normalized, the unresolved target is ignored, and a non-slash absolute target is joined onto the exec root. Non-Unix builds create no symlinks and still return success.
  - Empty files are not truncated if a regular file is already there. Zel matches that for a regular file, but exists follows links and a directory is left in place. Bazel deletes a non-file and creates an empty file.
  - Bazel drops nested runfiles-tree artifacts, drops entries obscured by a non-null ancestor, lets empty files overwrite an existing path, adds the runfile marker when no workspace-prefixed path was seen, and adds the repo mapping only when that artifact is non-null. Zel keeps those entries, does not let empty names overwrite, never adds the marker, and always writes the repo mapping as a regular UTF-8 file. An absolute runfiles key throws in Bazel. Rust join treats it as absolute and can write outside the tree.
  - Disabling runfiles clears children under the runfiles root, symlinks the manifest, and creates the workspace directory. Zel removes the runfiles directory and does not recreate the workspace directory or the manifest. Auto is skip on Windows in Bazel. Zel treats auto and a missing flag as true, so Windows still gets a full tree unless the flag is explicitly false.
  - Legacy external runfiles is a no-op in this Bazel. Zel adds external aliases for keys that do not start with the workspace name or an underscore.
  - Stale-entry cleanup ignores read and delete errors, so unreadable or non-empty directories can survive and the call still succeeds. Bazel retries after chmod and fails the action. Zel never chmods.
  - If an output service can create the symlink tree, Bazel skips the skip mode and calls that service. In-tree services return false. Zel has no output-service path.

### `src/main/java/com/google/devtools/build/lib/exec/TestLogHelper.java`
- Bazel lines: 146
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` (`strip_test_setup_header`)
- Verdict: partial
- Differences:
  - The header delimiter matches only a full line ended by a newline. A final line of 77 dashes with no newline is not a header, so the stream reprints the whole file. Zel treats that line as the header and returns an empty body.
  - A delimiter line ending in carriage return plus newline does not match, so the header is kept and the raw file is copied. Zel strips one trailing carriage return, so it drops the preamble.
  - Writing the test log compares the size cap to the raw file size, header included. The cached reprint compares the stripped length, so a log that is over the cap only because of the preamble is printed, and the too-large number is the body size.
  - Failed-attempt logs for a flaky test with one run per test are copied with no size cap. Writing the test log applies the cap to every log.
  - Errors output is not-passed. Flaky is passed, so those failed-attempt logs are omitted. Bazel prints them per failed attempt.
  - Cached logs are printed only for all output, and only the test log. Bazel still prints cached failed logs, including attempt logs, each through the size-capped writer.
  - An unreadable log becomes an empty body inside the normal banner and footer. Bazel throws after the opening banner is already flushed and does not write the footer. Callers then print that the test output could not be read.
  - Streamed output and the cached reprint append a newline when the body is non-empty and does not already end in one. The filter copies those bytes unchanged, so the footer can sit on the same line as the last log line.
  - Streamed output reads the finished file once. The filter is fed while the file grows, so post-header bytes are already on the console if the run stops before that final read.
  - With both more than one shard and more than one run, the banner is only the run count and the file is the run directory's test log. Bazel's test name includes the shard and the run on each log.
  - The opening banner is UTF-8. Bazel encodes that string with the default charset and replaces characters that charset cannot encode. The log body itself is raw bytes on both sides.

### `src/main/java/com/google/devtools/build/lib/exec/TestPolicy.java`
- Bazel lines: 103
- Zel counterpart: `/workspace/zel-cursor/crates/zel-util/src/action_env.rs` (`test_policy_environment`) and `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` (`build_test_env`)
- Verdict: partial
- Differences:
  - Placeholder expansion is not implemented. Bazel walks the policy map. A value that contains a placeholder start is either exactly inherited, in which case the key is omitted when the client map has no entry and a present entry is copied, or three literal replacements in order: the system user name, the test temporary directory, and the runfiles directory. A later pass expands a placeholder that an earlier replacement inserted. Zel only inserts the already expanded default keys, so the user-name placeholder, embedded placeholders, inherited, and the empty policy never run.
  - The built-in policy expands the timezone, the source directory, the Java and Python runfiles, the runfiles directory, the test temporary directory, and run-under runfiles. Zel also sets home to the temporary directory, so the map this method returns contains home.
  - Those path values are Bazel internal strings: Latin-1, one code unit per raw byte. Zel uses lossy path conversion, which replaces non-UTF-8 bytes with U+FFFD. Non-UTF-8 bytes in the runfiles or temporary path change those variables and Zel's home.
  - Bazel only adds fixed entries and client values for inherited names. An unset name only drops that name from that layer's own map, so unsetting the timezone or the test temporary directory leaves the policy value. Zel then deletes every recorded unset name from the accumulated map, so those flags remove policy variables.
  - Bazel returns an insertion-ordered map, with keys from each later resolve appended. Zel returns a sorted map, so spawn environment order and any fingerprint that walks that order differ.
  - A relative test-temporary-directory flag is substituted from the workspace plus the flag plus a hashed temporary directory in Bazel. Zel uses the flag plus the hashed directory as a plain filesystem path, so the test temporary directory and home differ when the flag is relative.
  - Running a test calls this method with exec-root-relative runfiles and temporary fragments, then applies the action environment, the test environment, the rule environment, and the setup variables. Zel's run path does not call it: paths are absolute except under a script path, the action environment is skipped, and unsetting a test-environment name is ignored.
  - The trailing setup-variable call is inlined differently. The test binary is the filename rather than the callable path. Shard variables are set whenever the shard count is positive, while Bazel sets them only when the total shards are greater than 1. The test-bridge fail-fast variable is never set. With runfiles disabled Bazel writes the manifest-only flag after the other layers, so it survives an unset, while Zel sets it earlier and a later unset removes it. Coverage variables are inserted only if absent after the rule environment in Bazel, while Zel merges them into the rule environment first and they override. Split coverage post-processing is always 0. The XML output, undeclared outputs, and coverage paths are absolute attempt-directory paths instead of exec-root-relative paths. The test-setup script prefixes relative paths with the exec-root working directory, so the test process matches in that directory, and the raw spawn environment and cache key still differ.

### `src/main/java/com/google/devtools/build/lib/exec/TestXmlOutputParser.java`
- Bazel lines: 355
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_status.rs` (`parse_test_xml`)
- Verdict: partial
- Differences:
  - A time with no dot or exponent is milliseconds via a long parse, so 12 stays 12 and a leading plus is accepted. Zel always does floating-point seconds times 1000, so 12 becomes 12000, and integers above 2^53 lose precision.
  - Seconds that contain a dot or exponent are a float then a round to a 32-bit int, with ties toward positive infinity and saturation at the 32-bit limits. Zel rounds a 64-bit float with ties away from zero and saturates at 64-bit limits. 1.0005 becomes 1000 versus 1001. A negative half rounds to 0 versus minus 1. Large values saturate in Bazel and stay exact or hit the 64-bit maximum in Zel.
  - Surrounding spaces throw in Bazel. Zel trims Unicode whitespace and then scales. NaN and infinities throw in Bazel and become 0 or the 64-bit limits in Zel. A hex float is accepted by Java and rejected by Zel, which drops the tree. A value one past the maximum signed 64-bit integer throws in Bazel and becomes that maximum in Zel.
  - The test decorator sets the name to each attribute's local name, so the stored name is the last attribute key, not the name value. Zel stores the name value. A lone classname stores the name classname in Bazel and leaves the name unset in Zel.
  - Bazel stops at the end tag of the first test suites or suite and never reads trailing markup. Zel tokenizes the whole buffer, so a later unclosed tag or comment fails and stores no case.
  - The JDK parser is namespace-aware. Zel matches the raw token. A prefixed suite is a suite in Bazel and no suite in Zel. A prefixed testcase child is a real child in Bazel and is skipped as unknown in Zel. An unbound prefix is fatal in Bazel and ignored in Zel. A prefixed name sets the name in Bazel and is ignored in Zel. With both an unprefixed and a prefixed name, Bazel's last attribute wins and Zel keeps only the unprefixed name.
  - The JDK parser expands numeric character references and DTD entities and normalizes attribute whitespace. Zel only replaces the five named entities, cuts a doctype at the first greater-than, and keeps raw whitespace. A raw ampersand or a duplicate attribute is fatal in Bazel. Zel keeps the ampersand and lets the last duplicate win.
  - A quoted greater-than ends the tag in Zel and is a normal value in the JDK parser. Attributes with no separating whitespace are fatal in Bazel and accepted in Zel. An end tag with a space after the slash is fatal in Bazel. Zel trims the end-tag name and accepts it.
  - The JDK parser decodes from the BOM and the encoding declaration. Zel reads the file as UTF-8, so non-UTF-8 input is dropped, and a Latin-1 declaration on UTF-8 bytes is decoded as Latin-1 by Bazel and as UTF-8 by Zel.

### `src/main/java/com/google/devtools/build/lib/exec/TreeDeleter.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - There is no tree-deleter type. Sandbox removal is inlined as remove-dir-all, and output trees are removed the same way.
  - The interface requires a missing path to be a no-op and a synchronous failure to throw. Zel discards every remove-dir-all error, so a permissions or directory-not-empty failure leaves the tree and still returns success. Cleaning the sandbox base returns success after those ignored deletes.
  - Asynchronous deleters must rename the tree away before deleting it, and may drop errors. Zel never renames into a trash directory and always deletes on the calling thread, so the sandbox path stays occupied until the walk finishes and the next action cannot reuse it early. The async idle-thread flag is only a help string.
  - Shutdown is specified to finish pending deletions. Zel has no deleter lifecycle and no join of background deletes on server shutdown.
  - Bazel's path delete-tree retries after chmod when a directory is not readable or writable, and does not follow symlinks. Remove-dir-all does not chmod, so a mode-000 directory that Bazel removes remains on disk in Zel because the error is ignored.

### `src/main/java/com/google/devtools/build/lib/exec/local/LocalEnvProvider.java`
- Bazel lines: 56
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs` (`rewrite_local_env`) and `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`xcode_local_env`)
- Verdict: partial
- Differences:
  - There is no operating-system dispatch. Bazel uses the current OS: a Mac OS X prefix selects the Xcode provider, a Windows prefix selects the Windows provider, and Linux, FreeBSD, OpenBSD, and unknown select the POSIX provider. Zel has no runtime switch. The rewrite always applies the POSIX temporary-directory rule. The Xcode rewrite is gated by the compile-time macOS target.
  - The POSIX temporary directory matches only where the rewrite is called, sandbox prepare and workers: drop the spawn's key, then set it from the client value when that value is non-empty, otherwise `/tmp`. The fallback directory is ignored. Null or empty and a missing or empty Rust value agree, including keeping a whitespace-only value. Unsandboxed local execution never calls the rewrite, so the child keeps the spawn environment. The local spawn runner always rewrites.
  - The Windows branch is absent. Bazel drops the exact temp keys, sets both from the client values or the fallback directory, then replaces every slash with a backslash. It does not invent the POSIX temporary directory. Zel never does this, and on those same paths it still forces the POSIX key and leaves the Windows keys unchanged.
  - Darwin does not use the Xcode provider. That provider always does the POSIX rewrite, then only if the version override or the SDK platform is present resolves the developer directory and the SDK root, and a tool failure fails the action. A pre-existing SDK root throws on the duplicate key. Results are cached for the server lifetime. Requires-darwin alone does not trigger this.
  - Zel's Xcode rewrite triggers on the version override, the SDK platform, or requires-darwin, returns immediately when both directories are already set, resolves a missing developer directory with xcode-select, defaults a missing platform to macosx, and lowercases it with Unicode lowercase. Xcrun inherits the parent environment and only overlays the developer directory. An existing SDK root is kept. It removes the override variables. Tool failure leaves the map unchanged and the action still runs. It does not set the temporary directory. A Darwin worker gets Xcode variables in Bazel and only the temporary-directory rewrite in Zel.
  - Tool stdout is decoded as UTF-8 with replacement in both, but Java trim strips code units at or below U+0020 and Rust trim strips Unicode whitespace and does not strip several C0 controls. A non-ASCII platform name and odd trailing whitespace can change the SDK argument or the stored path.
  - The no-op provider copies the spawn environment and is what remote persistent-worker signatures use, so those keep the spawn temporary directory. Zel has no copy-through provider. Local workers always rewrite, so the worker environment and worker key include the forced temporary directory.
  - Result maps keep the spawn map's iteration order and append rewritten keys. Zel returns a sorted map, so the rewritten keys sit in sorted position. Java map keys are UTF-16 code units. Rust string keys cannot represent unpaired surrogates. The ASCII names match. This file has no integer, regex, Latin-1, or JSON logic.

### `src/main/java/com/google/devtools/build/lib/exec/local/LocalExecutionOptions.java`
- Bazel lines: 90
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - The termination grace seconds are a Java int via integer decode, then a duration of that many seconds. Zel parses an unsigned 64-bit decimal. A leading-zero value is 8 seconds in Bazel and 10 in Zel. Hex, hash, and a negative value are accepted by Bazel and rejected by Zel. A negative value becomes a negative duration. Plain decimal in the 32-bit range matches, including the default 15 seconds. The alias is stored under the new name and last occurrence wins.
  - Local retries on crash are parsed and then ignored. Bazel retries a standalone local spawn whose exit is 134 or 138, up to that count, sleeping attempts times 1000 milliseconds between tries. A negative count never equals the attempt counter, so crashes retry without a cap. The default 0 matches.
  - The graceful SIGTERM flag is parsed and then ignored. Bazel sets the process-wrapper flag for every local spawn when the flag is true, and also when the spawn supports graceful termination. Zel only honors that execution-info key, so the flag does not change SIGTERM-then-SIGKILL behavior.
  - Lockfree output still adds standalone to the default dynamic local strategy list, but Zel rereads the raw string and treats only exact 0, false, and no as false, so Bazel false values f, n, False, and NO become true. Standalone execution always takes the output lock before the process starts. Bazel, when the flag is true, does not lock first, locks only after a non-zero speculative exit, and deletes outputs if that local process is interrupted.
  - The allowed-local-actions regex defaults to unset, and an explicit null is the literal pattern. Bazel compiles the pattern with dot-all after converting internal bytes to Unicode and full-matches the mnemonic. Zel full-matches with the download regex on the UTF-8 string. A dot does not match a newline. Dollar is only the absolute end. A non-BMP character is one atom in Zel and two in Java. A backreference is a literal digit. Lookaround, possessive quantifiers, and Unicode properties are not the Java engine. The options parser checks syntax with the Java-regex port, then execution compiles the download engine, so a Java-legal pattern the download engine rejects fails the command. A non-match is exit code 1 and execution failed. Bazel uses exit minus 1 and execution denied. Non-ASCII patterns also differ in the denial stderr because Bazel re-encodes the pattern.

### `src/main/java/com/google/devtools/build/lib/exec/local/LocalSpawnRunner.java`
- Bazel lines: 557
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/lib.rs` (`execute_spawn_local`)
- Verdict: partial
- Differences:
  - Can-exec is false when the spawn's path mapper is not a no-op. Zel always returns true, so a path-mapped spawn runs locally on the unmapped paths.
  - The allowed-local-actions regex full-matches the mnemonic with Java regex on Unicode-converted text. A miss writes the denial with exit minus 1 and execution denied. Zel matches Unicode scalars, a dot matches every scalar including newlines, and lookaround and backreferences are absent. A miss is exit 1 and execution failed.
  - An empty argument vector throws and becomes an unhandled local-spawn exception. Zel returns execution failed, exit 1, and stderr that the command line is empty.
  - Bazel rewrites the environment in the runner. On POSIX and the Xcode provider it drops the spawn's temporary directory and sets it to the client value or `/tmp`. On Windows it sets both temp keys. The Xcode logic runs only when the version override or the SDK platform is set, and those override variables stay. Zel keeps the spawn environment, does not force the temp keys, and if path is missing inserts a default path, so a different binary can be found. The caller's Xcode rewrite also fires on requires-darwin, uses xcode-select and xcrun, and deletes the override variables. Environment names come from a sorted map. Subprocess arguments and environment go through the internal-to-platform encoding. Zel passes the Rust string bytes.
  - Before exec, Bazel writes every virtual action input and updates runfiles trees. The Zel runner does neither, so those inputs can be missing or stale.
  - Bazel prefetches inputs unless local prefetch is disabled. The Zel runner never prefetches and ignores that key.
  - A relative first argument that has a parent directory is replaced with an absolute path under the exec root. Zel leaves it relative.
  - Bazel closes the child's stdin. Zel inherits it, so the action can read the server's stdin.
  - Timeout is whole seconds from an integer parse. Fractional and padded values fail the action and do not run. A negative integer is a non-zero duration, so with a process wrapper a timeout forces status timeout and exit 142. Zel parses a float: unparsable or non-positive values mean no timeout, and fractional seconds are enforced. It does not apply the post-exit wall-time check, so a process that exits 0 just after the deadline can succeed in Zel and time out in Bazel. On timeout Zel appends a sentence to stderr. Bazel does not.
  - Crash retries rerun while the raw exit is abort or bus error, sleeping attempt times 1000 milliseconds, up to that count. Zel runs once.
  - A failure to start the process writes an I/O exception, exit minus 1, and execution failed. Zel writes a failed-to-execute line, exit 127, and execution failed.
  - Zel checks inputs and returns missing inputs without running when the path is absent. A dangling symlink counts as present. Bazel execs the command. After exit 0, Zel returns missing outputs when a mandatory output is absent, keeping exit code 0. Bazel returns success and leaves that check to the executor.
  - Zel deletes every declared output before exec and creates parent directories. Bazel does not, so an output path that is also an input, or a symlink, is still there for the command.
  - With lockfree output, Bazel does not lock outputs before start, and after the subprocess has started an interrupt deletes outputs. Zel always locks with 0 before start and does not clean outputs on cancel. Cancel is an immediate kill of the process group. Bazel's process wrapper, when graceful termination is set, forwards SIGTERM and waits the grace seconds. Zel copies the graceful flag onto the wrapper options, but the wrapper never reads it.
  - Bazel creates a per-action work directory, passes it as the Windows temp fallback, and deletes it after the action. Zel never creates that directory.
  - Zel always starts a new session and sets umask 022. Bazel does that only when a process wrapper is installed, so otherwise the action stays in the server's process group and umask.
  - Setup, queue, and wall times are 32-bit milliseconds and wrap after about 24.8 days. Zel uses a saturating 64-bit value. Bazel also records wrapper execution statistics from a stats file. Zel attaches in-process resource usage and does not write that file.

### `src/main/java/com/google/devtools/build/lib/exec/local/PosixLocalEnvProvider.java`
- Bazel lines: 61
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs` (`rewrite_local_env`)
- Verdict: partial
- Differences:
  - Sandbox and persistent workers apply the same value rule as Bazel: drop the spawn's temporary-directory key, then set it from the client environment when that value is non-empty, otherwise `/tmp`. A missing key and an empty string both become `/tmp`. A whitespace-only client value is kept. The bin tools and the fallback directory do not affect the result on either side.
  - Standalone local execution does not. It passes the spawn environment through after clearing the process environment and never calls the rewrite. The local spawn runner always rewrites. A spawn temporary directory therefore survives on that path, and a spawn with no temporary directory runs with the variable unset even when the client exported one.
  - The returned map is sorted. Bazel's map follows the input map's iteration order and appends the temporary directory last. The process builder then copies into a hash-based process environment. Looking up distinct names matches. Anything that walks the block in map order can differ.
  - Client temporary-directory bytes that are not valid UTF-8 never reach the Rust map: the client keeps only entries that succeed as strings, so the rewrite treats that variable as absent and uses `/tmp`. Bazel stores those bytes in a Latin-1 Java string and, when the native encoding is ISO-8859-1, passes them through unchanged. Valid UTF-8 values are copied as-is. A Java null value is treated like missing. A sorted string map cannot store null, and both absence and an empty string already fall back to `/tmp`.

### `src/main/java/com/google/devtools/build/lib/exec/local/WindowsLocalEnvProvider.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Windows local-env rewrite exists. Sandboxed and worker spawns call the POSIX rewrite, which drops only the temporary-directory key and sets it from the client value or the literal `/tmp`. The Xcode rewrite returns the map unchanged off macOS. Spawn temp and temp keys are left as-is or absent.
  - Bazel copies every spawn entry except the exact keys temp and temp, then sets both to one path: client temp if non-null and non-empty, else client temp if non-null and non-empty, else the fallback directory. Bin tools are unused. Zel never reads those keys, never takes a fallback directory, and never writes those two keys, so a Windows local action's temp variables differ.
  - Null and empty are missing, and whitespace-only values are kept. A null fallback after both are missing throws on replace. A sorted string map cannot store null. The POSIX path substitutes `/tmp` for an empty temporary directory and does not throw.
  - The chosen path is rewritten so every slash becomes a backslash. Other characters, including existing backslashes and non-BMP scalars, stay. Zel does not convert separators on temp variables.
  - Matching is case-sensitive, so mixed-case temp keys and the POSIX temporary-directory key survive while the exact Windows keys are replaced. Zel removes only the POSIX key and keeps the Windows keys.
  - Bazel's result follows input iteration order with the two keys appended, and building throws on a duplicate key. Zel's map is sorted and silently overwrites, so both membership and iteration order differ for any environment fingerprint that walks the map.

### `src/main/java/com/google/devtools/build/lib/exec/local/XcodeLocalEnvProvider.java`
- Bazel lines: 317
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`xcode_local_env`)
- Verdict: partial
- Differences:
  - Bazel runs this provider for every Darwin local spawn. Zel calls the Xcode rewrite only for spawn actions in action execution. Sandbox and worker execution use the temporary-directory rewrite and never call the Xcode rewrite, so those actions get the temporary-directory rewrite without the developer directory or the SDK root.
  - On the spawn-action path, Zel leaves the temporary directory as it arrived. Bazel always drops the action's temporary directory and sets it from the client environment, or to `/tmp` when that value is missing or empty. The fallback directory is unused. A whitespace-only client value is kept.
  - Bazel enters the Xcode logic only when the action environment contains the version override or the SDK platform. Zel also enters when execution info contains requires-darwin, and then defaults a missing platform to MacOSX, so it sets the SDK root for actions where Bazel would not.
  - Zel returns the environment unchanged when both the developer directory and the SDK root are already set, so the override variables stay. Bazel still queries xcrun whenever the SDK platform is set.
  - After a resolved developer directory, Zel deletes the version override, the SDK version override, and the SDK platform. Bazel copies those keys through. The SDK version override is never read by Bazel.
  - Bazel sets the developer directory only when the version override is present and the developer directory is absent. A value starting with a slash is used as the directory. Any other value is parsed as a dotted version, and invalid text or an integer overflow fails the action, then passed to the embedded locator with the client environment inherited. Zel ignores the version string and, when the developer directory is absent, runs xcode-select in the server process environment. Failure leaves the action environment unchanged and does not fail the action.
  - When the developer directory is already in the action environment, Bazel's local developer directory stays empty, so xcrun is run with an empty environment and does not see that directory. The action still receives the original developer directory. Zel passes the existing developer directory to xcrun.
  - Bazel runs xcrun with the ASCII-lowercased platform and either an empty environment or only the developer directory. ASCII lower changes only A through Z. A non-zero exit or a signal fails the action. Zel uses Unicode lowercase, replaces the process environment with only the developer directory, and on any unsuccessful xcrun still inserts the developer directory, strips the override keys, and omits the SDK root.
  - Putting the SDK root throws when it is already present. Zel keeps the existing value.
  - Bazel caches successful locator and xcrun results for the server lifetime. Zel queries on every call, so a changed Xcode install is visible immediately. Failures are not cached in Bazel.
  - Both decode tool stdout as UTF-8 with replacement. Bazel trim strips leading and trailing code units at or below U+0020. Rust trim strips Unicode whitespace, so C0 controls stay in Zel and a non-breaking space stays in Bazel. That changes the developer directory and the SDK root when the tool output has that whitespace.
  - The action environment Bazel returns keeps the incoming map's iteration order. Zel's sorted map sorts keys, so the subprocess environment order differs.
  - Off macOS, the Xcode rewrite is a no-op. Bazel selects this provider only for Darwin.

### `src/main/java/com/google/devtools/build/lib/includescanning/CppIncludeScanningContextImpl.java`
- Bazel lines: 224
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No include-scanning context exists. When Bazel scans, finding additional inputs returns only the headers the scanner found. Zel hardcodes include scanning to false and the compile action always registers the source plus compilation-context headers, prunable headers, module maps, and builtin include files. Spawn inputs, cache keys, and which header edits rebuild the compile differ whenever scanning would drop an unused header or add a used header that was not in that declared set.
  - Search paths are a first-seen set of include directories, then system include directories, then every builtin include directory. Duplicates stay at the first occurrence because duplicate directories change include-next. Quote and framework lists are not deduplicated. Zel never builds that list.
  - Only absolute builtin directories are kept. Collecting treats an artifact whose root is the absolute root as an absolute include: it is dropped when it starts with one of those directories, otherwise it is an illegal absolute path. Zel neither drops those files nor raises that error.
  - Tree artifacts in the scanner sources are replaced by their children before the async process, preserving non-tree order. A null tree value returns null and the scanner does not run. Zel's tree expander returns the directory artifact itself and does not wait on children.
  - After the async process, a missing value returns null so Skyframe restarts. An I/O exception becomes a scanning I/O exception. A missing package becomes a package-load failure whose message can end in null. Zel has neither restart nor these failure details.
  - Collecting skips the source file so a tree-file source does not also add its parent. A child whose parent is a tree must be a tree-file artifact or the check throws. The parent tree is appended once per child, so the returned list can repeat that parent. The walk is concurrent-set key order, not insertion order. Zel does not apply this filter or parent promotion.
  - Builtin include files are inserted before scanning, so those files are returned unless they are the source or an absolute builtin path. Zel always attaches builtin include files as tools, including files this method would have skipped.
  - The async process is given the main source, the expanded sources, header data, command-line includes, grep includes, and the execution platform. No crate implements that scanner. Parsing a depfile only records paths on the action cache after the compile. That does not narrow inputs before the compile and does not apply the absolute-include error or the tree-parent rule.

### `src/main/java/com/google/devtools/build/lib/includescanning/IncludeHintsFunction.java`
- Bazel lines: 116
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never evaluates the include-hints key or loads hint rules from the hints file, so hint rules never add or drop scanned includes.
  - Bazel looks up the containing package for the hints file's parent and, when there is no containing package, fails persistently with the file-not-in-package code and a message that includes the reason when it is non-null. Zel does not emit that failure.
  - Bazel then depends on the file value and returns null while either dependency is missing so the node restarts. Zel has no such dependency, so a change to the hints file cannot invalidate this node.
  - An I/O or missing-build-file exception from the package lookup, the file value, or loading the rules becomes a persistent environmental exception with the read-failure code. Zel has no equivalent error path.
  - Include-scanning flags exist only as option strings. No crate loads or applies an include-hints file.

### `src/main/java/com/google/devtools/build/lib/includescanning/IncludeParser.java`
- Bazel lines: 993
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No scanner extracts include, include-next, import, or has-include inclusions from source bytes, so those inputs are never discovered. Bazel walks bytes with Latin-1 whitespace, skips a backslash plus newline and block comments, rejects a name whose first byte is a slash, and strips escaped newlines inside the name. The name itself is decoded with the platform default charset, typically UTF-8, so bytes 0x80 through 0xFF become U+FFFD, not ISO-8859-1.
  - There is no parser for grep-includes output. Bazel reads that stream as ISO-8859-1, splits on newline, and maps a leading quote, angle, q, or a to the four inclusion kinds. Any other prefix is an I/O exception.
  - There are no include-hints rules. Bazel reads the hints file as UTF-8, splits fields on whitespace, compiles each match pattern as anchored, rewrites backslash to dollar in the find-root replacement, and only allows four-column rules whose pattern matches a third-party prefix. Path, file, and quote and angle rules inject inclusions that are skipped unless the path stays under third_party. None of these artifacts or inclusions are added.
  - Remote grep-includes and the local-read fallback are absent, so a remote-only or unreadable generated header does not switch scanners. Include scanning is fixed to false even when the scanning flags are set, and the native compile action always attaches the full header set. With scanning enabled, Bazel narrows C and C++ inputs using this parser. The flags exist only as option tables.
  - Parsing a depfile is a different path and does not implement this class.

### `src/main/java/com/google/devtools/build/lib/includescanning/IncludeScannerHelper.java`
- Bazel lines: 108
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No function creates an inclusion with context or finds the context path position. Bazel runs this helper on every extracted inclusion while walking headers, so the headers discovered for include-next diverge whenever that walk runs.
  - A non-next inclusion, and an include-next whose caller already passed a context position other than minus 1, is returned unchanged. Nested include-next therefore resumes after the directory that contained the parent. Zel never carries that index forward.
  - A top-level include-next searches the quote paths for next-quote and the include paths for every other next kind. The first index whose relative path equals the source exec path wins, and the stored index is one past that directory. Relative joins an absolute operand unchanged, joins with a slash, and normalizes. Equality is UTF-16 code units, not Unicode scalars and not an OS case-insensitive compare. No match yields minus 1. Zel never probes include paths this way.
  - Quote and angle indexes are cached on the helper for one source file, keyed only by kind. The first top-level next-quote fixes the quote position from that path fragment. Every later top-level next-quote in the same file reuses it even when its own path would match a different directory. Zel has no such cache.
  - On a match, the context kind becomes quote or angle while the inclusion stays a next-inclusion, so lookup starts at that index. On no match, Bazel replaces the inclusion with a non-next inclusion of that kind and leaves the context position at minus 1, so include-next is then a plain include searched from index 0. Zel neither rewrites the kind nor keeps the sentinel.
  - Context indexes are 32-bit ints. Adding one wraps, so a path list of maximum int length stores the minimum int rather than a past-the-end index. The not-found sentinel is minus 1. Zel has no 32-bit counterpart of this index.

### `src/main/java/com/google/devtools/build/lib/includescanning/IncludeScannerSupplier.java`
- Bazel lines: 146
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never builds or interns an include scanner per quote, include, and framework path-list triplet. Scanner-for keys a cache on the three lists using list equality and path-fragment equality, so the same three lists share one scanner and a different order is a different scanner. Include-path identity never selects a shared scanner.
  - Bazel keeps one supplier-wide map of artifact to inclusion future and passes it into every scanner. In-flight and completed include parses of the same artifact are coalesced across scanners. Zel has no shared parse-future map.
  - Construction also installs one shared path-existence cache plus the include-pool executor, whether to shuffle, the output path, the exec root, the artifact factory, and the spawn-include scanner into each legacy scanner. The parser is read when the cache loads, so init must run first. Zel wires none of this, so the brittle include-scanning flag cannot narrow C and C++ inputs through this supplier.
  - Whether to shuffle is applied only when a scanner is created. Weak values mean a collected scanner is rebuilt on the next lookup and can reshuffle search order, changing which duplicate header is chosen. The parse-future map survives that collection. Zel has neither the shuffle nor the weak cache.
  - Init accepts a non-null parser only once, and only while both maps are empty. A second init, a null parser, or a lookup before init fails the command. Zel has no equivalent lifecycle gate. The option text does not implement this class.

### `src/main/java/com/google/devtools/build/lib/includescanning/IncludeScanningModule.java`
- Bazel lines: 377
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No module registers the C++ extraction, C++ scanning, or SWIG scanning contexts, and it does not restrict those types to the empty identifier. Finding used headers calls the scanning context and sorts the result by exec path into a stable nested set. Zel hardcodes include scanning to false, so the scanning flags cannot narrow compile inputs to discovered headers.
  - Extracting SWIG includes is absent. Bazel builds a per-call scanner over a command-scoped inclusion map, passes the source as the only input, empty quote paths, and empty modular headers, and keeps the first artifact when shared actions emit the same path. An unchecked I/O exception is rethrown as its cause. A missing package becomes an illegal state that SWIG has no hints. Zel has no SWIG scanner, so that inclusion set is never produced.
  - The hints filename returns null, so no sky function is registered and hints are empty. A non-null hints file would register the hints function and, on an execution exception, abort with an initialize-hints error. Zel evaluates neither the empty-hints path nor that failure.
  - Executor init never builds a spawn include scanner and never sets the output service or in-memory includes. Remote-versus-local extraction and in-memory include files therefore cannot change which headers are found. After the command those caches are cleared so the next command cannot reuse them.
  - The include pool is never created. Bazel uses virtual threads when async execution is on, otherwise a fixed pool or a direct executor, and whether to shuffle is the opposite of async execution, which changes walk order. Shutdown is uninterruptible only when reuse is enabled. Zel does not apply these flags, so scan scheduling and any order-dependent first-wins header choice do not occur.
  - The extraction context returns that command's artifact factory. Without the context, include extraction cannot resolve generated headers to those artifacts.
  - Command options return the scanning options only for the exact command name build. Zel records that class on build and inherits it, but nothing reads the parallelism, reuse, remote-extraction threshold, or in-memory flags to drive a scanner.

### `src/main/java/com/google/devtools/build/lib/includescanning/IncludeScanningOptions.java`
- Bazel lines: 96
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` and `/workspace/zel-cursor/crates/zel-options/src/converters_ext.rs`
- Verdict: partial
- Differences:
  - The parallelism converter is minimum 0, maximum the largest 32-bit int, and auto is the host CPU count. Bazel stores the converted integer. Zel checks the same spec but keeps the raw text. Invocation-policy allow and disallow uses integer equality in Bazel and raw-string equality in Zel, so a leading-zero value matches an allowed 8 in Bazel, because decode is octal, and not in Zel. Auto matches the allowed decimal host-CPU count only in Bazel. Policy errors quote the integer string versus the raw token.
  - Keyword values are checked against different host probes. Bazel uses the runtime processor count and the operating-system bean's total physical memory, then ceiling to an int. Zel uses available parallelism, falling back to 1, and meminfo or the macOS memory size, falling back to 1024 MB, then ceiling to a 32-bit int. Auto-minus-one and a host-RAM factor can pass the range check on one implementation and fail on the other.
  - The remote extraction threshold is a Java int via integer decode. A fullwidth digit is 1 in Bazel. Zel only accepts ASCII digits, so the same flag fails. On the parallelism flag, the Java integer parser also accepts that digit, while Zel's ASCII parser misses and the keyword pattern rejects it.
  - The float operand is a Java float parse. Zel parses a 64-bit float and narrows to 32-bit. For a value the keyword regex accepts, those differ by one unit in the last place. Rounding the host times that operand diverges once the host value is large enough, so a host-RAM factor can yield a different thread count.

### `src/main/java/com/google/devtools/build/lib/includescanning/IncludeScanningSkyFunctions.java`
- Bazel lines: 22
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - The only behavior is the non-hermetic include-hints function name. No crate defines that name. The sky-function names are only the file, directory, package, load, configured-target, action, and precomputed names. The hints function and the module registration of this name are also absent, so the hints file is never a dependency and hint-driven include edges are not produced.
  - Zel's function name has no hermeticity. Bazel uses non-hermetic so this node may be directly invalidated when the hints file changes outside its dependencies. Zel pruning keeps a value when recorded dependencies are unchanged, so a changed hints file would not force re-evaluation the way Bazel's non-hermetic name does.
  - Bazel interns the name and rejects a second name with the same string and a different hermeticity. Equality and hash are the Java string only, not hermeticity. Zel equality is UTF-8 content and has no intern or hermeticity clash check. The literal is ASCII, so the encoding itself does not change the name.

### `src/main/java/com/google/devtools/build/lib/includescanning/InclusionWithContext.java`
- Bazel lines: 121
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - There is no type that pairs an inclusion with the include-path index of the file that contained it and the non-next kind used to keep searching. The legacy scanner uses that pair both as the locate-cache key and to choose the path list and start index, so include-next resolution and quote-versus-angle search are absent.
  - The three-argument constructor rejects a null inclusion, and rejects a next context kind. A null context kind is legal. For a non-next inclusion the stored context kind is overwritten with the inclusion's own kind. For a next inclusion the supplied kind is kept, including null.
  - The name-and-kind constructor builds an inclusion with context position minus 1 and a null context kind, then applies the rewrite. A next kind therefore stays at position minus 1 with a null context kind. Creating an inclusion interns it and stores a path fragment. That path type is also unimplemented.
  - Equality is identity, then type. It always compares the wrapped inclusion and compares context kinds with enum reference equality. The context position is compared only when this inclusion is a next inclusion. Two non-next values with different path positions are equal, so the locate cache can reuse a hit across different positions. The hash mixes 31, adds the position only for next inclusions, and adds the enum hash or 0 if null. The enum hash is the JVM identity hash, stable only inside one process.
  - The context position is a signed 32-bit int. Callers treat minus 1 as top-level, 0 as include-next of a locally found header, and a positive value as the include-path slot to resume after. This file only stores the int.
  - Getting the context kind selects quote paths when it is quote and the system include paths otherwise, including when the kind is null or angle. The string form for a next inclusion includes the kind and the position, and a null kind renders as the characters null. This file has no regex, JSON, or map iteration of its own.

### `src/main/java/com/google/devtools/build/lib/includescanning/LegacyIncludeScanner.java`
- Bazel lines: 993
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - There is no include scanner. The scanning flags exist only as option metadata. Nothing walks include lines or fills the discovered-header set this class produces.
  - Quote search, angle search, and include-next restart at the previous include-path index are absent. Framework includes are also absent.
  - Quoted relative lookup against the includer directory, the source-only first-segment short-circuit, and the rejection of relative includes that escape the package root are absent.
  - The execroot uplevel rewrite, output-tree versus source existence checks, declared-header shortcuts, and illegal-output tracking are absent. The inclusion cache and its partial reuse rules, including the known incorrect reuse after an illegal output was seen, are absent.
  - Transitive visitation is absent: command-line includes, path-level and file-level hints, the modular-header cutoff, the shared parse cache, and the seeded shuffle of inclusions before walking them. With scanning enabled, the compile input set therefore disagrees.

### `src/main/java/com/google/devtools/build/lib/includescanning/PathExistenceCache.java`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - There is no include-path existence cache and no scanner caller. Include scanning never stats paths, so quote-include relative-directory pruning and header-existence decisions are not performed. The include-scanning flag exists only as an option string.
  - File existence is absent. Bazel resolves source exec paths through the artifact factory and non-source paths through the exec root, then checks whether the path is a file, following symlinks. A regular file or special file is true. A directory, a symlink whose target is a directory, or a missing path is false. Any stat I/O exception, including permission errors, becomes false.
  - Directory existence is absent. It always uses the source exec path and checks whether the path is a directory, following symlinks, so a symlink to a directory is true and a symlink to a file is false. Missing paths and stat errors are false.
  - File and directory results are separate concurrent maps keyed by path fragment, filled so the first stat wins and is reused for the rest of the build. A later create or delete of an include path is ignored. Zel never caches or single-flights these stats.

### `src/main/java/com/google/devtools/build/lib/includescanning/SpawnIncludeScanner.java`
- Bazel lines: 475
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No crate runs a grep-includes spawn or parses its output, so the bounding set of include paths this scanner returns is never produced. C and C++ compiles keep the full declared input tree.
  - Include scanning is hardcoded to false, so the scanning flags cannot turn this scanner on. Those flags exist only in the options tables.
  - Whether to parse remotely is absent: absolute artifact roots stay local, a zero threshold forces remote, a non-local output service plus a generated file forces remote, and a missing file or a size greater than the threshold is remote. File size is a 64-bit long compared with a promoted 32-bit threshold.
  - The sidecar layout is absent. Output files with no parent get a grepped-includes name beside the source. Everything else goes under a grepped-includes directory in the exec root.
  - Disk mode deletes the previous sidecar and creates parent directories only when it is not beside the file. In-memory mode skips both.
  - The grep arguments are the tool path, the input path, the output path, and the file-type string. Those path strings are path-fragment strings.
  - Execution info is copied from the owning compile action, then do-not-report is always set. In-memory output also sets inline outputs and no-local so grep cannot write the sidecar on local disk. The spawn environment is empty. Resources are fixed at 10 MB and 1 CPU and ignore the compile action's resource overrides.
  - Spawn inputs are the grep tool then the source. Tool files are empty. The sidecar is an output of the spawn but the output edges and the action outputs are empty, so orphan cleanup does not see it. The primary output is null.
  - Action identity differs from the compile action: the mnemonic is grep-includes, it is not shareable, and the progress message is extracting include lines. When a grep execution platform is passed, its properties replace the compile action's.
  - Only the first spawn result is read. A non-null in-memory byte string is parsed as the includes stream. Otherwise the sidecar path is parsed. Grep stdout and stderr are appended to the parent action's output. None of that diagnostic or parse path exists.

### `src/main/java/com/google/devtools/build/lib/includescanning/SwigIncludeParser.java`
- Bazel lines: 97
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No crate parses SWIG inclusions. Bazel records include, extern, and import only: case-sensitive ASCII after skipping whitespace, and only when whitespace, a block comment, a newline continuation, or a parenthesis group actually advances the cursor. An include glued to a quoted or angle name is ignored. Hints are always null, so no hint inclusions are added. Remote scanning is told file type swig.
  - Skipping parentheses counts raw parentheses with a signed 32-bit int, which wraps through the maximum int, ignores strings, escapes, and comments inside the group, and an unclosed parenthesis consumes the rest of the logical line. An include of a parenthesized name therefore swallows the name and yields nothing. Skipping whitespace uses Latin-1 whitespace, so several C0 controls separate a keyword and a non-breaking space does not, and an unclosed block comment leaves the last byte of the line unconsumed.
  - Quoted and angle names keep quote versus angle. A name whose first byte is a slash is dropped. Each backslash plus newline inside the name is removed. The name is decoded with the platform default charset, not ISO-8859-1. On UTF-8, invalid bytes become U+FFFD.
  - Any other remainder goes through the other-inclusion path. A slice that already starts with a slash is dropped before comment stripping, so a leading space saves an absolute path. The first line comment or block comment at a UTF-16 index greater than 0 cuts the rest of the line, including a real comment marker in a path. A marker at index 0 is kept. Unicode whitespace then trims, which includes a non-breaking space and excludes several C0 controls. A non-empty remainder is a quote include. Whitespace-only is dropped.
  - Validating the inclusion kind rejects next-quote and next-angle, but the keyword result cannot have next, so a local scan never classifies next as include-next and this filter drops nothing it produced.

### `src/main/java/com/google/devtools/build/lib/includescanning/SwigIncludeScanner.java`
- Bazel lines: 64
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - There is no SWIG include scanner. A SWIG source's discovered include set is never computed.
  - Bazel always installs a SWIG include parser with null hints. Remote grep-includes is invoked with file type swig. Hint rewriting does not apply. A missing package is treated as SWIG having no hints.
  - Quote paths are empty. The legacy scanner then sets both the quote list and the angle list to the include paths, and the framework index to 0. There is no separate quote-include list. Relative-to-includer lookup still runs only for quote includes.
  - Framework paths are empty, so framework-header search never runs.
  - The output-tree prefix classifies generated files, illegal outputs, and cc-inc-library headers. Zel does not.
  - Each scanner gets a new path-existence cache. The first-segment directory check that skips relative includes is cached only for that instance.
  - The spawn scanner is fixed at construction. Whether to shuffle, the include pool, and the shared inclusion cache are forwarded. The shuffle uses a constant seed in the parent. Zel has no pool, shuffle, or cache, so both membership and walk order differ whenever a scan would have run.
  - This class does not override the async process. Uplevel rewriting, illegal-output handling, and missing-value restarts are legacy-scanner behavior, which is also absent.

### `src/main/java/com/google/devtools/build/lib/sandbox/AbstractContainerizingSandboxedSpawn.java`
- Bazel lines: 201
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs`
- Verdict: partial
- Differences:
  - `createInputs` copies a non-null file mapping through `copyFile`, creates an empty file when the mapping is null, and symlinks only a non-null symlink target (files win if both maps contain the path). `prepare` always symlinks each input at `exec_root/<path>` and skips the path when any node already exists there.
  - Each input checks `Thread.interrupted()` and throws `InterruptedException`. `prepare` has no per-input interrupt check.
  - Directories come from input parents, mapped output-file parents, output-dir values, and writable dirs under the exec root. `strict=true` rejects absolute fragments and more than one leading `..`. `prepare` uses each path’s own parent plus `output_dirs`, and `create_dir_all` accepts absolute paths and `..` (an absolute `join` leaves the exec root).
  - After a successful populate, `SandboxStash.setLastModified(sandboxPath, currentTimeMillis)` runs when a stash exists. The Rust prepare path never records that timestamp.
  - `moveOutputs` copies sandbox path `value` to exec-root path `key` for both file and directory maps, `deleteTree`s an existing target (failure aborts), and on a failed rename copies a file (retry after `chmod 0644` if unreadable), a directory, or a symlink, or throws for any other type. `move_outputs` uses the same relative path on both sides for `spawn.outputs` only, ignores target-removal errors, and falls back to `copy_tree` (`std::fs::copy` permissions, no unreadable-file retry).
  - `delete` always calls `treeDeleter.deleteTree` and swallows only `IOException`. The Rust runner removes the sandbox only when debug is off, synchronously, and ignores every removal error.

### `src/main/java/com/google/devtools/build/lib/sandbox/AbstractSandboxSpawnRunner.java`
- Bazel lines: 453
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs`
- Verdict: partial
- Differences:
  - A failed subprocess start returns exit `-1` with stderr `Action failed to execute: java.io.IOException: ` plus the exception message, or the exception class name when the message is null, then any sandbox debug file (UTF-8, invalid bytes replaced with U+FFFD) and interactive debug instructions. Zel returns exit `127` and only `Action failed to execute: java.io.IOException: {error}\n`.
  - `createFileSystem` I/O throws `EnvironmentalExecException` (`COPY_INPUTS_IO_EXCEPTION`); other `prepareSpawn` I/O throws `UserExecException` (`EXECUTION_IO_EXCEPTION`, "I/O exception during sandboxed execution"), and the sandbox is still deleted unless `--sandbox_debug`. Zel returns `ExecutionFailed` with exit `1` and `Could not copy inputs into sandbox: …` on stderr, and leaves that sandbox directory in place.
  - Output-copy I/O throws `EnvironmentalExecException` (`COPY_OUTPUTS_IO_EXCEPTION`) instead of a spawn result. Zel overwrites the status to `ExecutionFailed`, sets the exit code to `1`, and appends the I/O error to stderr, so a timeout or other non-zero status is discarded.
  - `useSubprocessTimeout` is never enabled, so the child is not killed by `SubprocessBuilder`. After it exits, a non-zero timeout becomes `TIMEOUT` with exit `142` only when wall time is strictly greater, replacing whatever status the process returned. Zel kills once `elapsed >= timeout` and marks a timeout only if that deadline is seen while the child is still running, so a process that has already exited with wall time past the timeout stays success or non-zero. Zel also appends `Action timed out after {rounded seconds}s` to stderr.
  - Wall time is stored as `(int) milliseconds`, so values past 2^31−1 ms wrap as a signed 32-bit value. Zel stores `u64` milliseconds.
  - `getWritableDirs` omits the sandbox exec root on Windows and requires `TEMP` and `TMP` there, and `TMPDIR` elsewhere (`NullPointerException` if missing). Zel always adds the exec root and uses `TMPDIR` or `/tmp`. A `--sandbox_writable_path` that cannot be symlink-resolved throws on non-Windows. Zel keeps the unresolved path. `..` is normalized before the exec-root prefix check. Zel treats `execroot/../outside` as inside the exec root. Non-UTF-8 path bytes stay Latin-1 in Bazel and become U+FFFD in Zel.
  - A non-empty sandbox debug file and interactive debug instructions are reported as a `DEBUG` event, `Sandbox debug output for {mnemonic} {label}`. Zel does not read or report them.
  - Timeout and non-zero failures use the sandbox command line when `--sandbox_debug` is set (verbose forced), and otherwise the original spawn plus the `--sandbox_debug` suggestion. Zel does not add that suggestion on timeout, and the debug command omits the process-wrapper argv because the wrapper runs in-process.
  - `exec` acquires `spawn.getLocalResources()` at `DYNAMIC_STANDALONE` when speculating and `LOCAL` otherwise, after prefetching inputs. Zel’s runner does not acquire those resources or that priority, and a missing declared input returns `MissingInputs` with exit `1` before sandbox setup.
  - `cleanupSandboxBase` deletes only children of `sandboxBase/<runner>` and propagates delete errors. Zel also removes the runner directory and the base directory when empty, and ignores child deletion errors.

### `src/main/java/com/google/devtools/build/lib/sandbox/AsynchronousTreeDeleter.java`
- Bazel lines: 133
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No async trash mover. Bazel no-ops when the path is missing, otherwise renames it to `trashBase/<n>` (`n` from an `AtomicInteger` starting at 0, decimal, wrapping through negative `int` values) and deletes that path on a background pool. A failed rename deletes the original tree synchronously and does not enqueue. Failures of the background delete are swallowed.
  - The pool is one daemon min-priority thread (core = max = 1, 0s keep-alive, unbounded queue). `setThreads` throws when `threads <= 0` and only changes `maximumPoolSize`, which that queue never uses, so concurrency stays at one. `shutdown` does not wait, then drops the executor, so a later `deleteTree` or `setThreads` throws.
  - The first `deleteTree` creates only `trashBase` (`createDirectory`, not parents) behind an unsynchronized flag. Zel never moves trees under `_moved_trash_dir`. Sandbox cleanup removes the tree in place with synchronous `remove_dir_all` and ignores errors.

### `src/main/java/com/google/devtools/build/lib/sandbox/Cgroup.java`
- Bazel lines: 32
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No cgroup handle reports memory usage as a signed 32-bit kilobyte count (`getMemoryUsageInKb`).
  - No existence check (`exists`).
  - No `addProcess(long pid)` that can fail with `IOException`.
  - No `paths()` set of cgroup directories.
  - No `destroy()` that removes the cgroup.

### `src/main/java/com/google/devtools/build/lib/sandbox/CgroupsInfo.java`
- Bazel lines: 300
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No cgroup support probe: Bazel is supported only on Linux when the memoized blaze-spawns cgroup directory exists and is writable.
  - No root discovery from `/proc/self/mounts` (UTF-8). Non-Linux and read failures become an invalid root. The regex `^cgroup(|2)\s+(\S*)\s+cgroup2?\s+(\S*).*` keeps the last v1 mount whose options contain `memory` and the last v2 mount; a v1 memory mount wins over v2. If neither matches, the result is invalid and includes the mounts path and full file text.
  - No once-per-process blaze-spawns cgroup. If the root directory is missing, Bazel returns invalid and keeps the root version; otherwise it creates it from `/proc/self/cgroup`.
  - No `exists` (non-null directory), `canWrite`, singleton `paths`, or `getCgroupDir`. Invalid cgroups always report not existing and not writable, with a null directory.
  - Base `getMemoryUsageInKb` is always 0. Reading a usage file takes the first UTF-8 line, parses it as a long, divides by 1024, and narrows to a signed 32-bit int; `IOException` returns 0, while a bad number or an empty file propagates.
  - No `addProcess` (UTF-8 decimal pid written to `cgroup.procs`, I/O errors thrown) and no non-recursive `destroy` of the cgroup directory.
  - Invalid cgroups cannot create blaze-spawns or spawn cgroups; both calls ignore the path, directory name, and memory limit and return a new invalid cgroup with a fixed error.

### `src/main/java/com/google/devtools/build/lib/sandbox/CgroupsInfoV1.java`
- Bazel lines: 109
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No v1 cgroup object that records version V1 and creates `blaze_<current-pid>_spawns` under the memory controller path from the first `/proc/self/cgroup` line containing `memory`, split on `:` with a limit of 3 and the third field joined to the mount point.
  - Creating the Blaze-spawns cgroup from a non-root cgroup throws; any read, parse, or missing-memory-controller failure becomes an invalid cgroup instead of a directory. `mkdirs` failure is ignored, and the directory is marked delete-on-exit.
  - No per-spawn cgroup creation that requires a Blaze-spawns parent, refuses a non-writable parent with an invalid cgroup, `mkdirs` `<parent>/<dirName>` (ignoring `mkdirs` failure, delete-on-exit), and, only when `memoryLimitMb > 0`, writes `memory.limit_in_bytes` as the decimal of `memoryLimitMb * 1024 * 1024` in long arithmetic with no trailing newline. A write failure returns an invalid spawn cgroup; `memoryLimitMb <= 0` sets no limit.
  - No `getMemoryUsageInKb` that reads the first line of `memory.usage_in_bytes` as UTF-8, divides the parsed long by 1024, and narrows to a signed 32-bit int, returning 0 on `IOException` while letting a bad or empty file throw.

### `src/main/java/com/google/devtools/build/lib/sandbox/CgroupsInfoV2.java`
- Bazel lines: 143
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never builds a v2 cgroup hierarchy: it does not require a root cgroup, read the first `/proc/self/cgroup` line as UTF-8, split it on `:` with a limit of 3, and resolve the third field under the mount. An empty file throws `IllegalStateException`; any other failure becomes an invalid `BLAZE_SPAWNS` cgroup.
  - It does not create `blaze_<current-pid>_spawns.slice` as a sibling of the process cgroup leaf (`mkdirs`, `deleteOnExit`), nor read `cgroup.controllers` / `cgroup.subtree_control` and write `+memory +pids` when either `memory` or `pids` is absent from subtree control.
  - It does not create `<dirName>.scope` under the spawns cgroup. When `memoryLimitMb > 0` it does not write `memory.oom.group` as `1\n`, `memory.max` as `memoryLimitMb * 1024 * 1024` (int promoted to long), and `memory.swap.max` as `0`. A non-writable parent returns an invalid `SPAWN` cgroup before `mkdirs`; a later write failure returns invalid `SPAWN` after the directory exists. A successful limit of 0 still returns a valid `SPAWN` cgroup with no memory files written.
  - It does not report memory usage by reading `memory.current` and converting to KiB. `crates/zel-sandbox/src/linux.rs` only appends existing paths as `-C` on the linux-sandbox command line.

### `src/main/java/com/google/devtools/build/lib/sandbox/CopyingSandboxedSpawn.java`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No copying sandboxed spawn. `copyFile` stats the source with symlinks unfollowed: a symlink or regular file is copied by following the link, and the copy fails unless the followed target is a regular file, replacing the destination and keeping mtime and permissions. A symlink to a directory therefore fails. A real directory is created at the target (that directory only) and its children are copied: files as above, symlinks recreated with the original link text, nested directories recursed the same way, any other node type throwing, and a destination that is under the source rejected. Anything that is neither a symlink, a file, nor a directory is left untouched.
  - `copyOutputs` always runs the success callback before moving outputs out of the sandbox. A callback failure skips that move. The Docker runner uses the callback to drop the container id from later cleanup only once output copy has started.

### `src/main/java/com/google/devtools/build/lib/sandbox/DarwinSandboxedSpawnRunner.java`
- Bazel lines: 318
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs`
- Verdict: partial
- Differences:
  - Support probe: Bazel returns false unless embedded `process-wrapper` exists on a POSIX OS, then runs `sandbox-exec -p "(version 1) (allow default)" /usr/bin/true` with an empty env and cwd `/usr/bin`, treating any `CommandException` (including non-zero) as unsupported. Zel only checks compile-time macOS and `status.success()` of that same command (`env_clear`), with no `process-wrapper` requirement.
  - With network denied, Bazel writes `(deny network*)`, inbound `localhost:*`, remote `localhost:*`, and remote unix-socket. Zel also allows `(allow network-bind (local ip "*:*"))` and `(allow network* (local unix-socket))`.
  - Bazel forces network when test arguments contain `--wrapper_script_flag=--debug`, which overrides `block-network`. Zel uses only `block-network` / `requires-network` / `default_allow_network` (default true in both).
  - Darwin env rewrite is `XcodeLocalEnvProvider`: `TMPDIR` from the client or `/tmp`, and if `XCODE_VERSION_OVERRIDE` or `APPLE_SDK_PLATFORM` is set it adds `DEVELOPER_DIR` and `SDKROOT` while keeping those keys. Zel’s runner only does the Posix `TMPDIR` rewrite.
  - Sandboxed argv is `sandbox-exec -f sandbox.sb` plus `process-wrapper` (`--timeout=` / `--kill_delay=` as truncated whole seconds, `--stats=stats.out`, `--graceful_sigterm` when the execution requirement or `--experimental_process_wrapper_graceful_sigterm` is set). Zel runs the wrapper outside the sandbox, so the child is `sandbox-exec -f sandbox.sb` plus the spawn args, keeps fractional timeouts (`0` means none; a sub-second Bazel timeout becomes `--timeout=0`), and maps `sandbox-exec` `execvp` status 71 to exit code 1 with a process-wrapper-legacy stderr line. Graceful SIGTERM follows only the spawn execution-info key.
  - Always-writable dirs use `user.home` (platform-to-internal Latin-1) and fail construction if symlink resolution throws. Zel uses `HOME`, skips paths `canonicalize` cannot resolve, and stores `to_string_lossy` paths (`U+FFFD` for non-UTF-8). Profile `subpath` lines are `BTreeSet` order rather than `HashSet` order. `escapeSchemeString(null)` is `""`.
  - `--sandbox_writable_path` entries that cannot be symlink-resolved abort prepare in Bazel. Zel keeps the unresolved string.
  - Sandbox directory ids are a signed 32-bit `AtomicInteger` (`Integer.toString`, wraps negative). Zel uses a `u64`. An exec root with no file name is `execroot/_main` in Zel. `getBaseName()` on `/` is empty, so Bazel uses the `execroot` directory itself.

### `src/main/java/com/google/devtools/build/lib/sandbox/DockerCommandLineBuilder.java`
- Bazel lines: 191
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never builds a `docker run` argv: no `--rm`, `--network=none` when a network namespace is requested otherwise `--network=host`, optional `--privileged`, `-e KEY=VALUE` for each environment entry in map iteration order, `-v <sandboxExecRoot>:/execroot/<basename>` plus `-w` that path, additional `-v source:target` mounts in list order, `-u` only when uid or gid is nonzero (`uid`, `:gid`, or `uid:gid`, signed 32-bit decimal), `-l command_id=…` when the id is non-empty, `--name` from a non-null UUID, then the image and command arguments, all prefixed by the process-wrapper command line with an optional timeout. Bazel rejects a null exec root and empty image or command arguments.

### `src/main/java/com/google/devtools/build/lib/sandbox/DockerSandboxedSpawnRunner.java`
- Bazel lines: 475
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No `docker` spawn strategy and no support probe (`ProcessWrapper`, Linux `getuid`/`getgid`, `docker info`). `--experimental_enable_docker_sandbox`, `--experimental_docker_image`, `--experimental_docker_privileged`, `--experimental_docker_use_customized_images`, and `--experimental_docker_verbose` are declared and never read.
  - No per-spawn sandbox directories, platform `container-image` (`docker://` prefix, exactly one entry) or default-image selection, empty-image `NO_DOCKER_IMAGE` failure, or Linux customized-image `docker build` (skip when not customizing, when uid and gid are 0, or when not Linux; cache by base image).
  - No `docker run` argv (network namespace, privileged, extra mounts, timeout only when non-zero, uid/gid only when greater than 0), no UUID registration with `docker rm -fv` after the command, and no `DOCKER_COMMAND_FAILURE` when the docker client command fails.

### `src/main/java/com/google/devtools/build/lib/sandbox/DockerSandboxedStrategy.java`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel’s strategy identifier is exactly `docker` (`toString()`). Zel’s `StrategyKind` names are `standalone`, `processwrapper-sandbox`, `darwin-sandbox`, `linux-sandbox`, `worker`, `remote`, and `DynamicSpawnStrategy`, so a requested `docker` strategy is unknown.
  - The constructor only forwards the spawn runner and execution options to `AbstractSpawnStrategy`. Zel never constructs a docker strategy or docker spawn runner, so spawns are not executed in Docker.
  - `canExec` and `exec` are the inherited `AbstractSpawnStrategy` path (delegate to that runner). Zel has no docker branch of that path.

### `src/main/java/com/google/devtools/build/lib/sandbox/HardlinkedSandboxedSpawn.java`
- Bazel lines: 134
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Hermetic inputs are not materialized by resolving a symlink (`stat` without following, then the target), hardlinking a file, or walking a directory and hardlinking each child. A failed hardlink is not replaced by a full file copy, including when `--sandbox_debug` is set.
  - A source that is a subdirectory of the target is not rejected. Nodes that are neither a regular file nor a directory are left uncreated.
  - There is no interactive-debug command string: empty when the debug arguments are absent, otherwise the complete command description with the sandbox exec root as the working directory.

### `src/main/java/com/google/devtools/build/lib/sandbox/LinuxSandboxCommandLineBuilder.java`
- Bazel lines: 320
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/linux.rs`
- Verdict: partial
- Differences:
  - Bind mounts follow Java `Map` key order (`HashMap` unordered, `LinkedHashMap`/`ImmutableMap` insertion order). Rust `BTreeMap` emits them sorted by target path string.
  - Java drops `-m` only when `Path.equals` holds (same `FileSystem` instance and equal normalized path). The same path strings on different filesystems stay distinct map keys and still get `-m`. Rust compares and keys by string only, so those collapse and skip `-m`.
  - Writable paths, tmpfs directories, and cgroup dirs are sets: duplicates are dropped by `Path`, `PathFragment`, or `java.nio.file.Path` equality, and flags follow set iteration order. Rust `Vec`s keep duplicates and emit them in vector order.
  - `addWritablePath` is absent. Java replaces a null writable set with a `HashSet`, and `add` on the default `ImmutableSet` throws `UnsupportedOperationException`.
  - Both fake username and fake root throw `IllegalStateException` in Java. Rust returns `Err` with the same message and no argument vector.
  - `Duration.toSeconds()` is a signed `long` (a negative duration is printed with a minus sign; sub-second parts are truncated). Rust `as_secs()` is a non-negative `u64` with the same truncation.
  - Bazel path arguments are UTF-16 `getPathString()` values and may contain unpaired surrogates. Cgroup paths use `java.nio.file.Path.toString()`, which decodes native bytes with the JNU charset and replaces invalid UTF-8 with U+FFFD. Rust arguments must already be valid UTF-8.
  - A null writable/tmpfs/bind/cgroup collection, or a null command argument, throws `NullPointerException` (`ImmutableList` rejects nulls). Rust has no null collections; `Option` omits absent paths the same way a null path field does.

### `src/main/java/com/google/devtools/build/lib/sandbox/LinuxSandboxUtil.java`
- Bazel lines: 122
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - `isSupported` is true only when the embedded tool path `linux-sandbox` plus `OsUtils.executableExtension()` is non-null. Zel never looks up that embedded binary.
  - `validateBindMounts` walks each target-to-source entry and fails on the first problem: missing source (`Mount source '%s' does not exist.`), missing target (Bazel only bind-mounts onto an existing path), or a type mismatch. Directories match only when both sides are directories. Otherwise both sides must be a file or a symlink, and any other mix is reported as file versus directory. Zel does not run these checks.
  - `getInaccessibleHelperFile` and `getInaccessibleHelperDir` create `inaccessibleHelperFile` (touch) and `inaccessibleHelperDir` (create directory) under the sandbox base, then clear executable, writable, and readable in that order. Zel does not create those paths or clear those bits.

### `src/main/java/com/google/devtools/build/lib/sandbox/LinuxSandboxedSpawnRunner.java`
- Bazel lines: 560
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/linux.rs`
- Verdict: partial
- Differences:
  - Support is a live probe in Bazel: non-Linux or a missing embedded `linux-sandbox` is unsupported, then the result is cached per sandbox path and `linux-sandbox` is executed with `-T` set to `localSigkillGraceSeconds`, argv `/bin/true`, an empty subprocess environment, cwd the exec root, and stdout/stderr discarded; any command failure is unsupported. Zel never runs the helper: non-Linux or a missing/`None` binary is a reason string, an existing path is treated as supported with no cache, and `StrategyAvailability::detect` always passes `None`, so `linux-sandbox` is never registered.
  - Spawn preparation is absent. Bazel creates `sandboxBase/linux-sandbox/<id>/execroot/<workspace>`, rewrites the local env (`TMPDIR`), and adds writable `/dev/shm` (symlink-resolved) and `/tmp`. Hermetic `/tmp` (`_hermetic_tmp`) is used unless `useHermetic`, an explicit `/tmp` mount, `/tmp` itself among the known mounts, or `/tmp` as a tmpfs path; it bind-mounts the first two segments of Bazel paths under `/tmp`, rewrites other `/tmp/...` mounts into that directory, and throws if `/tmp` itself is mounted in that mode.
  - Network, identity, cgroups, and spawn type are absent. Bazel opens a network namespace (`-N` with loopback, otherwise `-n`) unless the network is allowed or the spawn requires it; `requires-fakeroot` sets fake root, otherwise fake username may be set; the new cgroup factory or a memory cgroup (`memoryLimitMb > 0` and the cgroup exists) is attached; a non-zero timeout, `stats.out`, optional `debug.out`, and interactive `/bin/sh -i` are passed; `useHermetic` returns a hardlinked spawn and otherwise a symlinked one. Existing inaccessible paths are bind-mounted onto the helper directory or file only when both helpers are non-null.
  - Post-conditions and cleanup are absent. For hermetic sandboxes Bazel throws if a real file input's contents proxy changed (virtual inputs, missing metadata, and non-files are skipped) and removes that spawn's cgroup. Base cleanup deletes the cgroup directory, the virtual cgroup instance, and the inaccessible helpers after `chmod` 0700 (directory) and 0600 (file).

### `src/main/java/com/google/devtools/build/lib/sandbox/LinuxSandboxedStrategy.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - When `sandboxBlockPath` is non-empty, Bazel creates an inaccessible helper file and then an inaccessible helper directory under the sandbox base and still returns a runner if that fails. An `IOException` is a warning, `Could not block access to: ` plus the paths joined with `,` and no spaces. If the file creation throws, both helpers stay null. If only the directory creation throws, the file path is passed and the directory stays null. Zel never creates those helpers, never emits that warning, and never bind-mounts blocked paths onto them.
  - `toString` is `linux-sandbox`. Zel keeps that name on a strategy kind, but startup always treats the helper binary as missing, so the strategy is not registered, and executing it fails with `strategy linux-sandbox cannot execute spawns in this build` instead of running the sandboxed spawn.

### `src/main/java/com/google/devtools/build/lib/sandbox/ProcessWrapperSandboxedSpawnRunner.java`
- Bazel lines: 115
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs`
- Verdict: partial
- Differences:
  - `isSupported` is POSIX (Linux, macOS, FreeBSD, OpenBSD) and requires an embedded `process-wrapper` binary. Zel enables the runner on any `unix` target with no binary check.
  - The sandbox directory id is `Integer.toString` of a signed `AtomicInteger`, so after 2^31−1 it becomes a negative name. Zel uses a `u64` counter (wraps only at 2^64).
  - The execroot workspace segment is `execRoot.getBaseName()` (empty on a root path, so the directory is `…/execroot`). Zel substitutes `_main` when there is no file name.
  - The environment is `LocalEnvProvider.forCurrentOs`: on Darwin it also adds `DEVELOPER_DIR` and `SDKROOT` from `XCODE_VERSION_OVERRIDE` and `APPLE_SDK_PLATFORM` and keeps those keys, in insertion order. Zel's prepare only rewrites `TMPDIR` (client value, or `/tmp`) into a sorted `BTreeMap`.
  - The spawned argv is the `process-wrapper` binary plus `--timeout=` from `Duration.toSeconds()` (always emitted; whole seconds; `0` is no alarm), `--kill_delay` when local options exist (default 15s), `--stats=<sandbox>/stats.out`, and `--graceful_sigterm` for `supports-graceful-termination` or `--experimental_process_wrapper_graceful_sigterm`. Zel runs `spawn.argv` in-process. The timeout is a trimmed `f64` from the `timeout` requirement (fractions run; unparseable values are ignored instead of failing the action). `graceful_sigterm` comes only from execution info and is not applied on cancel, which always sends `SIGKILL`.
  - A failed `execvp` of the action is wrapper exit status 1 with the `execvp` error on stderr (`NON_ZERO_EXIT`). Zel reports `ExecutionFailed` with exit code 127. An empty command is a wrapper usage failure. Zel returns exit code 1 and `empty command line`.
  - On Linux the wrapper sets `PR_SET_CHILD_SUBREAPER` and reaps the process group before returning. Zel calls `setsid` and does not become a child subreaper.
  - Inputs come from `getInputMapping` and `processInputFiles`: nested paths under a non-symlink are skipped, virtual inputs are materialized, empty inputs become empty files, symlink inputs are recreated with their link text, and outputs are remapped with `Spawn.getPathMapper`. Zel symlinks each `spawn.inputs` path to the same exec-root path and skips that nesting, virtual, empty, symlink, and path-mapper behavior. Non-UTF-8 paths stay intact in Java path strings. Zel `to_string_lossy` uses U+FFFD for the stats path, cwd, and writable-dir strings.
  - Writable dirs under the exec root are created, and directory creation rejects absolute paths and more than one `..`. A missing `TMPDIR` or `TEST_TMPDIR`, or a sandbox writable path that cannot be resolved, throws `IOException`. Zel creates those directories without the up-level check, and a failed canonicalize keeps the original string.
  - The returned spawn can reuse a stashed sandbox keyed by mnemonic (an empty mnemonic becomes `_NoMnemonic_`) and target label. Zel always builds a fresh tree and deletes it unless debug is set.
  - An `IOException` while setting up inputs or copying outputs leaves this runner as an execution exception. Zel returns `ExecutionFailed` with exit code 1 and `Could not copy inputs into sandbox` or appends `Could not copy outputs from sandbox` and forces exit code 1. A missing declared input is a dangling symlink in Java. Zel returns `MissingInputs` with exit code 1 and does not run. Copying an unreadable output retries after `chmod 0644` in Java. Zel's copy does not.

### `src/main/java/com/google/devtools/build/lib/sandbox/SandboxHelpers.java`
- Bazel lines: 954
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs`
- Verdict: partial
- Differences:
  - Sandbox reuse is absent: `cleanExisting` / both recursive cleaners never keep a correct symlink or empty file, never delete a wrong symlink, extra file, or unneeded directory, and never chmod `inaccessibleHelperDir` to rwx before a tree delete. `SandboxContents`, `createContentMap`, and ctime-based `updateContentMap` are absent, so a later spawn cannot reuse an in-memory tree.
  - `processInputFiles` is absent. Inputs are not required to be in hierarchical order, entries nested under a non-symlink input are not dropped, entries nested under a symlink are not kept, `VirtualActionInput`s are not atomically written, `EmptyActionInput` is not stored as a null file, and symlink inputs are not recorded as `readSymbolicLink` targets. `prepare` symlinks every `spawn.inputs` path at the exec-root path and skips the link only when the destination already exists.
  - `move_outputs` walks `spawn.outputs` only, using the same relative path for the sandbox source and the execroot target. Java moves every `files` entry before every `dirs` entry, and the sandbox path (`value`) can differ from the exec path (`key`). `output_dirs` are not moved unless they also appear in `outputs`. `getOutputs` path mapping is absent.
  - A missing source still leaves the existing target in place. If the target delete fails, the error is ignored; Java `deleteTree` throws, and for a directory it retries after making the directory readable and executable. A directory that survives that delete is then merge-copied (`create_dir_all` plus children) instead of being replaced.
  - After a failed rename, a symlink is recreated from its link text, a directory is copied sequentially with `read_dir`, and every other type (including fifo, socket, and device) is `fs::copy`. Java copies a file or a directory only after one chmod retry (`0644` / `0755`) on `FileAccessException`, copies a symlink as its link text, and throws on any other type. There is no one-shot cross-device warning. On a non-Unix build the symlink arm returns success without creating a link.
  - `createDirectories` strict mode is absent: absolute paths and multi-segment paths whose second segment contains `..` are not rejected, and `../` joined onto the sandbox root replaces the base. `getTmpDirPath` (`<worker-parent>/<basename>-tmp`) is not treated as an already-existing directory. Interrupted walks do not throw `InterruptedException`.
  - `mountAdditionalPaths` is absent. Parsed `mount_pairs` are never resolved (relative source under the sandbox exec root, target via the filesystem) or inserted so a later source wins, and an illegal path does not become `UserExecException` with `BIND_MOUNT_ANALYSIS_FAILURE`.
  - `shouldAllowNetwork` is absent, so a test argument `--wrapper_script_flag=--debug` does not force network access over `block-network`.

### `src/main/java/com/google/devtools/build/lib/sandbox/SandboxModule.java`
- Bazel lines: 509
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs`
- Verdict: partial
- Differences:
  - A non-empty `--sandbox_base` becomes `{productName}-sandbox.{SHA-256 hex of the output-base path}` and is symlink-resolved except on Darwin. Zel always uses `bazel-sandbox.{MD5 hex}` and never resolves symlinks; a non-UTF-8 output base is hashed as lossy UTF-8 (U+FFFD).
  - The first server build does not wipe an existing sandbox tree (`stale-trash-N` rename, synchronous delete of the inaccessible helper, async or sync delete of other entries, and whole-tree fallback on I/O failure). Zel only creates the base and ignores that error. Bazel turns setup I/O failures into exit 36 (`Sandbox.INITIALIZATION_FAILURE`).
  - No sandbox stash init, clean-on-clean, or shutdown, and no async tree deleter (`--experimental_sandbox_async_tree_delete_idle_threads` does not switch deleters, resize threads, or force another first-build wipe). The after-command check that only persistent entries remain (`.DS_Store`, stash, trash), including the inaccessible-helper chmod `0700` cleanup, is absent, so unexpected leftovers do not raise `IllegalStateException`.
  - `linux-sandbox` is never registered: support is probed with no binary, so it is always unsupported, and no Linux runner is built. Bazel runs the embedded `linux-sandbox` and, when that succeeds, makes `sandboxed` prefer it over processwrapper. Docker (PATH search for `docker` / `docker.exe`, `--experimental_enable_docker_sandbox`, verbose info when disabled) and Windows sandbox (`auto`/`no`/`yes`, exit 36 when `yes` and the binary is missing or invalid) are not registered.
  - Process-wrapper support is “any Unix,” and Darwin support is only a successful `sandbox-exec` probe. Bazel also requires a process-wrapper binary for both, and runs the Darwin probe with an empty environment plus the client environment.
  - When no sandbox strategy is registered, an empty `--spawn_strategy` still asks for `sandboxed` and fails registry setup. Bazel leaves the default unset unless processwrapper, Linux, Darwin, or Windows sandbox is available, then sets it to `sandboxed` only.
  - End-of-build cleanup ignores I/O errors and, when the base is empty, removes the sandbox base directory. Bazel warns `Failed to delete contents of sandbox …`, deletes each runner directory, and keeps the base for in-flight async deletes.

### `src/main/java/com/google/devtools/build/lib/sandbox/SandboxOptions.java`
- Bazel lines: 420
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs`
- Verdict: partial
- Differences:
  - `MountPairConverter` treats a segment as empty with `String.trim` (code units U+0000 through U+0020). Zel uses `str::trim` (Unicode `White_Space`), so a segment of only NBSP or other Unicode whitespace is rejected, and a segment of only C0 controls such as U+0001 is accepted.
  - `getInaccessiblePaths` lexically normalizes `.`, `..`, and repeated `/` before walking symlinks, so `..` does not cross a symlink, and a failed walk keeps that normalized path. Zel uses `std::fs::canonicalize` (OS realpath) and on failure keeps the original string.
  - Symlink walks stop at 32 links (`FileSystem.MAX_SYMLINKS`) and then keep the unresolved path. OS canonicalize allows a deeper chain and can return the final target.
  - Resolved path bytes are Latin-1 (each byte is one Java char, including symlink targets from `readlink`). Zel uses UTF-8 and `to_string_lossy`, so non-UTF-8 bytes become U+FFFD and non-ASCII paths address different files.
  - The result is an `ImmutableSet` in first-seen order. Zel collects a `BTreeSet` in lexicographic order.
  - A relative `--sandbox_block_path` or `--sandbox_writable_path` is a legal value here; a relative block path later throws `IllegalArgumentException` from `resolveSymbolicLinks`. Zel rejects any of those paths that do not start with `/` while building the sandbox config.
  - `--sandbox_tmpfs_path` is an `AbsolutePathFragmentConverter`: a leading `~/` is expanded with the user home (every `~` is replaced), then the path is lexically normalized (`/tmp/foo/../bar` and `/tmp/foo/` become `/tmp/bar` and `/tmp/foo`). Zel only checks for a leading `/` and keeps the raw string, so `~/...` is rejected.
  - `getLimits` always inserts `"memory"` mapped to `(double) memoryLimitMb`, then applies `--experimental_sandbox_limits` with last-wins and first-seen key order. Zel parses those flags and does not build that map.
  - `--reuse_sandbox_directories` sets `oldNameWarning` to false, so `--experimental_reuse_sandbox_directories` is silent. Zel always records `Option 'experimental_reuse_sandbox_directories' is deprecated: Use --reuse_sandbox_directories instead`.

### `src/main/java/com/google/devtools/build/lib/sandbox/SandboxStash.java`
- Bazel lines: 525
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never stashes or reuses sandbox directories. With reuse enabled, Bazel moves a mnemonic stash's `execroot` into the new sandbox (leaving sibling dirs such as hermetic tmp), then deletes the emptied stash. Zel always creates a fresh `<sandbox>/<runner>/<id>/execroot/<workspace>` and deletes it after the run unless sandbox debug is on.
  - TestRunner reuse is absent: Bazel skips XML-generation and coverage spawns (mnemonic `TestRunner` and exactly one output file), records `TEST_WORKSPACE/TEST_SRCDIR` before the rename, renames that runfiles tree onto the current test's path, and deletes a `TEST_TMPDIR` tree that starts with `_tmp` under `TEST_WORKSPACE`.
  - `--experimental_inmemory_sandbox_stashes` is only a declared, unhonored flag. Bazel asynchronously lists the moved tree, keeps `SandboxContents` and last-modified times, rewrites the contents map after a runfiles rename, and returns `Optional.of(contents)` or `Optional.empty()` when the stash exists but contents were not cached. A missing stash instance returns null.
  - Stash pick order is absent. Bazel prefers the stash whose target package path shares the longest prefix with the current target (`Arrays.mismatch` on `/`-split package segments; a null target ranks stashes with no remembered label first). Ties follow directory-entry order.
  - Reuse is not turned off after I/O failure, and there is no once-only warning. Bazel clears leftover `sandbox_stash` entries on the first access, rebuilds the singleton when the workspace name changes (deleting the old stash), drops the instance when reuse is disabled, and `clean` renames the stash to `__trash` for asynchronous deletion or deletes it in place if that rename fails, then clears the in-memory maps.

### `src/main/java/com/google/devtools/build/lib/sandbox/SandboxedSpawn.java`
- Bazel lines: 76
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs`
- Verdict: partial
- Differences:
  - `useSubprocessTimeout()` defaults to false and is never overridden, so no subprocess timeout is armed and a timeout is reported only when the limit is non-zero and wall time is strictly greater than it. `SandboxRunner::exec_with` always passes `timeout_secs` into process-wrapper, which SIGTERMs the process group once elapsed time is greater than or equal to a positive timeout, then forces timeout status and exit code 142 from the `timed_out` flag.

### `src/main/java/com/google/devtools/build/lib/sandbox/SymlinkedSandboxedSpawn.java`
- Bazel lines: 158
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs`
- Verdict: partial
- Differences:
  - Java reuses a stashed sandbox when `SandboxStash` is active: the stash key uses the mnemonic with null or empty rewritten to `_NoMnemonic_` (the mnemonic passed to the superclass is unchanged), plus environment, outputs, and target label. A null `Optional` skips cleaning; an empty one runs `cleanExisting` on the parent of `sandboxExecRoot` (sibling repos included) and returns without recording a content map; a present map is used to clean, and in-memory mode then stores a new content map. `delete` stashes the tree before the superclass delete. Zel always creates a fresh tree and removes it with `remove_dir_all` unless `--sandbox_debug`.
  - Java `copyFile` always creates a symlink to the source and fails if the destination already exists. Zel skips the destination when `symlink_metadata` succeeds, without checking that the existing link target matches, and the `symlink` call is Unix-only, so on other platforms the input is not created.
  - Java `getInteractiveDebugInstructions` is empty when the debug arguments are null; otherwise it returns the fixed interactive-shell prefix plus a `COMPLETE` command description (`prettyPrintArgs` false, debug argv, environment, no variables to clear, cwd = sandbox exec-root path, null checksum, platform, and runner). Zel has no such instructions.
  - Zel turns sandbox paths into strings with `to_string_lossy` (non-UTF-8 bytes become U+FFFD). Java keeps the path string from the filesystem encoding.

### `src/main/java/com/google/devtools/build/lib/sandbox/SynchronousTreeDeleter.java`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No `TreeDeleter`: `deleteTree` is a caller-thread `Path.deleteTree()` that throws `IOException` on failure, and `shutdown` is a no-op. Zel has no such type.
  - Sandbox trees are removed with `std::fs::remove_dir_all` and the `Result` is discarded, so a failed delete is not reported.
  - `Path.deleteTree` succeeds when the path is missing (`delete` returns false) and deletes a non-directory file. `remove_dir_all` returns an error for both a missing path and a non-directory.
  - `FileSystem.deleteTreesBelow` does not follow symlinks and, if listing or the first child delete fails, chmods the directory readable/executable or writable/executable and retries. `remove_dir_all` has no chmod retry.

### `src/main/java/com/google/devtools/build/lib/sandbox/WindowsSandboxUtil.java`
- Bazel lines: 230
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No availability probe: Bazel runs `<binary> -h` with the client environment, cwd `.`, stderr merged into stdout, and an uninterruptible wait, returning true only when the process exits 0. A start failure or any other exit code returns false (the captured bytes are decoded with the platform default charset and a single trailing `\n` is stripped only for the warning log). Zel never invokes the binary or checks that exit code.
  - No command-line builder: Bazel requires a non-null sandbox path and a non-empty argument list, then emits the binary, optional `-W`, `-T` and `-t` as truncated whole seconds, `-l`, `-L`, one `-w` per writable path in set iteration order, one `-r` per readable-map value (keys unused; the default map is key-sorted until replaced), one `-b` per inaccessible path, `-D` only when debug is set, `--`, then the command. Zel emits none of that argv.

### `src/main/java/com/google/devtools/build/lib/sandbox/WindowsSandboxedSpawn.java`
- Bazel lines: 84
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Windows detour spawn that keeps the real exec root and returns the stored arguments, environment, and mnemonic unchanged.
  - Sandbox debug path and statistics path are always absent (debug goes to stderr; no per-process statistics). Zel has no Windows spawn that reports both as missing.
  - `createFileSystem`, `copyOutputs`, and `delete` are no-ops. Zel does not implement that lifecycle.

### `src/main/java/com/google/devtools/build/lib/sandbox/WindowsSandboxedSpawnRunner.java`
- Bazel lines: 109
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No spawn runner creates an exec-root temp directory prefixed `windows-sandbox.`, a `work` subdirectory, and rewrites the spawn environment with the Windows local env provider before execution.
  - Unresolved symlink inputs are not rejected with an I/O error whose message lists the symlink paths joined by `", "`.
  - No windows-sandbox command line is built from the spawn arguments with writable dirs (sandbox writable dirs plus each output exec path), readable input files, inaccessible paths, sandbox debug, the configured kill delay, and a timeout only when the context timeout is non-zero.
  - The runner name `windows-sandbox` is not provided.

### `src/main/java/com/google/devtools/build/lib/sandbox/WindowsSandboxedStrategy.java`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No strategy name is `windows-sandbox`. Zel registers `standalone`, `processwrapper-sandbox`, `darwin-sandbox`, `linux-sandbox`, `worker`, `remote`, and `DynamicSpawnStrategy`, so `windows-sandbox` is an unregistered identifier.
  - Spawns are never executed under that strategy. The Java type only forwards to the shared spawn-strategy executor and reports `windows-sandbox`; Zel has no matching strategy or runner.

### `src/main/java/com/google/devtools/build/lib/sandbox/cgroups/Hierarchy.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Rust code parses `/proc` cgroup lines (`id:controllers:path`), skips non-matching lines, splits controllers on commas, or treats a hierarchy as cgroup v2 only when the id is 0 and the sole controller is empty. `Integer.parseInt` failure still aborts the parse.

### `src/main/java/com/google/devtools/build/lib/sandbox/cgroups/Mount.java`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No cgroup mount type with path, filesystem type, and option list, and no `isV2` check that the type is exactly `cgroup2`.
  - No `/proc/mounts` parse: Java keeps lines matching `^[^\s#]\S*\s+(?<file>\S*)\s+(?<vfstype>cgroup2?)\s+(?<mntops>\S*).*`, splits `mntops` on `,`, and builds a path from the file field; non-matches are skipped.
  - Null path, type, or opts throw; reading the mounts file as UTF-8 throws `IOException` on I/O or malformed input. None of that exists in zel.

### `src/main/java/com/google/devtools/build/lib/sandbox/cgroups/VirtualCgroup.java`
- Bazel lines: 299
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never builds a virtual cgroup. There is no memoized singleton, no non-Linux null sentinel (null cpu, null memory, empty paths, null cgroups info), no read of `/proc/self/mounts` and `/proc/self/cgroup`, no `blaze_<pid>_spawns.slice` child, and no shutdown hook that destroys that instance. An `IOException` during creation becomes that null sentinel; other failures (including a duplicate controller key in `ImmutableMap.toImmutableMap`) are not caught.
  - Root discovery stops once both cpu and memory are set. Cgroup v2 looks up the empty-string hierarchy, resolves the mount against that path, and if the result is not the mount itself uses the parent directory. Each v1 mount option and the v2 cgroup is recorded as controller info even when not writable; a non-writable path is skipped. Only the first writable `memory` and `cpu` controller is attached. V2 controller names come from whitespace tokens in `cgroup.controllers` (UTF-8); a missing file fails creation.
  - `createChild` adds a child cpu and/or memory controller only when the parent has that controller, records those paths, and stores the child for later destroy. Child cgroups info stays null except for the instance child, which receives the root info.
  - `destroy` destroys children first, then deletes each existing path as a file/empty directory and ignores a failed delete. `addProcess` overwrites every path’s `cgroup.procs` with the decimal pid in UTF-8.
  - `getMemoryUsageInKb` is 0 when memory is null or the read throws; otherwise it is `(int) (usageBytes / 1024)` (truncating signed 32-bit cast). `exists` is true only when a memory controller is present and reports that it exists.

### `src/main/java/com/google/devtools/build/lib/sandbox/cgroups/VirtualCgroupFactory.java`
- Bazel lines: 113
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No virtual-cgroup factory: no `NOOP` instance that always yields the null cgroup, and no per-id registry with `create`, `get`, and `remove`.
  - `create` does not return the null cgroup when `alwaysCreate` is false and both the default and requested limit maps are empty, or when the resolved CPU limit and the byte memory limit are both zero.
  - Memory is not taken from the request or else the default (`0.0`), scaled by `1024 * 1024`, truncated to a 64-bit integer, and applied with `setMaxBytes` only when that limit is positive and a memory controller exists.
  - CPU is not taken from the request or else the default (`0.0`) and applied with `setCpus` only when the limit is positive and a CPU controller exists.
  - A child named `{name}{id}.scope` is never created under the root, and `remove` does not destroy the stored cgroup before returning it (or return null when the id is absent).

### `src/main/java/com/google/devtools/build/lib/sandbox/cgroups/controller/Controller.java`
- Bazel lines: 49
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no cgroup controller: no `isLegacy`, no `exists` directory check (`Path.toFile().isDirectory()`, which follows symlinks), and no memory or CPU child, limit, or usage operations (`setMaxBytes` / `getMaxBytes` / `getUsageInBytes`, `setCpus(double)` / `getCpus` as `long`).

### `src/main/java/com/google/devtools/build/lib/sandbox/cgroups/controller/v1/LegacyController.java`
- Bazel lines: 33
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No v1 controller reports legacy (`isLegacy()` is always true).
  - No child creation: Bazel resolves `name` under the controller path, calls `File.mkdirs()` (creates the directory and missing parents, and succeeds if it already exists), ignores the boolean so a failed create does not throw, and returns that path. An absolute `name` replaces the parent (`Path.resolve`).

### `src/main/java/com/google/devtools/build/lib/sandbox/cgroups/controller/v1/LegacyCpu.java`
- Bazel lines: 53
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No v1 CPU controller: `child` does not create a nested cgroup controller, and `getPath` does not return the controller path.
  - `setCpus` is absent: it does not read `cpu.cfs_period_us`, round `cpus * period` to a long, and write that quota to `cpu.cfs_quota_us`.
  - `getCpus` is absent: it does not read `cpu.cfs_quota_us` and `cpu.cfs_period_us` and return integer `quota / period`.

### `src/main/java/com/google/devtools/build/lib/sandbox/cgroups/controller/v1/LegacyMemory.java`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel cgroup v1 memory controller stores a controller path, returns it, or creates a child controller at the child path.
  - No zel code writes the decimal byte limit to `memory.limit_in_bytes`.
  - No zel code reads and trims `memory.limit_in_bytes` or `memory.usage_in_bytes` and parses them as a signed 64-bit integer.

### `src/main/java/com/google/devtools/build/lib/sandbox/cgroups/controller/v2/UnifiedController.java`
- Bazel lines: 49
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel cgroup v2 controller reports non-legacy (`isLegacy` false).
  - Nothing reads `cgroup.controllers` as UTF-8 whitespace tokens and, only when that file is readable and `cgroup.subtree_control` is writable, overwrites the subtree file with `+<token>` values joined by a space (separator after every token, including the last). The child directory is still created with `mkdirs` and returned either way; write failures surface as `IOException`, and a failed `mkdirs` is ignored.

### `src/main/java/com/google/devtools/build/lib/sandbox/cgroups/controller/v2/UnifiedCpu.java`
- Bazel lines: 64
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No cgroup v2 CPU controller reads or writes `cpu.max`.
  - `setCpus` is absent: skip the first `cpu.max` token, parse the period with `Scanner.nextInt` (signed 32-bit), set the quota to `Math.round(period * cpus)` (ties toward +∞), and overwrite `cpu.max` with `"%d %d"` (quota, period) and no trailing newline.
  - `getCpus` is absent: if the first token is not a long (unlimited `max`), return `Runtime.availableProcessors()`; otherwise return integer `quota / period` (trunc toward zero). A zero period throws.
  - `child` is absent: it would return a CPU controller at the child cgroup path from `UnifiedController.getChild`.

### `src/main/java/com/google/devtools/build/lib/sandbox/cgroups/controller/v2/UnifiedMemory.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No cgroup v2 memory controller: `child` does not create a child cgroup and return a memory controller for it.
  - `setMaxBytes` does not write the decimal byte limit to `memory.max` and `0` to `memory.swap.max`.
  - `getMaxBytes` and `getUsageInBytes` do not read, trim, and parse `memory.max` and `memory.current` as 64-bit integers.

### `src/main/java/com/google/devtools/build/lib/sandbox/cgroups/proto/cgroups_info.proto`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel never builds a `CgroupsInfo` record of discovered cgroup controllers (`cgroup_controllers`).
  - Zel never stores each controller’s path, writability (`is_writable`), or version (`UNKNOWN` = 0, `V1` = 1, `V2` = 2), including the proto3 defaults of an empty path, `is_writable` false, and `UNKNOWN`.

### `src/main/java/com/google/devtools/build/lib/shell/AbnormalTerminationException.java`
- Bazel lines: 52
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No error carries both the failed command and a command result (stdout, stderr, and termination status) when execution ends abnormally, and nothing exposes that result after the failure the way `getResult()` does.
  - Message-only, cause-only, and message-plus-cause construction are absent. The cause-only Java path sets the exception message to `cause.toString()` and still stores the result.
  - This type is the checked parent of non-zero-exit failures, so one handler covers both signal death and a bad exit while still holding the result. Zel has no such split.

### `src/main/java/com/google/devtools/build/lib/shell/BadExitStatusException.java`
- Bazel lines: 36
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Rust error is raised only for a non-zero process exit, so callers cannot separate that case from signal or timeout termination and from exec failure.
  - The Java type carries the failed command, the command result (stdout, stderr, termination status), a message, and an optional cause, and it has no cause-only constructor. That payload and construction path are absent.

### `src/main/java/com/google/devtools/build/lib/shell/Command.java`
- Bazel lines: 441
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No `lib.shell.Command` port. Nearby spawners (`run_process`, `execute_spawn_local`, ad-hoc `std::process::Command`) are other executors and do not implement this type.
  - A null or empty argv throws `IllegalArgumentException`. `run_process` treats an empty argv as exit 0 with empty stdout and stderr. `execute_spawn_local` returns `ExecutionFailed` with exit code 1 and stderr `empty command line\n`.
  - A relative argv0 that has a parent is rewritten to an absolute path against the working directory, or the JVM working directory when that directory is null. A single path segment is left for `PATH` lookup. Nothing in Zel does that rewrite.
  - A null environment inherits the client environment. A non-null map replaces it. A timeout less than or equal to 0, including `Duration.ZERO` from the shorter constructors, means no timeout and is stored as `timeout.toMillis()`.
  - `execute` and `executeAsync` read stdout and stderr on side threads. The default stdin is immediate EOF (`NullInputStream.read` returns -1). The subprocess is killed on interrupt. A non-zero exit is `BadExitStatusException`, a signal or reader `IOException` is `AbnormalTerminationException`, and a failed start is `ExecFailedException`.
  - Stdin is copied synchronously and completely before the future is returned. An `IOException` while writing stdin is ignored. A null stdin fails the null check. Caller-supplied stdout and stderr streams are never closed.
  - `toDebugString` is `Executing (without brackets): [arg]…; environment: …; working dir: (current)|path`. `getMnemonic` is `<shell command>`.

### `src/main/java/com/google/devtools/build/lib/shell/CommandException.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No error type retains the failing command and is the common supertype of exec-failure and abnormal-termination errors, so there is no `getCommand()` to recover it.
  - The three constructors are absent: message only; cause only, where Java sets the detail message to `cause.toString()` (null message if the cause is null); and message plus cause. The command reference is stored with no null check.

### `src/main/java/com/google/devtools/build/lib/shell/CommandResult.java`
- Bazel lines: 161
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no command-result value pairing stdout and stderr byte streams with a termination status and optional wall, user, and system durations. Bazel rejects a null stream, status, or time optional with `NullPointerException`; an empty optional is valid.
  - `getStdout` and `getStderr` return `toByteArray()`. The shared empty-output stream always yields a zero-length array. The shared not-collected stream throws `IllegalStateException` (`Output was not collected`).
  - `logThis` logs the termination status, then returns without logging output when the stdout stream is the not-collected instance. Otherwise it logs truncated stdout and stderr, so a not-collected stderr still throws from `getStderr`.
  - `setWallExecutionTime`, `setUserExecutionTime`, and `setSystemExecutionTime` wrap a `Duration` with `Optional.of` (a null duration throws) or accept an `Optional` that can be empty. `build()` requires every field.

### `src/main/java/com/google/devtools/build/lib/shell/Consumers.java`
- Bazel lines: 294
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No stdout/stderr consumer pair. Both streams are submitted to one shared cached pool of daemon threads. A later `registerInput` replaces the previous future without cancelling it. `cancel` interrupts both futures. `waitForCompletion` waits uninterruptibly for stdout and only then for stderr, so a stdout failure never joins the stderr reader.
  - Accumulating mode appends into a growable byte buffer and does not close that buffer. Streaming mode rejects a null caller stream, copies into the supplied streams, and closes those streams only when `closeStreams` is true. The input is always closed afterward, and a close `IOException` is swallowed. Streaming does not return captured bytes: `toByteArray()` on the shared sentinel throws `IllegalStateException` ("Output was not collected").
  - A reader `IOException` wrapped in a `RuntimeException` is rethrown as that `IOException`. Any other `RuntimeException` is rethrown as-is. `OutOfMemoryError` is rethrown with no new wrapper. Any other `Error` becomes `Error("unhandled Error in worker thread", …)`. Any other cause becomes `RuntimeException("unknown execution problem", …)`.

### `src/main/java/com/google/devtools/build/lib/shell/ExecFailedException.java`
- Bazel lines: 28
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel throws `ExecFailedException` when subprocess start fails with an `IOException`, storing the `Command` and the cause. `getMessage()` is `cause.toString()`, or null if the cause is null, and there is no `CommandResult` or exit code. Nothing else in the shell exception hierarchy is used for this path.
  - Zel has no equivalent type. A local spawn that fails inside `process_wrapper::run` (including `Command::spawn`) is an `Ok` `SpawnResult` with `SpawnStatus::ExecutionFailed`, exit code 127, empty stdout, and stderr `Failed to execute '<argv0>': {e}\n` (`/workspace/zel-cursor/crates/zel-sandbox/src/lib.rs`). The sandbox runner uses the same status and exit code 127 with stderr `Action failed to execute: java.io.IOException: {e}\n` (`/workspace/zel-cursor/crates/zel-sandbox/src/sandbox.rs`). The Rust `io::Error` display is not Java `IOException.toString()`.
  - `ExecutionFailed` is also used for empty argv and sandbox input/output copy failures, those with exit code 1, so a failed exec is not a separate control-flow outcome from those cases.

### `src/main/java/com/google/devtools/build/lib/shell/ExecutionStatistics.java`
- Bazel lines: 156
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/process_wrapper.rs`
- Verdict: partial
- Differences:
  - Java `getResourceUsage` returns an empty `Optional` when `resource_usage` is unset and when the file is missing (`FileNotFoundException`); any other `IOException`, including a corrupt protobuf, is thrown. Rust `decode_execution_statistics` takes bytes only and returns `Ok` with an all-zero `ResourceUsage` when field 1 is absent, so a missing file and an unset message are not distinguished from a present zero usage; malformed input is `Err`.
  - User and system time in Java are `Duration.ofSeconds(sec, usec * 1000)`: the `long` multiply wraps, a negative or ≥1e9 nanosecond adjustment is normalized into seconds, negative durations are kept, and second overflow throws `ArithmeticException`. Rust clamps negative seconds and microseconds to 0, truncates microseconds to `u32` before `* 1000` (overflow panics in debug and wraps in release), and `Duration::new` panics when nanoseconds are ≥ 1_000_000_000 instead of carrying into seconds (including `usec >= 1_000_000`).
  - The other resource counters are the proto `int64` values unchanged on both sides.

### `src/main/java/com/google/devtools/build/lib/shell/FutureCommandResult.java`
- Bazel lines: 136
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel type blocks on an already-started subprocess and returns only after it terminates, with a non-blocking finished check.
  - No kill-on-interrupt path that destroys the subprocess, keeps waiting until it exits, then reports interruption without restoring the interrupt flag.
  - No kill-disabled path that swallows interruption, continues waiting, and restores the thread interrupt flag after exit.
  - No post-exit branch that cancels stdout/stderr consumers when the thread is already interrupted, and no interrupt-during-wait path that cancels those consumers and closes the process before propagating the interrupt.
  - No consumer I/O failure path that discards captured output to empty stdout and stderr, then throws abnormal termination on a successful status, or bad-exit versus abnormal termination (exited versus not exited) with the retrieval-error suffix when the status is unsuccessful.
  - No success path that returns accumulated stdout/stderr plus termination status only when the status is successful, and otherwise throws bad-exit when the process exited or abnormal termination when it did not.

### `src/main/java/com/google/devtools/build/lib/shell/InputStreamSink.java`
- Bazel lines: 132
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No discard sink: Bazel loops `InputStream.skip(Integer.MAX_VALUE)` until a following `read()` returns -1, and on `IOException` from `skip` falls back to reading into a shared 4096-byte buffer until EOF.
  - No copy sink: Bazel reads 2048-byte chunks, writes each chunk, and flushes the output after every read while `read` returns `>= 0`, without closing the output.
  - I/O failure becomes an unchecked `RuntimeException` wrapping `IOException`. A null `in` or `out` throws `NullPointerException` with message `"in"` or `"out"` before a runnable is returned, and the factory does not start that runnable.

### `src/main/java/com/google/devtools/build/lib/shell/JavaSubprocessFactory.java`
- Bazel lines: 258
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No subprocess factory applies platform-encoded argv, a cleared environment replaced by `env` or else `clientEnv` (platform-encoded keys and values), a working directory, stdout/stderr actions, and `redirectErrorStream` together.
  - Redirect actions are absent: `DISCARD` sends the stream to `/dev/null`, `REDIRECT` deletes an existing file and then appends, `STREAM` uses a pipe, and `redirectErrorStream` true merges stderr into stdout.
  - Deadline handling is absent: a positive timeout is `currentTimeMillis + timeoutMillis` with exact addition (overflow throws), otherwise there is no deadline. `finished` and `waitFor` call `destroy` (SIGTERM) once only when now is strictly past the deadline; `waitFor` then blocks until exit, and a negative remaining wait throws. `isAlive` does not kill or set timed-out. A non-positive timeout waits `Long.MAX_VALUE` milliseconds.
  - `destroy` always SIGTERMs and returns true; `close` is `destroyForcibly` (SIGKILL) without waiting; `exitValue` throws if the process is still running; the process id is `Process.pid()`.
  - Start is not serialized by a per-factory lock or forced onto one daemon `subprocess-fork` thread with an uninterruptible wait. An `IOException` whose message contains `Failed to exec spawn helper` is not wrapped as the JDK-upgrade `IllegalStateException`; other causes are not rethrown as that `IOException` or unchecked exception, or else as `IllegalStateException` (`Unexpected error starting subprocess`).

### `src/main/java/com/google/devtools/build/lib/shell/LogUtil.java`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no helper that maps a byte array to a log string: `null` or a zero-length array becomes `""`; a length above 150 is the first 150 bytes decoded as UTF-8 plus `[... truncated. original size was <length> bytes.]`; a shorter array is decoded with the JVM default charset.
  - Both Java `String` constructors replace malformed sequences with U+FFFD and do not throw, so the catch result `IOUtil.toTruncatedString: ` plus the exception message is only for an unexpected exception. The 150-byte cut can split a multibyte character, and `<length>` is the Java array length (`int`).

### `src/main/java/com/google/devtools/build/lib/shell/ShellUtils.java`
- Bazel lines: 235
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/java_native.rs`
- Verdict: partial
- Differences:
  - `shellEscape` and `prettyPrintArgv` are not ported. ShellUtils quotes unless every UTF-16 code unit is `Character.isLetterOrDigit` or in `@%-_+:,./` (Unicode letters and digits stay bare, `~` is quoted, a non-BMP character is quoted because each surrogate fails that test). An empty word is `''`; `prettyPrintArgv` joins those escapes with a single space.
  - `windowsEscapeArg` is not ported. Empty becomes `""`; the argument is quoted only if it contains a space or `"`; a backslash run is doubled only when it ends at `"` or at the end of the string.
  - `shell_tokenize` splits on every Unicode whitespace scalar. Java splits only on `' '` and `'\t'` and walks UTF-16 code units, so newline, CR, VT, FF, and non-breaking space stay inside a token.
  - In double quotes, Java keeps a backslash unless the next code unit is `\` or `"`. `shell_tokenize` always drops it, so `"\n"` is `\n` in Java and `n` in Zel.
  - Java throws `backslash at end of string` or `unterminated quotation` (`ctx.tokenize` then reports `{message} while tokenizing '{input}'`). `shell_tokenize` returns `unterminated escape` or `unterminated quotation in: {input}`.
  - `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs` `shell_tokenize_words` does not fail: a trailing backslash is dropped and an unterminated quote is still emitted, with the same whitespace and double-quote backslash differences.
  - `/workspace/zel-cursor/crates/zel-options/src/converters_ext.rs` `run_under` uses that same splitter. A trailing backslash is `Not a valid command prefix unterminated escape` rather than `Not a valid command prefix backslash at end of string`.

### `src/main/java/com/google/devtools/build/lib/shell/Subprocess.java`
- Bazel lines: 110
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No subprocess handle that returns whether `destroy` killed the process, returns an exit code only after termination (otherwise failing), or treats `finished` as a poll that may kill the process on timeout.
  - No side-effect-free liveness check, separate timeout flag, interruptible `waitFor`, or nullable stdin/stdout/stderr streams when those are redirected to files.
  - No process-id query, `close` that terminates and releases native process state, uninterruptible wait that retries through interruption and restores the interrupt status, or destroy-then-wait until the process has exited.

### `src/main/java/com/google/devtools/build/lib/shell/SubprocessBuilder.java`
- Bazel lines: 254
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No subprocess builder. The static factory is `WindowsSubprocessFactory` when JNI is available and the OS is Windows, otherwise `JavaSubprocessFactory`; `setDefaultSubprocessFactory(null)` restores that choice, and `start()` delegates to `factory.create` and propagates `IOException`.
  - `setArgv` rejects a null or empty argv. After `StringEncoding.internalToPlatform`, argv[0] must be absolute or a single filename (`File.getParent() == null`); a relative path with a directory component throws `IllegalArgumentException`. Absolute-ness follows `java.io.File` on the current OS.
  - Environment defaults to the constructor’s client env (`setEnv(null)` keeps that); a non-null map is snapshotted. stdout and stderr default to `STREAM`. `setStdout(REDIRECT)` throws `IllegalStateException` and does not change prior state; `STREAM` or `DISCARD` clears the stdout file; `setStdout(File)` and `setStderr(File)` force `REDIRECT`, including a null file. `setStderr` still stores the file when `redirectErrorStream` is true.
  - `timeoutMillis` defaults to 0, `redirectErrorStream` defaults to false, and a null working directory means the current process directory. Argv stays unset until `setArgv`.

### `src/main/java/com/google/devtools/build/lib/shell/SubprocessFactory.java`
- Bazel lines: 26
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No zel factory creates a subprocess from builder parameters and fails when the executable cannot be found or is not executable. Launch sites call `std::process::Command` directly, so a missing or non-executable program is reported by that caller (process error, spawn status, or a wrapper exit code) instead of one `IOException` from `create`.

### `src/main/java/com/google/devtools/build/lib/shell/TerminationStatus.java`
- Bazel lines: 315
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/test_runner.rs`
- Verdict: partial
- Differences:
  - Signal deaths are raw statuses `[129, 191]` (signals 1–63) when not timed out. `format_exit_status` treats `129..=192` (signals 1–64) as a signal, so status 192 is `"Signal 64"` instead of `"Exit 192"`.
  - Timeout is its own state: `exited()` is false, `toShortString` is `"Timeout"`, `toString` is `"Timed out"`, and `getExitCode` / `getTerminatingSignal` throw `IllegalStateException`. `format_exit_status` takes only an `i32`, never returns `"Timeout"`, and never throws, so a timeout stored as 0 is `"Exit 0"`.
  - Signals 1–31 have glibc names (16–31 include `"Stack fault"`, `"Child exited"`, `"Continued"`, the `"Stopped"` variants, `"Urgent I/O condition"`, `"CPU time limit exceeded"`, `"File size limit exceeded"`, `"Virtual timer expired"`, `"Profiling timer expired"`, `"Window changed"`, `"I/O possible"`, `"Power failure"`, `"Bad system call"`). Zel names only 1–15 and renders 16–31 as `"Signal {n}"`.
  - `success()` is `exited() && exit == 0`, so a timeout or a code in `[129, 191]` is unsuccessful. `crashed` is true only for 134 (SIGABRT) and 138 (SIGBUS). Neither predicate exists.
  - `getTerminatingSignal` is `waitResult - 128` and throws when the process exited or timed out. `toString` is `"Process exited with status N"` or `"Process terminated by signal N"`. Zel has no equivalent throw or those strings.
  - `equals` and `hashCode` use only `waitResult`, ignoring `timedOut` and the wall, user, and system durations. The builder throws `IllegalStateException` until both `waitResponse` and `timedOut` are set. Those durations default to empty. That state and those failures are absent.

### `src/main/java/com/google/devtools/build/lib/shell/WindowsSubprocess.java`
- Bazel lines: 341
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no Windows subprocess type (no `cfg(windows)` handle, waiter, or native stdio). Bazel submits a daemon cached-pool waiter that maps native `waitFor` `0` to success, `1` to timeout (the native wait already terminated the process), and any other code to `IllegalStateException` with the OS pid and last-error string.
  - A timeout `<= 0` is stored as `-1` (no timeout). `timedout()` stays false until `waitFor()` observes the waiter timeout; `finished`/`isAlive` follow that future. `waitFor` rethrows an unchecked waiter failure and wraps a checked cause as `IllegalStateException("Unexpected exception", cause)`.
  - `destroy`, `exitValue`, stdin write, and stdin close throw an empty `IllegalStateException` once the native handle is invalid. `exitValue` throws `IllegalStateException` when the native last-error string is non-empty, otherwise returns the 32-bit exit code. Stdin writes loop until `len` bytes are accepted; native `-1` is `IOException`, and a `0`-byte write is not rejected.
  - Stdout and stderr are null when redirected to files; stdin is always a stream. `available` on a closed stream throws `IllegalStateException("Stream already closed")`; `read` on a closed stream throws an empty `IllegalStateException`. Native read `0` is EOF (`-1`), `-1` is `IOException`, and a one-byte read returns the unsigned value `0`–`255`.
  - `close` (and the cleaner) closes each non-null stream and deletes the native process once. `getProcessId` returns that native handle, which becomes the invalid handle after cleanup, not the OS pid.

### `src/main/java/com/google/devtools/build/lib/shell/WindowsSubprocessFactory.java`
- Bazel lines: 160
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No Win32 subprocess factory: absolute argv0 is not rewritten to a Windows path (`/` to `\`); a relative argv0 is left unchanged, and argv0 is not quoted by this layer.
  - Remaining arguments are not joined with spaces, unescaped when argv0 is `cmd.exe` and otherwise passed through `windowsEscapeArg`.
  - No UTF-16LE environment block: case-insensitive map of `env` or, when `env` is null, the client environment; `SYSTEMROOT` and `SYSTEMDRIVE` filled from the client env only when missing or null; names containing `=` dropped; each entry `key=value` plus NUL, then a final NUL; an empty map is the four zero bytes of `"\0\0"` in UTF-16LE.
  - Redirects are not mapped to `NUL` (discard), the file path (redirect), or null (stream). CreateProcess failure does not delete the native process and throw `IOException` with the last error; timeout and stdout/stderr-redirected flags are not passed through.

### `src/main/java/com/google/devtools/build/lib/standalone/DummyCppIncludeExtractionContext.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No local `CppIncludeExtractionContext` is registered, so nothing returns `CommandEnvironment.getSkyframeBuildView().getArtifactFactory()` as the artifact resolver used when post-compile include discovery looks up headers.

### `src/main/java/com/google/devtools/build/lib/standalone/DummyCppIncludeScanningContext.java`
- Bazel lines: 32
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Bazel `findAdditionalInputs` always throws an unchecked `UnsupportedOperationException` whose message is `Include scanning unexpected for ` plus `action.describe()`, and never returns an artifact list or null. The execution context and include-scanning header data are ignored.
  - Zel has no include-scanning context and no equivalent throw when include scanning is invoked.

### `src/main/java/com/google/devtools/build/lib/standalone/StandaloneModule.java`
- Bazel lines: 104
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/strategy.rs`
- Verdict: partial
- Differences:
  - `standalone` and `local` are one `StandaloneSpawnStrategy` whose `canExec` is false when the spawn's path mapper is not a no-op. `StrategyKind::Standalone::can_exec` is always true, so path-mapped spawns still take standalone/local.
  - An empty `--test_strategy` keeps the last registered `TestActionContext`, `StandaloneTestStrategy`, whose attempt group interrupts other attempts for the same owner and shard. `--test_strategy=exclusive` uses `ExclusiveTestStrategy`, which delegates execution but its attempt group does not interrupt. Zel never cancels sibling attempts. Explicit `--test_strategy=standalone` also injects `no-remote-exec` and `no-sandbox` (`zel-server` `exec_phase.rs`); Java only selects the test context, so the spawn registry can still sandbox or run remotely.
  - The test temp root passed into that strategy is `workspace`-relative `--test_tmpdir` plus `_tmp`, or `execRoot/_tmp` when the flag is unset (an absolute `--test_tmpdir` replaces the workspace base). Zel joins the raw flag path (cwd-relative if relative) with `_tmp`. `bazel run` of a test (`zel-server` `lib.rs`) drops `_tmp` when the flag is set (`<flag>/<hash>`).
  - The local runner this module installs denies `--allowed_local_actions_regex` misses with status `EXECUTION_DENIED` and exit code `-1`, using `java.util.regex` full matches. Zel reports `ExecutionFailed` with exit code `1` and the Rust `regex` crate.

### `src/main/java/com/google/devtools/build/lib/worker/ErrorMessage.java`
- Bazel lines: 144
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - An unset or empty message stays `"Unknown error"` (empty after `String.trim()`, code points `<= U+0020`). Zel prints only the worker error string plus a newline, with no default and no trim.
  - `logText` is trimmed the same way; `""` becomes `"(empty)"` and is still emitted. Zel skips the section when `log_contents()` is empty and does not trim.
  - Logs longer than `logSizeLimit` (`int`, must be `> 0`, default `Integer.MAX_VALUE`; callers use 8192 and 4096) are cut to the last N UTF-16 code units, with a `" snippet"` header, `"[... truncated ...]\n"`, and `"N chars omitted"`. Zel never truncates.
  - The log file is decoded as UTF-8 with malformed bytes replaced (`InputStreamReader`). A read failure still prints the path and `"ERROR: IOException while trying to read log file:\n"` plus the stack, and forces the limit back to `Integer.MAX_VALUE`. `read_to_string` fails on invalid UTF-8 and on I/O errors; `unwrap_or_default()` yields `""`, so the log section is dropped.
  - The Java frame is `\n\n---8<---8<--- Start of log[, snippet][, file at <path>] ---8<---8<---\n` then the body then `\n---8<---8<--- End of log...`. Zel writes `---8<---8<--- Start of log, file at <mnemonic>\n` plus the raw log plus `---8<---8<--- End of log\n`: mnemonic instead of the log path, no snippet marker, and no guaranteed newline before the end marker.
  - A non-null exception inserts a trimmed `printStackTrace` (causes and suppressed) between the exception markers; `exception(null)` omits that block. Zel has no exception section.
  - Java order is message, then exception, then log, and that formatter is used for prepare, request, finish, empty response, and unparsable response. Zel appends a log only on the `exec` error path.

### `src/main/java/com/google/devtools/build/lib/worker/JsonWorkerMessageProcessor.java`
- Bazel lines: 185
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Duplicate `arguments`, `inputs`, `requestId`, `verbosity`, `sandboxDir`, or an input `path`/`digest` throws `IOException`. `work_request_from_json` keeps the first value and also accepts snake_case (`request_id`, `sandbox_dir`), which this parser skips as unknown.
  - `cancel` is never read, so it stays false. Rust sets it from a JSON boolean.
  - `requestId` and `verbosity` use `JsonReader.nextInt()`: a quoted or bare integer that fits in signed 32-bit, otherwise `NumberFormatException` or `IOException`. Rust parses numbers as `f64` and saturating-casts to `i32` (fractions truncate; out of range and NaN become `i32::MAX`, `i32::MIN`, or 0) and maps bad numeric strings to 0.
  - `nextString()` accepts a numeric token as its literal text for arguments, path, sandboxDir, and digest. Rust keeps only JSON strings, drops other array elements, and ignores null or wrong-typed fields instead of throwing. Non-object inputs throw here and are skipped there.
  - Input `digest` is `ByteString.copyFromUtf8` of the JSON string (opaque UTF-8; an unpaired `\u` surrogate becomes `?`). Rust base64-decodes it and uses an empty digest on failure. Unpaired `\u` surrogates become U+FFFD.
  - The reader is lenient (comments, single quotes, unquoted names, `=` / `=>`). `parse_json` is strict, and `read_json_object` skips any non-`{` bytes between values. Clean EOF throws `IOException` here; `read_json_object` returns `None`.
  - `writeWorkResponse` is compact protobuf JSON with `alwaysPrintFieldsWithNoPresence`, so `exitCode`, `output`, `requestId`, and `wasCancelled` are always present (Gson string escapes, through the writer’s charset). `/workspace/zel-cursor/crates/zel-sandbox/src/bin/zel-test-worker.rs` writes UTF-8 `exitCode`, `output`, and `requestId` only, and never writes `wasCancelled`. `close()` closes the response writer.

### `src/main/java/com/google/devtools/build/lib/worker/JsonWorkerProtocol.java`
- Bazel lines: 128
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Java writes compact protobuf JSON and then a newline (`NDJSON`) before flush. Rust `write_request` writes `work_request_to_json` and flushes with no trailing newline.
  - Java `JsonReader` is lenient (comments, single quotes, unquoted names/strings, `)]}'` prefix, `;` / `=` separators). Rust skips every non-`{` byte before the value, then accepts only strict JSON, so each side accepts documents the other rejects.
  - A repeated `exitCode`, `output`, or `requestId` makes Java throw `IOException`. Rust `field` keeps the first value and ignores the rest.
  - Java reads only `exitCode`, `output`, and `requestId`. Snake-case names are skipped, so `{"exit_code":1}` stays at the proto default `0`. Rust also accepts `exit_code`, `request_id`, and `wasCancelled` / `was_cancelled`. Java always leaves `wasCancelled` false.
  - Java `nextInt` requires an integral value in signed 32-bit range and fails otherwise (out-of-range `NumberFormatException` is not wrapped by the JSON catch). Rust parses numbers as `f64` and casts with `as i32` (fractional truncation, saturation, precision loss past 2^53). Non-numbers and unparsable strings become `0`. `output` must be a JSON string in Rust; any other type is ignored. Java `nextString` accepts a number token as its text and fails on null, bool, or object.
  - Java strings stay UTF-16, so a lone `\u` surrogate remains in `output`. Rust combines a surrogate pair into one scalar and turns a lone surrogate or invalid UTF-8 into U+FFFD.
  - Clean EOF before any `{` makes Java throw `IOException`. Rust `read_json_object` returns `Ok(None)`, which `WorkerProcess::exec` reports as the worker exiting with no response. A `{` followed by EOF is an error on both sides.

### `src/main/java/com/google/devtools/build/lib/worker/ProtoWorkerMessageProcessor.java`
- Bazel lines: 54
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Delimited length is a Java 32-bit varint (`readRawVarint32` / `writeUInt32NoTag`): a size with bit 31 set fails as a negative length, and bits above bit 31 in an over-long varint are ignored. `read_delimited` / `write_delimited` use a `u64` length and allocate `len as usize`, so a length ≥ 2^31 is a huge read and high bits are kept.
  - `writeWorkResponse` flushes stdout in a `finally` block, including when `writeDelimitedTo` throws. `write_delimited` flushes only after `write_all` succeeds.
  - `readWorkRequest` returns a parsed `WorkRequest` (`null` at clean EOF). `read_delimited` returns the raw frame (`Ok(None)` at clean EOF). `WorkRequest::decode` drops unknown fields and rejects wire types 3 and 4; protobuf-java keeps unknown fields on the message and skips groups. Strict UTF-8 and low-32-bit `int32` truncation match.

### `src/main/java/com/google/devtools/build/lib/worker/ProtoWorkerProtocol.java`
- Bazel lines: 56
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - The delimited length is a 32-bit varint (`CodedInputStream.readRawVarint32`). If bit 31 is set, the size is negative and `LimitedInputStream` returns EOF immediately, so `parseDelimitedFrom` yields a default `WorkResponse` (exit code 0) and does not consume the body. Rust `read_delimited` keeps the full `u64` and reads `len as usize` bytes, so the same prefix fails, panics on a too-large allocation, or consumes a different span. Value bits above bit 31 in a 10-byte length are dropped in Java and kept in Rust.
  - `WorkResponse.parseDelimitedFrom` rejects field number 0 and skips an unknown start-group (wire type 3). `WorkResponse::decode` skips field 0 and `skip_field` errors on wire type 3, so `read_response` fails the response.
  - `getResponse` clears `Thread.interrupted()` before the parse and restores that bit afterward. The Rust read has no interrupt flag, so a prior cancellation mark is not preserved across the read.

### `src/main/java/com/google/devtools/build/lib/worker/RecordingInputStream.java`
- Bazel lines: 163
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel has no filter that keeps a capped prefix of an input stream. Before `startRecording`, reads are passed through and nothing is stored; `startRecording` drops any previous buffer and sets the cap. `zel-sandbox` worker I/O only parses stdout as a protobuf or JSON `WorkResponse`.
  - Single-byte `read` passes the returned byte into the remaining-capacity check, so `0x00` is never recorded, while any byte `1..255` is stored when capacity remains. Bulk `read` stores `min(remaining, bytesRead)` and does not store EOF (`-1`). `skip` does not record.
  - `readRemaining` reads only `available()` bytes, limited to leftover capacity, and swallows `IOException`.
  - `getRecordedDataAsString` throws if recording never started. An empty buffer returns `""`. Otherwise the bytes are decoded as Java UTF-8 (invalid sequences become U+FFFD). If they are well-formed UTF-8 and the first 1024 UTF-16 code units contain no match of `[^\p{Print}\t\r\n]` under `UNICODE_CHARACTER_CLASS`, every other such code unit in the full string is replaced with `?`. Invalid UTF-8 or a non-printable in that prefix selects a hex dump of at most 64 lines of 16 bytes, labeled `Not UTF-8` even when the cause was a non-printable, and `first 1024 bytes ` when the buffer is longer. Hex is uppercase and grouped every 8 bytes; the text column prints bytes 32–127 as that code unit and every other byte, including 128–255, as `.`.

### `src/main/java/com/google/devtools/build/lib/worker/SandboxedWorker.java`
- Bazel lines: 245
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Hardened sandbox is not implemented. Java wraps the worker in `linux-sandbox` with writable paths (exec root, `<execroot>/tmp`, each configured path plus its symlink target, resolved `/dev/shm`, and `/tmp`), tmpfs directories, a persistent process, a sorted bind-mount map (`/tmp` to a per-worker tmp dir, additional mounts, and inaccessible paths bound to a helper directory when `isDirectory(NOFOLLOW)` and a helper file otherwise), a network namespace, fake hostname, and fake username only when requested. Rust starts `args[0]` directly with no mounts, namespaces, identity, or tmpfs isolation.
  - Java creates a cgroup before spawn (`cgroupFactory.create(workerId, empty)` or, when `useCgroupsOnLinux` or hardening is set, `worker_sandbox_<id>` with the hardened memory limit, otherwise 0), passes existing cgroup paths into `linux-sandbox`, adds the spawned pid, and destroys the cgroup after each `finishExecution` and again in `destroy`. Rust never creates, joins, limits, or destroys a cgroup.
  - Java builds the worker root through `WorkerExecRoot`: clean the parent tree (including sibling repos), keep the hardened tmp dir, create directories non-strictly, symlink file inputs, create an empty file when the input target is null, materialize symlink inputs and `workerFiles`, and optionally track contents in memory. Rust only creates parent directories and symlinks `spawn.inputs` when the destination is missing, so stale files, null targets, symlink inputs, worker files, the separate tmp dir, and in-memory tracking are skipped.
  - Java always `moveOutputs` from the worker work dir once a response is received. Rust renames each output (copy if rename fails) only after a successful protocol response and only when that sandbox path exists, then deletes input paths. A failed response leaves outputs in the worker root and does not remove inputs.
  - Java `destroy` deletes the inaccessible helper file and directory, `workDir`, and the hardened `*-tmp` directory, logging and swallowing `IOException`. Rust only SIGKILLs the process group and leaves the execroot on disk.
  - Java absolutizes a relative executable that has a parent against `workDir`, sets the cwd to `workDir`, and uses the worker-key environment. Rust leaves `args[0]` unchanged, uses cwd `.../execroot/<workspace>`, clears the environment, and if `PATH` is absent sets it to `/bin:/usr/bin:/usr/local/bin`. Java also fills the sandbox and then starts the process. Rust starts the process and then links inputs.

### `src/main/java/com/google/devtools/build/lib/worker/SandboxedWorkerProxy.java`
- Bazel lines: 133
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Java gives each proxy its own directory `workDir/__sandbox/<signed 32-bit workerId>/<exec-root base name>` and sends that relative path as `WorkRequest.sandbox_dir`. Zel always leaves `sandbox_dir` empty and uses one process-wide cwd, `workers_dir/worker-<u64 id>-<mnemonic>/execroot/<workspace>`, shared by every multiplexed request.
  - `prepareExecution` calls `createSandboxedProcess`, which once wipes `workDir` (`deleteTreesBelow`), materializes only the worker files there, and starts the multiplexer with that cwd. Zel starts the sandboxed worker directly in the shared exec root and does not stage or wipe a separate work directory.
  - Java builds dirs in a `LinkedHashSet` and inputs in a `HashSet` from input files, symlinks, and outputs, runs `cleanExisting` on the sandbox parent, creates directories with `strict=false` (absolute paths and multiple `..` allowed), then `createInputs` (symlink to the file, an empty file when the file mapping is null, or a symlink to the symlink target). Zel only symlinks a spawn input when the destination is missing and creates output parent directories. Stale paths are kept, null file inputs are not turned into empty files, and symlink targets are not applied separately.
  - After `finishExecution`, Java `moveOutputs` skips a missing sandbox source (leaving the exec-root target), otherwise `deleteTree`s the target, creates parents, `renameTo`s, and on failure copies a file (chmod `0644` and one retry if unreadable), a directory tree, or a symlink, throwing on any other type. Zel deletes declared outputs on the real exec root before the request, and after a successful response only `remove_file`s the target (not a directory tree) then `rename`s or `std::fs::copy`s the source, then deletes that request's input symlinks. Concurrent multiplex requests therefore share and mutate one tree.
  - `destroy` is synchronized, drops the multiplexer, then `sandboxDir.deleteTree()`, logging a warning and swallowing `IOException`. Zel kills the process group and does not delete the sandbox tree.

### `src/main/java/com/google/devtools/build/lib/worker/SingleplexWorker.java`
- Bazel lines: 267
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - The child environment is exactly `workerKey.getEnv()`, passed through `internalToPlatform` (raw Latin-1 bytes when `sun.jnu.encoding` is ISO-8859-1). Rust clears the environment, applies UTF-8 `key.env`, and if `PATH` is missing sets it to `/bin:/usr/bin:/usr/local/bin`.
  - When a cgroup factory is set, or `useCgroupsOnLinux` is on and cgroups are supported, the pid is placed in a cgroup (`worker_` plus the signed 32-bit `workerId`, memory limit 0 in the fallback). Rust never creates or attaches a cgroup.
  - `destroy` closes the protocol, then SIGTERMs the child and waits, including from a JVM shutdown hook. Rust starts the child in its own process group and `destroy` sends SIGKILL to that group, with no shutdown hook, so a SIGTERM handler does not run and the worker is orphaned if the parent exits without `destroy`.
  - `getExitValue` is `Optional.of(process.exitValue())` after exit (JDK signal death is 128 plus the signal) and empty while the process is running or not yet started. Rust does not report an exit code. `getProcessId` is -1 before start; Rust `pid()` is a `u32` only after spawn.
  - `diedUnexpectedly` is true only when the process exists, is dead, and was not deliberately destroyed. Rust `is_alive` does not distinguish a crash from SIGKILL.
  - `getResponse` ignores `requestId`, polls every 10ms, and throws `Worker process for {mnemonic} died while waiting for response` if the child dies before any stdout byte is available. Rust blocks in `read` and reports EOF as `worker process did not return a WorkResponse (it exited)` or a parse error, and waits indefinitely if stdout stays open after the child dies.
  - A failed read keeps at most 4096 stdout bytes, shown as UTF-8 with non-printable characters replaced by `?`, or as a hex dump when the bytes are not UTF-8. Rust failure text uses the stderr log via `read_to_string`, which yields an empty string when that log is not valid UTF-8.

### `src/main/java/com/google/devtools/build/lib/worker/WorkRequestHandler.java`
- Bazel lines: 768
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - Zel does not implement the worker-side request loop: reading until EOF, per-request threads (`requestId == 0` waits out the previous singleplex request; a still-active id aborts), cancellation (`wasCancelled` with no output/exit code, or the "does not support cancellation" output when no cancel callback is set), callback failures and interrupts as exit code 1, capturing redirected stdout/stderr as trimmed UTF-8 into the response, idle and CPU-time `System.gc` scheduling, and shutting down with exit code 1 on an uncaught `Error`. `zel-sandbox` only sends requests and cancels from the server, and `zel-test-worker` drops cancel requests without a response.

### `src/main/java/com/google/devtools/build/lib/worker/Worker.java`
- Bazel lines: 164
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - `finishExecution` calls `maybeUpdateStatus(ALIVE)`, which applies only when the current status priority is lower (`NOT_STARTED` → `ALIVE`). Pending-kill and killed statuses stay. Zel has no worker status; `WorkerPool::release` puts every process that returned a `WorkResponse` back in service, and `invalidate` destroys the process on any `exec` error.
  - The worker id stored and returned by this class is a signed 32-bit `int` (it can be negative if the factory counter wraps). Zel allocates `WorkerProcess.id` as a `u64` (`AtomicU64` starting at 1). `pid()` is a `u32` and exists only after a successful spawn, so there is no `getProcessId() == -1` “not started” result.
  - `getExitValue` is `Optional<Integer>`: empty while the process has not exited, otherwise that process’s exit code. `diedUnexpectedly` is a separate “dead, but not deliberately destroyed” check. Zel discards `try_wait` / `wait` statuses; a failed read becomes a fixed error string, and `WorkerSpawnRunner` reports exit code 1. Stdout EOF is always “terminated unexpectedly”, including after `destroy` has already killed the process.
  - `getRecordingStreamMessage` is the hook for the last unparsed stdout bytes. Zel does not keep that prefix. JSON framing uses `String::from_utf8_lossy` (invalid UTF-8 becomes U+FFFD) before parse; a proto decode failure is only the decoder error.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerExecRoot.java`
- Bazel lines: 134
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Zel only stages a sandboxed worker when `key.sandboxed` is set: it symlinks each `spawn.inputs` path from the real exec root if nothing is already at that path, creates parents of inputs and output files, and after a successful `WorkResponse` renames outputs back and deletes those input paths. Java rebuilds the root every request from `workerFiles` plus input files and symlinks, and `cleanExisting` runs from `workDir`'s parent so sibling-repository entries are deleted or kept and an existing symlink is reused only when its target still matches.
  - A null file mapping becomes an empty file; a null symlink mapping is skipped. Zel has no optional destination and never creates an empty file. Tool inputs are hashed into the worker key but are not part of the keep-set, and there is no `extraDirs` entry (hardened-sandbox tmp) that must survive cleanup.
  - Output directories are not created before the worker runs. Copy-back runs only after a successful response: `remove_file` plus `rename`, then `std::fs::copy`. Java `moveOutputs` always deletes the target tree first, moves output files and output dirs, and on a cross-device failure copies a file, a directory tree, or the symlink itself (with one `0644` retry for an unreadable file). A missing source leaves the target in place in both; a directory or symlink output does not survive Zel's copy fallback, and a failed or dead worker leaves the sandbox contents behind.
  - `useInMemoryTracking` (`SandboxContents` plus `currentTimeMillis`, refreshed with `updateContentMap`) is absent, so later cleanups cannot skip unchanged subtrees. Setup is not aborted when the thread is interrupted. Directory creation is not ordered as Java's `LinkedHashSet` (`extraDirs`, then worker files, file inputs, symlink inputs), so a directory symlink can be planted before a nested input and the write follows the link.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerFactory.java`
- Bazel lines: 301
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Worker ids start at 1 in both, but Java uses a signed 32-bit `AtomicInteger` (`getAndIncrement`, wraps through negatives and 0) while Rust uses `AtomicU64` (`fetch_add`).
  - Log file is `{workerType}-{id}-{mnemonic}.log`, and `getWorkerTypeName()` is `multiplex-worker` for a multiplex key and `worker` otherwise. Rust always uses `worker-{id}-{mnemonic}.log`.
  - Sandbox workdir for singleplex is `{base}/{type}-{id}-{mnemonic}/{execRoot basename}`; for multiplex sandbox it is `{base}/{mnemonic}-{type}-{multiplexerId}-workdir/{execRoot basename}` shared by the multiplexer. Rust uses `{workers_dir}/worker-{id}-{mnemonic}/execroot/{workspace_dir_name}` per process (extra `execroot` segment, caller-supplied workspace name, no shared multiplexer workdir).
  - `create` makes `workerBaseDir` and its parents only when it is not already a directory, then calls non-recursive `createDirectory` on the tree-deleter trash base when both are non-null. It does not create the workdir. Rust `create_dir_all`s the workers directory on every start and, if sandboxed, the exec root. There is no trash base.
  - Java only builds a `SandboxedWorkerProxy`, `SandboxedWorker` (hardened sandbox options, tree deleter, in-memory tracking, cgroup factory), `WorkerProxy`, or `SingleplexWorker` and does not spawn in the factory. Rust spawns one process immediately (`env_clear` plus the key env, default `PATH` if absent, new process group). Multiplex is a reader thread on that process, not a shared `WorkerMultiplexer` plus proxy. Cgroups, hardened sandbox, and in-memory tracking are absent.
  - Java logs INFO `Created new …` and `Destroying …` (status, optional `(code: …)`, 32-bit `key.hashCode()`) to the logger and, when a reporter is set, as `Event.info`, then calls `worker.destroy()`. Rust invalidate/destroy SIGKILLs the process group with no status, failure code, hash, or reporter event.
  - `validateWorker` returns false when status is invalid. If an exit code is present it calls `maybeUpdateStatus(KILLED_UNKNOWN)`, logs a WARNING with a 4096-byte log tail when `diedUnexpectedly()`, and returns false. If the combined worker-files hash differs it logs a WARNING of `TreeSet`-sorted paths whose digests differ, as lowercase base16 or `<none>`, and returns false. Rust only drops dead processes via `try_wait` on borrow. A changed files hash is a different `WorkerKey`, so there is no killed-unknown status, digest diff, or 4096-byte log cap on this path.
  - Java `equals` is base dir plus `useCgroupsOnLinux` plus hardened sandbox options (`hashCode` omits the cgroup flag) and is what decides whether the pool is rebuilt. Rust `WorkerPool` has no factory equality, and those cgroup and hardened-sandbox options are not modeled.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerFilesHash.java`
- Bazel lines: 122
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Java expands `spawn.getToolFiles()` (`expandArtifacts`, empty tree artifacts dropped, runfiles trees kept). A runfiles walk rejects an absolute root (`IllegalStateException`), skips null mappings, throws `MissingInputException` (`Missing input metadata for: '<exec path>'`) when metadata is missing, and omits a null digest only for `FileStateType.DIRECTORY` (any other null digest throws `IllegalStateException`). Surviving paths are `pathMapper.map(...)`. Other tools throw the same missing-metadata exception and store `pathMapper.map(execPath)` even when the digest is null. Rust reads `spawn.tool_inputs` from disk and drops any path `fs::read` fails on (missing file, directory, or other I/O error), with no path mapper, no runfiles expansion, and no child expansion of tree artifacts. The exec spawn is built with an empty `tool_inputs` list, so that path hashes nothing.
  - Java updates a Guava SHA-256 hasher with the raw metadata digest and returns a `HashCode`. A null digest NPEs in `getCombinedHash` on `digest.length`. Rust hashes `(digest_fn(file bytes) as a string)`'s bytes with the caller-supplied digest function (tests use SHA-256 hex) and returns that string.
  - Java frames each entry as `0x00`, a little-endian int32 path length, Latin-1 internal path bytes (`StringUnsafe.getInternalStringBytes` throws `IllegalArgumentException` unless the coder is Latin-1), a little-endian int32 digest length, and the raw digest. Rust frames UTF-8 path bytes, `0x00`, the digest string's bytes, and `0x00`, with no length prefixes.
  - Java's `TreeMap` orders by `String.compareTo` (UTF-16 code units) and keeps one digest per path (last insert wins). Rust sorts `(path, digest bytes)` in UTF-8/Unicode-scalar order and keeps duplicate paths, tie-breaking on the digest bytes, so non-ASCII and non-BMP paths differ in both bytes and order.
  - Java keeps the path-to-digest map for a later per-file staleness check; the combined hash is not part of `WorkerKey` equality. Rust does not keep the map, and the combined hash is part of worker-key equality, so a changed digest selects a different pool key.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerKey.java`
- Bazel lines: 241
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Bazel `equals`/`hashCode` ignore `workerFilesCombinedHash` and `workerFilesWithDigests` (validation only). Zel derives `Eq`/`Hash` over `worker_files_hash` and does not store the per-file digest map, so a digest change is a different pool key.
  - `useInMemoryTracking` is stored and included in Bazel equality and the cached hash. Zel has no such field, so keys that differ only in in-memory tracking are distinct in Bazel and identical in Zel.
  - Bazel caches a signed 32-bit `Objects.hash` (31-multiply wrap) of args, env, exec root, mnemonic, multiplex, cancellable, sandboxed, useInMemoryTracking, and `protocolFormat.toString()` (`"JSON"`/`"PROTO"`). Zel uses `DefaultHasher` (`u64`, exposed as metrics `key_hash`), includes the files hash, omits in-memory tracking, and hashes the protocol discriminant instead of the enum name.
  - Bazel `toString` is a Bourne shell command (`cd` exec root, `exec`, `env -`, env assignments sorted by key, space-separated escaped args) plus `# Runner: multiplex-worker` when multiplex, otherwise `worker` (`getWorkerTypeName` always passes `mustBeSandboxed=false`). Zel has no equivalent formatting or worker-type name.
  - Exec-root equality is path fragment plus filesystem identity (`Path.equals`). Zel stores a `String`, so the same path text on two filesystems is one key. Bazel also rejects null args, env, exec root, mnemonic, combined hash, and digest map with NPE (a null protocol format NPEs while hashing); Zel's owned fields cannot be null.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerLifecycleManager.java`
- Bazel lines: 336
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No background poller sleeps `workerMetricsPollInterval` and returns immediately when both the total and per-worker memory limits are 0 (`limitMb * 1000` KB).
  - No forced kill of a live worker whose memory is above the individual limit, no `KILLED_DUE_TO_MEMORY_PRESSURE` status, and no always-on warning when a reporter is present. A missing process handle skips the kill.
  - No idle-only eviction that sorts by memory descending, kills until summed usage is at or under the total limit, and logs an empty candidate set only once. Summation and the subtraction comparator are signed 32-bit.
  - No shrink-pool path that, after eviction, marks the remaining over-limit workers `PENDING_KILL_DUE_TO_MEMORY_PRESSURE` instead of killing them.
  - `WorkerOptions` does not store these limits, and live worker metrics omit memory, so none of this control loop runs.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerModule.java`
- Bazel lines: 299
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - The worker directory is `{productName}-workers` under the output base. Zel always uses `bazel-workers`, so a non-default `--product_name` keeps a different tree and a different set of warm workers.
  - A new server's first build deletes every direct `*.log` child of that directory (per-file warning `Could not delete old worker log '%s': %s`, directory warning `Could not delete old worker logs in '%s': %s`). Zel never deletes those logs.
  - The pool is recycled only when the worker directory, `--experimental_worker_use_cgroups_on_linux`, or hardened-sandbox options change (INFO `Worker factory configuration has changed, restarting worker pool...`, always), or when the `--worker_max_instances` / `--worker_max_multiplex_instances` lists differ in content or order (INFO `Worker pool configuration has changed, restarting worker pool...`). If both change, the second message is skipped because the pool is already null. Zel's key is the whole `WorkerOptions` debug string plus the exec root, so `--worker_verbose`, `--worker_quit_after_build`, `--worker_sandboxing`, multiplex, extra flags, cancellation, and strict flagfiles, or a different exec root, kill the pool, while cgroup and sandbox-hardening flag changes do not.
  - `--experimental_worker_sandbox_hardening` builds linux-sandbox options (fake host/user, debug, tmpfs and writable sets, memory limit, inaccessible paths, duplicate mounts kept last) and a Linux `VirtualCgroupFactory` (`worker_` plus sandbox limits only when hardening is on). Those options are not applied in Zel, and workers are not restarted for them.
  - On Linux, cgroup memory metrics are used only when `--experimental_worker_use_cgroups_on_linux` is set and either legacy cgroups work or the new cgroup memory controller exists; otherwise accounting falls back to `ps`. Zel does not make that choice.
  - Each build starts a daemon `WorkerLifecycleManager` (idle eviction from `--experimental_total_worker_memory_limit_mb`, per-worker `--experimental_worker_memory_limit_mb`, shrink, poll interval) and `workerPool.reset()` clears shrink counts. The manager is stopped and interrupted at the end of the build. Zel has no eviction thread and no per-build shrink reset.
  - The first build renames an existing `workerDir/_moved_trash_dir` to `_stale_trash`, recreates the trash directory, and deletes the old tree asynchronously. Failure is `ERROR: Could not trash dir in '%s': %s`. Zel does not.
  - `clean` shuts the pool down first (`Clean command is running, shutting down worker pool...` only if `--worker_verbose`). Zel's clean leaves `WORKER_POOLS` running.
  - `--worker_quit_after_build` closes the pool (idle and still-active workers), drops it, and the next build allocates a new pool and clears process metrics. The info line is verbose-only. Zel calls `shutdown` on the same pool (idle workers and the multiplex process only; a checked-out singleplex worker is not killed) and reuses that object, with no info line.
  - `--worker_verbose` attaches the command reporter to the factory and multiplexers for the command and clears it in `afterCommand`. Zel only copies verbosity into the work request; it does not emit these module info/warn lines.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerMultiplexer.java`
- Bazel lines: 528
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - A relative argv0 with a parent is rewritten to an absolute path under the work directory through Bazel's internal (Latin-1) ↔ platform encoding. Zel passes the UTF-8 `String` to `Command::new` and resolves it via `current_dir`.
  - The child environment is exactly `workerKey.getEnv()`. Zel clears the environment, applies that map, and if `PATH` is absent sets it to `/bin:/usr/bin:/usr/local/bin`.
  - Sandboxed startup wipes `workDir` and materializes only the worker files once (`deleteTreesBelow`, `cleanExisting`, `createInputs` of `limitedCopy(workerFiles)`). Zel makes a new execroot and, per request, symlinks every spawn input into that shared directory, then deletes those symlinks when the request returns, so concurrent multiplex requests share and unlink inputs.
  - Requests are enqueued on a `LinkedBlockingQueue` and written by a dedicated sender thread, so a proxy interrupt cannot split a frame. Zel writes the frame on the caller thread under a stdin mutex. A write `IOException` destroys the process (stdin is treated as corrupt) and releases the waiter. Zel returns the write error, leaves the process up, and leaves the `pending` entry in place so a later response for that id is discarded.
  - A read `IOException` or a null response (closed stdout) destroys the process. `InterruptedIOException` skips the report and still destroys. Zel treats EOF and every parse error the same: every waiter gets `"worker process terminated unexpectedly"`, the reader thread exits, and a still-living process is not killed, so a later `exec` waits forever on a channel nobody reads.
  - `getResponse` returns the buffered `WorkResponse` or null when the process is already dead, and throws `IOException("Worker process for " + mnemonic + " has died")` only after a semaphore wakeup with a null response. Zel returns `Ok` only for a parsed response and `Err` for death, EOF, or a parse failure. A second `putRequest` with the same id replaces the semaphore and the old waiter can block forever. Zel replaces the `Sender`, and the old waiter gets a disconnect error.
  - Cancel requests are queued with no semaphore, so they stay FIFO with already queued work and an unmatched response is dropped. Zel `send_cancel` writes immediately under the stdin lock (it can pass an `exec` that has not taken the lock) and ignores a write failure.
  - `destroyProcess` sends `Process.destroy()` (SIGTERM), then `waitFor` until it exits, which can block forever, then interrupts the sender and receiver and releases every semaphore. A JVM shutdown hook calls `destroyMultiplexer`, which also deletes `workDir` (and warns if a sandboxed multiplexer never had one). Zel puts the child in its own process group, SIGKILLs that group, and does not delete the execroot or record `exitValue`. Java `getExitValue` is the subprocess status. Zel does not surface it.
  - `createProcess` throws if the multiplexer was already killed or the process is already dead, and `getProcessId` returns `-1` when there is no process. Zel replaces a dead shared process from the pool and `pid()` is the live `u32` pid. The last 4096 bytes of stdout are kept for the failure text. Zel has no recording buffer and appends the stderr log instead.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerMultiplexerManager.java`
- Bazel lines: 177
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Bazel drops and destroys the multiplexer when the proxy refcount hits 0. Zel’s multiplex `release` only decrements `active` and leaves `shared` running until `invalidate` or `shutdown`.
  - Removing an unknown key throws `UserExecException` with `MULTIPLEXER_INSTANCE_REMOVAL_FAILURE` (message includes the mnemonic). `getMultiplexer` and `getRefCount` throw `MULTIPLEXER_DOES_NOT_EXIST`. Zel `release` and `invalidate` ignore a missing key and never emit those worker failure details.
  - `getInstance` always hands out the existing multiplexer and keeps the first caller’s log path. Zel `borrow` waits once `active` reaches `--worker_max_multiplex_instances` (default 8, floored at 1), restarts the process when it is not alive, and always logs to `worker-{id}-{mnemonic}.log`.
  - Multiplexer ids are a signed `AtomicInteger` starting at 1 (`getAndIncrement` wraps at `2^31-1`). Zel ids are a process-wide `AtomicU64` shared with singleplex workers (wraps at `2^64`). Proxy counts are a signed `Integer` (`+1`/`-1` wraps; destroy only at exactly 0) versus `usize` (`-= 1` panics on underflow in debug and wraps in release).
  - `beforeCommand` and `afterCommand` set or clear the reporter on every live multiplexer. Zel has no reporter fan-out.
  - The instance map shares keys the way `WorkerKey.equals` does, which omits the worker-files hash and includes `useInMemoryTracking`. Zel `WorkerKey` equality includes `worker_files_hash` and has no in-memory-tracking flag, so the same inputs share or split a multiplexer differently.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerOptions.java`
- Bazel lines: 284
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - `parse_instances_flag` is what execution applies to the raw `--worker_max_instances` and `--worker_max_multiplex_instances` strings. It splits on the last `=`, accepts only a decimal `usize` or the token `auto` (always 4). `MultiResourceConverter` splits on the first `=`, parses with `IntegerConverter(() -> 0, 0, Integer.MAX_VALUE)` (`HOST_CPUS` / `HOST_RAM` and `[-|*]<float>`, `Integer.decode` so a leading zero is octal), treats a whole value of `null` or `auto` as a null key and null value (dropped), and treats `name=auto` as a null value so the pool default applies (4 singleplex, 8 multiplex). `--worker_max_instances=null` and keyword forms such as `HOST_CPUS*.5` are command-line errors (exit 2) here and accepted in Bazel; `010` is 10 rather than 8; `auto` forces 4, so `--worker_max_multiplex_instances=Javac=auto` caps at 4 rather than 8, and a later bare `auto` overwrites an earlier empty-key limit.
  - `--experimental_worker_cancellation` is ignored. `create_worker_key` sets `cancellable` from `supports-worker-cancellation` alone, so a cancel is sent whenever the worker supports it. Bazel sends one only when this flag is also set (default false).
  - `--experimental_worker_allowlist` does not affect `can_exec`. Bazel refuses the worker strategy when that set is non-empty and the worker-key mnemonic is not in it.
  - These flags are not applied: `experimental_total_worker_memory_limit_mb` and `experimental_worker_memory_limit_mb` (default 0; above 0, Bazel evicts idle workers or kills a worker over the per-worker limit, with the limit in KB equal to MB times 1000), `experimental_shrink_worker_pool`, `experimental_worker_metrics_poll_interval` (default `5s`), `experimental_worker_use_cgroups_on_linux`, `experimental_worker_sandbox_hardening`, and `experimental_worker_sandbox_inmemory_tracking`.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerParser.java`
- Bazel lines: 254
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - `createWorkerKey` forces a sandbox when `dynamic` is true or `Spawns.usesPathMapping(spawn)` is true, and `compute` passes `context.speculating()`. `create_worker_key` sandboxes only for `dynamic`, and `WorkerSpawnRunner.exec` always passes `false`.
  - When the worker is sandboxed and its key mnemonic is in `workerSandboxInMemoryTracking`, Java sets `useInMemoryTracking` and includes that bit in `WorkerKey` equality. The Rust key has no such field, so the mnemonic list never changes identity or sandbox cleanup.
  - Java keeps the combined file hash and the `SortedMap` of per-file digests off `equals`/`hashCode` and uses them to invalidate the existing worker. Rust `WorkerKey` derives `Eq`/`Hash` over `worker_files_hash`, so a tool-input change is a new pool key. Java’s hash is SHA-256 of `0x00`, a big-endian int32 path length, Latin-1 path bytes, a big-endian int32 digest length, and the metadata digest, over path-mapped tool and runfiles entries (`TreeMap` order); missing metadata throws `MissingInputException`. Rust hashes UTF-8 path bytes, `0`, the injected digest of bytes reread from disk, and `0`, and drops tools it cannot read.
  - Flag detection uses `java.util.regex` `(?:@|--?flagfile=)(.+)`, full-match, where `.` rejects line terminators (`\n`, `\r`, U+0085, U+2028, U+2029). `flag_file_arg` accepts any non-empty suffix of `@`, `-flagfile=`, or `--flagfile=`, including those characters. `@@` handling matches: strict mode rejects a leading `@@`, legacy mode treats it as a flag file.
  - Flag-file failures are a `UserExecException` with `Worker.Code.NO_FLAGFILE`, and the message uses platform `%n` before the argument list. Rust returns only the message with a hardcoded `\n` and does not carry that failure code. An invalid `requires-worker-protocol` from `compute` is an `IOException` whose text is `requires-worker-protocol must be set to a valid worker protocol format: json or proto`; `worker_protocol_format` returns `{value} is not a supported worker protocol`.
  - The exec root on the key is a `Path`. Rust stores `to_string_lossy()`, so non-UTF-8 paths become U+FFFD and can share one key. `compute` rewrites the environment with the platform `LocalEnvProvider` (Xcode on Darwin, Windows, otherwise Posix `TMPDIR`). Rust always applies the Posix rewrite (`client` `TMPDIR`, or `/tmp`).

### `src/main/java/com/google/devtools/build/lib/worker/WorkerPool.java`
- Bazel lines: 95
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - `getMaxTotalPerKey`, `getNumActive`, and atomic `hasAvailableQuota` (`max - active > 0`) are absent. Rust only enforces a private `limit()` inside `borrow`, and `n.max(1)` still allows one worker when the configured maximum is 0.
  - `evictWorkers` (destroy only idle workers for the given `Integer` ids, skip any that became active, return the ids actually evicted) and `getIdleWorkers` (idle ids that may still become active) are absent. Rust never selects or reports workers by signed 32-bit id (`WorkerProcess.id` is `u64`).
  - `reset()` is absent. `close()` is only partly `shutdown()`: that drains the map and destroys idle singleplex workers and the multiplex shared process even while `active` proxies still hold it, and it does not destroy borrowed singleplex workers that are not in `idle`.
  - `returnWorker(key, worker)` is `release`, which ignores any caller-supplied key and uses `worker.key`. A missing pool entry drops the worker without returning it to idle or destroying it. Multiplex release only decrements `active` and does not check that the returned process is `shared`.
  - `invalidateWorker` destroys that worker. Rust `invalidate` also decrements `active` and, for a multiplex key, clears `shared` even if another proxy still uses that process, so the next `borrow` starts a replacement while outstanding borrowers see a destroyed worker.
  - `borrowWorker` still blocks and creates when under the limit, but a poisoned mutex or condvar panics instead of throwing `InterruptedException`. A multiplex start failure decrements `active` and returns the error without waiting.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerPoolConfig.java`
- Bazel lines: 60
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - `equals` and `hashCode` use only the two lists (`List.equals` on each `Map.Entry`, so order, duplicate entries, and null keys or values all count). Zel has no equality on those lists. The pool reuse in `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` matches the `Debug` text of the whole `WorkerOptions` plus the exec root, so a change to any other worker flag or to the exec root rebuilds the pool.
  - Each entry is a `String` and a nullable `Integer`. A null count stays null on the config object, and the pool later substitutes 4 for singleplex and 8 for multiplex. Zel stores `(String, usize)`. `parse_instances_flag` maps `auto` to 4 for both lists, so `name=auto` and a bare `auto` become a concrete 4 (bare `auto` also sets the empty-string default key). The token `null` is a `(null, null)` entry in Java and a parse error in Zel. A multiplex `auto` limit is therefore 4 instead of the default 8.
  - Stored counts are signed 32-bit `Integer` values, and this type does not do arithmetic on them. Zel uses `usize`, so a negative count cannot be stored, and a count above `Integer.MAX_VALUE` can.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerPoolImpl.java`
- Bazel lines: 432
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Java keeps a per-key pool whose map key ignores the combined files hash, and `borrowWorker` revalidates idle workers (dead, pending kill, or hash mismatch), destroying failures with `shouldShrinkPool=false`. Rust `HashMap` keys include `worker_files_hash`, so a hash change opens a new pool and leaves the previous processes in place. Rust only drops an idle singleplex worker when `is_alive()` is false.
  - Caps default to 4 singleplex and 8 multiplex, with last-wins and an empty-string fallback, in both. Java stores a null instance count as that default (overwriting an earlier value for the same mnemonic) and will keep 0 or a negative max, which makes `hasAvailableQuota` false. Rust `limit` is `usize` and clamps with `.max(1)`.
  - Java quota is `effectiveMax - acquired` (`acquired` is in-use only; idle workers are not counted). Shrink (`shrunk`, floor effective max 1) applies when an active worker is invalidated with pending memory eviction or when `returnWorker` fails validation; `reset` clears `shrunk`. Idle eviction sets `PENDING_KILL_DUE_TO_MEMORY_PRESSURE` and then returns before the shrink path, so it does not change `acquired` or `shrunk`. Rust never shrinks or resets the cap; `active` counts idle and borrowed singleplex workers together, and a failed `start` rolls the count back. Java increments `acquired` before `factory.create` and leaves it incremented if create throws.
  - Java waits on a FIFO `PendingWorkerRequest` latch: a return or a non-shrinking invalidation hands that waiter the worker or `null` while keeping `acquired` reserved so the waiter creates. Rust waits on a `Condvar` and `notify_all` after pushing the idle worker, so a new borrower can take the slot first.
  - Java uses the same borrow, LIFO idle deque, and active-set accounting for multiplex keys (distinct worker objects up to the multiplex max). Rust multiplexes one shared process, counts concurrent borrows, and `invalidate` `destroy`s that process and clears `shared` while decrementing `active` by one. `returnWorker` / active invalidate throw `IllegalStateException` when the worker is in neither the idle deque nor `activeSet`. Rust `release` drops a worker whose key is already gone, and `invalidate` still `destroy`s it without that check. `usize` decrement panics on underflow; Java `AtomicInteger` wraps.
  - Java `evictWorkers` destroys matching idle workers and returns the requested `Integer` ids that were actually idle, in request order. `getIdleWorkers`, `getMaxTotalPerKey`, `getNumActive`, and `hasAvailableQuota` have no Rust equivalents (`getPool` also creates the pool as a side effect). `close` destroys idle and active workers and leaves them in the deques without waking waiters. Rust `shutdown` drains the map and destroys idle workers plus the shared multiplex process, and borrowed singleplex processes keep running.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerProcessMetrics.java`
- Bazel lines: 212
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Memory sampling is absent. `addCollectedMetrics` stores `memoryInKb`, sets `isMeasurable`, and records `lastCollectedTime`; `onBeforeCommand` copies that memory into `priorMemoryInKb` (both default to 0). `toProto` always emits both memory fields and sets `collect_time_in_ms` only when a collection `Instant` is present. `WorkerMetricsRow` has no memory, measurable flag, prior memory, or collection time.
  - Invocation snapshots are absent. `newlyCreated` starts true and `onBeforeCommand` clears it; `priorActionsExecuted` is a copy of the action counter (starts at 0); `setLastCallTime` stores an `Instant` and `toProto` sets `last_action_start_time_in_ms` only when that value is present. Zel does not keep those fields.
  - `toProto` always sets `worker_status` from the held `WorkerProcessStatus` and sets `code` only when `getWorkerCode()` is present. Zel rows omit status and failure code, and `live_worker_metrics` drops processes that are not alive.
  - Worker ids are an insertion-ordered `List<Integer>`. `maybeAddWorkerId` appends an id only when it is not already present (the status argument is unused), so one process can publish several multiplex ids. Zel assigns one `u64` per OS process (`WORKER_IDS` starts at 1) and emits a single `worker_id`.
  - `actionsExecuted` is a signed `AtomicInteger` (`incrementAndGet` wraps past `Integer.MAX_VALUE`). Zel uses `AtomicU64` (wraps at 2^64) and increments it at the start of `WorkerProcess::exec`, including requests that later fail.
  - `toProto` publishes `processId` as `(int) processId` (low 32 bits, signed). Zel publishes `Child::id()` as `u32`.
  - `workerKeyHash` is the caller-supplied Java `int` (`WorkerKey.hashCode()`). Zel replaces it with `DefaultHasher` of the whole `WorkerKey` as a `u64`.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerProcessMetricsCollector.java`
- Bazel lines: 287
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - Java `getLiveWorkerProcessMetrics` / `getLiveWorkerMetrics` first runs `collectMetrics`, which can mark a non-killed worker `KILLED_UNKNOWN`, then drops every worker whose status `isKilled()`. Pids stay in the map for the whole build, including killed ones, until `clear` or `clearKilledWorkerProcessMetrics`. Rust `live_worker_metrics` lists only pooled processes whose `try_wait` is still running, so a dead process disappears and a still-running process that would have been unmeasurable stays listed. There is no killed-during-build history.
  - Java `collectResourceUsage` returns an empty snapshot when no non-killed pids remain. Darwin, and Linux when `useCgroupsOnLinux` is false (the default), uses `ps`. Linux with the flag drops cgroup entries for pids not in the alive set and uses cgroups. Any other OS returns empty. A missing pid and a real 0 KB reading are both unmeasurable (`getOrDefault(pid, 0)`): if `actionsExecuted > 0`, the status becomes `KILLED_UNKNOWN` (a later, more specific status may still replace it); a positive reading stores that KB value and `clock.now()`. Rust never samples `ps` or cgroups and stores no memory, measurable bit, collection time, prior-memory baseline, or `beforeCommand` reset.
  - Java `limitWorkerMetricsToPublish` orders by status (ALIVE, then `KILLED_DUE_TO_MEMORY_PRESSURE`, then every other status), then by descending `workerStats(0).workerMemoryInKb` with signed 32-bit `Integer.compare`, keeps `limit` rows (`MAX_PUBLISHED_WORKER_METRICS` is 50), then re-sorts those rows by the first worker id ascending. Rust has no publish cap. Java live metrics follow `ConcurrentHashMap` iteration order; Rust sorts live rows by `worker_id`.
  - Java `registerWorker` is synchronized and `computeIfAbsent` on pid, so the first registration freezes worker id, status, mnemonic, multiplex, sandbox, and the caller-supplied `int` worker-key hash. Later calls only set the last-call time from `clock.currentTimeMillis()` and append a worker id that is not already present. A non-null cgroup is stored with `putIfAbsent`. `clear()` drops the metrics map and leaves the cgroup map. Rust emits one row per live process, uses a process-global `u64` id starting at 1, hashes the whole `WorkerKey` with `DefaultHasher` into a `u64`, and keeps no last-call time, cgroup map, or extra multiplex worker ids on the shared process.
  - Java `onWorkerFinishExecution` increments a signed `AtomicInteger` only when that pid is already registered (an unknown pid returns) and wraps at 2^31. That count is what allows `KILLED_UNKNOWN`. Rust adds 1 to a `u64` at the start of `exec`, before a response, including requests that fail, and does not wrap at 32 bits.
  - Java snapshot and last-call times come from an injected `Clock` that stays unset until `setClock`, so a collection before that throws. Rust records no collector timestamps.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerProcessStatus.java`
- Bazel lines: 203
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - No status machine: initial `NOT_STARTED`, `isValid` only for `NOT_STARTED` or `ALIVE`, and updates only when the new status has a strictly higher `int` priority.
  - No pending-kill set. `setKilled` promotes those statuses to `KILLED_*` (`KILLED_UNKNOWN` at 5, specific exceptions at 6, memory pressure at 7) and returns false otherwise.
  - No `isPendingEviction` (only `PENDING_KILL_DUE_TO_MEMORY_PRESSURE`), no optional worker failure `Code` that is stored even when the status transition is rejected, and no `toWorkerStatus` mapping that reports pending kills as `ALIVE`.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerProxy.java`
- Bazel lines: 126
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - `prepareExecution` only starts the shared multiplexer with cwd `workDir` (the exec root) and the worker-key environment. If that process object already exists and is dead, the next call throws `IOException` (`Process is dead`); if the shared status was killed first, it throws `Multiplexer destroyed before created process`. Zel restarts a dead shared `WorkerProcess` on the next `WorkerPool::borrow`, clears the child environment, and sets `PATH` to `/bin:/usr/bin:/usr/local/bin` when the key has no `PATH`. A relative `argv[0]` that contains a directory is rewritten to an absolute path under `workDir` before spawn; Zel passes argv through and places the child in its own process group.
  - `getResponse` can return a null `WorkResponse` when the request has no semaphore or the process is already dead and nothing is buffered, and the wait throws `InterruptedException` if the caller is interrupted. Zel `WorkerProcess::exec` returns `Err` (`worker process terminated unexpectedly`, or a channel error) and blocks until a response or process death. A repeated `request_id` replaces the waiter: the previous Java wait stays blocked on the discarded semaphore; the previous Zel receiver fails when its channel is dropped.
  - `getExitValue` is empty while the process is running and otherwise the process exit code. `diedUnexpectedly` is true only when a process exists, is dead, and was not marked killed. Zel has neither query; a multiplex exec failure is reported as spawn exit code 1 and the shared worker is invalidated.
  - `getRecordingStreamMessage` is the multiplexer stdout recording, capped at 4096 bytes: well-formed UTF-8 with non-printable characters replaced by `?`, otherwise a hex dump. Zel does not record stdout; on failure it appends the stderr log, and `read_to_string` yields an empty log when that file is not UTF-8.
  - `destroy` drops the multiplexer refcount and, at zero, sends SIGTERM and waits for exit, which blocks if the process ignores SIGTERM. Removing a key that has no multiplexer is logged and swallowed. Zel `release` leaves the shared process running after the last in-flight use; `WorkerProcess::destroy` sends SIGKILL to the process group.
  - `isSandboxed` is always false and `getProcessId` is `-1` until the process exists, then the OS pid as a `long`. Zel uses one shared process for multiplex workers, so `key.sandboxed` can be true, and `pid()` is a `u32` only after the child has been spawned.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerSpawnRunner.java`
- Bazel lines: 714
- Zel counterpart: `/workspace/zel-cursor/crates/zel-sandbox/src/worker.rs`
- Verdict: partial
- Differences:
  - `canExec` is false when there are no tool files, and when a non-empty `--experimental_worker_allowlist` does not contain the worker-key mnemonic. `can_exec` only checks `supports-workers` / `supports-multiplex-workers`. `exec` does not throw the no-tools `UserExecException` (`NO_TOOLS`).
  - Java updates runfiles trees, prefetches inputs, and blocks on `ResourceManager` (`DYNAMIC_WORKER` when speculating, otherwise `LOCAL`) before borrowing a worker. Zel borrows from the pool immediately and does not update runfiles.
  - Work request inputs follow expanded spawn-input order. Digests are the lowercase hex of the metadata digest (empty if that digest is null or empty), including virtual inputs, and paths go through `StringEncoding.internalToUnicode` (non-ASCII internal Latin-1 bytes are decoded as UTF-8, with `U+FFFD` for invalid sequences). Zel re-hashes each `spawn.inputs` file from the exec root, skips unreadable files, sorts the paths, omits virtual inputs, and sends the stored Unicode path.
  - A missing input is `SpawnStatus::MissingInputs` with exit code 1 before the worker starts. Java does not do that check in this runner.
  - `@file` expansion matches on recursion, `@@`, and strict UTF-8. Java resolves the file in `SandboxInputs.getFiles()` (symlink and virtual inputs are absent and fail as “not a declared input”) and splits lines on `\n`, `\r\n`, and a lone `\r`, and it throws if the thread is interrupted. Zel requires an exact `spawn.inputs` path string, reads `exec_root.join(value)`, and `str::lines()` leaves a lone `\r` inside the argument. External-repo detection is `^@.*//.*` (`.` does not match line terminators) versus “contains `//`”, so an `@` argument with a line break before `//` is expanded in Java and left literal in Zel.
  - Multiplex request ids start at 1 and wrap as signed 32-bit; non-multiplex stays 0. Java’s counter is process-static, so ids are not reused across runners. Zel’s `AtomicI32` is per runner and starts over at 1.
  - `sandbox_dir` is left empty. Bazel’s sandboxed worker sets that field and keeps the process cwd at the exec root. Zel sandboxes by changing the worker cwd and symlinking inputs, and it deletes declared outputs before the request.
  - A `was_cancelled` response throws (`FINISH_FAILURE`: “Received cancel response for … without having cancelled”). Zel ignores the flag and maps `exit_code == 0` to success. Interrupt of a sandboxed worker does not send a cancel request, release the worker asynchronously, or `lockOutputFiles` with the process exit. `WorkerOptions.cancellation` is unused on this path.
  - Worker exit 0/non-zero maps to success / `NON_ZERO_EXIT` with that exit code, but Java also attaches a spawn failure detail (`worker spawn failed for <mnemonic>`, `NON_ZERO_EXIT`). Protocol and IO failures throw `UserExecException` with worker codes (`NO_RESPONSE`, `PARSE_RESPONSE_FAILURE`, prepare/request/finish/borrow). Zel returns `ExecutionFailed` with exit code 1, or `Err` for `@file` IO. An empty or unparsable response is not distinguished. The log is read as UTF-8 and dropped if it is not valid UTF-8, with no 8192-byte cap. JSON `exitCode` is an `f64` cast to `i32` (saturates); protobuf int32 does not.
  - After a successful worker exit, a missing mandatory output becomes `MissingOutputs` while keeping exit code 0. This runner does not reclassify success that way.
  - Wall time is `(int) duration.toMillis()` (signed 32-bit truncation). Zel stores `u64` milliseconds.

### `src/main/java/com/google/devtools/build/lib/worker/WorkerSpawnStrategy.java`
- Bazel lines: 33
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/strategy.rs`
- Verdict: partial
- Differences:
  - `canExec` is false unless the spawn supports workers or multiplex workers, `getToolFiles()` is non-empty, and, when `--experimental_worker_allowlist` is non-empty, the worker-key mnemonic is in that set. `StrategyKind::Worker::can_exec` is true for any spawn with `supports-workers=1` or `supports-multiplex-workers=1`, so empty tool files and the allowlist still select the worker strategy instead of falling through to sandboxed or local.
  - `exec` (inherited) skips the spawn cache when the branch is speculating and `usefulInDynamicExecution()` is false (`RemoteSpawnCache` returns false). `ExecRuntime::run_strategy` still performs a remote and disk cache lookup for a dynamic local worker branch, so that branch can finish from the remote cache.
  - `exec` reports the subcommand before the runner runs, using the spawn arguments, configuration checksum, and execution platform, including when inputs are missing. Zel announces it only after a non-missing-inputs result and prints the execution platform as `@@platforms//host:host`.
  - A runner `IOException` becomes an action failure whose message is `Exec failed due to IOException` (`EXEC_IO_EXCEPTION`). The same failures from `WorkerSpawnRunner::exec` (flag-file reads, directory creation) propagate as `io::Error` from `execute_action_with`. A non-success result uses `SpawnResult.getFailureMessage()` when it is non-empty; Zel always builds the `(Exit N)` or timeout text and does not prefer a runner-supplied message.
