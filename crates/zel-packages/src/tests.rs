//! Loading-phase tests: typed rules, conflicts, package(), glob, macros, loads, lookup.

use super::*;
use crate::lookup::{EventKind, PackageLookup};
use zel_vfs::InMemoryFileSystem;

struct Ws {
    fs: Arc<InMemoryFileSystem>,
    root: PathFragment,
}

const SH_STUB: &str = r#"def _lib(ctx):
    return [DefaultInfo(files = depset(ctx.files.srcs), runfiles = ctx.runfiles(files = ctx.files.srcs + ctx.files.data))]

def _bin(ctx):
    exe = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.symlink(output = exe, target_file = ctx.files.srcs[0], is_executable = True)
    return [DefaultInfo(executable = exe, files = depset([exe]), runfiles = ctx.runfiles(files = ctx.files.srcs + ctx.files.data))]

_attrs = {
    "srcs": attr.label_list(allow_files = True),
    "deps": attr.label_list(),
    "data": attr.label_list(allow_files = True),
}

sh_library = rule(implementation = _lib, attrs = _attrs)
sh_binary = rule(implementation = _bin, executable = True, attrs = _attrs)
sh_test = rule(implementation = _bin, test = True, attrs = _attrs)
"#;

impl Ws {
    fn new(files: &[(&str, &str)]) -> Self {
        let fs = Arc::new(InMemoryFileSystem::default());
        let root = PathFragment::create("/ws");
        fs.create_directory_and_parents(&root).unwrap();
        let ws = Self { fs, root };
        for (p, c) in files {
            ws.write(p, c);
        }
        ws
    }

    fn write(&self, p: &str, c: &str) {
        let path = self.root.get_relative_str(p);
        if let Some(parent) = path.parent_directory() {
            self.fs.create_directory_and_parents(&parent).unwrap();
        }
        // Bazel 9 has no native `sh_*` rules; in-memory workspaces load a stand-in for `@rules_shell`
        // (on the BUILD file's first line, so that reported line numbers do not move).
        let names: Vec<&str> = ["sh_binary", "sh_library", "sh_test"].into_iter().filter(|n| c.contains(&format!("{n}("))).collect();
        if (p == "BUILD" || p.ends_with("/BUILD")) && !names.is_empty() {
            let q: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
            let content = format!("load(\"//:sh_stub.bzl\", {}); {c}", q.join(", "));
            self.fs.write_file(&path, content.as_bytes()).unwrap();
            self.fs.write_file(&self.root.get_relative_str("sh_stub.bzl"), SH_STUB.as_bytes()).unwrap();
            return;
        }
        self.fs.write_file(&path, c.as_bytes()).unwrap();
    }

    fn loader(&self) -> Arc<PackageLoader> {
        self.loader_with(|_| {})
    }

    fn loader_with(&self, f: impl FnOnce(&mut LoaderConfig)) -> Arc<PackageLoader> {
        let mut cfg = LoaderConfig::new(
            self.fs.clone(),
            Arc::new(SimpleRepoResolver::new(self.root.clone())),
        );
        f(&mut cfg);
        PackageLoader::new(cfg)
    }

    fn load(&self, pkg: &str) -> Package {
        self.loader().load_package(&pid(pkg)).unwrap()
    }

    fn try_load(&self, pkg: &str) -> Result<Package, PackageLoadError> {
        self.loader().load_package(&pid(pkg))
    }
}

fn pid(p: &str) -> PackageIdentifier {
    PackageIdentifier::create_in_main_repo(p).unwrap()
}

fn errors(p: &Package) -> Vec<String> {
    p.events
        .iter()
        .filter(|e| e.kind == EventKind::Error)
        .map(|e| e.render())
        .collect()
}

/// Error lines of loading `pkg`: the package's error events, or, if loading failed outright
/// (a `.bzl` dependency that cannot be loaded), the failed load's events followed by
/// `ERROR: <message>`.
fn load_errors(ws: &Ws, pkg: &str) -> Vec<String> {
    match ws.try_load(pkg) {
        Ok(p) => errors(&p),
        Err(e) => e
            .events
            .iter()
            .filter(|ev| ev.kind == EventKind::Error)
            .map(|ev| ev.render())
            .chain(std::iter::once(format!("ERROR: {}", e.message)))
            .collect(),
    }
}

fn rule<'a>(p: &'a Package, n: &str) -> &'a RuleInstance {
    match p.targets.get(n) {
        Some(Target::Rule(r)) => r,
        other => panic!("{n} is not a rule: {other:?}"),
    }
}

#[test]
fn simple_rules_typed_and_defaults() {
    let ws = Ws::new(&[(
        "BUILD",
        r#"
filegroup(name = "fg", srcs = ["a.txt", ":other", "//x:y"], data = ["d.txt"])
genrule(name = "g", srcs = [":fg"], outs = ["out.txt"], cmd = "echo > $@")
sh_test(name = "t", srcs = ["t.sh"])
"#,
    ), ("a.txt", ""), ("d.txt", ""), ("t.sh", "")]);
    let p = ws.load("");
    assert!(!p.contains_errors, "{:?}", errors(&p));
    let fg = rule(&p, "fg");
    assert_eq!(fg.srcs, vec!["//:a.txt", "//:other", "//x:y"]);
    assert_eq!(
        fg.extra.attr_values["srcs"],
        AttrValue::LabelList(vec![
            Label::parse_canonical("//:a.txt").unwrap(),
            Label::parse_canonical("//:other").unwrap(),
            Label::parse_canonical("//x:y").unwrap()
        ])
    );
    // Defaults come from the rule class, not stored explicitly.
    let t = rule(&p, "t");
    assert!(!t.extra.attr_values.contains_key("size"));
    assert_eq!(t.attr_or_default("size"), Some(AttrValue::Str("medium".into())));
    assert_eq!(t.attr_or_default("shard_count"), Some(AttrValue::Int(-1)));
    assert!(t.executable && t.is_test_rule());
    // Outputs are targets.
    assert!(matches!(p.targets["out.txt"], Target::OutputFile { .. }));
    assert_eq!(rule(&p, "g").outs, vec!["out.txt"]);
    // Implicit input files exist only for files that exist ("other" and x:y don't).
    assert!(matches!(p.targets["a.txt"], Target::InputFile { .. }));
    assert!(matches!(p.targets["d.txt"], Target::InputFile { .. }));
    assert!(!p.targets.contains_key("other"));
}

#[test]
fn attribute_type_errors_and_unknown() {
    let cases: Vec<(&str, &str)> = vec![
        (
            "genrule(name = 'g', outs = ['o'], cmd = 1)",
            "BUILD:1:8: expected value of type 'string' for attribute 'cmd' in 'genrule' rule, but got 1 (int)",
        ),
        (
            "genrule(name = 'g', outs = 'o')",
            "expected value of type 'list(output)' for attribute 'outs' in 'genrule' rule, but got \"o\" (string)",
        ),
        (
            "genrule(name = 'g', outs = ['o'], bogus = 1)",
            "no such attribute 'bogus' in 'genrule' rule",
        ),
        (
            "genrule(name = 'g')",
            "missing value for mandatory attribute 'outs' in 'genrule' rule",
        ),
        (
            "genrule(outs = ['o'])",
            "missing value for mandatory attribute 'name' in 'genrule' rule",
        ),
        (
            "filegroup(name = 1)",
            "expected value of type 'string' for attribute 'name' in 'filegroup' rule, but got 1 (int)",
        ),
        (
            "filegroup(name = 'a b/../c')",
            "invalid target name 'a b/../c'",
        ),
        (
            "filegroup(name = 'f', srcs = ['bad label:x y//z'])",
            "invalid label 'bad label:x y//z' in element 0 of attribute 'srcs' in 'filegroup' rule",
        ),
        (
            "filegroup(name = 'f', srcs = [1])",
            "expected value of type 'string' for element 0 of attribute 'srcs' in 'filegroup' rule, but got 1 (int)",
        ),
        (
            "genrule(name = 'g', outs = [], cmd = '')",
            "attribute must be non empty",
        ),
        (
            "genrule(name = 'g', outs = ['//other:x'], cmd = '')",
            "label '//other:x' is not in the current package",
        ),
        (
            "sh_test(name = 't', size = 'huge')",
            "In rule 't', size 'huge' is not a valid size.",
        ),
        (
            "sh_test(name = 't', timeout = 'forever')",
            "In rule 't', timeout 'forever' is not a valid timeout.",
        ),
        (
            "alias(name = 'a')",
            "missing value for mandatory attribute 'actual' in 'alias' rule",
        ),
        ("filegroup('x')", "filegroup() accepts no positional arguments"),
        (
            "filegroup(name='f', visibility=['//visibility:public', '//x:g'])",
            "Public or private visibility labels (e.g. //visibility:public or //visibility:private) cannot be used in combination with other labels",
        ),
        (
            "filegroup(name='f', srcs=[':a', ':a'])",
            "Label '//:a' is duplicated in the 'srcs' attribute of rule 'f'",
        ),
    ];
    for (src, want) in cases {
        let ws = Ws::new(&[("BUILD", src)]);
        let p = ws.load("");
        let errs = errors(&p).join("\n");
        assert!(p.contains_errors, "no error for {src}");
        assert!(errs.contains(want), "for `{src}`:\n  got  {errs}\n  want {want}");
        assert!(errs.starts_with("ERROR: /ws/BUILD:1:"), "location prefix: {errs}");
    }
}

#[test]
fn duplicate_names_and_outputs() {
    let cases: Vec<(&str, &str)> = vec![
        (
            "filegroup(name = 'a')\nfilegroup(name = 'a')",
            "Error in filegroup: filegroup rule 'a' conflicts with existing filegroup rule, defined at /ws/BUILD:1:10",
        ),
        (
            "filegroup(name = 'o')\ngenrule(name = 'g', outs = ['o'], cmd = '')",
            "generated file 'o' in rule 'g' conflicts with existing filegroup rule, defined at /ws/BUILD:1:10",
        ),
        (
            "genrule(name = 'g', outs = ['o'], cmd = '')\nfilegroup(name = 'o')",
            "filegroup rule 'o' conflicts with existing generated file from rule 'g', defined at /ws/BUILD:1:8",
        ),
        (
            "genrule(name = 'g', outs = ['o', 'o'], cmd = '')",
            "rule 'g' has more than one generated file named 'o'",
        ),
        (
            "genrule(name = 'g1', outs = ['o'], cmd = '')\ngenrule(name = 'g2', outs = ['o'], cmd = '')",
            "generated file 'o' in rule 'g2' conflicts with existing generated file from rule 'g1', defined at /ws/BUILD:1:8",
        ),
        (
            "genrule(name = 'g1', outs = ['d/x'], cmd = '')\ngenrule(name = 'g2', outs = ['d'], cmd = '')",
            "output file 'd' of rule 'g2' conflicts with output file 'd/x' of rule 'g1'",
        ),
        (
            "genrule(name = 'g1', outs = ['d'], cmd = '')\ngenrule(name = 'g2', outs = ['d/x'], cmd = '')",
            "output file 'd/x' of rule 'g2' conflicts with output file 'd' of rule 'g1'",
        ),
        (
            "genrule(name = 'g', srcs = [':o'], outs = ['o'], cmd = '')",
            "rule 'g' has file 'o' as both an input and an output",
        ),
        (
            "exports_files(['a'])\nfilegroup(name = 'a')",
            "filegroup rule 'a' conflicts with existing source file, defined at /ws/BUILD:1:14",
        ),
        (
            "filegroup(name = 'a')\nexports_files(['a'])",
            "source file 'a' conflicts with existing filegroup rule, defined at /ws/BUILD:1:10",
        ),
    ];
    for (src, want) in cases {
        let ws = Ws::new(&[("BUILD", src)]);
        let p = ws.load("");
        let errs = errors(&p).join("\n");
        assert!(errs.contains(want), "for `{src}`:\n  got  {errs}\n  want {want}");
    }
}

#[test]
fn evaluation_stops_at_fatal_error_but_keeps_earlier_targets() {
    let ws = Ws::new(&[(
        "BUILD",
        "filegroup(name = 'a')\nfilegroup(name = 'a')\nfilegroup(name = 'late')",
    )]);
    let p = ws.load("");
    assert!(p.contains_errors);
    assert!(p.targets.contains_key("a"));
    assert!(!p.targets.contains_key("late"));
}

#[test]
fn undefined_names_and_restricted_syntax() {
    let ws = Ws::new(&[("BUILD", "nonexistent_rule(name = 'x')")]);
    let p = ws.load("");
    assert_eq!(
        errors(&p),
        vec!["ERROR: /ws/BUILD:1:1: name 'nonexistent_rule' is not defined".to_string()]
    );
    let ws = Ws::new(&[("BUILD", "def f():\n  pass\n")]);
    let p = ws.load("");
    assert!(errors(&p)[0].contains(
        "functions may not be defined in BUILD files. You may move the function to a .bzl file and load it."
    ));
    let ws = Ws::new(&[("BUILD", "for x in [1]:\n  pass\n")]);
    assert!(errors(&ws.load(""))[0].contains("`for` statements are not allowed in BUILD files."));
    let ws = Ws::new(&[("BUILD", "if True:\n  pass\n")]);
    assert!(errors(&ws.load(""))[0].contains("`if` statements are not allowed in BUILD files."));
    let ws = Ws::new(&[("BUILD", "filegroup(name='a', **{'srcs': []})")]);
    assert!(errors(&ws.load(""))[0].contains("**kwargs arguments are not allowed in BUILD files."));
    // Typo gets a suggestion.
    let ws = Ws::new(&[("BUILD", "filegrop(name='a')")]);
    assert!(errors(&ws.load(""))[0].contains("name 'filegrop' is not defined (did you mean 'filegroup'?)"));
}

#[test]
fn print_is_captured_once() {
    let ws = Ws::new(&[("BUILD", "print('hello', 1 << 70)\nfilegroup(name='a')")]);
    let p = ws.load("");
    let prints: Vec<_> = p.events.iter().filter(|e| e.kind == EventKind::Debug).collect();
    assert_eq!(prints.len(), 1, "{:?}", p.events);
    assert_eq!(prints[0].render(), "DEBUG: /ws/BUILD:1:6: hello 1180591620717411303424");
}

#[test]
fn package_function() {
    let ws = Ws::new(&[(
        "BUILD",
        r#"
package(
    default_visibility = ["//visibility:public"],
    default_testonly = True,
    default_deprecation = "old",
    features = ["f1"],
    default_compatible_with = ["//x:env"],
)
filegroup(name = "a")
filegroup(name = "b", visibility = ["//vis:__pkg__"], testonly = False)
"#,
    )]);
    let p = ws.load("");
    assert!(!p.contains_errors, "{:?}", errors(&p));
    assert_eq!(p.default_visibility, vec!["//visibility:public"]);
    let a = rule(&p, "a");
    assert_eq!(a.visibility, vec!["//visibility:public"]);
    assert!(!a.extra.visibility_explicit);
    assert!(a.testonly);
    assert_eq!(a.attr_or_default("deprecation"), Some(AttrValue::Str("old".into())));
    assert_eq!(a.attr_or_default("features"), Some(AttrValue::StrList(vec!["f1".into()])));
    let b = rule(&p, "b");
    assert!(b.extra.visibility_explicit);
    assert_eq!(b.visibility, vec!["//vis:__pkg__"]);
    assert!(!b.testonly);
    assert_eq!(
        b.attr_or_default("compatible_with"),
        Some(AttrValue::LabelList(vec![Label::parse_canonical("//x:env").unwrap()]))
    );
}

#[test]
fn package_function_errors() {
    for (src, want) in [
        ("package(default_visibility=[])\npackage(default_visibility=[])", "'package' can only be used once per BUILD file"),
        ("package()", "at least one argument must be given to the 'package' function"),
        ("package(bogus=1)", "unexpected keyword argument: bogus"),
        ("package('x')", "package() accepts no positional arguments"),
        ("package(default_visibility=['//a:b', '//visibility:private'])", "Public or private visibility labels"),
        ("package(default_compatible_with=['//a:b','//a:b'])", "duplicate label(s) in default_compatible_with: //a:b"),
        ("package(default_applicable_licenses=['//a:b'], default_package_metadata=['//a:c'])", "Can not set both default_package_metadata and default_applicable_licenses."),
        ("package(default_testonly='x')", "expected value of type 'bool' for attribute 'default_testonly' in 'package' rule, but got \"x\" (string)"),
    ] {
        let ws = Ws::new(&[("BUILD", src)]);
        let e = errors(&ws.load("")).join("\n");
        assert!(e.contains(want), "for `{src}`: {e}");
    }
}

#[test]
fn glob_semantics() {
    let ws = Ws::new(&[
        (
            "BUILD",
            r#"
filegroup(name = "all", srcs = glob(["**/*.txt"], exclude = ["skip/**"]))
filegroup(name = "dirs", srcs = glob(["*"], exclude_directories = 0))
filegroup(name = "files", srcs = glob(["*"]))
genrule(name = "subs", outs = ["subs.txt"], cmd = ",".join(subpackages(include = ["**"])))
"#,
        ),
        ("a.txt", ""),
        ("sub/b.txt", ""),
        ("skip/c.txt", ""),
        ("pkg/BUILD", ""),
        ("pkg/d.txt", ""),
        ("pkg/deeper/BUILD.bazel", ""),
    ]);
    let p = ws.load("");
    assert!(!p.contains_errors, "{:?}", errors(&p));
    assert_eq!(rule(&p, "all").srcs, vec!["//:a.txt", "//:sub/b.txt"]);
    // Subpackage `pkg` is excluded, regular dir `sub` and `skip` are included with dirs.
    assert_eq!(
        rule(&p, "dirs").srcs,
        vec!["//:BUILD", "//:a.txt", "//:skip", "//:sub"]
    );
    assert_eq!(rule(&p, "files").srcs, vec!["//:BUILD", "//:a.txt"]);
    assert_eq!(rule(&p, "subs").cmd.as_deref(), Some("pkg,pkg/deeper"));
}

#[test]
fn glob_errors_and_allow_empty() {
    for (src, want) in [
        ("x = glob([''])", "pattern cannot be empty (in glob pattern '')"),
        ("x = glob(['/abs'])", "pattern cannot be absolute (in glob pattern '/abs')"),
        ("x = glob(['a//b'])", "empty segment not permitted (in glob pattern 'a//b')"),
        ("x = glob(['../a'])", "segment '..' not permitted (in glob pattern '../a')"),
        ("x = glob(['a**'])", "recursive wildcard must be its own segment (in glob pattern 'a**')"),
        ("x = glob(['*.zzz'], allow_empty = False)", "glob pattern '*.zzz' didn't match anything, but allow_empty is set to False (the default value of allow_empty can be set with --incompatible_disallow_empty_glob)."),
        ("x = glob(['BUILD'], exclude = ['BUILD'], allow_empty = False)", "all files in the glob have been excluded, but allow_empty is set to False"),
        ("x = glob(['a'], allow_empty = 1)", "expected boolean for argument `allow_empty`, got `1`"),
        ("x = glob(['a'], exclude_directories = True)", "parameter 'exclude_directories' got value of type 'bool', want 'int'"),
        ("x = glob('a')", "expected value of type 'list(string)' for 'glob' argument, but got \"a\" (string)"),
        ("x = glob(['a'], bogus = 1)", "glob() got unexpected keyword argument 'bogus'"),
        ("x = subpackages(include = ['*'], allow_empty = False)", "subpackages pattern '*' didn't match anything, but allow_empty is set to False (the default value)"),
    ] {
        let ws = Ws::new(&[("BUILD", src)]);
        let e = errors(&ws.load("")).join("\n");
        assert!(e.contains(want), "for `{src}`: {e}");
    }
    let ws = Ws::new(&[("BUILD", "x = glob(['*.zzz'])\nfilegroup(name='a', srcs = x)")]);
    assert!(!ws.load("").contains_errors);
    // Config default (--incompatible_disallow_empty_glob).
    let ws = Ws::new(&[("BUILD", "x = glob(['*.zzz'])")]);
    let p = ws
        .loader_with(|c| c.allow_empty_glob = false)
        .load_package(&pid(""))
        .unwrap();
    assert!(errors(&p)[0].contains("didn't match anything"));
}

#[test]
fn native_introspection_functions() {
    let ws = Ws::new(&[(
        "pkg/BUILD",
        r#"
filegroup(name = "a", srcs = ["x", ":y", "//other:z"], tags = ["t"])
n = package_name()
r = repository_name()
rn = repo_name()
lbl = package_relative_label(":foo")
rule_a = existing_rule("a")
rules = existing_rules()
missing = existing_rule("nope")
genrule(name = "info", outs = [n + "_out", rn + "r.txt"], cmd = r + " " + lbl.name + " " + rule_a["kind"] + " " + str(len(rules)) + " " + str(missing) + " " + str(rule_a["srcs"]) + str(rule_a["tags"]))
"#,
    )]);
    let p = ws.load("pkg");
    assert!(!p.contains_errors, "{:?}", errors(&p));
    let g = rule(&p, "info");
    assert_eq!(g.outs, vec!["pkg_out", "r.txt"]);
    assert_eq!(
        g.cmd.as_deref(),
        Some("@ foo filegroup 1 None (\":x\", \":y\", \"//other:z\")(\"t\",)")
    );
}

#[test]
fn exports_files_and_visibility() {
    let ws = Ws::new(&[(
        "BUILD",
        "exports_files(['a.txt', 'b.txt'], visibility = ['//x:__pkg__'])\nexports_files(['c.txt'])\nexports_files(['b.txt'], visibility=['//y:__pkg__'])",
    )]);
    let p = ws.load("");
    assert!(errors(&p)[0].contains("visibility for exported file 'b.txt' declared twice"));
    let ws = Ws::new(&[("BUILD", "exports_files(['a.txt'], visibility = ['//x:__pkg__'])\nexports_files(['c.txt'])")]);
    let p = ws.load("");
    assert!(!p.contains_errors);
    assert_eq!(p.exported_files["a.txt"], vec![Label::parse_canonical("//x:__pkg__").unwrap()]);
    assert_eq!(p.exported_files["c.txt"], vec![Label::parse_canonical("//visibility:public").unwrap()]);
    assert!(matches!(p.targets["a.txt"], Target::InputFile { .. }));
    let ws = Ws::new(&[("BUILD", "exports_files(['bad//name'])")]);
    assert!(errors(&ws.load("")).len() == 1);
}

#[test]
fn package_group_declaration() {
    let ws = Ws::new(&[(
        "BUILD",
        "package_group(name='g', packages=['//a/...', '-//a/b', 'public'], includes=[':h', '//o:p'])\npackage_group(name='h')",
    )]);
    let p = ws.load("");
    assert!(!p.contains_errors, "{:?}", errors(&p));
    match &p.targets["g"] {
        Target::PackageGroup { packages, .. } => {
            assert_eq!(packages, &vec!["//a/...", "-//a/b", "public"])
        }
        t => panic!("{t:?}"),
    }
    assert_eq!(
        p.group_includes["g"],
        vec![Label::parse_canonical("//:h").unwrap(), Label::parse_canonical("//o:p").unwrap()]
    );
    for (src, want) in [
        ("package_group(name='g', packages=['foo'])", "invalid package name 'foo': must start with '//', '@', or be 'public' or 'private'"),
        ("package_group(name='g', packages=['-private'])", "Cannot negate \"private\" package specification"),
        ("package_group()", "package_group() missing 1 required named argument: name"),
        ("filegroup(name='g')\npackage_group(name='g')", "package group 'g' conflicts with existing filegroup rule"),
    ] {
        let ws = Ws::new(&[("BUILD", src)]);
        let e = errors(&ws.load("")).join("\n");
        assert!(e.contains(want), "for `{src}`: {e}");
    }
}

#[test]
fn environment_group_declaration() {
    let ws = Ws::new(&[(
        "BUILD",
        "environment(name='a')\nenvironment(name='b')\nenvironment_group(name='grp', environments=[':a', ':b'], defaults=[':a'])",
    )]);
    let p = ws.load("");
    assert!(!p.contains_errors, "{:?}", errors(&p));
    assert_eq!(p.environment_groups["grp"].0.len(), 2);
    assert_eq!(p.environment_groups["grp"].1.len(), 1);
    let ws = Ws::new(&[("BUILD", "environment_group(name='grp', environments=[':a'], defaults=[':z'])")]);
    assert!(errors(&ws.load(""))[0].contains("default environment '//:z' is not a member of environment group 'grp'"));
}

#[test]
fn bzl_load_macros_helpers_and_constants() {
    let ws = Ws::new(&[
        (
            "BUILD",
            r#"
load("//lib:defs.bzl", "my_lib", "VERSION", mylib2 = "my_lib")
load(":local.bzl", "local_rule")
my_lib(name = "x", srcs = ["s.txt"])
mylib2(name = "y")
local_rule(name = "z" + VERSION)
"#,
        ),
        ("s.txt", ""),
        ("lib/BUILD", ""),
        (
            "lib/defs.bzl",
            r#"
load(":helpers.bzl", "mk")
VERSION = "1"
_private = "hidden"
def my_lib(name, srcs = []):
    # helper from the defining module, a transitive load and native.*
    native.filegroup(name = mk(name), srcs = srcs)
    native.genrule(name = name, srcs = [mk(name)], outs = [name + ".out"], cmd = _private)
"#,
        ),
        ("lib/helpers.bzl", "def mk(n):\n    return n + '_files'\n"),
        ("local.bzl", "def local_rule(name):\n    native.filegroup(name = name)\n"),
    ]);
    let p = ws.load("");
    assert!(!p.contains_errors, "{:?}", errors(&p));
    for n in ["x", "x_files", "y", "y_files", "z1"] {
        assert!(p.targets.contains_key(n), "{n} missing: {:?}", p.targets.keys());
    }
    assert_eq!(rule(&p, "x").cmd.as_deref(), Some("hidden"));
    assert_eq!(rule(&p, "x_files").srcs, vec!["//:s.txt"]);
    assert_eq!(p.loads.len(), 2);
}

#[test]
fn bzl_load_errors() {
    // missing file
    let ws = Ws::new(&[("BUILD", "load(':nope.bzl', 'x')")]);
    let e = load_errors(&ws, "").join("\n");
    assert!(e.contains("cannot load '//:nope.bzl': no such file"), "{e}");
    // missing symbol (+ suggestion)
    let ws = Ws::new(&[
        ("BUILD", "load(':a.bzl', 'wrong')"),
        ("a.bzl", "right = 1\n"),
    ]);
    let e = load_errors(&ws, "").join("\n");
    assert!(e.contains("file '//:a.bzl' does not contain symbol 'wrong'"), "{e}");
    // starlark error inside bzl surfaces with location
    let ws = Ws::new(&[
        ("BUILD", "load(':a.bzl', 'x')"),
        ("a.bzl", "x = undefined_thing\n"),
    ]);
    let e = load_errors(&ws, "").join("\n");
    assert!(e.contains("ERROR: /ws/a.bzl:1:5: name 'undefined_thing' is not defined"), "{e}");
    assert!(e.contains("compilation of module 'a.bzl' failed"), "{e}");
    // cycles
    let ws = Ws::new(&[
        ("BUILD", "load(':a.bzl', 'x')"),
        ("a.bzl", "load(':b.bzl', 'y')\nx = 1\n"),
        ("b.bzl", "load(':a.bzl', 'x')\ny = 1\n"),
    ]);
    let e = load_errors(&ws, "").join("\n");
    assert!(e.contains("Starlark load cycle detected: //:a.bzl -> //:b.bzl -> //:a.bzl"), "{e}");
    // external repo that is not defined: surfaces, no scraper fallback
    let ws = Ws::new(&[("BUILD", "load('@rules_foo//foo:defs.bzl', 'foo_library')\nfoo_library(name='x')")]);
    let e = load_errors(&ws, "").join("\n");
    assert!(e.contains("Unable to find package for @@rules_foo//foo:defs.bzl: The repository '@@"), "{e}");
    assert!(ws.try_load("").is_err());
    // bzl without a package
    let ws = Ws::new(&[("BUILD", "load('//nopkg:a.bzl', 'x')"), ("nopkg/a.bzl", "x=1")]);
    let e = load_errors(&ws, "").join("\n");
    assert!(e.contains("Every .bzl file must have a corresponding package, but '//nopkg:a.bzl' does not have one."), "{e}");
    // load crossing a package boundary
    let ws = Ws::new(&[
        ("BUILD", "load('//:sub/a.bzl', 'x')"),
        ("sub/BUILD", ""),
        ("sub/a.bzl", "x=1"),
    ]);
    let e = load_errors(&ws, "").join("\n");
    assert!(e.contains("Label '//:sub/a.bzl' is invalid because 'sub' is a subpackage; perhaps you meant to put the colon here: '//sub:a.bzl'?"), "{e}");
    // a relative label without ':' is a label in the loading file's package (protobuf's BUILD
    // files `load("test_plugin_injection.bzl", ...)`); an empty or malformed one is invalid.
    let ws = Ws::new(&[("BUILD", "load('a.bzl', 'x')\nfilegroup(name = 'f' + str(x))"), ("a.bzl", "x=1")]);
    assert!(ws.load("").targets.contains_key("f1"), "{:?}", load_errors(&ws, ""));
    let ws = Ws::new(&[("BUILD", "load('', 'x')"), ("a.bzl", "x=1")]);
    assert!(load_errors(&ws, "")[0].contains("is not a valid label"));
}

#[test]
fn bzl_function_defined_in_one_module_uses_own_globals() {
    // A name defined in the BUILD file must NOT leak into the bzl, and vice versa.
    let ws = Ws::new(&[
        ("BUILD", "load(':a.bzl', 'f')\nVALUE = 'build'\nf(name = 'n')"),
        ("a.bzl", "def f(name):\n    native.filegroup(name = name + VALUE)\n"),
    ]);
    let e = load_errors(&ws, "").join("\n");
    assert!(e.contains("name 'VALUE' is not defined"), "{e}");
}

#[test]
fn starlark_rules_are_typed() {
    let ws = Ws::new(&[
        (
            "BUILD",
            r#"
load(":r.bzl", "my_rule")
my_rule(name = "a", srcs = ["f.txt"], opt = "yes", count = 3, out = "a.o")
my_rule(name = "b", srcs = [], mode = "x")
"#,
        ),
        ("f.txt", ""),
        (
            "r.bzl",
            r#"
def _impl(ctx):
    return [DefaultInfo()]
my_rule = rule(
    implementation = _impl,
    attrs = {
        "srcs": attr.label_list(allow_files = True, mandatory = True),
        "opt": attr.string(default = "no"),
        "count": attr.int(default = 1),
        "mode": attr.string(values = ["a", "b"], default = "a"),
        "out": attr.output(),
        "_hidden": attr.label(default = Label("//:hidden")),
    },
    outputs = {"log": "%{name}.log"},
)
"#,
        ),
    ]);
    let p = ws.load("");
    // rule b has invalid enum value `x` => fatal error at b, but a is fine.
    let e = errors(&p).join("\n");
    assert!(e.contains("invalid value in 'mode' attribute: has to be one of 'a', 'b' instead of 'x'"), "{e}");
    let a = rule(&p, "a");
    assert_eq!(a.rule_class, "my_rule");
    assert_eq!(a.attr_or_default("opt"), Some(AttrValue::Str("yes".into())));
    assert_eq!(a.attr_or_default("count"), Some(AttrValue::Int(3)));
    assert_eq!(a.attr_or_default("mode"), Some(AttrValue::Str("a".into())));
    assert!(matches!(p.targets["a.o"], Target::OutputFile { .. }));
    assert!(matches!(p.targets["a.log"], Target::OutputFile { .. }));
    assert_eq!(
        a.attr_or_default("_hidden"),
        Some(AttrValue::Label(Some(Label::parse_canonical("//:hidden").unwrap()))),
        "private attrs take their schema default"
    );
}

#[test]
fn starlark_rule_errors() {
    let bzl = "def _i(ctx): pass\nr = rule(implementation=_i, attrs={'x': attr.int(mandatory=True), 's': attr.string()})\n";
    for (call, want) in [
        ("r(name='a')", "missing value for mandatory attribute 'x' in 'r' rule"),
        ("r(name='a', x='no')", "expected value of type 'int' for attribute 'x' in 'r' rule, but got \"no\" (string)"),
        ("r(name='a', x=1, s=2)", "expected value of type 'string' for attribute 's' in 'r' rule, but got 2 (int)"),
        ("r(name='a', x=1, nope=2)", "no such attribute 'nope' in 'r' rule"),
        ("r(name='a', x=1, _x=2)", "no such attribute '_x' in 'r' rule"),
    ] {
        let ws = Ws::new(&[("BUILD", &format!("load(':r.bzl', 'r')\n{call}")), ("r.bzl", bzl)]);
        let e = load_errors(&ws, "").join("\n");
        assert!(e.contains(want), "for `{call}`: {e}");
    }
    // rule() misuse
    let ws = Ws::new(&[("BUILD", "load(':r.bzl', 'r')"), ("r.bzl", "def _i(ctx): pass\nr = rule(implementation=_i, attrs={'x': 1})")]);
    assert!(load_errors(&ws, "")[0].contains("want Attribute"));
    let ws = Ws::new(&[("BUILD", "load(':r.bzl', 'r')"), ("r.bzl", "r = rule()")]);
    assert!(load_errors(&ws, "")[0].contains("rule() missing 1 required positional argument: implementation"));
    // calling a rule during load
    let ws = Ws::new(&[
        ("BUILD", "load(':c.bzl', 'x')"),
        ("r.bzl", "def _i(ctx): pass\nr = rule(implementation=_i)"),
        ("c.bzl", "load(':r.bzl', 'r')\nr(name='x')\nx = 1"),
    ]);
    assert!(load_errors(&ws, "").join("\n").contains("can only be used while evaluating a BUILD file"));
    // an unexported rule
    let ws = Ws::new(&[("BUILD", "load(':r.bzl', 'mk')\nmk()(name='x')"), ("r.bzl", "def _i(ctx): pass\ndef mk():\n    return rule(implementation=_i)")]);
    assert!(load_errors(&ws, "").join("\n").contains("Invalid rule class hasn't been exported by a bzl file"));
}

#[test]
fn symbolic_macros() {
    let ws = Ws::new(&[
        (
            "BUILD",
            r#"
load(":m.bzl", "my_macro")
my_macro(name = "foo", srcs = ["a"], visibility = ["//vis:__pkg__"])
filegroup(name = "plain")
"#,
        ),
        (
            "m.bzl",
            r#"
def _impl(name, srcs, visibility, **kwargs):
    native.filegroup(name = name, srcs = srcs, visibility = visibility)
    native.genrule(name = name + "_gen", outs = [name + "_gen.txt"], cmd = "x", visibility = visibility)
    native.filegroup(name = "stray")
my_macro = macro(
    implementation = _impl,
    attrs = {"srcs": attr.label_list(mandatory = True)},
)
"#,
        ),
    ]);
    let p = ws.load("");
    assert!(!p.contains_errors, "{:?}", errors(&p));
    let foo = rule(&p, "foo");
    assert_eq!(foo.extra.macro_instance.as_deref(), Some("foo"));
    assert_eq!(foo.extra.generator_name.as_deref(), Some("foo"));
    assert_eq!(foo.srcs, vec!["//:a"]);
    assert!(p.targets.contains_key("foo_gen"));
    assert!(p.targets.contains_key("foo_gen.txt"));
    // Macro namespace violations are recorded (enforced in analysis) but are not load errors.
    assert_eq!(p.macro_namespace_violating_targets.get("stray").map(|s| s.as_str()), Some("foo"));
    assert!(!p.macro_namespace_violating_targets.contains_key("foo_gen"));
    assert!(rule(&p, "plain").extra.macro_instance.is_none());
    // Visibility of macro-created targets includes the defining package.
    assert!(foo.extra.effective_visibility.iter().any(|l| l.canonical_form() == "//:__pkg__"));
}

#[test]
fn symbolic_macro_errors() {
    let bzl = "def _i(name, **kw):\n    native.filegroup(name = name)\nm = macro(implementation=_i, attrs={'x': attr.int(mandatory=True)})\n";
    for (call, want) in [
        ("m(name='a')", "missing value for mandatory attribute 'x' in 'm' macro"),
        ("m(name='a', x='s')", "expected value of type 'int' for attribute 'x' in 'm' macro, but got \"s\" (string)"),
        ("m(name='a', x=1, y=1)", "no such attribute 'y' in 'm' macro"),
        ("m(x=1)", "missing value for mandatory attribute 'name' in 'm' macro"),
        ("filegroup(name='a')\nm(name='a', x=1)", "macro 'a' conflicts with an existing target."),
        ("m(name='a', x=1)\nm(name='a', x=1)", "macro 'a' conflicts with an existing macro"),
        ("m('a')", "m() accepts no positional arguments"),
    ] {
        let ws = Ws::new(&[("BUILD", &format!("load(':m.bzl', 'm')\n{call}")), ("m.bzl", bzl)]);
        let e = load_errors(&ws, "").join("\n");
        assert!(e.contains(want), "for `{call}`: {e}");
    }
    let ws = Ws::new(&[
        ("BUILD", "load(':c.bzl', 'y')"),
        ("m.bzl", bzl),
        ("c.bzl", "load(':m.bzl', 'm')\nm(name='x', x=1)\ny = 1"),
    ]);
    assert!(load_errors(&ws, "").join("\n").contains("Cannot instantiate a macro when loading a .bzl file."));
}

#[test]
fn legacy_macro_function() {
    let ws = Ws::new(&[
        ("BUILD", "load(':m.bzl', 'mk')\nmk('a')\nmk('b')"),
        ("m.bzl", "def mk(n):\n    native.filegroup(name = n)\n    native.genrule(name = n + '_g', srcs = [':' + n], outs = [n + '.o'], cmd = '')\n"),
    ]);
    let p = ws.load("");
    assert!(!p.contains_errors, "{:?}", errors(&p));
    assert_eq!(p.rules().count(), 4);
}

#[test]
fn input_file_and_subpackage_checks() {
    let ws = Ws::new(&[
        ("BUILD", "filegroup(name='a', srcs=['real.txt', 'missing.txt', 'sub/x.txt'])"),
        ("real.txt", ""),
        ("sub/BUILD", ""),
        ("sub/x.txt", ""),
    ]);
    let p = ws.load("");
    let e = errors(&p).join("\n");
    assert!(e.contains("Label '//:sub/x.txt' is invalid because 'sub' is a subpackage; perhaps you meant to put the colon here: '//sub:x.txt'?"), "{e}");
    assert!(p.targets.contains_key("real.txt"));
    assert!(!p.targets.contains_key("missing.txt"), "nonexistent files do not become targets");
}

#[test]
fn select_attributes() {
    let ws = Ws::new(&[(
        "BUILD",
        r#"
filegroup(name = "a", srcs = select({":cond": ["x"], "//conditions:default": ["y"]}) + ["z"])
genrule(name = "g", outs = select({"//conditions:default": ["o"]}), cmd = "")
"#,
    )]);
    let p = ws.load("");
    // outs is nonconfigurable
    assert!(errors(&p).join("\n").contains("attribute \"outs\" is not configurable"), "{:?}", errors(&p));
    let a = rule(&p, "a");
    assert!(a.extra.attr_values["srcs"].is_select());
    assert_eq!(a.srcs, vec!["//:y", "//:z"]);
    let labels: Vec<String> = a.extra.attr_values["srcs"].labels().iter().map(|l| l.canonical_form()).collect();
    let mut sorted = labels.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["//:cond", "//:x", "//:y", "//:z"]);
}

#[test]
fn package_lookup_rules() {
    let ws = Ws::new(&[
        ("dir_build/BUILD/keep", ""),   // a *directory* named BUILD
        ("both/BUILD", "filegroup(name='from_build')"),
        ("both/BUILD.bazel", "filegroup(name='from_bazel')"),
        ("only_build/BUILD", "filegroup(name='x')"),
        ("ignored/BUILD", "filegroup(name='x')"),
        ("ignored/deep/BUILD", "filegroup(name='x')"),
        ("deleted/BUILD", "filegroup(name='x')"),
        (".bazelignore", "# comment\n\nignored\n"),
    ]);
    // directory named BUILD is not a BUILD file
    let e = ws.try_load("dir_build").unwrap_err();
    assert!(e.message.starts_with("no such package 'dir_build': BUILD file not found in any of the following directories. Add a BUILD file to a directory to mark it as a package.\n - dir_build"), "{}", e.message);
    // BUILD.bazel wins over BUILD
    let p = ws.load("both");
    assert!(p.targets.contains_key("from_bazel") && !p.targets.contains_key("from_build"));
    assert!(ws.load("only_build").targets.contains_key("x"));
    // .bazelignore (prefix match)
    for pkg in ["ignored", "ignored/deep"] {
        let e = ws.try_load(pkg).unwrap_err();
        assert!(e.message.contains("Package is considered deleted due to .bazelignore"), "{}", e.message);
    }
    // --deleted_packages
    let loader = ws.loader_with(|c| {
        c.lookup.deleted_packages.insert(pid("deleted"));
    });
    let e = loader.load_package(&pid("deleted")).unwrap_err();
    assert!(e.message.contains("Package is considered deleted due to --deleted_packages"), "{}", e.message);
    assert!(loader.load_package(&pid("only_build")).is_ok());
    // unknown repository
    let rid = PackageIdentifier::create(
        zel_cmdline::RepositoryName::create("nope").unwrap(),
        PathFragment::empty(),
    )
    .unwrap();
    let e = ws.loader().load_package(&rid).unwrap_err();
    // Bazel's wording for an unresolvable repo (RepositoryMapping.getOrCreate / PackageLookupFunction).
    assert!(e.message.contains("The repository '@@[unknown repo 'nope' requested from @@]' could not be resolved"), "{}", e.message);
    // lookup API
    let l = ws.loader();
    let cfg = &l.config().lookup;
    assert!(matches!(cfg.lookup(&pid("both")), PackageLookup::Found { .. }));
}

#[test]
fn repo_bazel_ignore_directories_and_defaults() {
    let ws = Ws::new(&[
        ("REPO.bazel", "ignore_directories(['**/node_modules'])\nrepo(default_visibility = ['//visibility:public'])\n"),
        ("a/BUILD", "filegroup(name='x')"),
        ("a/node_modules/BUILD", "filegroup(name='x')"),
        ("node_modules/BUILD", ""),
    ]);
    let p = ws.load("a");
    assert_eq!(rule(&p, "x").visibility, vec!["//visibility:public"]);
    assert!(ws
        .try_load("a/node_modules")
        .unwrap_err()
        .message
        .contains("ignore_directories() in REPO.bazel"));
    assert!(ws.try_load("node_modules").is_err());
}

#[test]
fn prelude_is_injected() {
    let ws = Ws::new(&[
        ("tools/build_rules/prelude_bazel", "def helper(n):\n    native.filegroup(name = n)\nPRELUDE_CONST = 'c'\n"),
        ("tools/build_rules/BUILD", ""),
        ("BUILD", "helper('from_prelude')\nfilegroup(name = PRELUDE_CONST)"),
    ]);
    let p = ws.load("");
    assert!(!p.contains_errors, "{:?}", errors(&p));
    assert!(p.targets.contains_key("from_prelude") && p.targets.contains_key("c"));
}

#[test]
fn evaluate_build_file_wrapper() {
    let ws = Ws::new(&[("src/BUILD", "filegroup(name='a')")]);
    let pkg = Package::evaluate_build_file(
        pid("src"),
        &ws.root,
        ws.root.get_relative_str("src/BUILD"),
        "filegroup(name='a')\nfilegroup(name='b')",
        ws.fs.clone(),
    )
    .unwrap();
    assert_eq!(pkg.rules().map(|r| r.label.name().to_string()).collect::<Vec<_>>(), vec!["a", "b"]);
    let err = Package::evaluate_build_file(
        pid("src"),
        &ws.root,
        ws.root.get_relative_str("src/BUILD"),
        "bogus()",
        ws.fs.clone(),
    )
    .unwrap_err();
    assert!(err.contains("name 'bogus' is not defined"));
}

// ---------------------------------------------------------------------------------------------
// Target pattern resolution
// ---------------------------------------------------------------------------------------------

fn resolver(ws: &Ws, rel: &str) -> PatternResolver {
    PatternResolver::new(ws.loader(), PathFragment::create(rel))
}

fn labels(o: &ResolveOutcome) -> Vec<String> {
    o.targets.iter().map(|t| t.label().canonical_form()).collect()
}

fn tree() -> Ws {
    Ws::new(&[
        ("BUILD", "filegroup(name = 'root')\nexports_files(['top.txt'])"),
        ("top.txt", ""),
        ("a/BUILD", "filegroup(name = 'a1')\nfilegroup(name = 'a2', srcs = ['f.txt'])"),
        ("a/f.txt", ""),
        ("a/b/BUILD", "filegroup(name = 'b1')\ngenrule(name = 'gen', outs = ['gen.out'], cmd = 'x')"),
        ("a/b/c/BUILD", "filegroup(name = 'c1')"),
        ("ignored/BUILD", "filegroup(name = 'bad')"),
        ("a/ignored2/BUILD", "filegroup(name = 'bad2')"),
        ("noBuild/x.txt", ""),
        (".bazelignore", "ignored\na/ignored2\n"),
    ])
}

#[test]
fn resolve_single_and_package_wildcards() {
    let ws = tree();
    let r = resolver(&ws, "");
    let o = r.resolve_patterns(&["//a:a1".into(), "//a:all".into()]);
    assert!(o.errors.is_empty(), "{:?}", o.errors);
    assert_eq!(labels(&o), vec!["//a:a1", "//a:a2"]);
    // `:*` includes files (implicit inputs) and the package's BUILD file, which Bazel registers as an
    // input-file target of every package (loader.rs finish: `//pkg:BUILD`).
    let o = r.resolve_patterns(&["//a:*".into()]);
    assert_eq!(labels(&o), vec!["//a:BUILD", "//a:a1", "//a:a2", "//a:f.txt"]);
    let o = r.resolve_patterns(&["//a/b:all-targets".into()]);
    assert_eq!(labels(&o), vec!["//a/b:BUILD", "//a/b:b1", "//a/b:gen", "//a/b:gen.out"]);
    // shorthand
    let o = r.resolve_patterns(&["//a/b/c".into()]);
    assert!(o.targets.is_empty());
    assert_eq!(o.errors.len(), 1, "no target named `c` in //a/b/c");
    assert!(o.errors[0].message.starts_with("no such target '//a/b/c:c': target 'c' not declared in package 'a/b/c' defined by /ws/a/b/c/BUILD"), "{}", o.errors[0].message);
}

#[test]
fn resolve_recursive_patterns_respect_ignores() {
    let ws = tree();
    let r = resolver(&ws, "");
    let o = r.resolve_patterns(&["//...".into()]);
    assert!(o.errors.is_empty(), "{:?}", o.errors);
    let l = labels(&o);
    assert!(l.contains(&"//:root".to_string()));
    assert!(l.contains(&"//a/b/c:c1".to_string()));
    assert!(!l.iter().any(|x| x.contains("bad")), "{l:?}");
    // rules only: no files, no generated outputs
    assert!(!l.contains(&"//:top.txt".to_string()) && !l.contains(&"//a/b:gen.out".to_string()));
    let o = r.resolve_patterns(&["//a/...".into()]);
    assert_eq!(labels(&o), vec!["//a:a1", "//a:a2", "//a/b:b1", "//a/b:gen", "//a/b/c:c1"]);
    let o = r.resolve_patterns(&["//a/...:*".into()]);
    assert!(labels(&o).contains(&"//a/b:gen.out".to_string()));
    // no packages below a directory without BUILD files
    let o = r.resolve_patterns(&["//noBuild/...".into()]);
    assert_eq!(o.errors[0].message, "no targets found beneath 'noBuild'");
    let o = r.resolve_patterns(&["//doesnotexist/...".into()]);
    assert_eq!(o.errors[0].message, "no targets found beneath 'doesnotexist'");
    // ignored package asked for directly
    let o = r.resolve_patterns(&["//ignored:bad".into()]);
    assert!(o.errors[0].message.contains("Package is considered deleted due to .bazelignore"), "{:?}", o.errors);
}

#[test]
fn resolve_negative_patterns_subtract_in_order() {
    let ws = tree();
    let r = resolver(&ws, "");
    let o = r.resolve_patterns(&["//a/...".into(), "-//a/b/...".into()]);
    assert_eq!(labels(&o), vec!["//a:a1", "//a:a2"]);
    // A later positive pattern adds back what an earlier negative removed.
    let o = r.resolve_patterns(&["//a/...".into(), "-//a/b/...".into(), "//a/b:b1".into()]);
    assert_eq!(labels(&o), vec!["//a:a1", "//a:a2", "//a/b:b1"]);
    // Leading negative pattern has nothing to subtract from.
    let o = r.resolve_patterns(&["-//a/b/...".into(), "//a/b:b1".into()]);
    assert_eq!(labels(&o), vec!["//a/b:b1"]);
    let o = r.resolve_patterns(&["//a:a1".into(), "-//a:a1".into()]);
    assert!(o.targets.is_empty() && o.errors.is_empty());
}

#[test]
fn resolve_relative_to_client_directory() {
    let ws = tree();
    let r = resolver(&ws, "a");
    let o = r.resolve_patterns(&[":a1".into(), "b:b1".into()]);
    assert!(o.errors.is_empty(), "{:?}", o.errors);
    assert_eq!(labels(&o), vec!["//a:a1", "//a/b:b1"]);
    let o = r.resolve_patterns(&["...".into()]);
    assert_eq!(labels(&o), vec!["//a:a1", "//a:a2", "//a/b:b1", "//a/b:gen", "//a/b/c:c1"]);
    let o = r.resolve_patterns(&[":all".into()]);
    assert_eq!(labels(&o), vec!["//a:a1", "//a:a2"]);
    let o = r.resolve_patterns(&["b/..."  .into(), "-b/c/...".into()]);
    assert_eq!(labels(&o), vec!["//a/b:b1", "//a/b:gen"]);
    // Absolute patterns ignore the directory.
    let o = r.resolve_patterns(&["//:root".into()]);
    assert_eq!(labels(&o), vec!["//:root"]);
}

#[test]
fn resolve_path_as_target() {
    let ws = tree();
    let r = resolver(&ws, "");
    // File in package `a` (exists, referenced by a2).
    let o = r.resolve_patterns(&["a/f.txt".into()]);
    assert_eq!(labels(&o), vec!["//a:f.txt"], "{:?}", o.errors);
    // A directory that is a package itself resolves to the target named like the directory.
    let o = r.resolve_patterns(&["a".into()]);
    assert!(o.errors[0].message.starts_with("no such target '//a:a': target 'a' not declared in package 'a'"), "{:?}", o.errors);
    // From within a/: `f.txt` relative to the client directory.
    let r2 = resolver(&ws, "a");
    assert_eq!(labels(&r2.resolve_patterns(&["f.txt".into()])), vec!["//a:f.txt"]);
    // No enclosing package at all (root has a BUILD here, so use a workspace without one).
    let ws2 = Ws::new(&[("x/y.txt", "")]);
    let o = resolver(&ws2, "").resolve_patterns(&["x/y.txt".into()]);
    assert_eq!(o.errors[0].message, "couldn't determine target from filename 'x/y.txt'");
}

#[test]
fn resolve_error_messages() {
    let ws = tree();
    let r = resolver(&ws, "");
    let o = r.resolve_patterns(&["//:nonexistent".into()]);
    assert_eq!(
        o.errors[0].message,
        "no such target '//:nonexistent': target 'nonexistent' not declared in package '' defined by /ws/BUILD"
    );
    let o = r.resolve_patterns(&["//nope:x".into()]);
    assert_eq!(
        o.errors[0].message,
        "no such package 'nope': BUILD file not found in any of the following directories. Add a BUILD file to a directory to mark it as a package.\n - nope"
    );
    // A source file that exists but isn't exported.
    let ws3 = Ws::new(&[("BUILD", "filegroup(name='x')"), ("secret.txt", ""), ("dir/f", "")]);
    let o = resolver(&ws3, "").resolve_patterns(&["//:secret.txt".into(), "//:dir".into()]);
    assert!(o.errors[0].message.ends_with("; however, a source file of this name exists.  (Perhaps add 'exports_files([\"secret.txt\"])' to BUILD?)"), "{}", o.errors[0].message);
    assert!(o.errors[1].message.ends_with("; however, a source directory of this name exists.  (Perhaps add 'exports_files([\"dir\"])' to BUILD, or define a filegroup?)"), "{}", o.errors[1].message);
    // Typos get suggestions.
    let ws4 = Ws::new(&[("BUILD", "filegroup(name='server')\nfilegroup(name='client')")]);
    let o = resolver(&ws4, "").resolve_patterns(&["//:sever".into()]);
    assert!(o.errors[0].message.ends_with(" (did you mean server?)"), "{}", o.errors[0].message);
    // Rule consuming a source of that name.
    // (An implicit input-file target only exists for a source file that is present on disk.)
    let ws5 = Ws::new(&[("BUILD", "filegroup(name='core_lib', srcs=['foo_bar.cc'])"), ("foo_bar.cc", "")]);
    let o = resolver(&ws5, "").resolve_patterns(&["//:foo_bar".into()]);
    assert!(o.errors[0].message.ends_with(" (did you mean core_lib?)"), "{}", o.errors[0].message);
    // Syntax errors.
    let o = r.resolve_patterns(&["//a//b".into()]);
    assert!(o.errors[0].message.starts_with("invalid package name 'a//b'"), "{:?}", o.errors);
    // Package with errors: reported, with the package's own events.
    let ws6 = Ws::new(&[("BUILD", "bogus()\nfilegroup(name='ok')")]);
    let r6 = resolver(&ws6, "");
    let o = r6.resolve_patterns(&["//:all".into()]);
    assert_eq!(o.errors[0].message, "error loading package '': Package '' contains errors");
    assert!(o.events.iter().any(|e| e.render().contains("name 'bogus' is not defined")));
}

#[test]
fn resolve_external_repo_patterns() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    let ext = PathFragment::create("/ob/external/rules_x+1");
    for (p, c) in [
        ("/ws/BUILD", "filegroup(name='m')"),
        ("/ob/external/rules_x+1/BUILD", "filegroup(name='root')"),
        ("/ob/external/rules_x+1/sub/BUILD", "filegroup(name='s')\nexports_files(['f'])"),
        ("/ob/external/rules_x+1/sub/f", ""),
    ] {
        let path = PathFragment::create(p);
        fs.create_directory_and_parents(&path.parent_directory().unwrap()).unwrap();
        fs.write_file(&path, c.as_bytes()).unwrap();
    }
    let mut res = SimpleRepoResolver::new(root);
    res.external.insert("rules_x+1".into(), ext);
    res.apparent.insert("rules_x".into(), zel_cmdline::RepositoryName::create("rules_x+1").unwrap());
    let loader = PackageLoader::new(LoaderConfig::new(fs, Arc::new(res)));
    let r = PatternResolver::new(loader, PathFragment::empty());
    let o = r.resolve_patterns(&["@rules_x//...".into()]);
    assert!(o.errors.is_empty(), "{:?}", o.errors);
    let l: Vec<String> = o.targets.iter().map(|t| t.label().canonical_form()).collect();
    assert_eq!(l, vec!["@@rules_x+1//:root", "@@rules_x+1//sub:s"]);
    let o = r.resolve_patterns(&["@rules_x//sub:*".into(), "@@rules_x+1//:root".into()]);
    // sub: BUILD, f, s + root
    assert_eq!(o.targets.len(), 4, "{:?}", o.errors);
    // //... in the main repo does not include the external repo.
    let o = r.resolve_patterns(&["//...".into()]);
    assert_eq!(o.targets.len(), 1);
    let o = r.resolve_patterns(&["@nonexistent//...".into()]);
    assert!(o.errors[0].message.contains("The repository '@@[unknown repo 'nonexistent' requested from @@]' could not be resolved"), "{:?}", o.errors);
}

#[test]
fn generated_rule_class_schemas() {
    let rcs = crate::rule_class::native_rule_classes();
    // Declaration order comes from real Bazel (`Rule.getAttributes()`).
    let g = &rcs["genrule"];
    let pos = |n: &str| g.attr_order.iter().position(|x| x == n).unwrap();
    assert_eq!(g.attr_order[0], "name");
    assert!(pos("visibility") < pos("srcs") && pos("srcs") < pos("tools") && pos("tools") < pos("outs"));
    assert!(g.implicit.contains_key("$genrule_setup") && g.implicit.contains_key(":action_listener"));
    assert!(g.attr("exec_tools").is_none());
    assert_eq!(g.attr("stamp").unwrap().ty, crate::attr::AttrType::Tristate);
    assert!(g.attr("tools").unwrap().tool_dep == Some(true));
    assert!(g.attr("visibility").unwrap().order_independent && g.attr("visibility").unwrap().nodep);
    assert!(!g.attr("outs").unwrap().configurable);
    assert!(g.attr("outs").unwrap().mandatory);
    // test_suite is testonly by default and has the `$implicit_tests` magic attribute.
    let ts = &rcs["test_suite"];
    assert!(ts.implicit.contains_key("$implicit_tests"));
    assert_eq!(ts.attr("testonly").unwrap().default_value(), crate::attr::AttrValue::Bool(true));
    // Starlark kinds.
    let t = crate::rule_class::starlark_rule_class("t", vec![crate::attr::Attribute::new("x", crate::attr::AttrType::String)], false, true, vec![], false);
    assert!(t.implicit.contains_key("$test_wrapper") && t.implicit.contains_key(":coverage_support"));
    assert_eq!(t.attr_order.last().unwrap(), "x");
    assert!(t.attr("size").is_some() && t.attr("env").is_none());
    let p = crate::rule_class::starlark_rule_class("p", vec![], false, false, vec![], false);
    assert!(p.implicit.is_empty() || !p.implicit.contains_key("$test_wrapper"));
    assert!(p.attr("expect_failure").is_some());
}

#[test]
fn order_independent_tags_and_implicit_tests() {
    let ws = Ws::new(&[(
        "BUILD",
        "sh_test(name='t1', srcs=['a.sh'], tags=['b', 'a'])\n\
         sh_test(name='t2', srcs=['a.sh'], tags=['manual'])\n\
         test_suite(name='all')\n\
         test_suite(name='sel', tags=['a'])\n",
    )]);
    let pkg = ws.load("");
    let rule = |n: &str| match &pkg.targets[n] {
        crate::Target::Rule(r) => r.clone(),
        _ => panic!(),
    };
    assert_eq!(
        rule("t1").extra.attr_values["tags"],
        crate::attr::AttrValue::StrList(vec!["a".into(), "b".into()])
    );
    let labels = |n: &str| -> Vec<String> {
        rule(n).extra.attr_values["$implicit_tests"].labels().iter().map(|l| l.canonical_form()).collect()
    };
    assert_eq!(labels("all"), vec!["//:t1"]);
    assert_eq!(labels("sel"), vec!["//:t1"]);
}

// ---------------------------------------------------------------------------------------------
// Syntax errors and load failures surface like Bazel (verified against Bazel 9.3.2).

#[test]
fn build_syntax_errors_are_reported_like_bazel() {
    let ws = Ws::new(&[("BUILD", "this is not valid\n")]);
    let p = ws.load("");
    assert!(p.contains_errors);
    assert_eq!(
        errors(&p),
        vec![
            "ERROR: /ws/BUILD:1:6: 'is' not supported, use '==' instead".to_string(),
            "ERROR: /ws/BUILD:1:6: syntax error at 'is': expected newline".to_string(),
        ]
    );
    // BUILD-file restrictions end loading before name resolution would report anything.
    let ws = Ws::new(&[("BUILD", "def f(): pass\nundefined_thing()\n")]);
    assert_eq!(
        errors(&ws.load("")),
        vec!["ERROR: /ws/BUILD:1:1: functions may not be defined in BUILD files. You may move the function to a .bzl file and load it.".to_string()]
    );
    // Resolver errors come in traversal order: argument checks before the callee's name.
    let ws = Ws::new(&[("BUILD", "f(a = 1, 2)\n")]);
    assert_eq!(
        errors(&ws.load("")),
        vec![
            "ERROR: /ws/BUILD:1:10: positional argument may not follow keyword argument".to_string(),
            "ERROR: /ws/BUILD:1:1: name 'f' is not defined".to_string(),
        ]
    );
}

#[test]
fn load_failures_abort_package_loading() {
    // Missing .bzl file: an exception, not a package with errors.
    let ws = Ws::new(&[("BUILD", "load(':nope.bzl', 'x')\n")]);
    let e = ws.try_load("").unwrap_err();
    assert_eq!(e.message, "error loading package '': cannot load '//:nope.bzl': no such file");
    // Malformed labels are reported at the label, then loading fails.
    let ws = Ws::new(&[("BUILD", "load('a', 'x')\n")]);
    let e = ws.try_load("").unwrap_err();
    assert_eq!(e.message, "error loading package '': malformed load statements");
    assert_eq!(
        e.events.iter().map(|ev| ev.render()).collect::<Vec<_>>(),
        vec!["ERROR: /ws/BUILD:1:6: in load statement: The label must reference a file with extension \".bzl\" or \".scl\"".to_string()]
    );
    // Resolver errors (private symbol) are reported before any load is attempted.
    let ws = Ws::new(&[("BUILD", "load('//:missing.bzl', '_p')\n")]);
    let p = ws.load("");
    assert_eq!(errors(&p), vec!["ERROR: /ws/BUILD:1:25: symbol '_p' is private and cannot be imported".to_string()]);
    // A .bzl with static errors fails to *compile*; the BUILD's load is fatal.
    let ws = Ws::new(&[("BUILD", "load(':a.bzl', 'x')\n"), ("a.bzl", "x = (\n")]);
    let e = ws.try_load("").unwrap_err();
    assert_eq!(e.message, "error loading package '': compilation of module 'a.bzl' failed");
    assert_eq!(e.events[0].render(), "ERROR: /ws/a.bzl:2:1: syntax error at 'newline': expected expression");
}

#[test]
fn missing_symbol_is_an_evaluation_error_at_the_symbol() {
    let ws = Ws::new(&[("BUILD", "load(':a.bzl', 'wrong')\n"), ("a.bzl", "right = 1\n")]);
    let p = ws.load("");
    let e = errors(&p).join("\n");
    // (The source line is only shown for files that exist on disk; this workspace is in memory.)
    assert!(
        e.contains("ERROR: Traceback (most recent call last):\n\tFile \"/ws/BUILD\", line 1, column 17, in <toplevel>\n"),
        "{e}"
    );
    assert!(e.contains("Error: file '//:a.bzl' does not contain symbol 'wrong'"), "{e}");
}

#[test]
fn package_with_errors_is_a_pattern_error_with_details() {
    let ws = Ws::new(&[("BUILD", "filegroup(name = 'a')\nthis is not valid\n")]);
    let o = resolver(&ws, "").resolve_patterns(&["//:all".into()]);
    let pe = o.errors[0].package_errors.as_ref().expect("package errors");
    assert_eq!(pe.package.package_fragment().to_string(), "");
    assert!(!pe.include_files);
    assert_eq!(pe.first_error, "'is' not supported, use '==' instead");
}


// ---- round 6A: rule classes derived from the Bazel sources -----------------------------------

#[test]
fn generated_rule_defs_contain_no_probe_fixture_constants() {
    let src = include_str!("rule_defs_gen.rs");
    for bad in ["//:dummy", "//:cs\"", "//:tt\"", "//:p_genrule", "\"true\""] {
        assert!(!src.contains(bad), "probe constant {bad} leaked into rule_defs_gen.rs");
    }
}

#[test]
fn native_rule_classes_follow_9_3_2_definitions() {
    let rcs = crate::rule_class::native_rule_classes();
    for k in ["extra_action", "action_listener", "starlark_doc_extract", "genquery", "label_flag", "label_setting", "alias"] {
        assert!(rcs.contains_key(k), "{k} missing");
    }
    // `LabelBuildSettings` (9.3.2): no `on_leave_scope`; late-bound `:alias`.
    let lf = &rcs["label_flag"];
    assert!(!lf.attr_order.contains(&"on_leave_scope".to_string()));
    assert!(lf.attr_or_implicit(":alias").unwrap().default.is_none());
    // Mandatory attributes have no default (`alias.actual`, `genquery.scope`).
    assert!(rcs["alias"].attr("actual").unwrap().default.is_none());
    assert!(rcs["alias"].attr("actual").unwrap().mandatory);
    assert!(rcs["genquery"].attr("scope").unwrap().default.is_none());
    // `NativeBuildRule`: `licenses` is nonconfigurable; the exec-configured `:action_listener` is a tool dep.
    assert!(!rcs["filegroup"].attr("licenses").unwrap().configurable);
    assert_eq!(rcs["filegroup"].attr_or_implicit(":action_listener").unwrap().tool_dep, Some(true));
    // `exemptFromConstraintChecking` rules do not have `target_compatible_with`.
    assert!(rcs["constraint_value"].attr("target_compatible_with").is_none());
}

#[test]
fn package_defaults_are_recorded_as_not_explicit() {
    let ws = Ws::new(&[("p/BUILD", "package(default_testonly = True, features = ['f'])\nfilegroup(name = 'a')\nfilegroup(name = 'b', features = ['own'])\n")]);
    let p = ws.load("p");
    let rule = |n: &str| match &p.targets[n] {
        Target::Rule(r) => r.clone(),
        _ => panic!("not a rule"),
    };
    let a = rule("a");
    assert!(a.extra.pkg_default_attrs.contains(&"testonly".to_string()));
    assert!(a.extra.pkg_default_attrs.contains(&"features".to_string()));
    let b = rule("b");
    assert!(b.extra.pkg_default_attrs.contains(&"testonly".to_string()));
    assert!(!b.extra.pkg_default_attrs.contains(&"features".to_string()));
    assert!(b.extra.own_features.is_some());
}
