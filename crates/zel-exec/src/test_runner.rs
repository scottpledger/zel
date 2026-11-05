//! Standalone test strategy: runs tests through Bazel's real `tools/test/test-setup.sh`
//! (embedded verbatim), with the environment from `TestRunnerAction`, sharding, `--runs_per_test`,
//! flaky attempts, timeouts and Bazel-format summaries (`TestSummaryPrinter`,
//! `TerminalTestResultNotifier`).

use crate::test_status::{self, TestResultData};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zel_sandbox::{execute_spawn_local, Spawn, SpawnStatus};
use zel_vfs::{LocalFileSystem, PathFragment};

/// Bazel's `tools/test/test-setup.sh`, embedded verbatim.
pub const TEST_SETUP_SH: &str = include_str!("../resources/test-setup.sh");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestStatus {
    NoStatus = 0,
    Passed = 1,
    Flaky = 2,
    Timeout = 3,
    Failed = 4,
    Incomplete = 5,
    FailedToBuild = 7,
    BlazeHaltedBeforeTesting = 8,
}

impl TestStatus {
    pub fn name(self) -> &'static str {
        match self {
            Self::NoStatus => "NO STATUS",
            Self::Passed => "PASSED",
            Self::Flaky => "FLAKY",
            Self::Timeout => "TIMEOUT",
            Self::Failed => "FAILED",
            Self::Incomplete => "INCOMPLETE",
            Self::FailedToBuild => "FAILED TO BUILD",
            Self::BlazeHaltedBeforeTesting => "BLAZE HALTED BEFORE TESTING",
        }
    }
    pub fn is_passed(self) -> bool {
        matches!(self, Self::Passed | Self::Flaky)
    }
    fn from_i32(v: u64) -> Self {
        match v {
            1 => Self::Passed,
            2 => Self::Flaky,
            3 => Self::Timeout,
            4 => Self::Failed,
            5 => Self::Incomplete,
            7 => Self::FailedToBuild,
            8 => Self::BlazeHaltedBeforeTesting,
            _ => Self::NoStatus,
        }
    }
}

/// Default timeouts by size, in seconds (`TestTimeout`): small 60, medium 300, large 900, enormous 3600.
pub fn default_timeout_for_size(size: &str) -> u64 {
    match size {
        "small" => 60,
        "large" => 900,
        "enormous" => 3600,
        _ => 300,
    }
}

#[derive(Debug, Clone)]
pub struct TestSpec {
    pub label: String,
    pub exe_path: PathBuf,
    /// `<exe>.runfiles`
    pub runfiles_dir: PathBuf,
    pub exec_root: PathBuf,
    pub workspace_name: String,
    /// `bazel-testlogs/<pkg>/<name>`
    pub test_log_dir: PathBuf,
    pub size: String,
    pub timeout_secs: u64,
    pub shard_count: u32,
    pub runs_per_test: u32,
    pub flaky_attempts: u32,
    pub runs_per_test_detects_flakes: bool,
    pub run_under: Option<String>,
    pub args: Vec<String>,
    /// The rule-specified test environment, already resolved against the client environment
    /// (`TestRunnerAction.getExtraTestEnv`: `RunEnvironmentInfo`, or `env`/`env_inherit` of native
    /// rules); applied after `--action_env` and `--test_env`.
    pub env: BTreeMap<String, String>,
    /// The configuration-level layers beneath it (see [`build_test_env`]).
    pub base_env: TestBaseEnv,
    pub test_filter: Option<String>,
    pub coverage: bool,
    /// Coverage mode: `collect_coverage.sh` (Bazel's, embedded verbatim), the LCOV merger and the
    /// instrumented-files manifest (`COVERAGE_MANIFEST`).
    pub coverage_script: Option<PathBuf>,
    pub lcov_merger: Option<PathBuf>,
    pub coverage_manifest: Option<PathBuf>,
    /// `--zip_undeclared_test_outputs` (default false): exports `TEST_UNDECLARED_OUTPUTS_ZIP`.
    pub zip_undeclared_outputs: bool,
    pub setup_script: PathBuf,
    /// Extra execution requirements (e.g. `timeout` override for tests).
    pub kill_delay_ms: Option<u64>,
    /// `--test_tmpdir`: base temporary directory.
    pub test_tmpdir: Option<PathBuf>,
    /// `--[no]incompatible_check_sharding_support`
    pub check_sharding_support: bool,
}

/// The configuration-level environment layers of a test (`TestPolicy.computeTestEnvironment`).
#[derive(Debug, Clone, Default)]
pub struct TestBaseEnv {
    /// `--action_env` (the configuration's action environment: strict/default `PATH`, ...).
    pub action_env: zel_util::action_env::ActionEnvironment,
    /// `--test_env`.
    pub test_env: zel_util::action_env::ActionEnvironment,
    /// The command's client environment, source of the inherited variables.
    pub client_env: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct AttemptResult {
    pub exit_code: i32,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub log: Vec<u8>,
    pub sharding_unsupported: bool,
    pub run: u32,
    pub shard: u32,
    pub attempt: u32,
    /// Spawn runner that executed the test (`darwin-sandbox`, `local`...); empty when the spawn
    /// itself failed (an action without spawns counts as internal in `SpawnStats`).
    pub runner: String,
    /// The test did not write `test.xml`, so Bazel runs a second spawn to generate it
    /// (`StandaloneTestStrategy.createXmlGeneratingSpawn`).
    pub xml_generated: bool,
}

#[derive(Debug, Clone)]
pub struct TestResult {
    pub status: TestStatus,
    pub attempts: Vec<AttemptResult>,
    pub failed_attempts: usize,
    pub duration_ms: u64,
    pub times_ms: Vec<u64>,
    pub log_path: PathBuf,
    pub xml_path: PathBuf,
    pub cached: bool,
    pub runs: usize,
    pub exit_code: i32,
    pub start_time_ms: u64,
    pub has_coverage: bool,
    /// Parsed `test.xml` (`TestXmlOutputParser`), stored in `test.cache_status`.
    pub test_case: Option<test_status::TestCase>,
    /// Parsed `test.xml` of every run/shard unit in execution order (`None` = no test case
    /// information); Bazel's `TestSummary` collects the cases of all of them.
    pub unit_cases: Vec<Option<test_status::TestCase>>,
}

/// Writes `test-setup.sh` to `dest` (executable) if it does not already have the right contents.
pub fn install_test_setup(dest: &Path) -> std::io::Result<()> {
    if std::fs::read_to_string(dest).map(|c| c == TEST_SETUP_SH).unwrap_or(false) {
        return Ok(());
    }
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(dest, TEST_SETUP_SH)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Bazel's `tools/test/collect_coverage.sh`, embedded verbatim.
pub const COLLECT_COVERAGE_SH: &str = include_str!("../resources/collect_coverage.sh");
/// The LCOV merger / coverage report generator (`CoverageOutputGenerator`) as a shell+awk tool.
pub const LCOV_MERGER_SH: &str = include_str!("../resources/lcov_merger.sh");
/// Bazel's `tools/test/collect_cc_coverage.sh`, embedded verbatim.
pub const COLLECT_CC_COVERAGE_SH: &str = include_str!("../resources/collect_cc_coverage.sh");

fn install_script(dest: &Path, contents: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(dest).map(|c| c == contents).unwrap_or(false) {
        return Ok(());
    }
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(dest, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Installs `collect_coverage.sh` and `lcov_merger` next to `test-setup.sh`.
pub fn install_coverage_tools(tools_test_dir: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    let collect = tools_test_dir.join("collect_coverage.sh");
    let merger = tools_test_dir.join("lcov_merger");
    install_script(&collect, COLLECT_COVERAGE_SH)?;
    install_script(&merger, LCOV_MERGER_SH)?;
    install_script(&tools_test_dir.join("collect_cc_coverage.sh"), COLLECT_CC_COVERAGE_SH)?;
    Ok((collect, merger))
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Computes the 32-hex character hash for a test's unique tmpdir per `TestStrategy.getTmpDirName`.
pub fn test_tmpdir_hash(exe_path: &Path, shard: u32, run: u32) -> String {
    let mut fp = zel_util::Fingerprint::new();
    fp.add_str(&exe_path.to_string_lossy());
    fp.add_i32(shard as i32);
    fp.add_i32(run as i32);
    let hex = fp.hex_digest_and_reset();
    if hex.len() >= 32 {
        hex[..32].to_string()
    } else {
        hex
    }
}

/// Builds the environment `TestRunnerAction` + `StandaloneTestStrategy` give a test, for one
/// shard/run/attempt (variables consumed by `test-setup.sh` and the test).
pub fn build_test_env(
    spec: &TestSpec,
    attempt_dir: &Path,
    shard: u32,
    run: u32,
    _attempt: u32,
) -> BTreeMap<String, String> {
    let tmp = if let Some(base_tmp) = &spec.test_tmpdir {
        let hash = test_tmpdir_hash(&spec.exe_path, shard, run);
        base_tmp.join("_tmp").join(hash)
    } else {
        let hash = test_tmpdir_hash(&spec.exe_path, shard, run);
        spec.exec_root.join("_tmp").join(hash)
    };
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::create_dir_all(&tmp);
    let undeclared = attempt_dir.join("test.outputs");
    // `TestPolicy.computeTestEnvironment`, in this order: the default policy (TZ, TEST_SRCDIR,
    // TEST_TMPDIR, HOME, ...), then the configuration's action environment (`--action_env`, which
    // carries PATH), then `--test_env`, then the rule's environment; every layer can override or
    // (action/test layers) unset what the earlier ones set.
    let mut env = zel_util::action_env::test_policy_environment(&s(&spec.runfiles_dir), &s(&tmp));
    spec.base_env.action_env.resolve(&mut env, &spec.base_env.client_env);
    spec.base_env.test_env.resolve(&mut env, &spec.base_env.client_env);
    for (k, v) in &spec.env {
        env.insert(k.clone(), v.clone());
    }
    // `TestRunnerAction.setupEnvVariables`.
    let set = |env: &mut BTreeMap<String, String>, k: &str, v: String| {
        env.insert(k.to_string(), v);
    };
    set(&mut env, "TEST_WORKSPACE", spec.workspace_name.clone());
    set(&mut env, "TEST_TARGET", spec.label.clone());
    let binary = format!("./{}", spec.exe_path.file_name().unwrap_or_default().to_string_lossy());
    set(&mut env, "TEST_BINARY", binary);
    set(&mut env, "TEST_SIZE", spec.size.clone());
    set(&mut env, "TEST_TIMEOUT", spec.timeout_secs.to_string());
    // Different `TEST_RANDOM_SEED` / `TEST_RUN_NUMBER` per run when a test runs several times;
    // `--test_env` and rules may set them first.
    if spec.runs_per_test > 1 {
        env.entry("TEST_RUN_NUMBER".into()).or_insert_with(|| (run + 1).to_string());
        env.entry("TEST_RANDOM_SEED".into()).or_insert_with(|| (run + 1).to_string());
    }
    set(&mut env, "TEST_PREMATURE_EXIT_FILE", s(&attempt_dir.join("test.exited_prematurely")));
    set(&mut env, "TEST_WARNINGS_OUTPUT_FILE", s(&attempt_dir.join("test.warnings")));
    set(&mut env, "TEST_LOGSPLITTER_OUTPUT_FILE", s(&attempt_dir.join("test.raw_splitlogs/test.splitlogs")));
    set(&mut env, "TEST_INFRASTRUCTURE_FAILURE_FILE", s(&attempt_dir.join("test.infrastructure_failure")));
    set(&mut env, "TEST_UNUSED_RUNFILES_LOG_FILE", s(&attempt_dir.join("test.unused_runfiles_log")));
    set(&mut env, "TEST_UNDECLARED_OUTPUTS_DIR", s(&undeclared));
    set(&mut env, "TEST_UNDECLARED_OUTPUTS_MANIFEST", s(&attempt_dir.join("test.outputs_manifest/MANIFEST")));
    if spec.zip_undeclared_outputs {
        set(&mut env, "TEST_UNDECLARED_OUTPUTS_ZIP", s(&attempt_dir.join("test.outputs/outputs.zip")));
    }
    set(&mut env, "TEST_UNDECLARED_OUTPUTS_ANNOTATIONS", s(&attempt_dir.join("test.outputs_manifest/ANNOTATIONS")));
    set(&mut env, "TEST_UNDECLARED_OUTPUTS_ANNOTATIONS_DIR", s(&attempt_dir.join("test.outputs_manifest")));
    set(&mut env, "XML_OUTPUT_FILE", s(&attempt_dir.join("test.xml")));
    if spec.shard_count > 0 {
        set(&mut env, "TEST_TOTAL_SHARDS", spec.shard_count.to_string());
        set(&mut env, "TEST_SHARD_INDEX", shard.to_string());
        set(&mut env, "TEST_SHARD_STATUS_FILE", s(&attempt_dir.join("test.shard")));
    }
    if let Some(f) = &spec.test_filter {
        set(&mut env, "TESTBRIDGE_TEST_ONLY", f.clone());
    }
    if spec.coverage {
        // `TestRunnerAction.setupEnvironment` (coverage mode).
        set(&mut env, "RUNTEST_PRESERVE_CWD", "1".into());
        // collect_coverage.sh expects exec-root-relative paths (it prefixes `$ROOT/`).
        let rel = |p: &Path| p.strip_prefix(&spec.exec_root).map(s).unwrap_or_else(|_| s(p));
        set(&mut env, "COVERAGE_DIR", rel(&attempt_dir.join("_coverage")));
        if let Some(m) = &spec.coverage_manifest {
            set(&mut env, "COVERAGE_MANIFEST", rel(m));
        }
        set(&mut env, "COVERAGE_OUTPUT_FILE", rel(&attempt_dir.join("coverage.dat")));
        set(&mut env, "SPLIT_COVERAGE_POST_PROCESSING", "0".into());
        set(&mut env, "IS_COVERAGE_SPAWN", "0".into());
        // The merger comes from the test rule's `_lcov_merger` attribute in Bazel.
        if let Some(l) = &spec.lcov_merger {
            set(&mut env, "LCOV_MERGER", s(l));
        }
    }
    env
}

/// Renders an exit code or terminating signal as Bazel does (`TerminationStatus.toShortString`).
pub fn format_exit_status(exit_code: i32) -> String {
    if exit_code > 128 && exit_code <= 128 + 64 {
        let sig = exit_code - 128;
        match sig {
            1 => "Hangup".to_string(),
            2 => "Interrupt".to_string(),
            3 => "Quit".to_string(),
            4 => "Illegal instruction".to_string(),
            5 => "Trace/breakpoint trap".to_string(),
            6 => "Aborted".to_string(),
            7 => "Bus error".to_string(),
            8 => "Floating point exception".to_string(),
            9 => "Killed".to_string(),
            10 => "User defined signal 1".to_string(),
            11 => "Segmentation fault".to_string(),
            12 => "User defined signal 2".to_string(),
            13 => "Broken pipe".to_string(),
            14 => "Alarm clock".to_string(),
            15 => "Terminated".to_string(),
            _ => format!("Signal {sig}"),
        }
    } else {
        format!("Exit {exit_code}")
    }
}

/// Minimal JUnit XML for a test that did not write `XML_OUTPUT_FILE` itself
/// (like `test-setup.sh`'s generated report).
pub fn synthesize_xml(label: &str, exit_code: i32, timed_out: bool, duration_ms: u64, log: &str) -> String {
    let esc = |t: &str| t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let failed = exit_code != 0 || timed_out;
    let msg = if timed_out {
        "timed out".to_string()
    } else {
        format!("exited with error code {exit_code}")
    };
    let target_name = label.rsplit_once(':').map(|x| x.1).unwrap_or(label);
    let _ = duration_ms;
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.push_str(&format!(
        "<testsuites>\n  <testsuite name=\"{0}\" tests=\"1\" failures=\"0\" errors=\"{1}\">\n    <testcase name=\"{0}\" classname=\"\" status=\"run\" duration=\"0\" time=\"0\">",
        esc(target_name), if failed { 1 } else { 0 }
    ));
    if failed {
        x.push_str(&format!("<error message=\"{}\"></error>", esc(&msg)));
    }
    let trimmed_log = log.strip_suffix('\n').unwrap_or(log);
    x.push_str(&format!(
        "</testcase>\n      <system-out>\nGenerated test.log (if the file is not UTF-8, then this may be unreadable):\n<![CDATA[{}]]>\n      </system-out>\n    </testsuite>\n</testsuites>\n",
        trimmed_log.replace("]]>", "]]>]]&gt;<![CDATA[")
    ));
    x
}

/// How a test is executed through the spawn strategy registry (`StandaloneTestStrategy` creates a
/// `TestRunner` spawn and hands it to the `SpawnStrategyResolver`).
pub struct TestExec<'a> {
    pub runtime: &'a std::sync::Arc<crate::ExecRuntime>,
    pub reporter: &'a zel_events::Reporter,
    /// Exec-root-relative inputs of the test spawn (the test binary and its runfiles): they
    /// make up the action's remote-cache key and are checked for existence before running.
    pub inputs: Vec<String>,
    /// Execution requirements derived from the test's tags (`local`, `no-sandbox`, `no-cache`...).
    pub execution_info: BTreeMap<String, String>,
    /// Resources a test of this size needs (`TestRunnerAction.estimateResourceConsumption`).
    pub resources: crate::resources::ResourceSet,
    pub announce_subcommands: bool,
    /// `BuildOptions.checksum()` of the test target's configuration.
    pub config_checksum: String,
}

fn run_attempt(
    spec: &TestSpec,
    dir: &Path,
    shard: u32,
    run: u32,
    attempt: u32,
    exec: Option<&TestExec>,
) -> AttemptResult {
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::create_dir_all(dir);
    let env = build_test_env(spec, dir, shard, run, attempt);
    let cwd = if spec.coverage {
        spec.exec_root.clone()
    } else {
        let cwd = spec.runfiles_dir.join(&spec.workspace_name);
        if cwd.is_dir() { cwd } else { spec.exec_root.clone() }
    };
    // test-setup.sh `cd`s into $TEST_SRCDIR/$TEST_WORKSPACE itself relative to PWD, so run from
    // the exec root and give it relative script/test paths exactly like the spawn does.
    let mut argv = vec![s(&spec.setup_script)];
    if spec.coverage {
        if let Some(c) = &spec.coverage_script {
            argv.push(s(c));
        }
    }
    if let Some(ru) = &spec.run_under {
        // `RunUnder`: the command is tokenized like a shell would and precedes the test binary.
        // `TestTargetExecutionSettings.needsShell`: a first token without '/' (a builtin or a
        // PATH lookup) is wrapped in a shell invocation.
        let words = shell_tokenize_words(ru.trim());
        if words.first().map_or(false, |w| !w.contains('/')) {
            argv.push("/bin/bash".into());
            argv.push("-c".into());
            argv.push("\"$@\"".into());
            argv.push("/bin/bash".into());
        }
        argv.extend(words);
    }
    let exe_name = spec.exe_path.file_name().unwrap_or_default();
    let has_in_runfiles = spec.runfiles_dir.join(&spec.workspace_name).join(exe_name).exists();
    if has_in_runfiles {
        argv.push(format!("./{}", exe_name.to_string_lossy()));
    } else {
        argv.push(s(&spec.exe_path));
    }
    argv.extend(spec.args.iter().cloned());
    let mut info = BTreeMap::new();
    if let Some(e) = exec {
        info.extend(e.execution_info.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    info.insert("timeout".to_string(), spec.timeout_secs.to_string());
    if let Some(k) = spec.kill_delay_ms {
        info.insert("zel-kill-delay-ms".to_string(), k.to_string());
    }
    let rel_exec = |p: &Path| -> PathFragment {
        if let Ok(r) = p.strip_prefix(&spec.exec_root) {
            PathFragment::create(r.to_str().unwrap())
        } else {
            PathFragment::create(p.to_str().unwrap())
        }
    };
    let mut inputs: Vec<PathFragment> = exec
        .map(|e| e.inputs.iter().map(|i| PathFragment::create(i)).collect())
        .unwrap_or_default();
    if let Some(m) = &spec.coverage_manifest {
        inputs.push(rel_exec(m));
    }
    let mut outputs = vec![
        rel_exec(&dir.join("test.log")),
        rel_exec(&dir.join("test.xml")),
        rel_exec(&dir.join("test.shard")),
        rel_exec(&dir.join("test.infrastructure_failure")),
        rel_exec(&dir.join("test.outputs")),
    ];
    if spec.coverage {
        outputs.push(rel_exec(&dir.join("coverage.dat")));
        outputs.push(rel_exec(&dir.join("_coverage")));
    }
    let sp = Spawn {
        mnemonic: "TestRunner".into(),
        owner_label: spec.label.clone(),
        argv,
        env,
        inputs,
        outputs,
        execution_info: info,
        kill_delay: exec.and_then(|e| e.runtime.kill_delay),
        ..Default::default()
    };
    let execroot = PathFragment::create(&s(&spec.exec_root));
    let t = std::time::Instant::now();
    let fs = zel_vfs::LocalFileSystem::new(zel_vfs::DigestHashFunction::Sha256);
    if let Some(e) = exec {
        if e.announce_subcommands {
            let runner = "darwin-sandbox";
            let cfg_hash = e.config_checksum.as_str();
            e.reporter.handle(zel_events::Event::subcommand(format!(
                "# sh_test rule target {} [action 'Testing {}', configuration: {cfg_hash}, execution platform: @@platforms//host:host, mnemonic: TestRunner]\n{}\n# Configuration: {cfg_hash}\n# Execution platform: @@platforms//host:host\n# Runner: {runner}",
                spec.label,
                spec.label,
                crate::describe_command(&sp.argv, &sp.env, &execroot, true)
            )));
        }
    }
    let res = match exec {
        // Through the strategy registry (remote / dynamic / sandboxed / local ...), holding the
        // test's resources for the duration of the spawn.
        Some(e) => {
            let _held = e.runtime.resources.acquire(&e.resources);
            let desc = format!("Testing {}", spec.label);
            match e.runtime.execute(&sp, &desc, &execroot, &fs, e.reporter) {
                Ok(r) => r,
                Err(msg) => Err(std::io::Error::other(format!("{} failed: {msg}", desc))),
            }
        }
        None => execute_spawn_local(&sp, &execroot, &fs),
    };
    let dur = t.elapsed().as_millis() as u64;
    match res {
        Ok(r) => {
            let mut log = r.stdout.clone();
            log.extend_from_slice(&r.stderr);
            let timed_out = r.status == SpawnStatus::Timeout;
            if timed_out || log.windows(15).any(|w| w == b"Action timed ou") {
                let text = String::from_utf8_lossy(&log);
                let cleaned: String = text
                    .lines()
                    .filter(|l| !l.starts_with("Action timed out after "))
                    .map(|l| format!("{l}\n"))
                    .collect();
                log = cleaned.into_bytes();
            }
            let mut exit_code = if timed_out { 142 } else { r.exit_code };
            let mut sharding_unsupported = false;
            if exit_code == 0 && !timed_out && spec.shard_count > 1 && spec.check_sharding_support {
                if !dir.join("test.shard").exists() {
                    exit_code = 1;
                    sharding_unsupported = true;
                    log.extend_from_slice(b"Sharding requested, but the test runner did not advertise support for it by touching TEST_SHARD_STATUS_FILE. Either remove the 'shard_count' attribute, use a test runner that supports sharding or temporarily disable this check via --noincompatible_check_sharding_support.\n");
                }
            }
            let xml = dir.join("test.xml");
            let xml_generated = !xml.exists();
            // The test spawn (and the XML generator spawn) are logged like any other spawn.
            if let Some(e) = exec {
                let cwd_rel = |p: &Path| rel_exec(p).path_string().to_string();
                // The test action's own log paths (not the per-attempt scratch directory) and the
                // setup script under its repository exec path.
                let log_dir = if dir.to_string_lossy().contains("test_attempts") {
                    // `<unit>/test_attempts/attempt_N.tmp` -> `<unit>`
                    let mut ld = dir.to_path_buf();
                    while ld.file_name().map_or(false, |n| n != "test_attempts") && ld.pop() {}
                    ld.pop();
                    ld
                } else if dir.ends_with("attempt") {
                    dir.parent().unwrap_or(dir).to_path_buf()
                } else {
                    dir.to_path_buf()
                };
                // `TestRunnerAction`'s declared outputs (per-action testlogs directory).
                let outputs_logged: Vec<String> = [
                    "test.exited_prematurely",
                    "test.infrastructure_failure",
                    "test.outputs",
                    "test.outputs_manifest/ANNOTATIONS",
                    "test.outputs_manifest/ANNOTATIONS.pb",
                    "test.outputs_manifest/MANIFEST",
                    "test.raw_splitlogs/test.splitlogs",
                    "test.unused_runfiles_log",
                    "test.warnings",
                    "test.xml",
                ]
                .iter()
                .map(|n| cwd_rel(&log_dir.join(n)))
                .collect();
                let mut logged_argv = sp.argv.clone();
                if let Some(a0) = logged_argv.first_mut() {
                    if a0.ends_with("tools/test/test-setup.sh") {
                        *a0 = "external/bazel_tools/tools/test/test-setup.sh".to_string();
                    }
                }
                let info = zel_events::ActionInfo {
                    id: cwd_rel(&dir.join("test.log")),
                    owner: spec.label.clone(),
                    mnemonic: "TestRunner".into(),
                    progress_message: format!("Testing {}", spec.label),
                    argv: logged_argv,
                    env: sp.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                    inputs: logged_test_inputs(spec, &dir),
                    outputs: outputs_logged,
                    is_spawn: true,
                };
                let res_info = zel_events::ActionResultInfo {
                    cache_hit: false,
                    success: true,
                    exit_code: r.exit_code,
                    runner: Some(r.runner_name.clone()),
                    wall_time_ms: dur,
                    start_millis: 0,
                    ..Default::default()
                };
                let gen_res_info = zel_events::ActionResultInfo { exit_code: 0, ..res_info.clone() };
                e.reporter.action_started(&info);
                e.reporter.action_finished(&info, &res_info);
                if xml_generated {
                    let gen = zel_events::ActionInfo {
                        id: cwd_rel(&dir.join("test.xml")),
                        owner: spec.label.clone(),
                        mnemonic: "TestRunner".into(),
                        progress_message: format!("Testing {}", spec.label),
                        argv: vec![
                            "external/bazel_tools/tools/test/generate-xml.sh".into(),
                            cwd_rel(&log_dir.join("test.log")),
                            cwd_rel(&log_dir.join("test.xml")),
                            (dur / 1000).to_string(),
                            r.exit_code.to_string(),
                        ],
                        env: {
                            let mut e: BTreeMap<String, String> = sp.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                            e.insert("TEST_NAME".into(), spec.label.clone());
                            e.insert("TEST_SHARD_INDEX".into(), shard.to_string());
                            e.insert("TEST_TOTAL_SHARDS".into(), spec.shard_count.to_string());
                            e.into_iter().collect()
                        },
                        inputs: vec![
                            (cwd_rel(&log_dir.join("test.log")), false),
                            ("external/bazel_tools/tools/test/generate-xml.sh".to_string(), false),
                        ],
                        outputs: vec![cwd_rel(&log_dir.join("test.xml"))],
                        is_spawn: true,
                    };
                    e.reporter.action_started(&gen);
                    e.reporter.action_finished(&gen, &gen_res_info);
                }
            }
            if xml_generated {
                let _ = std::fs::write(
                    &xml,
                    synthesize_xml(&spec.label, exit_code, timed_out, dur, &String::from_utf8_lossy(&log)),
                );
            }
            AttemptResult { exit_code, timed_out, duration_ms: dur, log, sharding_unsupported, run, shard, attempt, runner: r.runner_name.clone(), xml_generated }
        }
        Err(e) => AttemptResult {
            exit_code: 1,
            timed_out: false,
            duration_ms: dur,
            log: format!("Failed to run test: {e}\n").into_bytes(),
            sharding_unsupported: false,
            run,
            shard,
            attempt,
            runner: String::new(),
            xml_generated: false,
        },
    }
}

/// Runs a test target: for each of `runs_per_test` runs and each shard, up to `flaky_attempts`
/// attempts (stopping at the first pass). Writes `test.log`, `test.xml`, `test.cache_status` (and
/// `test_attempts/attempt_N.log` for failed attempts of flaky runs) under `test_log_dir`.
pub fn run_test(spec: &TestSpec) -> std::io::Result<TestResult> {
    run_test_in(spec, None)
}

/// [`run_test`] through the spawn strategy registry of `exec` (when given).
pub fn run_test_in(spec: &TestSpec, exec: Option<&TestExec>) -> std::io::Result<TestResult> {
    std::fs::create_dir_all(&spec.test_log_dir)?;
    let _ = std::fs::create_dir_all(spec.test_log_dir.join("test.outputs_manifest"));
    let _ = std::fs::create_dir_all(spec.test_log_dir.join("test.raw_splitlogs"));
    let runs = spec.runs_per_test.max(1);
    let shards = spec.shard_count.max(1);
    let mut all_attempts: Vec<AttemptResult> = Vec::new();
    let mut failed_attempts = 0usize;
    let mut times = Vec::new();
    let mut overall = TestStatus::Passed;
    let mut full_log: Vec<u8> = Vec::new();
    let mut any_flaky = false;
    let mut xml_src: Option<PathBuf> = None;
    let mut last_exit = 0;
    let start_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut coverage_files: Vec<PathBuf> = Vec::new();
    for run in 0..runs {
        for shard in 0..shards {
            let base = if runs > 1 {
                spec.test_log_dir.join(format!("run_{}_of_{}", run + 1, runs))
            } else {
                spec.test_log_dir.clone()
            };
            let base = if spec.shard_count > 0 {
                base.join(format!("shard_{}_of_{}", shard + 1, spec.shard_count))
            } else {
                base
            };
            std::fs::create_dir_all(&base)?;
            let _ = std::fs::create_dir_all(base.join("test.outputs_manifest"));
            let _ = std::fs::create_dir_all(base.join("test.raw_splitlogs"));
            let max_attempts = spec.flaky_attempts.max(1);
            let mut unit_status = TestStatus::Failed;
            let mut unit_failed = 0;
            for attempt in 1..=max_attempts {
                let adir = if max_attempts > 1 {
                    base.join(format!("test_attempts/attempt_{attempt}.tmp"))
                } else {
                    base.join("attempt")
                };
                let r = run_attempt(spec, &adir, shard, run, attempt, exec);
                last_exit = r.exit_code;
                times.push(r.duration_ms);
                let pass = r.exit_code == 0 && !r.timed_out && !r.sharding_unsupported;
                if r.sharding_unsupported {
                    unit_status = TestStatus::NoStatus;
                } else if pass {
                    unit_status = TestStatus::Passed;
                } else if r.timed_out {
                    unit_status = TestStatus::Timeout;
                } else {
                    unit_status = TestStatus::Failed;
                }
                if max_attempts > 1 && (!pass && attempt < max_attempts) {
                    let att_dir = base.join("test_attempts");
                    let _ = std::fs::create_dir_all(&att_dir);
                    let _ = std::fs::write(
                        att_dir.join(format!("attempt_{attempt}.log")),
                        &r.log,
                    );
                    let x = adir.join("test.xml");
                    if x.exists() {
                        let _ = std::fs::copy(&x, att_dir.join(format!("attempt_{attempt}.xml")));
                    }
                }
                full_log.extend_from_slice(&r.log);
                if !pass {
                    unit_failed += 1;
                    failed_attempts += 1;
                }
                // Keep the last attempt's XML.
                let x = adir.join("test.xml");
                if x.exists() {
                    let dst = base.join("test.xml");
                    let _ = std::fs::copy(&x, &dst);
                    xml_src = Some(dst);
                }
                // Copy per-attempt undeclared outputs next to the log.
                let uo = adir.join("test.outputs");
                if uo.is_dir() {
                    let _ = std::fs::remove_dir_all(base.join("test.outputs"));
                    let _ = copy_dir(&uo, &base.join("test.outputs"));
                }
                if max_attempts == 1 || attempt == max_attempts || pass {
                    let _ = std::fs::write(base.join("test.log"), &r.log);
                }
                if spec.coverage && pass {
                    let cov = adir.join("coverage.dat");
                    if cov.exists() {
                        let dst = base.join("coverage.dat");
                        if std::fs::copy(&cov, &dst).is_ok() {
                            coverage_files.push(dst);
                        }
                    }
                }
                let _ = std::fs::remove_dir_all(&adir);
                all_attempts.push(r);
                if pass {
                    break;
                }
            }
            if unit_status == TestStatus::Passed && unit_failed > 0 {
                any_flaky = true;
            }
            if unit_status == TestStatus::NoStatus {
                overall = TestStatus::NoStatus;
                break;
            } else if !unit_status.is_passed() {
                // First failing status wins; timeout stays timeout.
                if overall == TestStatus::Passed || overall == TestStatus::Flaky {
                    overall = unit_status;
                }
            }
        }
        if overall == TestStatus::NoStatus {
            break;
        }
    }
    if overall == TestStatus::Passed && any_flaky {
        overall = TestStatus::Flaky;
    }
    if spec.runs_per_test_detects_flakes && runs > 1 {
        let any_pass = all_attempts.iter().any(|a| a.exit_code == 0 && !a.timed_out && !a.sharding_unsupported);
        let any_fail = all_attempts.iter().any(|a| a.exit_code != 0 || a.timed_out || a.sharding_unsupported);
        if any_pass && any_fail {
            overall = TestStatus::Flaky;
        }
    }
    let log_path = spec.test_log_dir.join("test.log");
    let xml_path = spec.test_log_dir.join("test.xml");
    if runs > 1 || spec.shard_count > 0 {
        // Aggregate: top-level test.log concatenates all units.
        std::fs::write(&log_path, &full_log)?;
        if let Some(x) = xml_src {
            let _ = std::fs::copy(&x, &xml_path);
        }
    }
    let duration_ms = times.iter().copied().max().unwrap_or(0);
    let result = TestResult {
        status: overall,
        failed_attempts,
        attempts: all_attempts,
        duration_ms,
        times_ms: times,
        log_path,
        xml_path: xml_path.clone(),
        cached: false,
        runs: runs as usize,
        exit_code: last_exit,
        start_time_ms: start_ms,
        has_coverage: spec.coverage,
        test_case: parse_xml_file(&xml_path),
        unit_cases: (0..runs)
            .flat_map(|run| (0..shards).map(move |shard| (run, shard)))
            .map(|(run, shard)| {
                let base = if runs > 1 {
                    spec.test_log_dir.join(format!("run_{}_of_{}", run + 1, runs))
                } else {
                    spec.test_log_dir.clone()
                };
                let base = if spec.shard_count > 0 {
                    base.join(format!("shard_{}_of_{}", shard + 1, spec.shard_count))
                } else {
                    base
                };
                parse_xml_file(&base.join("test.xml"))
            })
            .collect(),
    };
    if spec.coverage {
        merge_coverage(spec, &coverage_files)?;
    }
    write_cache_status(&spec.test_log_dir, &result)?;
    Ok(result)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let t = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &t)?;
        } else {
            std::fs::copy(e.path(), t)?;
        }
    }
    Ok(())
}

/// Parses `test.xml` into the `TestCase` tree stored in `test.cache_status`.
fn parse_xml_file(p: &Path) -> Option<test_status::TestCase> {
    let text = std::fs::read_to_string(p).ok()?;
    test_status::parse_test_xml(&text).ok().flatten()
}

/// Merges per-shard/run `coverage.dat` files into `<test_log_dir>/coverage.dat` with the LCOV
/// merger (a single file is copied as is).
fn merge_coverage(spec: &TestSpec, files: &[PathBuf]) -> std::io::Result<()> {
    let out = spec.test_log_dir.join("coverage.dat");
    match files.len() {
        0 => {
            // "Always create the coverage report": an empty file when nothing was collected.
            if !out.exists() {
                std::fs::write(&out, b"")?;
            }
        }
        1 => {
            if files[0] != out {
                std::fs::copy(&files[0], &out)?;
            }
        }
        _ => {
            let Some(merger) = &spec.lcov_merger else { return Ok(()) };
            let list = spec.test_log_dir.join("coverage_reports.tmp");
            let text: Vec<String> = files.iter().map(|f| s(f)).collect();
            std::fs::write(&list, text.join("\n"))?;
            let status = std::process::Command::new(merger)
                .arg(format!("--reports_file={}", s(&list)))
                .arg(format!("--output_file={}", s(&out)))
                .status()?;
            let _ = std::fs::remove_file(&list);
            if !status.success() {
                return Err(std::io::Error::new(std::io::ErrorKind::Other, "lcov merger failed"));
            }
        }
    }
    Ok(())
}

/// Writes `test.cache_status`, the serialized `TestResultData` (`test_status.proto`).
pub fn write_cache_status(dir: &Path, r: &TestResult) -> std::io::Result<()> {
    let passed = r.status.is_passed();
    let mut failed_logs: Vec<String> = Vec::new();
    if !passed && r.status != TestStatus::Incomplete {
        failed_logs.push(s(&r.log_path));
    }
    if let Ok(rd) = std::fs::read_dir(dir.join("test_attempts")) {
        let mut v: Vec<String> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |x| x == "log"))
            .map(|p| s(&p))
            .collect();
        v.sort();
        failed_logs.extend(v);
    }
    let d = TestResultData {
        cachable: Some(r.status != TestStatus::Incomplete),
        test_passed: Some(passed),
        status: Some(r.status as u64),
        failed_logs,
        has_coverage: Some(r.has_coverage),
        is_remote_strategy: Some(false),
        test_times: r.times_ms.iter().map(|t| *t as i64).collect(),
        passed_log: passed.then(|| s(&r.log_path)),
        test_process_times: r.times_ms.iter().map(|t| *t as i64).collect(),
        run_duration_millis: Some(r.duration_ms as i64),
        start_time_millis_epoch: Some(r.start_time_ms as i64),
        exit_code: Some(r.exit_code),
        ..Default::default()
    };
    std::fs::write(dir.join("test.cache_status"), d.encode())
}

/// Reads a previously written `test.cache_status` (for `--cache_test_results`).
pub fn read_cache_status(dir: &Path) -> Option<TestResult> {
    let b = std::fs::read(dir.join("test.cache_status")).ok()?;
    let d = TestResultData::decode(&b).ok()?;
    let status = TestStatus::from_i32(d.status?);
    if !dir.join("test.log").exists() {
        return None;
    }
    let dur = d.run_duration_millis.unwrap_or(0).max(0) as u64;
    let xml_path = dir.join("test.xml");
    let test_case = d.test_case.or_else(|| parse_xml_file(&xml_path));
    Some(TestResult {
        status,
        attempts: Vec::new(),
        failed_attempts: d.test_times.len().saturating_sub(1),
        duration_ms: dur,
        times_ms: if d.test_times.is_empty() { vec![dur] } else { d.test_times.iter().map(|t| *t as u64).collect() },
        log_path: dir.join("test.log"),
        xml_path,
        cached: true,
        runs: 1,
        exit_code: d.exit_code.unwrap_or(0),
        start_time_ms: d.start_time_millis_epoch.unwrap_or(0).max(0) as u64,
        has_coverage: d.has_coverage.unwrap_or(false),
        unit_cases: vec![test_case.clone()],
        test_case,
    })
}

fn time_in_sec(ms: u64) -> String {
    format!("{:.1}s", ms as f64 / 1000.0)
}

/// Formats one summary line exactly like `TestSummaryPrinter.print` (verbose summary on).
pub fn format_summary_line(label: &str, r: &TestResult) -> String {
    let cache_msg = if r.cached && !matches!(r.status, TestStatus::NoStatus | TestStatus::Incomplete | TestStatus::FailedToBuild) {
        "(cached) "
    } else {
        ""
    };
    let message = format!("{}{}", cache_msg, r.status.name());
    let pad = 78usize.saturating_sub(message.len());
    let mut line = format!("{label:<pad$} {message}");
    let attempts = r.attempts.len();
    if attempts > 1 {
        match r.status {
            TestStatus::Flaky => line.push_str(&format!(", failed in {} out of {}", r.failed_attempts, attempts)),
            TestStatus::Timeout | TestStatus::Failed => {
                line.push_str(&format!(" in {} out of {}", r.failed_attempts, attempts))
            }
            _ => {}
        }
    }
    if !matches!(r.status, TestStatus::NoStatus | TestStatus::FailedToBuild) && !r.times_ms.is_empty() {
        if r.times_ms.len() == 1 {
            line.push_str(&format!(" in {}", time_in_sec(r.times_ms[0])));
        } else {
            let n = r.times_ms.len() as f64;
            let min = *r.times_ms.iter().min().unwrap();
            let max = *r.times_ms.iter().max().unwrap();
            let sum: u64 = r.times_ms.iter().sum();
            let sumsq: f64 = r.times_ms.iter().map(|&v| (v as f64) * (v as f64)).sum();
            let mean = sum as f64 / n;
            let dev = ((sumsq - sum as f64 * mean) / n).max(0.0).sqrt();
            line.push_str(&format!(
                " in {0}\n  Stats over {1} runs: max = {0}, min = {2}, avg = {3}, dev = {4}",
                time_in_sec(max),
                r.times_ms.len(),
                time_in_sec(min),
                time_in_sec(mean as u64),
                time_in_sec(dev as u64)
            ));
        }
    }
    line
}

/// `StringUtil.joinEnglishList(items, "and")`.
pub fn join_english_list(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        2 => format!("{} and {}", items[0], items[1]),
        n => format!("{}, and {}", items[..n - 1].join(", "), items[n - 1]),
    }
}

/// The `Executed N out of M tests: ...` trailer (`TerminalTestResultNotifier.printStats`).
pub fn format_stats(
    executed: usize,
    results: &[(String, TestStatus)],
) -> String {
    let pass = results.iter().filter(|(_, s)| s.is_passed()).count();
    let nostatus = results
        .iter()
        .filter(|(_, s)| matches!(s, TestStatus::NoStatus | TestStatus::BlazeHaltedBeforeTesting))
        .count();
    let f2b = results.iter().filter(|(_, s)| *s == TestStatus::FailedToBuild).count();
    let failed_local = results.len().saturating_sub(pass).saturating_sub(nostatus).saturating_sub(f2b);
    let mut parts: Vec<String> = Vec::new();
    if pass == 1 {
        parts.push("1 test passes".into());
    } else if pass > 0 {
        parts.push(format!("{pass} tests pass"));
    }
    let mut add = |c: usize, sing: &str, plur: &str, msg: &str| {
        if c > 0 {
            parts.push(format!("{} {} {}", c, if c == 1 { sing } else { plur }, msg));
        }
    };
    add(f2b, "fails", "fail", "to build");
    add(failed_local, "fails", "fail", "locally");
    add(nostatus, "was", "were", "skipped");
    if results.is_empty() {
        return String::new();
    }
    format!(
        "\nExecuted {} out of {} {}: {}.\n",
        executed,
        results.len(),
        if results.len() == 1 { "test" } else { "tests" },
        join_english_list(&parts)
    )
}

/// The `Finished with N passing and M failing tests up to date, K out of date.` trailer
/// (`TerminalTestResultNotifier.printStats` with `--check_tests_up_to_date`).
pub fn format_check_up_to_date_stats(results: &[(String, TestStatus)]) -> String {
    let pass = results.iter().filter(|(_, s)| s.is_passed()).count();
    let nostatus = results
        .iter()
        .filter(|(_, s)| matches!(s, TestStatus::NoStatus | TestStatus::BlazeHaltedBeforeTesting))
        .count();
    let failed = results.len().saturating_sub(pass).saturating_sub(nostatus);
    format!(
        "\nFinished with {} passing and {} failing tests up to date, {} out of date.\n",
        pass, failed, nostatus
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(name: &str, script: &str) -> TestSpec {
        let root = std::env::temp_dir().join(format!("zel-tr-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let rf = root.join("bin/t.runfiles/_main");
        std::fs::create_dir_all(&rf).unwrap();
        let exe = root.join("bin/t");
        std::fs::write(&exe, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let setup = root.join("test-setup.sh");
        install_test_setup(&setup).unwrap();
        TestSpec {
            label: "//pkg:t".into(),
            exe_path: exe,
            runfiles_dir: root.join("bin/t.runfiles"),
            exec_root: root.clone(),
            workspace_name: "_main".into(),
            test_log_dir: root.join("testlogs/pkg/t"),
            size: "medium".into(),
            timeout_secs: 60,
            shard_count: 0,
            runs_per_test: 1,
            flaky_attempts: 1,
            runs_per_test_detects_flakes: false,
            run_under: None,
            args: vec![],
            env: BTreeMap::new(),
            base_env: TestBaseEnv {
                action_env: zel_util::action_env::EnvConfig::default().action_environment(false),
                ..Default::default()
            },
            test_filter: None,
            coverage: false,
            coverage_script: None,
            lcov_merger: None,
            coverage_manifest: None,
            zip_undeclared_outputs: false,
            setup_script: setup,
            kill_delay_ms: Some(200),
            test_tmpdir: None,
            check_sharding_support: false,
        }
    }

    #[test]
    fn pass_fail_and_env() {
        let mut sp = setup("pf", "#!/bin/sh\necho \"$TEST_TARGET $TEST_SIZE ${TEST_TOTAL_SHARDS:-0} $TEST_WORKSPACE $1\"\n[ -d \"$TEST_TMPDIR\" ] && [ \"$PWD\" = \"$TEST_SRCDIR/_main\" ] || exit 7\n");
        sp.args = vec!["argA".into()];
        let r = run_test(&sp).unwrap();
        assert_eq!(r.status, TestStatus::Passed, "{}", String::from_utf8_lossy(&r.attempts[0].log));
        let log = std::fs::read_to_string(&r.log_path).unwrap();
        assert!(log.contains("Executing tests from //pkg:t"), "{log}");
        assert!(log.contains("//pkg:t medium 0 _main argA"), "{log}");
        assert!(r.xml_path.exists());
        assert!(read_cache_status(&sp.test_log_dir).unwrap().cached);

        let sp = setup("fail", "#!/bin/sh\necho boom\nexit 3\n");
        let r = run_test(&sp).unwrap();
        assert_eq!(r.status, TestStatus::Failed);
        assert!(std::fs::read_to_string(&r.xml_path).unwrap().contains("exited with error code 3"));
    }

    #[test]
    fn flaky_timeout_runs_and_shards() {
        // Fails the first attempt, passes the second.
        let mut sp = setup("flaky", "#!/bin/sh\nf=\"$TEST_SRCDIR/../../flaky_marker\"\nif [ -e \"$f\" ]; then exit 0; fi\ntouch \"$f\"; exit 1\n");
        sp.flaky_attempts = 2;
        let _ = std::fs::remove_file(sp.exec_root.join("flaky_marker"));
        let r = run_test(&sp).unwrap();
        assert_eq!(r.status, TestStatus::Flaky, "{:?}", r.status);
        assert_eq!(r.attempts.len(), 2);
        assert!(format_summary_line("//pkg:t", &r).contains("FLAKY, failed in 1 out of 2"));
        assert!(sp.test_log_dir.join("test_attempts/attempt_1.log").exists());
        assert!(sp.test_log_dir.join("test_attempts/attempt_1.xml").exists());

        let mut sp = setup("to", "#!/bin/sh\nsleep 30\n");
        sp.timeout_secs = 1;
        let r = run_test(&sp).unwrap();
        assert_eq!(r.status, TestStatus::Timeout);

        let mut sp = setup("shard", "#!/bin/sh\necho \"shard $TEST_SHARD_INDEX of $TEST_TOTAL_SHARDS\"\n");
        sp.shard_count = 3;
        sp.runs_per_test = 2;
        let r = run_test(&sp).unwrap();
        assert_eq!(r.status, TestStatus::Passed);
        assert_eq!(r.attempts.len(), 6);
        let log = std::fs::read_to_string(&r.log_path).unwrap();
        assert!(log.contains("shard 2 of 3"));
        assert!(sp.test_log_dir.join("run_2_of_2/shard_3_of_3/test.log").exists());
        assert!(format_summary_line("//pkg:t", &r).contains("Stats over 6 runs"));

        // Sharded test with check_sharding_support=true fails if it does not touch TEST_SHARD_STATUS_FILE
        let mut sp = setup("shard_support", "#!/bin/sh\nexit 0\n");
        sp.shard_count = 2;
        sp.check_sharding_support = true;
        let r = run_test(&sp).unwrap();
        assert_eq!(r.status, TestStatus::NoStatus);
        let log = std::fs::read_to_string(&r.log_path).unwrap();
        assert!(log.contains("Sharding requested, but the test runner did not advertise support for it by touching TEST_SHARD_STATUS_FILE"));
    }

    #[test]
    fn summary_formatting_matches_bazel() {
        let r = TestResult {
            exit_code: 0,
            start_time_ms: 0,
            has_coverage: false,
            test_case: None,
            unit_cases: Vec::new(),
            status: TestStatus::Passed,
            attempts: vec![],
            failed_attempts: 0,
            duration_ms: 100,
            times_ms: vec![100],
            log_path: PathBuf::new(),
            xml_path: PathBuf::new(),
            cached: false,
            runs: 1,
        };
        let l = format_summary_line("//pkg:t", &r);
        assert_eq!(l.find("PASSED"), Some(78 - 6 + 1));
        assert!(l.starts_with("//pkg:t   ") && l.ends_with(" PASSED in 0.1s"));
        let mut c = r.clone();
        c.cached = true;
        assert!(format_summary_line("//a:b", &c).contains("(cached) PASSED in 0.1s"));
        assert_eq!(
            format_stats(1, &[("a".into(), TestStatus::Passed), ("b".into(), TestStatus::Failed)]),
            "\nExecuted 1 out of 2 tests: 1 test passes and 1 fails locally.\n"
        );
        assert_eq!(
            format_stats(3, &[("a".into(), TestStatus::Passed), ("b".into(), TestStatus::Passed), ("c".into(), TestStatus::Passed)]),
            "\nExecuted 3 out of 3 tests: 3 tests pass.\n"
        );
        assert_eq!(format_stats(0, &[]), "");
    }

    #[test]
    fn custom_test_tmpdir_is_honored() {
        let mut sp = setup("tmpdir", "#!/bin/sh\ncase \"$TEST_TMPDIR\" in */custom_tmp_*/*) exit 0;; *) exit 1;; esac\n");
        let custom = std::env::temp_dir().join(format!("custom_tmp_{}", std::process::id()));
        sp.test_tmpdir = Some(custom.clone());
        let r = run_test(&sp).unwrap();
        assert_eq!(r.status, TestStatus::Passed);
        let _ = std::fs::remove_dir_all(&custom);
    }
}

/// `ShellUtils.tokenize`: whitespace separated words with single/double quotes and backslash escapes.
pub fn shell_tokenize_words(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                        in_word = true;
                    }
                }
                c if c.is_whitespace() => {
                    if in_word {
                        out.push(std::mem::take(&mut cur));
                        in_word = false;
                    }
                }
                c => {
                    cur.push(c);
                    in_word = true;
                }
            },
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

/// Inputs of the test spawn as the execution log records them: every file of the runfiles tree
/// plus the XML generator and the test setup script (`TestRunnerAction.getInputs`).
fn logged_test_inputs(spec: &TestSpec, dir: &Path) -> Vec<(String, bool)> {
    let _ = dir;
    fn walk(p: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(p) else { return };
        for e in rd.flatten() {
            let path = e.path();
            let Ok(md) = std::fs::symlink_metadata(&path) else { continue };
            // Symlinks to directories (tree artifacts) are single inputs like in Bazel's tree.
            if md.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&spec.runfiles_dir, &mut files);
    let rel = |p: &Path| -> String {
        p.strip_prefix(&spec.exec_root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string_lossy().into_owned())
    };
    let mut v: Vec<String> = files
        .iter()
        .filter(|p| p.file_name().map_or(true, |n| n != "MANIFEST"))
        .map(|p| rel(p))
        .collect();
    v.push("external/bazel_tools/tools/test/generate-xml.sh".to_string());
    v.push("external/bazel_tools/tools/test/test-setup.sh".to_string());
    v.sort();
    v.dedup();
    v.into_iter().map(|p| (p, false)).collect()
}
