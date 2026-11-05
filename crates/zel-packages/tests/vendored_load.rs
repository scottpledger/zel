//! Loads (evaluates) real rules_cc / rules_java / rules_python / protobuf `.bzl` entry points from
//! `third_party/bcr` through the production package loader. Progress probe for engine conformance.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use zel_cmdline::{Label, RepositoryName};
use zel_packages::lookup::SimpleRepoResolver;
use zel_packages::{LoaderConfig, PackageLoader};
use zel_vfs::{DigestHashFunction, LocalFileSystem, PathFragment};

fn loader() -> Option<Arc<PackageLoader>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party");
    let bcr = root.join("bcr");
    if !bcr.exists() {
        return None;
    }
    let mut external = HashMap::new();
    let mut apparent = HashMap::new();
    for e in std::fs::read_dir(&bcr).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().to_string();
        let mut vers: Vec<_> = std::fs::read_dir(e.path()).unwrap().map(|v| v.unwrap().path()).collect();
        vers.sort();
        let Some(v) = vers.last() else { continue };
        let canon = name.clone();
        external.insert(canon.clone(), PathFragment::create(&std::fs::canonicalize(v).unwrap().to_string_lossy()));
        apparent.insert(name.clone(), RepositoryName::create(&canon).unwrap());
    }
    // Apparent names that differ from module names.
    apparent.insert("com_google_protobuf".into(), RepositoryName::create("protobuf").unwrap());
    apparent.insert("abseil-cpp".into(), RepositoryName::create("abseil-cpp").unwrap());
    let main = PathFragment::create(&std::env::temp_dir().join("zel-vendored-main").to_string_lossy());
    std::fs::create_dir_all(main.path_string()).unwrap();
    let mut r = SimpleRepoResolver::new(main);
    r.external = external;
    r.apparent = apparent;
    let fs = Arc::new(LocalFileSystem::new(DigestHashFunction::Sha256));
    Some(PackageLoader::new(LoaderConfig::new(fs, Arc::new(r))))
}

fn try_load(l: &Arc<PackageLoader>, label: &str, sym: &str) -> Result<(), String> {
    let lab = Label::parse_canonical(label).map_err(|e| e.to_string())?;
    l.load_bzl_export(&lab, sym).map(|_| ())
}

#[test]
fn probe_entry_points() {
    let Some(l) = loader() else { return };
    let entries = [
        ("@@bazel_features//:features.bzl", "bazel_features"),
        ("@@rules_cc//cc:cc_library.bzl", "cc_library"),
        ("@@rules_cc//cc:cc_binary.bzl", "cc_binary"),
        ("@@rules_cc//cc:cc_test.bzl", "cc_test"),
        ("@@rules_cc//cc:cc_import.bzl", "cc_import"),
        ("@@rules_cc//cc:defs.bzl", "cc_common"),
        ("@@rules_cc//cc/toolchains:cc_toolchain.bzl", "cc_toolchain"),
        ("@@rules_cc//cc/private/toolchain:unix_cc_toolchain_config.bzl", "cc_toolchain_config"),
        ("@@rules_java//java:java_library.bzl", "java_library"),
        ("@@rules_java//java:java_binary.bzl", "java_binary"),
        ("@@rules_java//java:java_test.bzl", "java_test"),
        ("@@rules_java//java:java_import.bzl", "java_import"),
        ("@@rules_python//python:py_library.bzl", "py_library"),
        ("@@rules_python//python:py_binary.bzl", "py_binary"),
        ("@@rules_python//python:py_test.bzl", "py_test"),
        ("@@protobuf//bazel:proto_library.bzl", "proto_library"),
        ("@@protobuf//bazel:cc_proto_library.bzl", "cc_proto_library"),
        ("@@protobuf//bazel:java_proto_library.bzl", "java_proto_library"),
        ("@@protobuf//bazel:py_proto_library.bzl", "py_proto_library"),
    ];
    match l.builtins_error() {
        None => eprintln!("OK   @_builtins exports.bzl"),
        Some(e) => eprintln!("FAIL @_builtins exports.bzl: {e}"),
    }
    let mut failed = 0;
    for (label, sym) in entries {
        match try_load(&l, label, sym) {
            Ok(()) => eprintln!("OK   {label}%{sym}"),
            Err(e) => {
                failed += 1;
                eprintln!("FAIL {label}%{sym}: {}", e.lines().next().unwrap_or(""));
            }
        }
    }
    eprintln!("{} of {} entry points load", entries.len() - failed, entries.len());
}
