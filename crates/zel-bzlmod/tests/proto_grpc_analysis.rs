//! End-to-end analysis tests for `proto_library`, `cc_proto_library`, `generate_cc`, and
//! `cc_grpc_library` against vendored `@protobuf`, `@rules_proto`, `@rules_cc`, and `@bazel_tools`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zel_analysis::{ActionAnalysisMetadata, BuildConfigurationKey};
use zel_bzlmod::session::{BzlmodConfig, Session};
use zel_cmdline::Label;
use zel_packages::{LoaderConfig, PackageLoader};
use zel_rules::{AnalysisConfig, TargetResolver};
use zel_vfs::{DigestHashFunction, LocalFileSystem};

struct ProtoTestEnv {
    reg_dir: PathBuf,
    ws_dir: PathBuf,
    ob_dir: PathBuf,
    resolver: Arc<TargetResolver>,
}

impl Drop for ProtoTestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.reg_dir);
        let _ = std::fs::remove_dir_all(&self.ws_dir);
        let _ = std::fs::remove_dir_all(&self.ob_dir);
    }
}

fn setup_env(tag: &str, files: &[(&str, &str)]) -> Option<ProtoTestEnv> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !repo_root.join("third_party/bcr/protobuf").exists()
        || !repo_root.join("third_party/bcr/rules_cc").exists()
    {
        return None;
    }

    let pid = std::process::id();
    let tmp = std::env::temp_dir();
    let reg_dir = tmp.join(format!("zel-proto-grpc-reg-{tag}-{pid}"));
    let ws_dir = tmp.join(format!("zel-proto-grpc-ws-{tag}-{pid}"));
    let ob_dir = tmp.join(format!("zel-proto-grpc-ob-{tag}-{pid}"));
    let _ = std::fs::remove_dir_all(&reg_dir);
    let _ = std::fs::remove_dir_all(&ws_dir);
    let _ = std::fs::remove_dir_all(&ob_dir);

    let st = std::process::Command::new("python3")
        .arg(repo_root.join("scripts/make_vendored_registry.py"))
        .arg(&reg_dir)
        .status();
    if !matches!(st, Ok(s) if s.success()) {
        return None;
    }

    let write_ws = |rel: &str, content: &str| {
        let p = ws_dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    };

    write_ws(
        "MODULE.bazel",
        r#"module(name = "proto_grpc_ws")
bazel_dep(name = "rules_cc", version = "0.2.17")
bazel_dep(name = "protobuf", version = "33.4")
bazel_dep(name = "rules_proto", version = "7.1.0")
bazel_dep(name = "platforms", version = "1.0.0")
"#,
    );

    let toolchains_bzl = r##"
def _bin_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(output = out, content = "#!/bin/sh\n", is_executable = True)
    return [DefaultInfo(executable = out, files = depset([out]))]

stub_binary = rule(implementation = _bin_impl, executable = True)
"##;
    let toolchains_build = r#"
load("@protobuf//bazel/toolchains:proto_lang_toolchain.bzl", "proto_lang_toolchain")
load("@protobuf//bazel/toolchains:proto_toolchain.bzl", "proto_toolchain")
load("@rules_cc//cc:cc_library.bzl", "cc_library")
load("@rules_cc//cc/private/toolchain:unix_cc_toolchain_config.bzl", "cc_toolchain_config")
load("@rules_cc//cc/toolchains:cc_toolchain.bzl", "cc_toolchain")
load(":defs.bzl", "stub_binary")

package(default_visibility = ["//visibility:public"])

filegroup(name = "empty", srcs = [])

stub_binary(name = "protoc_bin")

proto_toolchain(
    name = "proto_tc",
    proto_compiler = ":protoc_bin",
)

cc_library(
    name = "proto_runtime",
    hdrs = ["proto_runtime.h"],
)

proto_lang_toolchain(
    name = "cc_proto_lang_toolchain",
    command_line = "--cpp_out=$(OUT)",
    progress_message = "Generating C++ proto_library %{label}",
    runtime = ":proto_runtime",
    toolchain_type = "@protobuf//bazel/private:cc_toolchain_type",
)

toolchain(
    name = "cc_proto_tc",
    toolchain = ":cc_proto_lang_toolchain",
    toolchain_type = "@protobuf//bazel/private:cc_toolchain_type",
)

cc_toolchain_config(
    name = "k8_cc_config",
    abi_libc_version = "local",
    abi_version = "local",
    compiler = "gcc",
    cpu = "k8",
    host_system_name = "local",
    target_libc = "local",
    target_system_name = "local",
    tool_paths = {
        "ar": "/usr/bin/ar",
        "cpp": "/usr/bin/cpp",
        "gcc": "/usr/bin/gcc",
        "gcov": "/usr/bin/gcov",
        "ld": "/usr/bin/ld",
        "nm": "/usr/bin/nm",
        "objdump": "/usr/bin/objdump",
        "strip": "/usr/bin/strip",
    },
    toolchain_identifier = "local_k8",
)

cc_toolchain(
    name = "k8_cc_toolchain",
    all_files = ":empty",
    ar_files = ":empty",
    as_files = ":empty",
    compiler_files = ":empty",
    dwp_files = ":empty",
    linker_files = ":empty",
    objcopy_files = ":empty",
    strip_files = ":empty",
    supports_param_files = 0,
    toolchain_config = ":k8_cc_config",
    toolchain_identifier = "local_k8",
)

toolchain(
    name = "cc_tc",
    toolchain = ":k8_cc_toolchain",
    toolchain_type = "@bazel_tools//tools/cpp:toolchain_type",
)
"#;

    write_ws("toolchains/defs.bzl", toolchains_bzl);
    write_ws("toolchains/BUILD", toolchains_build);
    write_ws("toolchains/proto_runtime.h", "#pragma once\n");
    for (rel, content) in files {
        write_ws(rel, content);
    }

    let cfg = BzlmodConfig {
        workspace: ws_dir.clone(),
        output_base: ob_dir.clone(),
        client_env: std::env::vars().collect::<BTreeMap<_, _>>(),
        registries: vec![zel_bzlmod::file_url_for_path(&reg_dir)],
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
        experimental_sibling_repository_layout: false,
        incompatible_no_implicit_watch_label: true,
        incompatible_use_cc_configure_from_rules_cc: false,
        isolated_extension_usages: false,
        main_mapping_errors: false,
        nofetch: false,
        credential_helpers: vec![],
        incompatible_use_cc_configure_from_rules_cc: true,
    };

    let session = Session::open(cfg).unwrap_or_else(|e| panic!("Session::open: {}", e.message));
    // Point `@bazel_tools//tools/proto:protoc` at our hermetic `@@//toolchains:protoc_bin` so
    // `generate_cc` / `cc_grpc_library` tests don't need to analyze all 150 C++ targets of protoc.
    let bt_proto_build = ob_dir.join("external/bazel_tools/tools/proto/BUILD");
    let _ = zel_bazel_tools::materialize(&ob_dir.join("external/bazel_tools"));
    std::fs::write(
        bt_proto_build,
        r#"package(default_visibility = ["//visibility:public"])
alias(name = "protoc", actual = "@@//toolchains:protoc_bin")
alias(name = "cc_toolchain", actual = "@@//toolchains:cc_proto_lang_toolchain")
"#,
    )
    .unwrap();

    let repo_resolver = Arc::new(zel_bzlmod::resolver::BzlmodRepoResolver::new(
        session.rt.clone(),
    ));
    let fs = Arc::new(LocalFileSystem::new(DigestHashFunction::Sha256));
    let loader = PackageLoader::new(LoaderConfig::new(fs, repo_resolver));
    let mut acfg = AnalysisConfig::new();
    acfg.registered_toolchains = vec![
        Label::parse_canonical("//toolchains:proto_tc_toolchain").unwrap(),
        Label::parse_canonical("//toolchains:cc_proto_tc").unwrap(),
        Label::parse_canonical("//toolchains:cc_tc").unwrap(),
    ];
    let resolver = TargetResolver::new(loader, acfg);

    Some(ProtoTestEnv {
        reg_dir,
        ws_dir,
        ob_dir,
        resolver,
    })
}

#[test]
fn proto_library_cc_proto_library_generate_cc_and_cc_grpc_library_analyze() {
    let Some(env) = setup_env(
        "full",
        &[
            (
                "common/base.proto",
                "syntax = \"proto3\";\npackage common;\nmessage Base {}\n",
            ),
            (
                "common/BUILD",
                r#"
load("@rules_proto//proto:defs.bzl", "proto_library")

package(default_visibility = ["//visibility:public"])

package_group(
    name = "allowed_consumers",
    packages = ["//common", "//svc/..."],
)

proto_library(
    name = "base_proto",
    srcs = ["base.proto"],
    allow_exports = ":allowed_consumers",
)
"#,
            ),
            (
                "svc/echo.proto",
                "syntax = \"proto3\";\npackage svc;\nimport \"common/base.proto\";\nservice Echo {}\n",
            ),
            (
                "svc/sub/virt.proto",
                "syntax = \"proto3\";\npackage svc;\nservice Virt {}\n",
            ),
            (
                "svc/BUILD",
                r#"
load("@bazel_tools//third_party/grpc/bazel:cc_grpc_library.bzl", "cc_grpc_library")
load("@bazel_tools//third_party/grpc/bazel:generate_cc.bzl", "generate_cc")
load("@protobuf//bazel:cc_proto_library.bzl", "cc_proto_library")
load("@protobuf//bazel:proto_library.bzl", "proto_library")

proto_library(
    name = "echo_proto",
    srcs = ["echo.proto"],
    deps = ["//common:base_proto"],
)

cc_proto_library(
    name = "echo_cc_proto",
    deps = [":echo_proto"],
)

generate_cc(
    name = "echo_raw_pb",
    srcs = [":echo_proto"],
)

generate_cc(
    name = "base_staged_pb",
    srcs = ["//common:base_proto"],
)

proto_library(
    name = "virt_proto",
    srcs = ["sub/virt.proto"],
    strip_import_prefix = "sub",
    import_prefix = "api/v1",
)

generate_cc(
    name = "virt_grpc_codegen",
    srcs = [":virt_proto"],
    plugin = "@bazel_tools//third_party/grpc:cpp_plugin",
    generate_mocks = True,
)

cc_grpc_library(
    name = "echo_cc_grpc",
    srcs = [":echo_proto"],
    deps = [":echo_cc_proto"],
    generate_mocks = True,
)

generate_cc(
    name = "mixed_virt_and_real",
    srcs = [":echo_proto", ":virt_proto"],
)
"#,
            ),
            (
                "forbidden/BUILD",
                r#"
load("@protobuf//bazel:proto_library.bzl", "proto_library")

proto_library(
    name = "illegal_reexport",
    deps = ["//common:base_proto"],
)
"#,
            ),
            (
                "bad_grpc/BUILD",
                r#"
load("@bazel_tools//third_party/grpc/bazel:cc_grpc_library.bzl", "cc_grpc_library")

cc_grpc_library(
    name = "multi_srcs",
    srcs = ["//svc:echo_proto", "//common:base_proto"],
    deps = [],
)
"#,
            ),
        ],
    ) else {
        return;
    };

    let cfg = BuildConfigurationKey::target("k8", "fastbuild");

    // 1. `proto_library` produces descriptor set action `GenProtoDescriptorSet` and `ProtoInfo`.
    let echo_proto = env
        .resolver
        .analyze(&Label::parse_canonical("//svc:echo_proto").unwrap(), &cfg)
        .expect("proto_library analysis failed");
    assert!(echo_proto.default_info.files.iter().any(|f| f
        .exec_path
        .path_string()
        .ends_with("echo_proto-descriptor-set.proto.bin")));
    assert!(echo_proto.actions.iter().any(|a| matches!(
        a,
        ActionAnalysisMetadata::SpawnAction { mnemonic, .. } if mnemonic == "GenProtoDescriptorSet"
    )));

    // 2. `allow_exports` via `PackageSpecificationInfo.contains`: allowed in `//svc`, rejected in `//forbidden`.
    let forbidden_err = env
        .resolver
        .analyze(
            &Label::parse_canonical("//forbidden:illegal_reexport").unwrap(),
            &cfg,
        )
        .unwrap_err();
    assert!(
        forbidden_err.contains("can't be reexported in package '//forbidden'"),
        "unexpected error for disallowed proto_library export: {forbidden_err}"
    );

    // 3. `cc_proto_library` runs `cc_proto_aspect` and provides `CcInfo`.
    let echo_cc_proto = env
        .resolver
        .analyze(
            &Label::parse_canonical("//svc:echo_cc_proto").unwrap(),
            &cfg,
        )
        .expect("cc_proto_library analysis failed");
    assert!(
        echo_cc_proto
            .providers
            .keys()
            .any(|k| k.ends_with("%CcInfo") || k == "CcInfo"),
        "expected CcInfo provider on cc_proto_library, got {:?}",
        echo_cc_proto.providers.keys().collect::<Vec<_>>()
    );
    let cc_proto_outs: Vec<String> = echo_cc_proto
        .default_info
        .files
        .iter()
        .map(|a| a.root_relative_path.path_string().to_string())
        .collect();
    assert!(
        cc_proto_outs.iter().any(|p| p.ends_with("svc/echo.pb.h")),
        "missing echo.pb.h in {cc_proto_outs:?}"
    );
    assert!(
        cc_proto_outs.iter().any(|p| p.ends_with("svc/echo.pb.cc")),
        "missing echo.pb.cc in {cc_proto_outs:?}"
    );

    // 4. `generate_cc` without plugin generates `.pb.h` and `.pb.cc` with `--cpp_out=...`.
    let raw_pb = env
        .resolver
        .analyze(&Label::parse_canonical("//svc:echo_raw_pb").unwrap(), &cfg)
        .expect("generate_cc (no plugin) failed");
    let raw_outs: Vec<String> = raw_pb
        .default_info
        .files
        .iter()
        .map(|a| a.root_relative_path.path_string().to_string())
        .collect();
    assert_eq!(raw_outs, vec!["svc/echo.pb.h", "svc/echo.pb.cc"]);

    // 5. Cross-package non-virtual proto source is staged with `CopySourceProto`.
    let staged_pb = env
        .resolver
        .analyze(
            &Label::parse_canonical("//svc:base_staged_pb").unwrap(),
            &cfg,
        )
        .expect("cross-package generate_cc failed");
    assert!(staged_pb.actions.iter().any(|a| matches!(
        a,
        ActionAnalysisMetadata::SpawnAction { mnemonic, .. } if mnemonic == "CopySourceProto"
    )));
    assert!(staged_pb.actions.iter().any(|a| matches!(
        a,
        ActionAnalysisMetadata::SpawnAction { mnemonic, .. } if mnemonic == "CppGrpcGenSource"
    )));

    // 6. Virtual-import `proto_library` + `generate_cc` with plugin + `generate_mocks = True`.
    let virt_grpc = env
        .resolver
        .analyze(
            &Label::parse_canonical("//svc:virt_grpc_codegen").unwrap(),
            &cfg,
        )
        .expect("virtual-import generate_cc failed");
    let virt_outs: Vec<String> = virt_grpc
        .default_info
        .files
        .iter()
        .map(|a| a.root_relative_path.path_string().to_string())
        .collect();
    assert!(virt_outs
        .iter()
        .any(|p| p.ends_with("_virtual_imports/virt_proto/api/v1/virt.grpc.pb.h")));
    assert!(virt_outs
        .iter()
        .any(|p| p.ends_with("_virtual_imports/virt_proto/api/v1/virt.grpc.pb.cc")));
    assert!(virt_outs
        .iter()
        .any(|p| p.ends_with("_virtual_imports/virt_proto/api/v1/virt_mock.grpc.pb.h")));

    // 7. `cc_grpc_library` creates `_echo_cc_grpc_grpc_codegen` (`CppGrpcGenSource`) and wraps it
    // in a `cc_library` returning `CcInfo`.
    let codegen_ct = env
        .resolver
        .analyze(
            &Label::parse_canonical("//svc:_echo_cc_grpc_grpc_codegen").unwrap(),
            &cfg,
        )
        .expect("_echo_cc_grpc_grpc_codegen failed");
    let grpc_spawn_argv = codegen_ct
        .actions
        .iter()
        .find_map(|a| match a {
            ActionAnalysisMetadata::SpawnAction { mnemonic, argv, .. }
                if mnemonic == "CppGrpcGenSource" =>
            {
                Some(argv.clone())
            }
            _ => None,
        })
        .expect("expected CppGrpcGenSource spawn");
    assert!(grpc_spawn_argv
        .iter()
        .any(|a| a.starts_with("--plugin=protoc-gen-PLUGIN=")));
    assert!(grpc_spawn_argv
        .iter()
        .any(|a| a.starts_with("--PLUGIN_out=generate_mock_code=true:")));

    let echo_grpc = env
        .resolver
        .analyze(&Label::parse_canonical("//svc:echo_cc_grpc").unwrap(), &cfg)
        .expect("cc_grpc_library analysis failed");
    assert!(echo_grpc
        .providers
        .keys()
        .any(|k| k.ends_with("%CcInfo") || k == "CcInfo"));

    // 8. `cc_grpc_library` rejects `srcs` when len(srcs) != 1, and `generate_cc` rejects mixed
    // virtual + real `.proto` inputs.
    let multi_err = env
        .resolver
        .analyze(
            &Label::parse_canonical("//bad_grpc:multi_srcs").unwrap(),
            &cfg,
        )
        .unwrap_err();
    assert!(
        multi_err.contains("Only one srcs value supported"),
        "unexpected error for multi srcs: {multi_err}"
    );

    let mixed_err = env
        .resolver
        .analyze(
            &Label::parse_canonical("//svc:mixed_virt_and_real").unwrap(),
            &cfg,
        )
        .unwrap_err();
    assert!(
        mixed_err.contains("Proto sources must be either all virtual imports or all real"),
        "unexpected error for mixed virtual/real protos: {mixed_err}"
    );
}

#[test]
fn generate_cc_and_cc_grpc_library_well_known_protos_wire_proto_paths() {
    let Some(env) = setup_env(
        "wkt",
        &[
            (
                "wkt_svc/audit.proto",
                "syntax = \"proto3\";\npackage wkt_svc;\nimport \"google/protobuf/timestamp.proto\";\nimport \"google/protobuf/empty.proto\";\nservice Audit {}\n",
            ),
            (
                "wkt_svc/BUILD",
                r#"
load("@bazel_tools//third_party/grpc/bazel:cc_grpc_library.bzl", "cc_grpc_library")
load("@bazel_tools//third_party/grpc/bazel:generate_cc.bzl", "generate_cc")
load("@protobuf//bazel:cc_proto_library.bzl", "cc_proto_library")
load("@protobuf//bazel:proto_library.bzl", "proto_library")

proto_library(
    name = "audit_proto",
    srcs = ["audit.proto"],
)

cc_proto_library(
    name = "audit_cc_proto",
    deps = [":audit_proto"],
)

generate_cc(
    name = "audit_without_wkt",
    srcs = [":audit_proto"],
    well_known_protos = False,
)

generate_cc(
    name = "audit_with_wkt",
    srcs = [":audit_proto"],
    well_known_protos = True,
)

cc_grpc_library(
    name = "audit_cc_grpc",
    srcs = [":audit_proto"],
    deps = [":audit_cc_proto"],
    well_known_protos = True,
)
"#,
            ),
        ],
    ) else {
        return;
    };

    let cfg = BuildConfigurationKey::target("k8", "fastbuild");

    let without_wkt = env
        .resolver
        .analyze(
            &Label::parse_canonical("//wkt_svc:audit_without_wkt").unwrap(),
            &cfg,
        )
        .expect("audit_without_wkt failed");
    let without_argv = without_wkt
        .actions
        .iter()
        .find_map(|a| match a {
            ActionAnalysisMetadata::SpawnAction { mnemonic, argv, .. }
                if mnemonic == "CppGrpcGenSource" =>
            {
                Some(argv.clone())
            }
            _ => None,
        })
        .expect("expected CppGrpcGenSource on audit_without_wkt");
    assert!(
        !without_argv.iter().any(|a| a.contains("timestamp_proto")),
        "well_known_protos=False should not add timestamp_proto include path: {without_argv:?}"
    );

    let with_wkt = env
        .resolver
        .analyze(
            &Label::parse_canonical("//wkt_svc:audit_with_wkt").unwrap(),
            &cfg,
        )
        .expect("audit_with_wkt failed");
    let (with_argv, with_inputs) = with_wkt
        .actions
        .iter()
        .find_map(|a| match a {
            ActionAnalysisMetadata::SpawnAction {
                mnemonic,
                argv,
                inputs,
                ..
            } if mnemonic == "CppGrpcGenSource" => Some((argv.clone(), inputs.clone())),
            _ => None,
        })
        .expect("expected CppGrpcGenSource on audit_with_wkt");
    assert!(
        with_argv
            .iter()
            .any(|a| a.starts_with("--proto_path=")
                && a.contains("_virtual_imports/timestamp_proto")),
        "missing timestamp_proto --proto_path in {with_argv:?}"
    );
    assert!(
        with_argv
            .iter()
            .any(|a| a.starts_with("--proto_path=") && a.contains("_virtual_imports/any_proto")),
        "missing any_proto --proto_path in {with_argv:?}"
    );
    assert!(
        with_argv
            .iter()
            .any(|a| a.starts_with("--proto_path=")
                && a.contains("_virtual_imports/descriptor_proto")),
        "missing descriptor_proto --proto_path in {with_argv:?}"
    );
    assert!(
        with_inputs.iter().any(|f| f
            .exec_path
            .path_string()
            .ends_with("google/protobuf/timestamp.proto")),
        "missing google/protobuf/timestamp.proto in action inputs"
    );

    let grpc_codegen = env
        .resolver
        .analyze(
            &Label::parse_canonical("//wkt_svc:_audit_cc_grpc_grpc_codegen").unwrap(),
            &cfg,
        )
        .expect("_audit_cc_grpc_grpc_codegen failed");
    let grpc_argv = grpc_codegen
        .actions
        .iter()
        .find_map(|a| match a {
            ActionAnalysisMetadata::SpawnAction { mnemonic, argv, .. }
                if mnemonic == "CppGrpcGenSource" =>
            {
                Some(argv.clone())
            }
            _ => None,
        })
        .expect("expected CppGrpcGenSource on _audit_cc_grpc_grpc_codegen");
    assert!(
        grpc_argv
            .iter()
            .any(|a| a.starts_with("--proto_path=") && a.contains("_virtual_imports/empty_proto")),
        "cc_grpc_library well_known_protos=True did not forward empty_proto --proto_path: {grpc_argv:?}"
    );
}
