//! Analyzes a real `cc_binary` (rules_cc) through the library-level analysis engine, timing phases.
//! Requires `third_party/bcr` and python3 (builds the local registry).
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use zel_bzlmod::session::{BzlmodConfig, Session};

#[test]
fn analyze_cc_binary() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !root.join("third_party/bcr/rules_cc").exists() || std::env::var("ZEL_CC_ANALYSIS").is_err() {
        return;
    }
    let reg = PathBuf::from("/tmp/zel-registry");
    let _ = std::process::Command::new("python3").arg(root.join("scripts/make_vendored_registry.py")).arg(&reg).status();
    let ws = PathBuf::from("/tmp/zel-cc-ws");
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join("hello")).unwrap();
    std::fs::write(ws.join("MODULE.bazel"), "module(name = \"ws\")\nbazel_dep(name = \"rules_cc\", version = \"0.2.17\")\nbazel_dep(name = \"platforms\", version = \"1.0.0\")\n").unwrap();
    std::fs::write(ws.join("hello/BUILD"), "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\ncc_binary(name = \"hello\", srcs = [\"hello.cc\"])\n").unwrap();
    std::fs::write(ws.join("hello/hello.cc"), "int main(){return 0;}\n").unwrap();
    let cfg = BzlmodConfig {
        workspace: ws,
        output_base: PathBuf::from("/tmp/zel-cc-ob"),
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
    let t = Instant::now();
    let s = Session::open(cfg).unwrap_or_else(|e| panic!("{}", e.message));
    eprintln!("session open: {:?}", t.elapsed());
    let resolver = Arc::new(zel_bzlmod::resolver::BzlmodRepoResolver::new(s.rt.clone()));
    let lcfg = zel_packages::LoaderConfig::new(
        Arc::new(zel_vfs::LocalFileSystem::new(zel_vfs::DigestHashFunction::Sha256)),
        resolver,
    );
    let loader = zel_packages::PackageLoader::new(lcfg);
    let t = Instant::now();
    let pkg = loader.load_package(&zel_cmdline::PackageIdentifier::create_in_main_repo("hello").unwrap());
    eprintln!("load package: {:?} ok={}", t.elapsed(), pkg.is_ok());
    if let Ok(p) = &pkg {
        for e in &p.events {
            eprintln!("event: {}", e.render());
        }
    }
    if let Ok(pk) = std::env::var("ZEL_PROBE_PKG") {
        for one in pk.split(',') {
            let (repo, p) = one.split_once("//").unwrap();
            let id = zel_cmdline::PackageIdentifier::create(
                zel_cmdline::RepositoryName::create_unvalidated(repo.trim_start_matches('@')),
                zel_vfs::PathFragment::create(p),
            ).unwrap();
            match loader.load_package(&id) {
                Ok(p) => {
                    eprintln!("PKG {one}: {} targets, contains_errors={}", p.targets.len(), p.contains_errors);
                    for e in &p.events { eprintln!("  event: {}", e.render()); }
                }
                Err(e) => eprintln!("PKG {one}: load error {e}"),
            }
        }
    }
    let t = Instant::now();
    let analyzer = zel_rules::TargetResolver::new(loader, zel_rules::AnalysisConfig::new());
    let label = zel_cmdline::Label::parse_canonical("@@//hello:hello").unwrap();
    let r = analyzer.analyze(&label, &zel_analysis::BuildConfigurationKey::target("darwin_arm64", "fastbuild"));
    eprintln!("analyze: {:?}", t.elapsed());
    match r {
        Ok(ct) => eprintln!("ANALYZED actions={}", ct.actions.len()),
        Err(e) => eprintln!("ANALYSIS ERROR: {e}"),
    }
}
