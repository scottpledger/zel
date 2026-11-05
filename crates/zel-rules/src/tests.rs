//! Analysis-phase tests: real `ctx`, actions, providers, selects, native rules, visibility.

use super::*;
use std::sync::Arc;
use zel_analysis::{ActionKind, ConfiguredTarget};
use zel_packages::{LoaderConfig, PackageLoader, SimpleRepoResolver};
use zel_vfs::{FileSystem, InMemoryFileSystem, PathFragment};

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
        // on the BUILD file's first line.
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
    fn resolver(&self) -> Arc<TargetResolver> {
        self.resolver_with(BuildConfigurationKey::target("k8", "fastbuild"), AnalysisConfig::new()).0
    }
    fn resolver_with(
        &self,
        cfg: BuildConfigurationKey,
        acfg: AnalysisConfig,
    ) -> (Arc<TargetResolver>, BuildConfigurationKey) {
        let lc = LoaderConfig::new(self.fs.clone(), Arc::new(SimpleRepoResolver::new(self.root.clone())));
        (TargetResolver::new(PackageLoader::new(lc), acfg), cfg)
    }
    fn analyze(&self, label: &str) -> Result<Arc<ConfiguredTarget>, String> {
        let r = self.resolver();
        r.analyze(&Label::parse_canonical(label).unwrap(), &BuildConfigurationKey::target("k8", "fastbuild"))
    }
}

fn spawn_of(ct: &ConfiguredTarget, idx: usize) -> (String, Vec<String>, Vec<String>, Vec<String>) {
    let a = &ct.typed_actions[idx];
    let (argv, _) = a.command_line();
    (
        a.mnemonic.clone(),
        argv,
        a.inputs.iter().map(|i| i.exec_path.to_string()).collect(),
        a.outputs.iter().map(|i| i.exec_path.to_string()).collect(),
    )
}

#[test]
fn starlark_rule_runs_with_real_ctx_and_registers_write_action() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, "hello " + ctx.attr.who + "\n")
    return [DefaultInfo(files = depset([out]))]

greet = rule(implementation = _impl, attrs = {"who": attr.string(default = "world")})
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"greet\")\ngreet(name = \"g\", who = \"zel\")\n"),
    ]);
    let ct = ws.analyze("//:g").unwrap();
    assert_eq!(ct.typed_actions.len(), 1);
    let a = &ct.typed_actions[0];
    assert_eq!(a.mnemonic, "FileWrite");
    match &a.kind {
        ActionKind::FileWrite { content, is_executable } => {
            assert_eq!(content.render(), b"hello zel\n");
            assert!(!is_executable);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(a.outputs[0].exec_path.to_string(), "bazel-out/k8-fastbuild/bin/g.txt");
    assert_eq!(ct.default_info.files.len(), 1);
    assert_eq!(ct.default_info.files[0].exec_path.to_string(), "bazel-out/k8-fastbuild/bin/g.txt");
    assert_eq!(ct.actions.len(), 1);
}

#[test]
fn run_shell_and_args_with_deps_and_providers() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
MyInfo = provider(fields = ["val", "files"])

def _lib_impl(ctx):
    return [MyInfo(val = ctx.attr.val, files = depset(ctx.files.srcs))]

lib = rule(
    implementation = _lib_impl,
    attrs = {"val": attr.string(), "srcs": attr.label_list(allow_files = True)},
)

def _bin_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".out")
    args = ctx.actions.args()
    args.add("--out", out)
    args.add_all("--in", [f for d in ctx.attr.deps for f in d[MyInfo].files.to_list()])
    args.add_joined("--vals", [d[MyInfo].val for d in ctx.attr.deps], join_with = ",")
    ctx.actions.run_shell(
        outputs = [out],
        inputs = [f for d in ctx.attr.deps for f in d[MyInfo].files.to_list()],
        command = "echo hi > $1",
        arguments = [args],
        mnemonic = "MyGen",
        progress_message = "Generating %{output} for %{label}",
        env = {"A": "b"},
    )
    return [DefaultInfo(files = depset([out]))]

bin = rule(implementation = _bin_impl, attrs = {"deps": attr.label_list(providers = [MyInfo])})
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "lib", "bin")
lib(name = "a", val = "A", srcs = ["a.txt"])
lib(name = "b", val = "B", srcs = ["b.txt"])
bin(name = "x", deps = [":a", ":b"])
"#,
        ),
        ("a.txt", "a"),
        ("b.txt", "b"),
    ]);
    let ct = ws.analyze("//:x").unwrap();
    let (mn, argv, inputs, outputs) = spawn_of(&ct, 0);
    assert_eq!(mn, "MyGen");
    assert_eq!(
        argv,
        vec![
            "/bin/bash", "-c", "echo hi > $1", "",
            "--out", "bazel-out/k8-fastbuild/bin/x.out",
            "--in", "a.txt", "b.txt",
            "--vals", "A,B"
        ]
    );
    assert_eq!(inputs, vec!["a.txt", "b.txt"]);
    assert_eq!(outputs, vec!["bazel-out/k8-fastbuild/bin/x.out"]);
    let a = &ct.typed_actions[0];
    assert_eq!(a.progress_message.as_deref(), Some("Generating x.out for //:x"));
    assert_eq!(a.env["A"], "b");
}

#[test]
fn missing_provider_error_text() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
P = provider()
def _a(ctx):
    return []
a = rule(implementation = _a)
def _b(ctx):
    x = ctx.attr.dep[P]
    return []
b = rule(implementation = _b, attrs = {"dep": attr.label()})
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"a\", \"b\")\na(name = \"a\")\nb(name = \"b\", dep = \":a\")\n"),
    ]);
    let e = ws.analyze("//:b").unwrap_err();
    assert!(e.contains("<target //:a> (rule 'a') doesn't contain declared provider 'P'"), "{e}");
    assert!(e.contains("in b rule //:b"), "{e}");
}

#[test]
fn executable_rule_requires_executable() {
    let ws = Ws::new(&[
        ("defs.bzl", "def _i(ctx):\n    return []\nr = rule(implementation = _i, executable = True)\n"),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\")\n"),
    ]);
    let e = ws.analyze("//:r").unwrap_err();
    assert!(e.contains("The rule 'r' is executable. It needs to create an executable File and pass it as the 'executable' parameter to the DefaultInfo it returns."), "{e}");
}

#[test]
fn executable_rule_gets_runfiles_support_and_manifest_action() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _i(ctx):
    exe = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(exe, "echo hi\n", is_executable = True)
    data = ctx.runfiles(files = ctx.files.data)
    return [DefaultInfo(executable = exe, runfiles = data)]
r = rule(implementation = _i, executable = True, attrs = {"data": attr.label_list(allow_files = True)})
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\", data = [\"d.txt\"])\n"),
        ("d.txt", "d"),
    ]);
    let ct = ws.analyze("//:r").unwrap();
    let rs = ct.runfiles_support.as_ref().expect("runfiles support");
    assert_eq!(rs.runfiles_dir, "bazel-out/k8-fastbuild/bin/r.runfiles");
    let m = rs.runfiles.manifest_entries("_main");
    assert_eq!(m["_main/d.txt"], "d.txt");
    assert_eq!(m["_main/r"], "bazel-out/k8-fastbuild/bin/r");
    assert_eq!(ct.default_info.executable.as_ref().unwrap().exec_path.to_string(), "bazel-out/k8-fastbuild/bin/r");
    assert!(ct.typed_actions.iter().any(|a| a.mnemonic == "SourceSymlinkManifest"));
}

#[test]
fn action_conflict_is_reported() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _i(ctx):
    out = ctx.actions.declare_file("o")
    ctx.actions.write(out, "1")
    ctx.actions.write(out, "2")
    return []
r = rule(implementation = _i)
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\")\n"),
    ]);
    let e = ws.analyze("//:r").unwrap_err();
    assert!(e.contains("file 'bazel-out/k8-fastbuild/bin/o' is generated by these conflicting actions:\nLabel: //:r\nRuleClass: r\n"), "{e}");
}

#[test]
fn declare_file_outside_package_is_rejected() {
    let ws = Ws::new(&[
        ("defs.bzl", "def _i(ctx):\n    ctx.actions.declare_file('../x')\n    return []\nr = rule(implementation = _i)\n"),
        ("p/BUILD", "load(\"//:defs.bzl\", \"r\")\nr(name = \"r\")\n"),
        ("BUILD", ""),
    ]);
    let e = ws.analyze("//p:r").unwrap_err();
    assert!(e.contains("the output artifact '"), "{e}");
    assert!(e.contains("is not under package directory 'p' for target '//p:r'"), "{e}");
}

#[test]
fn symlink_expand_template_and_output_checks() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _i(ctx):
    t = ctx.file.tmpl
    out = ctx.actions.declare_file("out.txt")
    ctx.actions.expand_template(template = t, output = out, substitutions = {"%N%": ctx.label.name})
    link = ctx.actions.declare_file("link")
    ctx.actions.symlink(output = link, target_file = out)
    return [DefaultInfo(files = depset([link]))]
r = rule(implementation = _i, attrs = {"tmpl": attr.label(allow_single_file = True)})
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\", tmpl = \"t.in\")\n"),
        ("t.in", "name=%N%"),
    ]);
    let ct = ws.analyze("//:r").unwrap();
    assert_eq!(ct.typed_actions.len(), 2);
    assert_eq!(ct.typed_actions[0].mnemonic, "TemplateExpand");
    assert_eq!(ct.typed_actions[1].mnemonic, "Symlink");
    assert_eq!(ct.typed_actions[1].inputs[0].exec_path.to_string(), "bazel-out/k8-fastbuild/bin/out.txt");
}

#[test]
fn symlink_errors_match_bazel() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _i(ctx):
    l = ctx.actions.declare_file("l")
    ctx.actions.symlink(output = l)
    return []
r = rule(implementation = _i)
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\")\n"),
    ]);
    let e = ws.analyze("//:r").unwrap_err();
    assert!(e.contains("Exactly one of \"target_file\" or \"target_path\" is required"), "{e}");
}

#[test]
fn expand_location_and_make_variables_in_ctx() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _i(ctx):
    out = ctx.actions.declare_file("o.txt")
    cmd = ctx.expand_location("cat $(location :src) > $(rootpath :src)", targets = ctx.attr.srcs)
    cmd = ctx.expand_make_variables("cmd", cmd + " $(COMPILATION_MODE) $$ $(BINDIR)", {})
    ctx.actions.write(out, cmd)
    return [DefaultInfo(files = depset([out]))]
r = rule(implementation = _i, attrs = {"srcs": attr.label_list(allow_files = True)})
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\", srcs = [\":src\"])\nfilegroup(name = \"src\", srcs = [\"s.txt\"])\n"),
        ("s.txt", "s"),
    ]);
    let ct = ws.analyze("//:r").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
    assert_eq!(
        String::from_utf8(content.render()).unwrap(),
        "cat s.txt > s.txt fastbuild $ bazel-out/k8-fastbuild/bin"
    );
}

#[test]
fn select_resolution_with_config_setting_and_error_text() {
    let ws = Ws::new(&[
        (
            "BUILD",
            r#"
config_setting(name = "opt", values = {"compilation_mode": "opt"})
config_setting(name = "fast", values = {"compilation_mode": "fastbuild"})
filegroup(name = "fg", srcs = select({":opt": ["o.txt"], ":fast": ["f.txt"]}))
filegroup(name = "nomatch", srcs = select({":opt": ["o.txt"]}))
"#,
        ),
        ("o.txt", ""),
        ("f.txt", ""),
    ]);
    let ct = ws.analyze("//:fg").unwrap();
    assert_eq!(ct.default_info.files[0].exec_path.to_string(), "f.txt");
    let e = ws.analyze("//:nomatch").unwrap_err();
    assert!(e.contains("configurable attribute \"srcs\" in //:nomatch doesn't match this configuration. Would a default condition help?\n\nConditions checked:\n //:opt\n\nTo see a condition's definition, run: bazel query --output=build <condition label>."), "{e}");
    // opt config picks the other branch.
    let (r, _) = ws.resolver_with(BuildConfigurationKey::target("k8", "opt"), AnalysisConfig::new());
    let ct = r.analyze(&Label::parse_canonical("//:fg").unwrap(), &BuildConfigurationKey::target("k8", "opt")).unwrap();
    assert_eq!(ct.default_info.files[0].exec_path.to_string(), "o.txt");
}

#[test]
fn genrule_command_line_and_expansion() {
    let ws = Ws::new(&[
        (
            "p/BUILD",
            r#"
genrule(
    name = "g",
    srcs = ["in.txt"],
    outs = ["out.txt"],
    cmd = "cat $(location in.txt) $< > $@; echo $(RULEDIR) $(@D)",
)
"#,
        ),
        ("p/in.txt", "x"),
    ]);
    let ct = ws.analyze("//p:g").unwrap();
    let (mn, argv, inputs, outputs) = spawn_of(&ct, 0);
    assert_eq!(mn, "Genrule");
    assert_eq!(argv[0], "/bin/bash");
    assert_eq!(argv[1], "-c");
    assert_eq!(
        argv[2],
        "source external/bazel_tools/tools/genrule/genrule-setup.sh; cat p/in.txt p/in.txt > bazel-out/k8-fastbuild/bin/p/out.txt; echo bazel-out/k8-fastbuild/bin/p bazel-out/k8-fastbuild/bin/p"
    );
    assert_eq!(inputs, vec!["p/in.txt", "external/bazel_tools/tools/genrule/genrule-setup.sh"]);
    assert_eq!(outputs, vec!["bazel-out/k8-fastbuild/bin/p/out.txt"]);
    assert_eq!(ct.typed_actions[0].progress_message.as_deref(), None);
}

#[test]
fn genrule_errors() {
    let ws = Ws::new(&[
        (
            "BUILD",
            r#"
genrule(name = "nocmd", outs = ["o"])
genrule(name = "two", srcs = ["a", "b"], outs = ["o2"], cmd = "cp $< $@")
"#,
        ),
        ("a", ""),
        ("b", ""),
    ]);
    let e = ws.analyze("//:nocmd").unwrap_err();
    assert!(e.contains("missing value for `cmd` attribute, you can also set `cmd_ps` or `cmd_bat` on Windows and `cmd_bash` on other platforms."), "{e}");
    let e = ws.analyze("//:two").unwrap_err();
    assert!(e.contains("variable '$<' : more than one input file"), "{e}");
}

#[test]
fn visibility_and_testonly_enforced() {
    let ws = Ws::new(&[
        ("a/BUILD", "filegroup(name = \"priv\", srcs = [])\nfilegroup(name = \"pub\", srcs = [], visibility = [\"//visibility:public\"])\nfilegroup(name = \"to\", srcs = [], testonly = True, visibility = [\"//visibility:public\"])\n"),
        ("b/BUILD", "filegroup(name = \"bad\", srcs = [\"//a:priv\"])\nfilegroup(name = \"good\", srcs = [\"//a:pub\"])\nfilegroup(name = \"bad_to\", srcs = [\"//a:to\"])\n"),
    ]);
    let e = ws.analyze("//b:bad").unwrap_err();
    assert!(e.contains("Visibility error:\ntarget '//a:priv' is not visible from\ntarget '//b:bad'\nRecommendation: modify the visibility declaration if you think the dependency is legitimate. For more info see https://bazel.build/concepts/visibility"), "{e}");
    ws.analyze("//b:good").unwrap();
    let e = ws.analyze("//b:bad_to").unwrap_err();
    assert!(e.contains("non-test target '//b:bad_to' depends on testonly target '//a:to' and doesn't have testonly attribute set"), "{e}");
}

#[test]
fn language_rules_are_not_native() {
    let ws = Ws::new(&[("BUILD", "cc_library(name = \"l\", srcs = [\"a.cc\"])\n"), ("a.cc", "")]);
    // Bazel 9: cc_library comes from @rules_cc; without a load() it is undefined.
    let e = ws.analyze("//:l").unwrap_err();
    assert!(e.contains("name 'cc_library' is not defined"), "{e}");
}

#[test]
fn transition_changes_output_dir_with_st_hash() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _t(settings, attr):
    return {"//command_line_option:compilation_mode": "opt"}
opt_t = transition(implementation = _t, inputs = [], outputs = ["//command_line_option:compilation_mode"])
def _i(ctx):
    out = ctx.actions.declare_file("o")
    ctx.actions.write(out, ctx.var["COMPILATION_MODE"])
    return [DefaultInfo(files = depset([out]))]
r = rule(implementation = _i, cfg = opt_t)
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\")\n"),
    ]);
    let ct = ws.analyze("//:r").unwrap();
    let p = ct.typed_actions[0].outputs[0].exec_path.to_string();
    // compilation_mode is explicit in the directory name, so there is no ST-hash.
    assert!(p.starts_with("bazel-out/k8-opt/bin/"), "{p}");
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
    assert_eq!(content.render(), b"opt");
}

#[test]
fn transition_on_other_option_gets_real_st_hash() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _t(settings, attr):
    return {"//command_line_option:copt": ["-O2", "-g"]}
copt_t = transition(implementation = _t, inputs = [], outputs = ["//command_line_option:copt"])
def _i(ctx):
    out = ctx.actions.declare_file("o")
    ctx.actions.write(out, "x")
    return [DefaultInfo(files = depset([out]))]
r = rule(implementation = _i, cfg = copt_t)
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\")\n"),
    ]);
    let ct = ws.analyze("//:r").unwrap();
    let p = ct.typed_actions[0].outputs[0].exec_path.to_string();
    // sha256(varint-len-prefixed "//command_line_option:copt=[-O2, -g]")[..12]
    let want = zel_analysis::transition_directory_name_fragment(["//command_line_option:copt=[-O2, -g]"]);
    assert!(p.starts_with(&format!("bazel-out/k8-fastbuild-{want}/bin/")), "{p} vs {want}");
}

#[test]
fn attr_transition_applies_to_dependency() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _t(settings, attr):
    return {"//command_line_option:cpu": "arm64"}
cpu_t = transition(implementation = _t, inputs = [], outputs = ["//command_line_option:cpu"])
def _leaf(ctx):
    o = ctx.actions.declare_file("leaf")
    ctx.actions.write(o, "x")
    return [DefaultInfo(files = depset([o]))]
leaf = rule(implementation = _leaf)
def _top(ctx):
    return [DefaultInfo(files = depset(ctx.files.dep))]
top = rule(implementation = _top, attrs = {"dep": attr.label(cfg = cpu_t)})
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"leaf\", \"top\")\nleaf(name = \"l\")\ntop(name = \"t\", dep = \":l\")\n"),
    ]);
    let ct = ws.analyze("//:t").unwrap();
    let f = ct.default_info.files[0].exec_path.to_string();
    // cpu is explicit in the directory name, so there is no ST-hash.
    assert!(f.starts_with("bazel-out/arm64-fastbuild/bin/"), "{f}");
}

#[test]
fn toolchain_resolution_picks_registered_toolchain() {
    let ws = Ws::new(&[
        (
            "tc/BUILD",
            r#"
load(":defs.bzl", "my_toolchain")
toolchain_type(name = "t")
my_toolchain(name = "impl", tool = "mytool")
toolchain(name = "tc", toolchain_type = ":t", toolchain = ":impl")
"#,
        ),
        (
            "tc/defs.bzl",
            r#"
def _tc(ctx):
    return [platform_common.ToolchainInfo(tool = ctx.attr.tool)]
my_toolchain = rule(implementation = _tc, attrs = {"tool": attr.string()})
"#,
        ),
        (
            "BUILD",
            r#"
load("//tc:defs.bzl", "use_tc")
use_tc(name = "u")
"#,
        ),
        (
            "tc/use.bzl",
            "",
        ),
    ]);
    ws.write(
        "tc/defs.bzl",
        r#"
def _tc(ctx):
    return [platform_common.ToolchainInfo(tool = ctx.attr.tool)]
my_toolchain = rule(implementation = _tc, attrs = {"tool": attr.string()})

def _use(ctx):
    out = ctx.actions.declare_file("u.txt")
    ctx.actions.write(out, ctx.toolchains["//tc:t"].tool)
    return [DefaultInfo(files = depset([out]))]
use_tc = rule(implementation = _use, toolchains = ["//tc:t"])
"#,
    );
    let mut acfg = AnalysisConfig::new();
    acfg.registered_toolchains = vec![Label::parse_canonical("//tc:tc").unwrap()];
    let (r, cfg) = ws.resolver_with(BuildConfigurationKey::target("k8", "fastbuild"), acfg);
    let ct = r.analyze(&Label::parse_canonical("//:u").unwrap(), &cfg).unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
    assert_eq!(content.render(), b"mytool");

    // No registered toolchains → Bazel's error text.
    let (r, cfg) = ws.resolver_with(BuildConfigurationKey::target("k8", "fastbuild"), AnalysisConfig::new());
    let e = r.analyze(&Label::parse_canonical("//:u").unwrap(), &cfg).unwrap_err();
    assert!(e.contains("While resolving toolchains for target //:u: No matching toolchains found for target platform @@platforms//host:host:\n  //tc:t\nTo debug, rerun with --toolchain_resolution_debug='//tc:t'"), "{e}");
}

#[test]
fn build_setting_value_and_flag_values_config_setting() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _flag(ctx):
    return []
string_flag = rule(implementation = _flag, build_setting = config.string(flag = True))
def _user(ctx):
    out = ctx.actions.declare_file("v")
    ctx.actions.write(out, "x")
    return []
user = rule(implementation = _user)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "string_flag")
string_flag(name = "color", build_setting_default = "red")
config_setting(name = "is_red", flag_values = {":color": "red"})
config_setting(name = "is_blue", flag_values = {":color": "blue"})
filegroup(name = "fg", srcs = select({":is_red": ["r.txt"], ":is_blue": ["b.txt"]}))
"#,
        ),
        ("r.txt", ""),
        ("b.txt", ""),
    ]);
    let ct = ws.analyze("//:fg").unwrap();
    assert_eq!(ct.default_info.files[0].exec_path.to_string(), "r.txt");
    let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
    cfg.flag_values.insert("//:color".into(), "blue".into());
    let (r, _) = ws.resolver_with(cfg.clone(), AnalysisConfig::new());
    let ct = r.analyze(&Label::parse_canonical("//:fg").unwrap(), &cfg).unwrap();
    assert_eq!(ct.default_info.files[0].exec_path.to_string(), "b.txt");
}

#[test]
fn starlark_executable_symlink_action() {
    // `ctx.actions.symlink(is_executable = True)` is a `SymlinkAction.toExecutable`: mnemonic
    // `ExecutableSymlink`, progress message with the root-relative output path.
    let ws = Ws::new(&[
        ("BUILD", "sh_binary(name = \"s\", srcs = [\"s.sh\"], data = [\"d.txt\"])\n"),
        ("s.sh", "#!/bin/sh\n"),
        ("d.txt", ""),
    ]);
    let ct = ws.analyze("//:s").unwrap();
    assert_eq!(ct.typed_actions[0].mnemonic, "ExecutableSymlink");
    assert_eq!(ct.typed_actions[0].progress_message.as_deref(), Some("Creating symlink s"));
    assert_eq!(ct.default_info.executable.as_ref().unwrap().exec_path.to_string(), "bazel-out/k8-fastbuild/bin/s");
    assert!(ct.default_info.default_runfiles.iter().any(|f| f.exec_path.to_string() == "d.txt"));
}

#[test]
fn provider_fields_schema_errors() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            "P = provider(fields = ['a'])\ndef _i(ctx):\n    return [P(a = 1, b = 2)]\nr = rule(implementation = _i)\n",
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\")\n"),
    ]);
    let e = ws.analyze("//:r").unwrap_err();
    assert!(e.contains("got unexpected field 'b' in call to instantiate provider P"), "{e}");
}

#[test]
fn default_info_runfiles_conflict_and_files_type() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            "def _i(ctx):\n    return [DefaultInfo(runfiles = ctx.runfiles(), data_runfiles = ctx.runfiles())]\nr = rule(implementation = _i)\n",
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\")\n"),
    ]);
    let e = ws.analyze("//:r").unwrap_err();
    assert!(e.contains("Cannot specify the provider 'runfiles' together with 'data_runfiles' or 'default_runfiles'"), "{e}");
}

#[test]
fn alias_and_filegroup_forward_providers() {
    let ws = Ws::new(&[
        ("BUILD", "filegroup(name = \"fg\", srcs = [\"a.txt\"])\nalias(name = \"al\", actual = \":fg\")\n"),
        ("a.txt", ""),
    ]);
    let ct = ws.analyze("//:al").unwrap();
    assert_eq!(ct.label.canonical_form(), "//:al");
    assert_eq!(ct.default_info.files[0].exec_path.to_string(), "a.txt");
}

#[test]
fn dependency_cycle_is_detected() {
    let ws = Ws::new(&[("BUILD", "filegroup(name = \"a\", srcs = [\":b\"])\nfilegroup(name = \"b\", srcs = [\":a\"])\n")]);
    let e = ws.analyze("//:a").unwrap_err();
    assert!(e.contains("cycle in dependency graph"), "{e}");
}

#[test]
fn mandatory_providers_are_enforced() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
P = provider()
def _p(ctx):
    return [P()]
def _n(ctx):
    return []
def _c(ctx):
    return []
has_p = rule(implementation = _p)
no_p = rule(implementation = _n)
consumer = rule(implementation = _c, attrs = {"dep": attr.label(providers = [P])})
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"has_p\", \"no_p\", \"consumer\")\nhas_p(name = \"a\")\nno_p(name = \"b\")\nconsumer(name = \"ok\", dep = \":a\")\nconsumer(name = \"bad\", dep = \":b\")\n"),
    ]);
    ws.analyze("//:ok").unwrap();
    let e = ws.analyze("//:bad").unwrap_err();
    assert!(e.contains("in dep attribute of consumer rule //:bad: '//:b' does not have mandatory providers: 'P'"), "{e}");
}

#[test]
fn implicit_outputs_and_attr_output_are_predeclared() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _i(ctx):
    ctx.actions.write(ctx.outputs.log, "l")
    ctx.actions.write(ctx.outputs.extra, "e")
    return []
r = rule(
    implementation = _i,
    attrs = {"extra": attr.output()},
    outputs = {"log": "%{name}.log"},
)
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\", extra = \"r_extra.txt\")\n"),
    ]);
    let ct = ws.analyze("//:r").unwrap();
    let mut files: Vec<String> = ct.default_info.files.iter().map(|f| f.exec_path.to_string()).collect();
    files.sort();
    assert_eq!(
        files,
        vec!["bazel-out/k8-fastbuild/bin/r.log", "bazel-out/k8-fastbuild/bin/r_extra.txt"]
    );
    assert_eq!(ct.typed_actions.len(), 2);
    // The output file targets are addressable and resolve to the same artifacts.
    let o = ws.analyze("//:r.log").unwrap();
    assert_eq!(o.default_info.files[0].exec_path.to_string(), "bazel-out/k8-fastbuild/bin/r.log");
}

#[test]
fn depsets_propagate_transitively_through_providers() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
Info = provider(fields = ["files"])
def _i(ctx):
    trans = [d[Info].files for d in ctx.attr.deps]
    return [Info(files = depset(ctx.files.srcs, transitive = trans, order = "postorder"))]
lib = rule(implementation = _i, attrs = {"srcs": attr.label_list(allow_files = True), "deps": attr.label_list()})
def _o(ctx):
    out = ctx.actions.declare_file("all.txt")
    ctx.actions.write(out, "\n".join([f.path for f in ctx.attr.dep[Info].files.to_list()]))
    return [DefaultInfo(files = depset([out]))]
top = rule(implementation = _o, attrs = {"dep": attr.label()})
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "lib", "top")
lib(name = "c", srcs = ["c.txt"])
lib(name = "b", srcs = ["b.txt"], deps = [":c"])
lib(name = "a", srcs = ["a.txt"], deps = [":b", ":c"])
top(name = "t", dep = ":a")
"#,
        ),
        ("a.txt", ""),
        ("b.txt", ""),
        ("c.txt", ""),
    ]);
    let ct = ws.analyze("//:t").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
    assert_eq!(String::from_utf8(content.render()).unwrap(), "c.txt\nb.txt\na.txt");
}

#[test]
fn param_files_and_map_each() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _fmt(f):
    return "-I" + f.dirname

def _i(ctx):
    out = ctx.actions.declare_file("o")
    args = ctx.actions.args()
    args.use_param_file("@%s", use_always = True)
    args.set_param_file_format("multiline")
    args.add_all(ctx.files.srcs, map_each = _fmt, uniquify = True)
    args.add("--out", out)
    ctx.actions.run(outputs = [out], inputs = ctx.files.srcs, executable = "/usr/bin/tool", arguments = [args, "tail"], mnemonic = "Tool")
    return []
r = rule(implementation = _i, attrs = {"srcs": attr.label_list(allow_files = True)})
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\", srcs = [\"d/a.h\", \"d/b.h\", \"e/c.h\"])\n"),
        ("d/a.h", ""),
        ("d/b.h", ""),
        ("e/c.h", ""),
    ]);
    let ct = ws.analyze("//:r").unwrap();
    let (argv, params) = ct.typed_actions[0].command_line();
    assert_eq!(argv, vec!["/usr/bin/tool", "@bazel-out/k8-fastbuild/bin/o-0.params", "tail"]);
    assert_eq!(
        String::from_utf8(params[0].1.clone()).unwrap(),
        "-Id\n-Ie\n--out\nbazel-out/k8-fastbuild/bin/o\n"
    );
    // The lowered flat model writes the param file first and feeds it to the spawn.
    assert_eq!(ct.actions.len(), 2);
    assert_eq!(ct.actions[0].mnemonic(), "FileWrite");
    assert_eq!(ct.actions[1].mnemonic(), "Tool");
    assert!(ct.actions[1].inputs().iter().any(|i| i.exec_path.to_string().ends_with("o-0.params")));
}

#[test]
fn int_list_and_string_keyed_label_dict_attrs() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _impl(ctx):
    out = ctx.actions.declare_file("o")
    parts = [str(i) for i in ctx.attr.nums]
    for k in sorted(ctx.attr.named.keys()):
        parts.append(k + "=" + ctx.attr.named[k].label.name)
    ctx.actions.write(out, ",".join(parts))
    return [DefaultInfo(files = depset([out]))]
r = rule(implementation = _impl, attrs = {
    "nums": attr.int_list(),
    "named": attr.string_keyed_label_dict(),
})
"#,
        ),
        (
            "BUILD",
            "load(\":defs.bzl\", \"r\")\nfilegroup(name = \"f\")\nr(name = \"r\", nums = [1, 2, 3], named = {\"x\": \":f\"})\n",
        ),
    ]);
    let ct = ws.analyze("//:r").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
    assert_eq!(content.render(), b"1,2,3,x=f");
    // Wrong element type → Bazel's error text.
    let ws = Ws::new(&[
        ("defs.bzl", "def _i(ctx):\n    return []\nr = rule(implementation = _i, attrs = {\"nums\": attr.int_list()})\n"),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\", nums = [\"a\"])\n"),
    ]);
    let e = ws.analyze("//:r").unwrap_err();
    assert!(e.contains("expected value of type 'int' for element 0 of attribute 'nums' in 'r' rule"), "{e}");
}

#[test]
fn test_suite_expansion() {
    let ws = Ws::new(&[
        (
            "BUILD",
            r#"
sh_test(name = "t1", srcs = ["t.sh"], tags = ["fast"])
sh_test(name = "t2", srcs = ["t.sh"], tags = ["slow"])
sh_test(name = "tm", srcs = ["t.sh"], tags = ["manual"])
test_suite(name = "all")
test_suite(name = "fast_only", tags = ["fast"])
test_suite(name = "explicit", tests = [":tm", ":t2"])
test_suite(name = "nested", tests = [":all", ":t1"])
"#,
        ),
        ("t.sh", ""),
    ]);
    let r = ws.resolver();
    let names = |l: &str, f: &[&str]| -> Vec<String> {
        let f: Vec<String> = f.iter().map(|s| s.to_string()).collect();
        let mut v: Vec<String> = r
            .expand_test_suites(&Label::parse_canonical(l).unwrap(), &f)
            .unwrap()
            .iter()
            .map(|l| l.name().to_string())
            .collect();
        v.sort();
        v
    };
    assert_eq!(names("//:all", &[]), vec!["t1", "t2"]);
    assert_eq!(names("//:fast_only", &[]), vec!["t1"]);
    assert_eq!(names("//:explicit", &[]), vec!["t2", "tm"]);
    assert_eq!(names("//:nested", &[]), vec!["t1", "t2"]);
    assert_eq!(names("//:all", &["-slow"]), vec!["t1"]);
    assert_eq!(names("//:t1", &[]), vec!["t1"]);
}

#[test]
fn ctx_attr_name_is_the_target_name() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            "def _i(ctx):\n    o = ctx.actions.declare_file(ctx.attr.name + '.out')\n    ctx.actions.write(o, 'x')\n    return [DefaultInfo(files = depset([o]))]\nr = rule(implementation = _i)\n",
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"nm\")\n"),
    ]);
    let ct = ws.analyze("//:nm").unwrap();
    assert_eq!(ct.default_info.files[0].exec_path.to_string(), "bazel-out/k8-fastbuild/bin/nm.out");
}

const ASPECT_DEFS: &str = r#"
CountInfo = provider(fields = ["n"])

def _asp_impl(target, ctx):
    n = 1
    for d in ctx.rule.attr.deps:
        n += d[CountInfo].n
    return [CountInfo(n = n)]

count_asp = aspect(implementation = _asp_impl, attr_aspects = ["deps"])

def _lib(ctx):
    return [DefaultInfo()]
lib = rule(implementation = _lib, attrs = {"deps": attr.label_list()})

def _top(ctx):
    out = ctx.actions.declare_file("n.txt")
    total = 0
    for d in ctx.attr.deps:
        total += d[CountInfo].n
    ctx.actions.write(out, str(total))
    return [DefaultInfo(files = depset([out]))]
top = rule(implementation = _top, attrs = {"deps": attr.label_list(aspects = [count_asp])})
"#;

#[test]
fn aspect_propagates_along_attr_aspects_and_is_visible_to_the_rule() {
    let ws = Ws::new(&[
        ("defs.bzl", ASPECT_DEFS),
        (
            "BUILD",
            "load(\":defs.bzl\", \"lib\", \"top\")\nlib(name = \"a\")\nlib(name = \"b\", deps = [\":a\"])\nlib(name = \"c\", deps = [\":a\", \":b\"])\ntop(name = \"t\", deps = [\":c\"])\n",
        ),
    ]);
    let ct = ws.analyze("//:t").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
    // count(a)=1, count(b)=2, count(c)=1+1+2=4.
    assert_eq!(content.render(), b"4");
}

#[test]
fn aspect_required_providers_stops_propagation() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
Marker = provider(fields = [])
SeenInfo = provider(fields = ["label"])
def _asp(target, ctx):
    return [SeenInfo(label = str(target.label))]
asp = aspect(implementation = _asp, attr_aspects = ["deps"], required_providers = [Marker])
def _marked(ctx):
    return [Marker()]
marked = rule(implementation = _marked, attrs = {"deps": attr.label_list()})
def _plain(ctx):
    return []
plain = rule(implementation = _plain, attrs = {"deps": attr.label_list()})
def _top(ctx):
    seen = []
    for d in ctx.attr.deps:
        seen.append(str(SeenInfo in d))
    out = ctx.actions.declare_file("o")
    ctx.actions.write(out, ",".join(seen))
    return [DefaultInfo(files = depset([out]))]
top = rule(implementation = _top, attrs = {"deps": attr.label_list(aspects = [asp])})
"#,
        ),
        (
            "BUILD",
            "load(\":defs.bzl\", \"marked\", \"plain\", \"top\")\nmarked(name = \"m\")\nplain(name = \"p\")\ntop(name = \"t\", deps = [\":m\", \":p\"])\n",
        ),
    ]);
    let ct = ws.analyze("//:t").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
    assert_eq!(content.render(), b"True,False");
}

#[test]
fn top_level_aspect_with_actions_output_groups_and_parameters() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _asp(target, ctx):
    out = ctx.actions.declare_file(ctx.label.name + "." + ctx.attr.suffix)
    ctx.actions.write(out, ctx.rule.kind + ":" + ",".join([f.basename for f in ctx.rule.files.srcs]) + ":" + ",".join(ctx.aspect_ids))
    return [OutputGroupInfo(report = depset([out]))]
asp = aspect(
    implementation = _asp,
    attrs = {"suffix": attr.string(values = ["txt", "log"], default = "txt")},
)
"#,
        ),
        ("BUILD", "filegroup(name = \"fg\", srcs = [\"a.txt\", \"b.txt\"])\n"),
        ("a.txt", ""),
        ("b.txt", ""),
    ]);
    let r = ws.resolver();
    let asp = r.loader().load_bzl_export(&Label::parse_canonical("//:defs.bzl").unwrap(), "asp").unwrap();
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");
    let l = Label::parse_canonical("//:fg").unwrap();
    let ct = r.analyze_with_aspects(&l, &cfg, std::slice::from_ref(&asp), &Default::default()).unwrap();
    let report = &ct.output_groups.groups["report"];
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].exec_path.to_string(), "bazel-out/k8-fastbuild/bin/fg.txt");
    let a = ct.typed_actions.last().unwrap();
    let ActionKind::FileWrite { content, .. } = &a.kind else { panic!() };
    assert_eq!(content.render(), b"filegroup:a.txt,b.txt://:defs.bzl%asp");
    // Parameters from --aspects_parameters.
    let mut params = std::collections::BTreeMap::new();
    params.insert("suffix".to_string(), "log".to_string());
    let r2 = ws.resolver();
    let asp2 = r2.loader().load_bzl_export(&Label::parse_canonical("//:defs.bzl").unwrap(), "asp").unwrap();
    let ct = r2.analyze_with_aspects(&l, &cfg, std::slice::from_ref(&asp2), &params).unwrap();
    assert!(ct.output_groups.groups["report"][0].exec_path.to_string().ends_with("fg.log"));
    params.insert("suffix".to_string(), "bad".to_string());
    let r3 = ws.resolver();
    let asp3 = r3.loader().load_bzl_export(&Label::parse_canonical("//:defs.bzl").unwrap(), "asp").unwrap();
    let e = r3.analyze_with_aspects(&l, &cfg, std::slice::from_ref(&asp3), &params).unwrap_err();
    assert!(e.contains("has to be one of 'txt', 'log' instead of 'bad'"), "{e}");
}

#[test]
fn aspect_errors_have_bazel_format() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _asp(target, ctx):
    return "nope"
asp = aspect(implementation = _asp)
def _lib(ctx):
    return []
lib = rule(implementation = _lib, attrs = {"dep": attr.label(aspects = [asp])})
def _p(ctx):
    return []
p = rule(implementation = _p)
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"lib\", \"p\")\np(name = \"d\")\nlib(name = \"l\", dep = \":d\")\n"),
    ]);
    let e = ws.analyze("//:l").unwrap_err();
    assert!(
        e.contains("in //:defs.bzl%asp aspect on p rule //:d: Aspect implementation should return a list, or a provider instance, but got string"),
        "{e}"
    );
    let ws = Ws::new(&[
        ("defs.bzl", "def _a(target, ctx):\n    return []\nbad = aspect(implementation = _a, attrs = {\"x\": attr.label()})\n"),
        ("BUILD", "load(\":defs.bzl\", \"bad\")\n"),
    ]);
    let e = ws.analyze("//:nothing").unwrap_err();
    assert!(e.contains("Aspect parameter attribute 'x' must have type 'bool', 'int' or 'string' and use the 'values' restriction."), "{e}");
}

#[test]
fn exec_groups_resolve_their_own_toolchains() {
    let ws = Ws::new(&[
        ("tc/BUILD", ""),
        (
            "tc/defs.bzl",
            r#"
def _tc(ctx):
    return [platform_common.ToolchainInfo(name = ctx.attr.n)]
my_tc = rule(implementation = _tc, attrs = {"n": attr.string()})
"#,
        ),
        (
            "BUILD",
            r#"
load("//tc:defs.bzl", "my_tc")
load(":defs.bzl", "uses")
toolchain_type(name = "ta")
toolchain_type(name = "tb")
my_tc(name = "ia", n = "A")
my_tc(name = "ib", n = "B")
toolchain(name = "tca", toolchain_type = ":ta", toolchain = ":ia")
toolchain(name = "tcb", toolchain_type = ":tb", toolchain = ":ib")
uses(name = "u")
"#,
        ),
        (
            "defs.bzl",
            r#"
def _impl(ctx):
    out = ctx.actions.declare_file("o")
    ctx.actions.write(out, ctx.toolchains["//:ta"].name + ctx.exec_groups["g"].toolchains["//:tb"].name)
    return [DefaultInfo(files = depset([out]))]
uses = rule(
    implementation = _impl,
    toolchains = ["//:ta"],
    exec_groups = {"g": exec_group(toolchains = ["//:tb"])},
)
"#,
        ),
    ]);
    let mut acfg = AnalysisConfig::new();
    acfg.registered_toolchains = vec![Label::parse_canonical("//:tca").unwrap(), Label::parse_canonical("//:tcb").unwrap()];
    let (r, cfg) = ws.resolver_with(BuildConfigurationKey::target("k8", "fastbuild"), acfg);
    let ct = r.analyze(&Label::parse_canonical("//:u").unwrap(), &cfg).unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
    assert_eq!(content.render(), b"AB");
}

#[test]
fn split_transition_and_split_attr() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _t(settings, attr):
    return {
        "dbg": {"//command_line_option:copt": ["-g"]},
        "opt": {"//command_line_option:copt": ["-O2"]},
    }
split_t = transition(implementation = _t, inputs = [], outputs = ["//command_line_option:copt"])
def _leaf(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, ctx.var["COMPILATION_MODE"])
    return [DefaultInfo(files = depset([out]))]
leaf = rule(implementation = _leaf)
def _top(ctx):
    out = ctx.actions.declare_file("o")
    parts = []
    for k in sorted(ctx.split_attr.dep.keys()):
        parts.append(k + "=" + ctx.split_attr.dep[k].files.to_list()[0].path)
    parts.append("flat=" + str(len(ctx.attr.dep)))
    ctx.actions.write(out, "\n".join(parts))
    return [DefaultInfo(files = depset([out]))]
top = rule(implementation = _top, attrs = {"dep": attr.label(cfg = split_t)})
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"leaf\", \"top\")\nleaf(name = \"l\")\ntop(name = \"t\", dep = \":l\")\n"),
    ]);
    let ct = ws.analyze("//:t").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
    let text = String::from_utf8(content.render()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    let dbg_hash = zel_analysis::transition_directory_name_fragment(["//command_line_option:copt=[-g]"]);
    let opt_hash = zel_analysis::transition_directory_name_fragment(["//command_line_option:copt=[-O2]"]);
    assert_eq!(lines[0], format!("dbg=bazel-out/k8-fastbuild-{dbg_hash}/bin/l.txt"));
    assert_eq!(lines[1], format!("opt=bazel-out/k8-fastbuild-{opt_hash}/bin/l.txt"), "{text}");
    assert_eq!(lines[2], "flat=2");
}

#[test]
fn run_environment_info_is_validated_and_exposed() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r##"
def _impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(exe, "#!/bin/sh\n", is_executable = True)
    return [
        DefaultInfo(executable = exe),
        RunEnvironmentInfo(environment = {"A": "1", "B": "x"}, inherited_environment = ["HOME", "PATH"]),
    ]
t = rule(implementation = _impl, executable = True)
"##,
        ),
        ("BUILD", "load(\":defs.bzl\", \"t\")\nt(name = \"t\")\n"),
    ]);
    let ct = ws.analyze("//:t").unwrap();
    let env = ct.run_environment.as_ref().expect("run environment");
    assert_eq!(env.environment.get("A").map(String::as_str), Some("1"));
    assert_eq!(env.inherited_environment, vec!["HOME".to_string(), "PATH".to_string()]);
    let ws = Ws::new(&[
        (
            "defs.bzl",
            "def _i(ctx):\n    return [RunEnvironmentInfo(environment = {\"A\": 1})]\nt = rule(implementation = _i)\n",
        ),
        ("BUILD", "load(\":defs.bzl\", \"t\")\nt(name = \"t\")\n"),
    ]);
    let e = ws.analyze("//:t").unwrap_err();
    assert!(e.contains("environment"), "{e}");
}

#[test]
fn fragments_and_instrumented_files_info() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _lib(ctx):
    return [
        DefaultInfo(),
        coverage_common.instrumented_files_info(ctx, source_attributes = ["srcs"], extensions = ["c"]),
    ]
lib = rule(implementation = _lib, attrs = {"srcs": attr.label_list(allow_files = True), "deps": attr.label_list()})
def _bin(ctx):
    out = ctx.actions.declare_file("o")
    info = coverage_common.instrumented_files_info(ctx, source_attributes = ["srcs"], dependency_attributes = ["deps"])
    ctx.actions.write(out, ",".join(ctx.fragments.cpp.copts) + "|" + str(ctx.fragments.platform.platform) + "|" + ",".join([f.basename for f in info.instrumented_files.to_list()]) + "|" + str(ctx.configuration.coverage_enabled))
    return [DefaultInfo(files = depset([out])), info]
bin = rule(implementation = _bin, fragments = ["cpp", "platform"], attrs = {"srcs": attr.label_list(allow_files = True), "deps": attr.label_list()})
"#,
        ),
        (
            "BUILD",
            "load(\":defs.bzl\", \"lib\", \"bin\")\nlib(name = \"l\", srcs = [\"a.c\", \"a.h\"])\nbin(name = \"b\", srcs = [\"m.c\"], deps = [\":l\"])\n",
        ),
        ("a.c", ""),
        ("a.h", ""),
        ("m.c", ""),
    ]);
    let run = |coverage: bool| {
        let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
        cfg.options.insert("copt".into(), "-O1,-Wall".into());
        if coverage {
            cfg.options.insert("collect_code_coverage".into(), "true".into());
        }
        let (r, cfg) = ws.resolver_with(cfg, AnalysisConfig::new());
        let ct = r.analyze(&Label::parse_canonical("//:b").unwrap(), &cfg).unwrap();
        let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
        String::from_utf8(content.render()).unwrap()
    };
    assert_eq!(run(false), "-O1,-Wall|@@platforms//host:host||False");
    assert_eq!(run(true), "-O1,-Wall|@@platforms//host:host|m.c,a.c|True");
}

#[test]
fn analysis_test_rules_late_bound_defaults_and_config_exec() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r##"
def _leaf(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, ctx.var["COMPILATION_MODE"])
    return [DefaultInfo(files = depset([out]))]
leaf = rule(implementation = _leaf)

def _check(ctx):
    dep = ctx.attr.target_under_test[0]
    path = dep.files.to_list()[0].path
    ok = "-opt" in path or "k8-opt" in path
    return [AnalysisTestResultInfo(success = ok, message = "bad path: " + path)]
check_test = rule(
    implementation = _check,
    analysis_test = True,
    attrs = {"target_under_test": attr.label(cfg = analysis_test_transition(settings = {"//command_line_option:compilation_mode": "opt"}))},
)

def _lb(ctx):
    out = ctx.actions.declare_file("lb")
    ctx.actions.write(out, str(ctx.attr._tc.label) if ctx.attr._tc else "none")
    return [DefaultInfo(files = depset([out]))]
lb = rule(implementation = _lb, attrs = {"_tc": attr.label(default = configuration_field(fragment = "cpp", name = "crosstool_top"))})

def _tool(ctx):
    out = ctx.actions.declare_file("tool.txt")
    ctx.actions.write(out, "x")
    return [DefaultInfo(files = depset([out]))]
tool = rule(implementation = _tool)
def _use(ctx):
    return [DefaultInfo(files = depset(ctx.files.tool))]
use = rule(implementation = _use, attrs = {"tool": attr.label(cfg = config.exec(exec_group = "g"))}, exec_groups = {"g": exec_group()})
"##,
        ),
        (
            "BUILD",
            "load(\":defs.bzl\", \"leaf\", \"check_test\", \"lb\", \"use\", \"tool\")\nleaf(name = \"l\")\ncheck_test(name = \"t\", target_under_test = \":l\")\nlb(name = \"lb\")\ntool(name = \"tool\")\nuse(name = \"u\", tool = \":tool\")\n",
        ),
    ]);
    let ct = ws.analyze("//:t").unwrap();
    assert!(ct.is_test);
    let exe = ct.default_info.executable.clone().expect("analysis test executable");
    assert_eq!(exe.exec_path.to_string(), "bazel-out/k8-fastbuild/bin/t");
    let ActionKind::FileWrite { content, is_executable } = &ct.typed_actions[0].kind else { panic!() };
    assert!(is_executable);
    assert_eq!(content.render(), b"#!/bin/sh\nexit 0\n");
    // Late-bound default from the configuration option.
    let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
    cfg.options.insert("crosstool_top".into(), "//tc:toolchain".into());
    let (r, cfg) = ws.resolver_with(cfg, AnalysisConfig::new());
    let e = r.analyze(&Label::parse_canonical("//:lb").unwrap(), &cfg).unwrap_err();
    assert!(e.contains("no such package 'tc'"), "{e}");
    let ct = ws.analyze("//:lb").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
    assert_eq!(content.render(), b"none");
    // cfg = config.exec(exec_group = ...): the tool is built in the exec configuration.
    let ct = ws.analyze("//:u").unwrap();
    assert_eq!(
        ct.default_info.files[0].exec_path.to_string(),
        format!("bazel-out/{}-opt-exec/bin/tool.txt", zel_analysis::build_options::auto_host_cpu())
    );
}

#[test]
fn subrules_get_ctx_and_implicit_attributes() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r##"
def _helper_impl(ctx, text, suffix = ".txt", _tool = None):
    out = ctx.actions.declare_file(ctx.label.name + suffix)
    ctx.actions.write(out, text + ":" + _tool.basename)
    return out
helper = subrule(implementation = _helper_impl, attrs = {"_tool": attr.label(default = "//:tool.sh", allow_single_file = True)})

def _impl(ctx):
    out = helper("hello", suffix = ".out")
    return [DefaultInfo(files = depset([out]))]
uses = rule(implementation = _impl, subrules = [helper])

def _bad(ctx):
    helper("x")
    return []
undeclared = rule(implementation = _bad)
"##,
        ),
        ("BUILD", "load(\":defs.bzl\", \"uses\", \"undeclared\")\nexports_files([\"tool.sh\"])\nuses(name = \"u\")\nundeclared(name = \"bad\")\n"),
        ("tool.sh", ""),
    ]);
    let ct = ws.analyze("//:u").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
    assert_eq!(content.render(), b"hello:tool.sh");
    assert_eq!(ct.default_info.files[0].exec_path.to_string(), "bazel-out/k8-fastbuild/bin/u.out");
    let e = ws.analyze("//:bad").unwrap_err();
    assert!(e.contains("does not declare it in 'subrules'"), "{e}");
}

#[test]
fn tree_artifact_args_use_expansion_markers() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r##"
def _impl(ctx):
    d = ctx.actions.declare_directory("d")
    ctx.actions.run_shell(outputs = [d], command = "mkdir -p $1", arguments = [d.path])
    out = ctx.actions.declare_file("o")
    args = ctx.actions.args()
    args.add_all([d], format_each = "--in=%s")
    args.add_all("-I", [d], expand_directories = False)
    ctx.actions.run_shell(outputs = [out], inputs = [d], command = "true", arguments = [args])
    return [DefaultInfo(files = depset([out]))]
r = rule(implementation = _impl)
"##,
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\")\n"),
    ]);
    let ct = ws.analyze("//:r").unwrap();
    let (_, argv, _, _) = spawn_of(&ct, 1);
    let marker = &argv[4];
    assert!(marker.starts_with(zel_analysis::TREE_MARKER), "{argv:?}");
    let expanded = zel_analysis::expand_tree_artifact_args(&argv, &|d| {
        assert_eq!(d, "bazel-out/k8-fastbuild/bin/d");
        Ok(vec![format!("{d}/a"), format!("{d}/b")])
    })
    .unwrap();
    assert_eq!(
        &expanded[4..],
        &[
            "--in=bazel-out/k8-fastbuild/bin/d/a".to_string(),
            "--in=bazel-out/k8-fastbuild/bin/d/b".to_string(),
            "-I".to_string(),
            "bazel-out/k8-fastbuild/bin/d".to_string(),
        ]
    );
}

#[test]
fn ctx_target_platform_has_constraint_and_platform_constraints() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r##"
def _impl(ctx):
    out = ctx.actions.declare_file("o")
    cv = ctx.attr.cv[platform_common.ConstraintValueInfo]
    ctx.actions.write(out, str(ctx.target_platform_has_constraint(cv)))
    return [DefaultInfo(files = depset([out]))]
r = rule(implementation = _impl, attrs = {"cv": attr.label(providers = [platform_common.ConstraintValueInfo])})
"##,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "r")
constraint_setting(name = "color")
constraint_value(name = "red", constraint_setting = ":color")
constraint_value(name = "blue", constraint_setting = ":color")
platform(name = "p_red", constraint_values = [":red"])
r(name = "wants_red", cv = ":red")
r(name = "wants_blue", cv = ":blue")
"#,
        ),
    ]);
    let run = |target: &str| {
        let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
        cfg.platforms = vec!["//:p_red".to_string()];
        let (r, cfg) = ws.resolver_with(cfg, AnalysisConfig::new());
        let ct = r.analyze(&Label::parse_canonical(target).unwrap(), &cfg).unwrap();
        let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else { panic!() };
        String::from_utf8(content.render()).unwrap()
    };
    assert_eq!(run("//:wants_red"), "True");
    assert_eq!(run("//:wants_blue"), "False");
}

#[test]
fn registered_execution_platforms_and_toolchain_patterns_expand_lazily() {
    let files: &[(&str, &str)] = &[
        (
            "defs.bzl",
            r##"
def _tc(ctx):
    return [platform_common.ToolchainInfo(tag = "cc")]
my_tc = rule(implementation = _tc)
def _use(ctx):
    out = ctx.actions.declare_file("o")
    ctx.actions.write(out, ctx.toolchains["//:tt"].tag)
    return [DefaultInfo(files = depset([out]))]
use = rule(implementation = _use, toolchains = ["//:tt"])
"##,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "my_tc", "use")
constraint_setting(name = "gizmo")
constraint_value(name = "has_gizmo", constraint_setting = ":gizmo")
platform(name = "gizmo_exec", constraint_values = [":has_gizmo"])
toolchain_type(name = "tt")
my_tc(name = "impl")
toolchain(name = "tc", toolchain_type = ":tt", toolchain = ":impl", exec_compatible_with = [":has_gizmo"])
use(name = "u")
"#,
        ),
    ];
    let ws = Ws::new(files);
    let analyze = |acfg: AnalysisConfig| {
        let (r, cfg) = ws.resolver_with(BuildConfigurationKey::target("k8", "fastbuild"), acfg);
        r.analyze(&Label::parse_canonical("//:u").unwrap(), &cfg).map(|ct| ct.default_info.files[0].exec_path.to_string())
    };
    // Toolchain registered by pattern, but no execution platform has the gizmo constraint.
    let mut acfg = AnalysisConfig::new();
    acfg.toolchain_patterns = vec!["//:all".to_string()];
    let e = analyze(acfg).unwrap_err();
    assert!(e.contains("No matching toolchains found for target platform"), "{e}");
    // With the execution platform registered (pattern), resolution succeeds.
    let mut acfg = AnalysisConfig::new();
    acfg.toolchain_patterns = vec!["//:all".to_string()];
    acfg.execution_platform_patterns = vec!["//:gizmo_exec".to_string()];
    assert_eq!(analyze(acfg).unwrap(), "bazel-out/k8-fastbuild/bin/o");
    // A pattern naming a non-toolchain target is an error (and only when toolchains are needed).
    let mut acfg = AnalysisConfig::new();
    acfg.toolchain_patterns = vec!["//:impl".to_string()];
    let e = analyze(acfg).unwrap_err();
    assert!(e.contains("invalid registered toolchain '//:impl'"), "{e}");
}
