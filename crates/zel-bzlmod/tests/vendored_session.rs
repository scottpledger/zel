//! Resolves a root module depending on rules_cc/rules_java/rules_python/protobuf against the local
//! vendored registry (`scripts/make_vendored_registry.py /tmp/zel-registry`) and fetches every repo
//! (including module-extension generated repos). Probe for repo-rule/extension conformance.
use std::collections::BTreeMap;
use std::path::PathBuf;
use zel_bzlmod::session::{BzlmodConfig, Session};

#[test]
fn fetch_all_repos_from_vendored_registry() {
    // Needs third_party/bcr (scripts/vendor_rules_sources.py) and python3; skipped otherwise.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !root.join("third_party/bcr/rules_cc").exists() {
        eprintln!("third_party/bcr not vendored; skipping");
        return;
    }
    let reg = PathBuf::from("/tmp/zel-registry");
    let _ = std::fs::remove_dir_all(&reg);
    let st = std::process::Command::new("python3")
        .arg(root.join("scripts/make_vendored_registry.py"))
        .arg(&reg)
        .status();
    if !matches!(st, Ok(s) if s.success()) {
        eprintln!("python3 unavailable; skipping");
        return;
    }
    let ws = PathBuf::from("/tmp/zel-probe-ws");
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(
        ws.join("MODULE.bazel"),
        "module(name = \"probe\")\nbazel_dep(name = \"rules_cc\", version = \"0.2.17\")\nbazel_dep(name = \"rules_java\", version = \"9.1.0\")\nbazel_dep(name = \"rules_python\", version = \"1.7.0\")\nbazel_dep(name = \"protobuf\", version = \"33.4\")\nbazel_dep(name = \"platforms\", version = \"1.0.0\")\n",
    )
    .unwrap();
    let ob = PathBuf::from("/tmp/zel-probe-ob");
    let cfg = BzlmodConfig {
        workspace: ws,
        output_base: ob,
        client_env: std::env::vars().collect::<BTreeMap<_, _>>(),
        registries: vec!["file:///tmp/zel-registry".into()],
        check_direct_dependencies: zel_bzlmod::resolve::CheckMode::Off,
        bazel_compatibility_mode: zel_bzlmod::resolve::CheckMode::Off,
        lockfile_mode: zel_bzlmod::lockfile::LockfileMode::Off,
        override_repository: BTreeMap::new(),
        distdirs: vec![],
        repository_cache: None,
        disable_download: true,
        vendor_dir: None,
        ignore_dev_dependency: false,
        allow_yanked: vec![],
        override_modules: vec![],
        inject_modules: vec![],
        inject_repositories: vec![],
        module_mirrors: vec![],
        downloader_rewriter: None,
        semantics_fingerprint: vec![],
        download_retries: 0,
        force: false,
        bazel_version: "9.3.2".into(),
        repo_contents_cache: None,
        experimental_repository_ctx_execute_wasm: false,
        isolated_extension_usages: false,
        main_mapping_errors: false,
        nofetch: false,
    };
    let s = match Session::open(cfg) {
        Ok(s) => s,
        Err(e) => panic!("open: {}", e.message),
    };
    if let Ok(l) = std::env::var("ZEL_PROBE_LOAD") {
        for one in l.split(',') {
            match s.rt.load_bzl(one, "", "") {
                Ok(m) => eprintln!("LOAD OK {one}: {} exports", m.len()),
                Err(e) => eprintln!("LOAD FAIL {one}: {e}"),
            }
        }
    }
    if std::env::var("ZEL_PROBE_ALL").is_ok() {
        let names = s.rt.all_repo_names().unwrap();
        eprintln!("{} repos", names.len());
        return;
    }
    // Lazily resolved, like a real build: load the real rule entry points through the production
    // package loader backed by this Bzlmod session.
    let resolver = std::sync::Arc::new(zel_bzlmod::resolver::BzlmodRepoResolver::new(s.rt.clone()));
    let cfg = zel_packages::LoaderConfig::new(
        std::sync::Arc::new(zel_vfs::LocalFileSystem::new(zel_vfs::DigestHashFunction::Sha256)),
        resolver.clone(),
    );
    let loader = zel_packages::PackageLoader::new(cfg);
    let entries = [
        ("@@bazel_features+//:features.bzl", "bazel_features"),
        ("@@rules_cc+//cc:cc_library.bzl", "cc_library"),
        ("@@rules_cc+//cc:cc_binary.bzl", "cc_binary"),
        ("@@rules_cc+//cc:cc_test.bzl", "cc_test"),
        ("@@rules_cc+//cc:cc_import.bzl", "cc_import"),
        ("@@rules_cc+//cc:cc_shared_library.bzl", "cc_shared_library"),
        ("@@rules_cc+//cc/toolchains:cc_toolchain.bzl", "cc_toolchain"),
        ("@@rules_cc+//cc/private/toolchain:unix_cc_toolchain_config.bzl", "cc_toolchain_config"),
        ("@@rules_java+//java:java_library.bzl", "java_library"),
        ("@@rules_java+//java:java_binary.bzl", "java_binary"),
        ("@@rules_java+//java:java_test.bzl", "java_test"),
        ("@@rules_java+//java:java_import.bzl", "java_import"),
        ("@@rules_java+//java/common:java_common.bzl", "java_common"),
        ("@@rules_python+//python:py_library.bzl", "py_library"),
        ("@@rules_python+//python:py_binary.bzl", "py_binary"),
        ("@@rules_python+//python:py_test.bzl", "py_test"),
        ("@@rules_python+//python:py_runtime.bzl", "py_runtime"),
        ("@@protobuf+//bazel:proto_library.bzl", "proto_library"),
        ("@@protobuf+//bazel:cc_proto_library.bzl", "cc_proto_library"),
        ("@@protobuf+//bazel:java_proto_library.bzl", "java_proto_library"),
        ("@@protobuf+//bazel:py_proto_library.bzl", "py_proto_library"),
    ];
    let mut ok = 0;
    for (label, sym) in entries {
        let lab = zel_cmdline::Label::parse_canonical(label).unwrap();
        match loader.load_bzl_export(&lab, sym) {
            Ok(_) => {
                ok += 1;
                eprintln!("OK   {label}%{sym}")
            }
            Err(e) => eprintln!("FAIL {label}%{sym}: {}", e.replace('\n', " | ")),
        }
    }
    eprintln!("ENTRY POINTS: {ok} of {} load", entries.len());
    assert_eq!(ok, entries.len(), "real rules_cc/rules_java/rules_python/protobuf entry points must load");
    for l in s.rt.take_log() {
        eprintln!("{l}");
    }
    if let Some(e) = loader.builtins_error() {
        eprintln!("BUILTINS ERROR: {e}");
    }
}
