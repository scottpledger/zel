use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use zel_bzlmod::netrc::{resolve_auth_headers, resolve_netrc_headers};
use zel_bzlmod::resolve::{resolve, CheckMode, ResolveOptions};
use zel_bzlmod::rt::{ExternalOptions, Rt};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn non_root_module_extension_labels_bind_to_own_repo_and_rule_allowed_at_load() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!("zel-bzlmod-ext-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ws = root.join("ws");
    let dep = root.join("dep");
    let reg = root.join("reg");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(dep.join("incredible")).unwrap();
    let manifest_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = manifest_root.join("scripts/make_vendored_registry.py");
    let ok = ["python3", "python"].iter().any(|py| {
        std::process::Command::new(py)
            .arg(&script)
            .arg(&reg)
            .status()
            .is_ok_and(|s| s.success())
    });
    assert!(
        ok,
        "python3 or python is required to build vendored test registry"
    );
    std::fs::create_dir_all(reg.join("modules/dep/1.0")).unwrap();

    std::fs::write(
        ws.join("MODULE.bazel"),
        "module(name = \"root\")\nbazel_dep(name = \"dep\", version = \"1.0\")\n",
    )
    .unwrap();
    std::fs::write(
        dep.join("MODULE.bazel"),
        "module(name = \"dep\", version = \"1.0\")\next = use_extension(\"//:defs.bzl\", \"myext\")\next2 = use_extension(\"//incredible:conflict.bzl\", \"myext2\")\nuse_repo(ext, \"r1\")\nuse_repo(ext2, \"r2\")\n",
    )
    .unwrap();
    std::fs::write(dep.join("BUILD"), "").unwrap();
    std::fs::write(
        dep.join("defs.bzl"),
        "my_rule = rule(implementation = lambda ctx: [])\ndef init_rule():\n    my_rule(name = 'x')\ndef _ext(ctx):\n    init_rule()\nmyext = module_extension(implementation = _ext)\n",
    )
    .unwrap();
    std::fs::write(
        dep.join("incredible/conflict.bzl"),
        "def _ext(ctx): pass\nmyext2 = module_extension(implementation = _ext)\n",
    )
    .unwrap();

    std::fs::write(
        reg.join("modules/dep/1.0/MODULE.bazel"),
        "module(name = \"dep\", version = \"1.0\")\next = use_extension(\"//:defs.bzl\", \"myext\")\next2 = use_extension(\"//incredible:conflict.bzl\", \"myext2\")\nuse_repo(ext, \"r1\")\nuse_repo(ext2, \"r2\")\n",
    )
    .unwrap();
    std::fs::write(
        reg.join("modules/dep/1.0/source.json"),
        format!(
            "{{\"type\": \"local_path\", \"path\": \"{}\"}}",
            dep.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();

    let dep_path = dep.clone();
    let mut opts = ResolveOptions::new(ws.clone());
    opts.registries = vec![zel_bzlmod::file_url_for_path(&reg)];
    opts.check_direct_dependencies = CheckMode::Off;
    opts.bazel_compatibility_mode = CheckMode::Off;
    opts.repo_fetcher = Some(Arc::new(move |_, _| Ok(dep_path.clone())));
    let graph = resolve(&opts).unwrap();
    let ids: Vec<(String, String)> = graph
        .extension_usages
        .iter()
        .map(|(id, _, _)| (id.bzl_file.clone(), id.name.clone()))
        .collect();
    assert!(
        ids.iter()
            .any(|(f, n)| f == "@@dep+//:defs.bzl" && n == "myext"),
        "{ids:?}"
    );
    assert!(
        ids.iter()
            .any(|(f, n)| f == "@@dep+//incredible:conflict.bzl" && n == "myext2"),
        "{ids:?}"
    );

    // `rule()` is allowed when loading defs.bzl, and errors only when invoked inside the extension.
    let ext_id = graph
        .extension_usages
        .iter()
        .find(|(id, _, _)| id.name == "myext")
        .unwrap()
        .0
        .clone();
    let ext_opts = ExternalOptions {
        output_base: root.join("ob"),
        workspace_root: ws.clone(),
        ..Default::default()
    };
    let rt = Rt::new(ext_opts, Arc::new(graph));
    let err = rt.eval_extension(&ext_id).unwrap_err();
    assert!(
        err.contains("a rule can only be instantiated while evaluating a BUILD file or a legacy or symbolic macro"),
        "{err}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn netrc_and_credential_helper_auth_headers() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("zel-netrc-auth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let netrc = dir.join(".netrc");
    std::fs::write(&netrc, "machine example.com login foo password bar\n").unwrap();

    // Server process NETRC/HOME/USERPROFILE must be ignored when absent from `client_env`.
    let prev_netrc = std::env::var_os("NETRC");
    let prev_home = std::env::var_os("HOME");
    let prev_userprofile = std::env::var_os("USERPROFILE");
    std::env::set_var("NETRC", &netrc);
    std::env::set_var("HOME", &dir);
    std::env::set_var("USERPROFILE", &dir);
    let empty_env = BTreeMap::new();
    let from_proc_netrc =
        resolve_netrc_headers("https://example.com/modules/a/1.0/MODULE.bazel", &empty_env);
    let from_proc_auth = resolve_auth_headers(
        "https://example.com/modules/a/1.0/MODULE.bazel",
        &[],
        &empty_env,
    )
    .unwrap();
    match prev_netrc {
        Some(v) => std::env::set_var("NETRC", v),
        None => std::env::remove_var("NETRC"),
    }
    match prev_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match prev_userprofile {
        Some(v) => std::env::set_var("USERPROFILE", v),
        None => std::env::remove_var("USERPROFILE"),
    }
    assert!(from_proc_netrc.is_empty(), "{from_proc_netrc:?}");
    assert!(from_proc_auth.is_empty(), "{from_proc_auth:?}");

    let mut env = BTreeMap::new();
    env.insert("NETRC".to_string(), netrc.to_string_lossy().into_owned());

    let hdrs =
        resolve_auth_headers("https://example.com/modules/a/1.0/MODULE.bazel", &[], &env).unwrap();
    assert_eq!(
        hdrs,
        vec![(
            "Authorization".to_string(),
            "Basic Zm9vOmJhcg==".to_string()
        )]
    );

    #[cfg(unix)]
    {
        let helper = dir.join("helper.sh");
        write_executable_script(
            &helper,
            b"#!/bin/sh\necho '{\"headers\":{\"Authorization\":[\"Bearer TOKEN\"]}}'\n",
        );
        let hdrs2 = resolve_auth_headers(
            "https://example.com/modules/a/1.0/MODULE.bazel",
            &[("example.com".to_string(), helper)],
            &env,
        )
        .unwrap();
        assert_eq!(
            hdrs2,
            vec![("Authorization".to_string(), "Bearer TOKEN".to_string())]
        );

        let empty_helper = dir.join("empty_helper.sh");
        std::fs::write(&empty_helper, "#!/bin/sh\ncat >/dev/null\necho '{}'\n").unwrap();
        std::fs::set_permissions(&empty_helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hdrs_empty = resolve_auth_headers(
            "https://example.com/modules/a/1.0/MODULE.bazel",
            &[("example.com".to_string(), empty_helper)],
            &env,
        )
        .unwrap();
        assert!(
            hdrs_empty.is_empty(),
            "matching helper returning empty headers must not fall back to .netrc: {hdrs_empty:?}"
        );

        let fail_helper = dir.join("fail_helper.sh");
        std::fs::write(
            &fail_helper,
            "#!/bin/sh\ncat >/dev/null\necho 'denied' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fail_helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = resolve_auth_headers(
            "https://example.com/modules/a/1.0/MODULE.bazel",
            &[("example.com".to_string(), fail_helper)],
            &env,
        )
        .unwrap_err();
        assert!(err.contains("denied"), "{err}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bzlmod_extension_label_helpers_visibility_and_sibling_layout() {
    let root = std::env::temp_dir().join(format!("zel-bzlmod-lbl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ws = root.join("ws");
    let helper = root.join("helper");
    let leaf = root.join("leaf");
    let reg = root.join("reg");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&helper).unwrap();
    std::fs::create_dir_all(&leaf).unwrap();
    let manifest_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let st = std::process::Command::new("python3")
        .arg(manifest_root.join("scripts/make_vendored_registry.py"))
        .arg(&reg)
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::create_dir_all(reg.join("modules/helper/1.0")).unwrap();
    std::fs::create_dir_all(reg.join("modules/leaf/2.0")).unwrap();

    let helper_mod = "module(name = \"helper\", version = \"1.0\")\nbazel_dep(name = \"leaf\", version = \"2.0\", repo_name = \"my_leaf\")\n";
    let leaf_mod = "module(name = \"leaf\", version = \"2.0\")\n";
    std::fs::write(helper.join("MODULE.bazel"), helper_mod).unwrap();
    std::fs::write(helper.join("BUILD"), "").unwrap();
    std::fs::write(
        helper.join("blazel_utils.bzl"),
        r#"
def _to_label(repo_name):
    if not repo_name.startswith("@"):
        repo_name = "@" + repo_name
    return Label(repo_name + "//:unused")

def get_canonical_repo_name(apparent_repo_name):
    return _to_label(apparent_repo_name).workspace_name

def get_repo_root(apparent_repo_name):
    return _to_label(apparent_repo_name).workspace_root
"#,
    )
    .unwrap();
    std::fs::write(leaf.join("MODULE.bazel"), leaf_mod).unwrap();
    std::fs::write(leaf.join("BUILD"), "").unwrap();

    std::fs::write(reg.join("modules/helper/1.0/MODULE.bazel"), helper_mod).unwrap();
    std::fs::write(
        reg.join("modules/helper/1.0/source.json"),
        format!(
            "{{\"type\": \"local_path\", \"path\": \"{}\"}}",
            helper.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    std::fs::write(reg.join("modules/leaf/2.0/MODULE.bazel"), leaf_mod).unwrap();
    std::fs::write(
        reg.join("modules/leaf/2.0/source.json"),
        format!(
            "{{\"type\": \"local_path\", \"path\": \"{}\"}}",
            leaf.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();

    std::fs::write(
        ws.join("MODULE.bazel"),
        "module(name = \"root\")\nbazel_dep(name = \"helper\", version = \"1.0\")\next = use_extension(\"//:ext.bzl\", \"ext\")\nuse_repo(ext, \"generated\")\n",
    )
    .unwrap();
    std::fs::write(ws.join("BUILD"), "").unwrap();
    std::fs::write(
        ws.join("ext.bzl"),
        r#"
load("@helper//:blazel_utils.bzl", "get_canonical_repo_name", "get_repo_root")

def _repo_impl(rctx):
    rctx.file("BUILD", "")
    rctx.file("info.txt", rctx.attr.info)

_repo = repository_rule(
    implementation = _repo_impl,
    attrs = {"info": attr.string()},
)

def _ext_impl(mctx):
    info = "|".join([
        get_canonical_repo_name("my_leaf"),
        get_repo_root("my_leaf"),
        Label("@helper//:unused").workspace_root,
        Label("//:unused").workspace_root,
    ])
    _repo(name = "generated", info = info)

ext = module_extension(implementation = _ext_impl)
"#,
    )
    .unwrap();

    let mut ropts = ResolveOptions::new(ws.clone());
    ropts.registries = vec![zel_bzlmod::file_url_for_path(&reg)];
    ropts.check_direct_dependencies = CheckMode::Off;
    ropts.bazel_compatibility_mode = CheckMode::Off;
    let graph = Arc::new(resolve(&ropts).unwrap());

    // Default layout: external/<canonical>.
    let ob1 = root.join("ob1");
    let rt1 = Rt::new(
        ExternalOptions {
            output_base: ob1,
            workspace_root: ws.clone(),
            ..Default::default()
        },
        Arc::clone(&graph),
    );
    let gen_dir1 = rt1.fetch_repo("+ext+generated").unwrap();
    assert_eq!(
        std::fs::read_to_string(gen_dir1.join("info.txt")).unwrap(),
        "leaf+|external/leaf+|external/helper+|"
    );

    // Sibling layout: ../<canonical>.
    let ob2 = root.join("ob2");
    let rt2 = Rt::new(
        ExternalOptions {
            output_base: ob2,
            workspace_root: ws.clone(),
            experimental_sibling_repository_layout: true,
            ..Default::default()
        },
        Arc::clone(&graph),
    );
    let gen_dir2 = rt2.fetch_repo("+ext+generated").unwrap();
    assert_eq!(
        std::fs::read_to_string(gen_dir2.join("info.txt")).unwrap(),
        "leaf+|../leaf+|../helper+|"
    );

    // Unmapped `platforms` and typo `my_leef` in `helper+` fail with invalid Label error.
    for (expr, want) in [
        (
            "get_canonical_repo_name('platforms')",
            "'workspace_name' is not allowed on invalid Label @@[unknown repo 'platforms' requested from @@helper+]//:unused",
        ),
        (
            "get_repo_root('my_leef')",
            "'workspace_root' is not allowed on invalid Label @@[unknown repo 'my_leef' requested from @@helper+ (did you mean 'my_leaf'?)]//:unused",
        ),
    ] {
        std::fs::write(
            ws.join("ext.bzl"),
            format!(
                "load('@helper//:blazel_utils.bzl', 'get_canonical_repo_name', 'get_repo_root')\ndef _ext_impl(mctx):\n    _ = {expr}\next = module_extension(implementation = _ext_impl)\n"
            ),
        )
        .unwrap();
        let ob_bad = root.join("ob_bad");
        let _ = std::fs::remove_dir_all(&ob_bad);
        let rt_bad = Rt::new(
            ExternalOptions {
                output_base: ob_bad,
                workspace_root: ws.clone(),
                ..Default::default()
            },
            Arc::clone(&graph),
        );
        let ext_id = graph
            .extension_usages
            .iter()
            .find(|(id, _, _)| id.name == "ext")
            .unwrap()
            .0
            .clone();
        let err = rt_bad.eval_extension(&ext_id).unwrap_err();
        assert!(err.contains(want), "for {expr}: {err}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
fn write_executable_script(path: &std::path::Path, content: &[u8]) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(content).unwrap();
    file.sync_all().unwrap();
    drop(file);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn concurrent_credential_helper_creation_and_subprocess_spawning() {
    use std::os::unix::process::CommandExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Barrier;

    let dir = std::env::temp_dir().join(format!("zel-netrc-concurrent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let worker_threads = 4usize;
    let forker_threads = 4usize;
    let barrier = Arc::new(Barrier::new(worker_threads + forker_threads));
    let mut handles = Vec::new();

    // Background threads using `pre_exec` force `fork()` + `execve()` (rather than `posix_spawn`),
    // reproducing the exact Linux multi-threaded write-fd inheritance race that triggers `ETXTBSY`.
    for _ in 0..forker_threads {
        let stop = Arc::clone(&stop);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            while !stop.load(Ordering::Relaxed) {
                let mut cmd = std::process::Command::new("/bin/sh");
                cmd.args(["-c", ":"]);
                // SAFETY: no-op `pre_exec` closure is async-signal-safe; used solely to force `fork()`.
                unsafe {
                    cmd.pre_exec(|| Ok(()));
                }
                let status = cmd.status().unwrap();
                assert!(status.success());
            }
        }));
    }

    for tid in 0..worker_threads {
        let dir = dir.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let env = BTreeMap::new();
            for iter in 0..20usize {
                let helper = dir.join(format!("helper-{tid}-{iter}.sh"));
                let expected = format!("Bearer TOK_{tid}_{iter}");
                let script = format!(
                    "#!/bin/sh\necho '{{\"headers\":{{\"Authorization\":[\"{expected}\"]}}}}'\n"
                );
                write_executable_script(&helper, script.as_bytes());
                let hdrs = resolve_auth_headers(
                    "https://example.com/modules/a/1.0/MODULE.bazel",
                    &[("example.com".to_string(), helper)],
                    &env,
                )
                .unwrap();
                assert_eq!(hdrs, vec![("Authorization".to_string(), expected)]);
            }
        }));
    }

    for h in handles.drain(forker_threads..) {
        h.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().unwrap();
    }

    let _ = std::fs::remove_dir_all(&dir);
}
