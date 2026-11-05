# zel-skyframe

Incremental evaluation engine modelled on Bazel's Skyframe.

* `engine` — `InMemoryMemoizingEvaluator`: versioned nodes (`last_changed` / `last_evaluated`), dependency groups and
  reverse deps, dirty lifecycle (`NeedsRebuild` / `CheckDependencies` / clean), **change pruning**, function restarts,
  parallel evaluation on a worker pool (`EvaluationOptions::threads`), `keep_going` vs abort-on-first-error, error bubbling with
  root causes (`get_value_or_error` to handle), transient-error re-evaluation, panic capture, cancellation, cycle detection
  with `CycleInfo` (path-to-cycle + cycle) and Bazel-format reports (`cycles::CyclesReporter`).
* `diff` — `StatDiffAwareness`: per-command multi-threaded stat of every `FILE_STATE` / `DIRECTORY_LISTING_STATE` node
  (`FileSystemValueChecker`), racy-file handling; `ExplicitDiffAwareness` for externally supplied change sets.
* `recording_fs` — thread-local recording `FileSystem`; opaque loaders (BUILD / .bzl / glob) get *exact* file and directory
  dependencies without cooperating.
* `executor::SkyframeExecutor` — server-lifetime owner: `sync()` per command (inject loading options, diff, invalidate),
  `PACKAGE_LOAD` nodes behind `zel_packages::PackageCache` (package loading is memoized across commands),
  `discard_analysis_cache()`, `ActionExecutionFunction` (+ `ActionRunner` trait).

Wired in `zel-server`: `BlazeRuntime::skyframe`, `loading.rs::make_loader`, `clean`, `--discard_analysis_cache`.

## Not done

* No native `--watchfs` backend (inotify/FSEvents need FFI or crates): `--watchfs` uses the stat-diff checker.
* `CONFIGURED_TARGET` nodes exist (analysis agent's function) but the server's analysis loop does not yet evaluate through
  them; `ACTION_EXECUTION` nodes are implemented and tested with a fake `ActionRunner` but `exec_phase` does not run through
  them (action-level incrementality still comes from the persistent action cache).
* Dependencies discovered through recording are registered after the loader ran (a file changed *during* a load is only
  noticed by the next diff).
