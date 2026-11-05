//! The `@bazel_tools//tools/build_defs/repo` rules are Starlark in Bazel; zel must run the real embedded files
//! (no native re-implementations).
use std::path::Path;

fn embedded(path: &str) -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../zel-bazel-tools/resources/tools/build_defs/repo");
    std::fs::read_to_string(dir.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn embedded_repo_bzl_files_are_the_real_ones() {
    let utils = embedded("utils.bzl");
    for f in [
        "def parse_netrc", "def read_netrc", "def use_netrc", "def get_auth", "def patch(", "def update_attrs", "def workspace_and_buildfile",
        "def maybe", "def read_user_netrc", "def download_remote_files",
    ] {
        assert!(utils.contains(f), "utils.bzl is missing {f}");
    }
    assert!(embedded("http.bzl").contains("http_archive = repository_rule("));
    assert!(embedded("git.bzl").contains("git_repository = repository_rule("));
    assert!(embedded("local.bzl").contains("local_repository = repository_rule("));
    assert!(embedded("git_worker.bzl").contains("def git_repo("));
    assert!(embedded("cache.bzl").contains("def get_default_canonical_id"));
}

#[test]
fn no_native_repo_reimplementation_remains() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert!(!src.join("native_repos.rs").exists());
    for f in ["rt.rs", "fetch.rs", "session.rs"] {
        let t = std::fs::read_to_string(src.join(f)).unwrap();
        assert!(!t.contains("native_repo_rule") && !t.contains("native_repos::"), "{f} still references native repo rules");
    }
}

#[test]
fn embedded_repo_bzl_equals_the_bazel_9_3_2_reference_files() {
    // The embedded `@bazel_tools//tools/build_defs/repo/*.bzl` are the 9.3.2 sources (scripts/sync_bazel_tools.sh);
    // the fixture holds the files a real 9.3.2 server materializes.
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bazel_9.3.2_repo_bzl");
    let embedded = Path::new(env!("CARGO_MANIFEST_DIR")).join("../zel-bazel-tools/resources/tools/build_defs/repo");
    for f in ["http.bzl", "git.bzl", "utils.bzl", "local.bzl", "cache.bzl", "git_worker.bzl", "jvm.bzl", "java.bzl"] {
        assert!(d.join(f).is_file(), "{f}");
        assert_eq!(std::fs::read(d.join(f)).unwrap(), std::fs::read(embedded.join(f)).unwrap(), "{f} differs from the embedded copy");
    }
}
