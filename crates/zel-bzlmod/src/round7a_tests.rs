//! Tests for the round-7A fixes: registry URL validation, `Fingerprint`, recorded-input syntax,
//! downloader-config parsing, semantics fingerprint, isolated-extension flag, lockfile registry hashes.

use crate::hash::Fingerprint;
use crate::lockfile::{escape_unescape_roundtrip_for_tests, recorded_inputs_outdated, unescape_input};
use crate::registry::validate_registry_url;
use std::collections::BTreeMap;

#[test]
fn registry_url_validation_matches_java_uri() {
    let err = |u: &str| validate_registry_url(u).unwrap_err();
    assert_eq!(err("ftp://x"), "Unrecognized registry URL protocol: ftp://x");
    assert_eq!(err("/tmp/reg"), "Registry URL has no scheme -- supported schemes are: http://, https:// and file://: /tmp/reg");
    assert_eq!(err(""), "Registry URL has no scheme -- supported schemes are: http://, https:// and file://: ");
    assert!(err("file:reg").starts_with("Registry URL path is not valid -- did you mean to use file:///foo/bar"));
    assert!(err("foo:bar").starts_with("Registry URL path is not valid"));
    assert_eq!(err("http:"), "Expected scheme-specific part at index 5: http:");
    assert_eq!(err("http://[bad"), "Expected closing bracket for IPv6 address at index 11: http://[bad");
    assert_eq!(err("file:///tmp/a b"), "Illegal character in path at index 13: file:///tmp/a b");
    assert_eq!(err("FILE:///tmp/x"), "Unrecognized registry URL protocol: FILE:///tmp/x");
    assert!(err("file:///tmp/a%zz").starts_with("Malformed escape pair at index"));
    for ok in ["file:///tmp/reg", "https://bcr.bazel.build", "http://127.0.0.1:8080/x/", "file://localhost/tmp", "file://reg"] {
        assert!(validate_registry_url(ok).is_ok(), "{ok}");
    }
}

#[test]
fn fingerprint_uses_protobuf_encoding() {
    // addInt(300) = varint ac 02; addInt(-1) = ten byte sign-extended varint; addString = length + bytes.
    let mut f = Fingerprint::new();
    f.add_int(300).add_int(-1).add_string("ab").add_boolean(true).add_nullable_string(None);
    let mut raw = vec![0xac, 0x02];
    raw.extend([0xff; 9]);
    raw.push(0x01);
    raw.extend([2, b'a', b'b', 1, 0]);
    assert_eq!(f.digest(), crate::hash::sha256(&raw));
    let mut g = Fingerprint::new();
    g.add_strings(&["x", "yz"]);
    assert_eq!(g.digest(), crate::hash::sha256(&[2, 1, b'x', 2, b'y', b'z']));
}

#[test]
fn recorded_input_escaping() {
    assert_eq!(crate::ctx::escape_input("a b\nc\\d"), "a\\sb\\nc\\\\d");
    assert_eq!(unescape_input("a\\sb\\nc\\\\d").as_deref(), Some("a b\nc\\d"));
    assert_eq!(unescape_input("\\0"), None);
    assert_eq!(crate::ctx::format_recorded("ENV:X", None), "ENV:X \\0");
    assert_eq!(crate::ctx::format_recorded("ENV:X", Some("a b")), "ENV:X a\\sb");
    assert!(escape_unescape_roundtrip_for_tests("weird \\ value\nwith newline"));
}

#[test]
fn recorded_inputs_detect_changes() {
    let d = std::env::temp_dir().join(format!("zel-7a-ri-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("f.txt"), "one").unwrap();
    let ext = d.join("ext");
    let digest = crate::hash::sha256_hex(b"one");
    let names = Fingerprint::new().add_strings(&["a.txt"]).hex_digest();
    std::fs::write(d.join("sub/a.txt"), "").unwrap();
    let inputs = vec![
        "ENV:X \\0".to_string(),
        format!("FILE:@@//f.txt {digest}"),
        "FILE:@@//missing ENOENT".to_string(),
        "FILE:@@//sub DIR".to_string(),
        format!("DIRENTS:@@//sub {names}"),
    ];
    let env: BTreeMap<String, String> = BTreeMap::new();
    assert_eq!(recorded_inputs_outdated(&inputs, &env, &d, &ext), None);
    let mut e2 = env.clone();
    e2.insert("X".into(), "v".into());
    assert!(recorded_inputs_outdated(&inputs, &e2, &d, &ext).unwrap().contains("environment variable X changed: <unset> -> 'v'"));
    std::fs::write(d.join("f.txt"), "two").unwrap();
    assert!(recorded_inputs_outdated(&inputs, &env, &d, &ext).unwrap().contains("f.txt"));
    std::fs::write(d.join("f.txt"), "one").unwrap();
    std::fs::write(d.join("sub/b.txt"), "").unwrap();
    assert!(recorded_inputs_outdated(&inputs, &env, &d, &ext).unwrap().contains("directory entries"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn downloader_config_files() {
    use crate::download::UrlRewriter;
    let d = std::env::temp_dir().join(format!("zel-7a-dc-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("a.cfg"), "block *\nall_blocked_message hi there\n").unwrap();
    std::fs::write(d.join("b.cfg"), "allow example.com\n").unwrap();
    std::fs::write(d.join("bad.cfg"), "# c\nfrobnicate x\n").unwrap();
    assert!(UrlRewriter::from_config_paths(&d, &[]).unwrap().is_none());
    assert!(UrlRewriter::from_config_paths(&d, &["a.cfg".into(), "".into()]).unwrap().is_none());
    let e = UrlRewriter::from_config_paths(&d, &["bad.cfg".into()]).unwrap_err();
    assert_eq!(e, "Failed to parse downloader config at bad.cfg:2: Unable to parse: frobnicate x");
    let e = UrlRewriter::from_config_paths(&d, &["nope.cfg".into(), "gone.cfg".into()]).unwrap_err();
    assert_eq!(e, format!("Failed to parse downloader config: Unable to find downloader config file {0}/nope.cfg,{0}/gone.cfg", d.display()));
    // Several files share allow/block lists: `allow` of one file wins over `block *` of another.
    let r = UrlRewriter::from_config_paths(&d, &["a.cfg".into(), "b.cfg".into()]).unwrap().unwrap();
    assert_eq!(r.amend(&["https://example.com/x".to_string(), "https://other.org/y".to_string()]), vec!["https://example.com/x".to_string()]);
    assert_eq!(r.all_blocked_message.as_deref(), Some("hi there"));
    assert!(UrlRewriter::parse("c", "rewrite a b${urlencode(x)}\n").unwrap_err().contains("Invalid urlencode syntax in `rewrite` replacement"));
    assert!(UrlRewriter::parse("c", "rewrite a/(.*) b/${urlencode($1)}\n").is_ok());
    assert!(UrlRewriter::parse("c", "rewrite a/(.*) b/urlencode($1)\n").is_ok());
    assert!(UrlRewriter::parse("c", "rewrite ( x\n").unwrap_err().contains(" at index "));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn semantics_fingerprint_depends_on_flags() {
    use crate::semantics_fp::stable_fingerprint;
    let base = stable_fingerprint(&BTreeMap::new());
    assert_eq!(base, stable_fingerprint(&BTreeMap::new()));
    let mut o = BTreeMap::new();
    o.insert("experimental_isolated_extension_usages".to_string(), true);
    assert_ne!(base, stable_fingerprint(&o));
    let mut o2 = BTreeMap::new();
    o2.insert("incompatible_disallow_empty_glob".to_string(), false);
    assert_ne!(base, stable_fingerprint(&o2));
}

#[test]
fn isolated_extension_usages_flag_gates_isolate() {
    use crate::module_file::{eval_module_file, ModuleFileOptions};
    use crate::model::ModuleKey;
    let mk = |flag: bool| ModuleFileOptions {
        key: ModuleKey::root(),
        ignore_dev_deps: false,
        isolated_extension_usages: flag,
        registry_url: None,
        file_name: "MODULE.bazel".into(),
        include_reader: None,
        builtin_modules: vec![],
    };
    let src = "module(name=\"r\")\ne = use_extension(\"//:e.bzl\", \"x\", isolate = True)\nuse_repo(e, \"a\")\n";
    let e = eval_module_file(src, mk(false)).unwrap_err();
    assert!(e.contains("parameter 'isolate' is experimental and thus unavailable with the current flags"), "{e}");
    let ok = eval_module_file(src, mk(true)).unwrap();
    assert!(ok.module.extension_usages[0].isolation_key.is_some());
}

#[test]
fn default_repository_cache_follows_output_user_root() {
    let p = crate::session::default_repository_cache(std::path::Path::new("/x/ob/hash"));
    assert!(p.ends_with("cache/repos/v1"));
    crate::session::set_output_user_root(std::path::Path::new("/the/user/root"));
    assert_eq!(crate::session::default_repository_cache(std::path::Path::new("/x/ob/hash")), std::path::PathBuf::from("/the/user/root/cache/repos/v1"));
}

#[test]
fn strict_bazel_tools_closure_requires_registry_modules() {
    use crate::resolve::{resolve, ResolveOptions, STRICT_BAZEL_TOOLS};
    let d = std::env::temp_dir().join(format!("zel-7a-strict-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("reg")).unwrap();
    std::fs::write(d.join("MODULE.bazel"), "module(name = \"r\")\n").unwrap();
    let mut o = ResolveOptions::new(&d);
    o.registries = vec![format!("file://{}/reg", d.display())];
    STRICT_BAZEL_TOOLS.with(|h| h.set(true));
    let e = resolve(&o).err().expect("strict closure must fail on an empty registry");
    STRICT_BAZEL_TOOLS.with(|h| h.set(false));
    assert!(e.message.contains("not found in registries"), "{}", e.message);
    assert!(resolve(&o).is_ok());
    let _ = std::fs::remove_dir_all(&d);
}
