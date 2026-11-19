use std::collections::BTreeMap;
use std::sync::Arc;
use zel_bzlmod::netrc::resolve_auth_headers;
use zel_bzlmod::resolve::{resolve, CheckMode, ResolveOptions};
use zel_bzlmod::rt::{ExternalOptions, Rt};

#[test]
fn non_root_module_extension_labels_bind_to_own_repo_and_rule_allowed_at_load() {
    let root = std::env::temp_dir().join(format!("zel-bzlmod-ext-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ws = root.join("ws");
    let dep = root.join("dep");
    let reg = root.join("reg");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(dep.join("incredible")).unwrap();
    let manifest_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let st = std::process::Command::new("python3")
        .arg(manifest_root.join("scripts/make_vendored_registry.py"))
        .arg(&reg)
        .status()
        .unwrap();
    assert!(st.success());
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
    let dir = std::env::temp_dir().join(format!("zel-netrc-auth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let netrc = dir.join(".netrc");
    std::fs::write(&netrc, "machine example.com login foo password bar\n").unwrap();
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
        std::fs::write(
            &helper,
            "#!/bin/sh\necho '{\"headers\":{\"Authorization\":[\"Bearer TOKEN\"]}}'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
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
    }

    let _ = std::fs::remove_dir_all(&dir);
}
