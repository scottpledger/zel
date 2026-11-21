//! Analysis-phase tests: real `ctx`, actions, providers, selects, native rules, visibility.

use super::*;
use std::sync::Arc;
use zel_analysis::{ActionKind, ConfiguredTarget};
use zel_cmdline::RepositoryName;
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
        let names: Vec<&str> = ["sh_binary", "sh_library", "sh_test"]
            .into_iter()
            .filter(|n| c.contains(&format!("{n}(")))
            .collect();
        if (p == "BUILD" || p.ends_with("/BUILD")) && !names.is_empty() {
            let q: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
            let content = format!("load(\"//:sh_stub.bzl\", {}); {c}", q.join(", "));
            self.fs.write_file(&path, content.as_bytes()).unwrap();
            self.fs
                .write_file(
                    &self.root.get_relative_str("sh_stub.bzl"),
                    SH_STUB.as_bytes(),
                )
                .unwrap();
            return;
        }
        self.fs.write_file(&path, c.as_bytes()).unwrap();
    }
    fn resolver(&self) -> Arc<TargetResolver> {
        self.resolver_with(
            BuildConfigurationKey::target("k8", "fastbuild"),
            AnalysisConfig::new(),
        )
        .0
    }
    fn resolver_with(
        &self,
        cfg: BuildConfigurationKey,
        acfg: AnalysisConfig,
    ) -> (Arc<TargetResolver>, BuildConfigurationKey) {
        let lc = LoaderConfig::new(
            self.fs.clone(),
            Arc::new(SimpleRepoResolver::new(self.root.clone())),
        );
        (TargetResolver::new(PackageLoader::new(lc), acfg), cfg)
    }
    fn analyze(&self, label: &str) -> Result<Arc<ConfiguredTarget>, String> {
        let r = self.resolver();
        r.analyze(
            &Label::parse_canonical(label).unwrap(),
            &BuildConfigurationKey::target("k8", "fastbuild"),
        )
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
        (
            "BUILD",
            "load(\":defs.bzl\", \"greet\")\ngreet(name = \"g\", who = \"zel\")\n",
        ),
    ]);
    let ct = ws.analyze("//:g").unwrap();
    assert_eq!(ct.typed_actions.len(), 1);
    let a = &ct.typed_actions[0];
    assert_eq!(a.mnemonic, "FileWrite");
    match &a.kind {
        ActionKind::FileWrite {
            content,
            is_executable,
        } => {
            assert_eq!(content.render(), b"hello zel\n");
            assert!(!is_executable);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        a.outputs[0].exec_path.to_string(),
        "bazel-out/k8-fastbuild/bin/g.txt"
    );
    assert_eq!(ct.default_info.files.len(), 1);
    assert_eq!(
        ct.default_info.files[0].exec_path.to_string(),
        "bazel-out/k8-fastbuild/bin/g.txt"
    );
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
            "/bin/bash",
            "-c",
            "echo hi > $1",
            "",
            "--out",
            "bazel-out/k8-fastbuild/bin/x.out",
            "--in",
            "a.txt",
            "b.txt",
            "--vals",
            "A,B"
        ]
    );
    assert_eq!(inputs, vec!["a.txt", "b.txt"]);
    assert_eq!(outputs, vec!["bazel-out/k8-fastbuild/bin/x.out"]);
    let a = &ct.typed_actions[0];
    assert_eq!(
        a.progress_message.as_deref(),
        Some("Generating x.out for //:x")
    );
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
        (
            "BUILD",
            "load(\":defs.bzl\", \"a\", \"b\")\na(name = \"a\")\nb(name = \"b\", dep = \":a\")\n",
        ),
    ]);
    let e = ws.analyze("//:b").unwrap_err();
    assert!(
        e.contains("<target //:a> (rule 'a') doesn't contain declared provider 'P'"),
        "{e}"
    );
    assert!(e.contains("in b rule //:b"), "{e}");
}

#[test]
fn executable_rule_requires_executable() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            "def _i(ctx):\n    return []\nr = rule(implementation = _i, executable = True)\n",
        ),
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
        (
            "BUILD",
            "load(\":defs.bzl\", \"r\")\nr(name = \"r\", data = [\"d.txt\"])\n",
        ),
        ("d.txt", "d"),
    ]);
    let ct = ws.analyze("//:r").unwrap();
    let rs = ct.runfiles_support.as_ref().expect("runfiles support");
    assert_eq!(rs.runfiles_dir, "bazel-out/k8-fastbuild/bin/r.runfiles");
    let m = rs.runfiles.manifest_entries("_main");
    assert_eq!(m["_main/d.txt"], "d.txt");
    assert_eq!(m["_main/r"], "bazel-out/k8-fastbuild/bin/r");
    assert_eq!(
        ct.default_info
            .executable
            .as_ref()
            .unwrap()
            .exec_path
            .to_string(),
        "bazel-out/k8-fastbuild/bin/r"
    );
    assert!(ct
        .typed_actions
        .iter()
        .any(|a| a.mnemonic == "SourceSymlinkManifest"));
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
    assert!(
        e.contains("is not under package directory 'p' for target '//p:r'"),
        "{e}"
    );
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
        (
            "BUILD",
            "load(\":defs.bzl\", \"r\")\nr(name = \"r\", tmpl = \"t.in\")\n",
        ),
        ("t.in", "name=%N%"),
    ]);
    let ct = ws.analyze("//:r").unwrap();
    assert_eq!(ct.typed_actions.len(), 2);
    assert_eq!(ct.typed_actions[0].mnemonic, "TemplateExpand");
    assert_eq!(ct.typed_actions[1].mnemonic, "Symlink");
    assert_eq!(
        ct.typed_actions[1].inputs[0].exec_path.to_string(),
        "bazel-out/k8-fastbuild/bin/out.txt"
    );
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
    assert!(
        e.contains("Exactly one of \"target_file\" or \"target_path\" is required"),
        "{e}"
    );
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
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
    assert_eq!(
        String::from_utf8(content.render()).unwrap(),
        "cat ./s.txt > ./s.txt fastbuild $ bazel-out/k8-fastbuild/bin"
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
    let (r, _) = ws.resolver_with(
        BuildConfigurationKey::target("k8", "opt"),
        AnalysisConfig::new(),
    );
    let ct = r
        .analyze(
            &Label::parse_canonical("//:fg").unwrap(),
            &BuildConfigurationKey::target("k8", "opt"),
        )
        .unwrap();
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
    assert_eq!(
        inputs,
        vec![
            "p/in.txt",
            "external/bazel_tools/tools/genrule/genrule-setup.sh"
        ]
    );
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
    assert!(
        e.contains("variable '$<' : more than one input file"),
        "{e}"
    );
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
    let ws = Ws::new(&[
        ("BUILD", "cc_library(name = \"l\", srcs = [\"a.cc\"])\n"),
        ("a.cc", ""),
    ]);
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
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
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
    let want =
        zel_analysis::transition_directory_name_fragment(["//command_line_option:copt=[-O2, -g]"]);
    assert!(
        p.starts_with(&format!("bazel-out/k8-fastbuild-{want}/bin/")),
        "{p} vs {want}"
    );
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
        ("tc/use.bzl", ""),
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
    let ct = r
        .analyze(&Label::parse_canonical("//:u").unwrap(), &cfg)
        .unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
    assert_eq!(content.render(), b"mytool");

    // No registered toolchains → Bazel's error text.
    let (r, cfg) = ws.resolver_with(
        BuildConfigurationKey::target("k8", "fastbuild"),
        AnalysisConfig::new(),
    );
    let e = r
        .analyze(&Label::parse_canonical("//:u").unwrap(), &cfg)
        .unwrap_err();
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
    let ct = r
        .analyze(&Label::parse_canonical("//:fg").unwrap(), &cfg)
        .unwrap();
    assert_eq!(ct.default_info.files[0].exec_path.to_string(), "b.txt");
}

#[test]
fn starlark_executable_symlink_action() {
    // `ctx.actions.symlink(is_executable = True)` is a `SymlinkAction.toExecutable`: mnemonic
    // `ExecutableSymlink`, progress message with the root-relative output path.
    let ws = Ws::new(&[
        (
            "BUILD",
            "sh_binary(name = \"s\", srcs = [\"s.sh\"], data = [\"d.txt\"])\n",
        ),
        ("s.sh", "#!/bin/sh\n"),
        ("d.txt", ""),
    ]);
    let ct = ws.analyze("//:s").unwrap();
    assert_eq!(ct.typed_actions[0].mnemonic, "ExecutableSymlink");
    assert_eq!(
        ct.typed_actions[0].progress_message.as_deref(),
        Some("Creating symlink s")
    );
    assert_eq!(
        ct.default_info
            .executable
            .as_ref()
            .unwrap()
            .exec_path
            .to_string(),
        "bazel-out/k8-fastbuild/bin/s"
    );
    assert!(ct
        .default_info
        .default_runfiles
        .iter()
        .any(|f| f.exec_path.to_string() == "d.txt"));
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
    assert!(
        e.contains("got unexpected field 'b' in call to instantiate provider P"),
        "{e}"
    );
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
    let ws = Ws::new(&[(
        "BUILD",
        "filegroup(name = \"a\", srcs = [\":b\"])\nfilegroup(name = \"b\", srcs = [\":a\"])\n",
    )]);
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
        (
            "BUILD",
            "load(\":defs.bzl\", \"r\")\nr(name = \"r\", extra = \"r_extra.txt\")\n",
        ),
    ]);
    let ct = ws.analyze("//:r").unwrap();
    let mut files: Vec<String> = ct
        .default_info
        .files
        .iter()
        .map(|f| f.exec_path.to_string())
        .collect();
    files.sort();
    assert_eq!(
        files,
        vec![
            "bazel-out/k8-fastbuild/bin/r.log",
            "bazel-out/k8-fastbuild/bin/r_extra.txt"
        ]
    );
    assert_eq!(ct.typed_actions.len(), 2);
    // The output file targets are addressable and resolve to the same artifacts.
    let o = ws.analyze("//:r.log").unwrap();
    assert_eq!(
        o.default_info.files[0].exec_path.to_string(),
        "bazel-out/k8-fastbuild/bin/r.log"
    );
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
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
    assert_eq!(
        String::from_utf8(content.render()).unwrap(),
        "c.txt\nb.txt\na.txt"
    );
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
    assert_eq!(
        argv,
        vec![
            "/usr/bin/tool",
            "@bazel-out/k8-fastbuild/bin/o-0.params",
            "tail"
        ]
    );
    assert_eq!(
        String::from_utf8(params[0].1.clone()).unwrap(),
        "-Id\n-Ie\n--out\nbazel-out/k8-fastbuild/bin/o\n"
    );
    // The lowered flat model writes the param file first and feeds it to the spawn.
    assert_eq!(ct.actions.len(), 2);
    assert_eq!(ct.actions[0].mnemonic(), "FileWrite");
    assert_eq!(ct.actions[1].mnemonic(), "Tool");
    assert!(ct.actions[1]
        .inputs()
        .iter()
        .any(|i| i.exec_path.to_string().ends_with("o-0.params")));
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
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
    assert_eq!(content.render(), b"1,2,3,x=f");
    // Wrong element type → Bazel's error text.
    let ws = Ws::new(&[
        ("defs.bzl", "def _i(ctx):\n    return []\nr = rule(implementation = _i, attrs = {\"nums\": attr.int_list()})\n"),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"r\", nums = [\"a\"])\n"),
    ]);
    let e = ws.analyze("//:r").unwrap_err();
    assert!(
        e.contains("expected value of type 'int' for element 0 of attribute 'nums' in 'r' rule"),
        "{e}"
    );
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
    assert_eq!(
        ct.default_info.files[0].exec_path.to_string(),
        "bazel-out/k8-fastbuild/bin/nm.out"
    );
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
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
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
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
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
        (
            "BUILD",
            "filegroup(name = \"fg\", srcs = [\"a.txt\", \"b.txt\"])\n",
        ),
        ("a.txt", ""),
        ("b.txt", ""),
    ]);
    let r = ws.resolver();
    let asp = r
        .loader()
        .load_bzl_export(&Label::parse_canonical("//:defs.bzl").unwrap(), "asp")
        .unwrap();
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");
    let l = Label::parse_canonical("//:fg").unwrap();
    let ct = r
        .analyze_with_aspects(&l, &cfg, std::slice::from_ref(&asp), &Default::default())
        .unwrap();
    let report = &ct.output_groups.groups["report"];
    assert_eq!(report.len(), 1);
    assert_eq!(
        report[0].exec_path.to_string(),
        "bazel-out/k8-fastbuild/bin/fg.txt"
    );
    let a = ct.typed_actions.last().unwrap();
    let ActionKind::FileWrite { content, .. } = &a.kind else {
        panic!()
    };
    assert_eq!(content.render(), b"filegroup:a.txt,b.txt://:defs.bzl%asp");
    // Parameters from --aspects_parameters.
    let mut params = std::collections::BTreeMap::new();
    params.insert("suffix".to_string(), "log".to_string());
    let r2 = ws.resolver();
    let asp2 = r2
        .loader()
        .load_bzl_export(&Label::parse_canonical("//:defs.bzl").unwrap(), "asp")
        .unwrap();
    let ct = r2
        .analyze_with_aspects(&l, &cfg, std::slice::from_ref(&asp2), &params)
        .unwrap();
    assert!(ct.output_groups.groups["report"][0]
        .exec_path
        .to_string()
        .ends_with("fg.log"));
    params.insert("suffix".to_string(), "bad".to_string());
    let r3 = ws.resolver();
    let asp3 = r3
        .loader()
        .load_bzl_export(&Label::parse_canonical("//:defs.bzl").unwrap(), "asp")
        .unwrap();
    let e = r3
        .analyze_with_aspects(&l, &cfg, std::slice::from_ref(&asp3), &params)
        .unwrap_err();
    assert!(
        e.contains("has to be one of 'txt', 'log' instead of 'bad'"),
        "{e}"
    );
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
    acfg.registered_toolchains = vec![
        Label::parse_canonical("//:tca").unwrap(),
        Label::parse_canonical("//:tcb").unwrap(),
    ];
    let (r, cfg) = ws.resolver_with(BuildConfigurationKey::target("k8", "fastbuild"), acfg);
    let ct = r
        .analyze(&Label::parse_canonical("//:u").unwrap(), &cfg)
        .unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
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
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
    let text = String::from_utf8(content.render()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    let dbg_hash =
        zel_analysis::transition_directory_name_fragment(["//command_line_option:copt=[-g]"]);
    let opt_hash =
        zel_analysis::transition_directory_name_fragment(["//command_line_option:copt=[-O2]"]);
    assert_eq!(
        lines[0],
        format!("dbg=bazel-out/k8-fastbuild-{dbg_hash}/bin/l.txt")
    );
    assert_eq!(
        lines[1],
        format!("opt=bazel-out/k8-fastbuild-{opt_hash}/bin/l.txt"),
        "{text}"
    );
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
    assert_eq!(
        env.inherited_environment,
        vec!["HOME".to_string(), "PATH".to_string()]
    );
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
            cfg.options
                .insert("collect_code_coverage".into(), "true".into());
        }
        let (r, cfg) = ws.resolver_with(cfg, AnalysisConfig::new());
        let ct = r
            .analyze(&Label::parse_canonical("//:b").unwrap(), &cfg)
            .unwrap();
        let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
            panic!()
        };
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
    let exe = ct
        .default_info
        .executable
        .clone()
        .expect("analysis test executable");
    assert_eq!(exe.exec_path.to_string(), "bazel-out/k8-fastbuild/bin/t");
    let ActionKind::FileWrite {
        content,
        is_executable,
    } = &ct.typed_actions[0].kind
    else {
        panic!()
    };
    assert!(is_executable);
    assert_eq!(content.render(), b"#!/bin/sh\nexit 0\n");
    // Late-bound default from the configuration option.
    let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
    cfg.options
        .insert("crosstool_top".into(), "//tc:toolchain".into());
    let (r, cfg) = ws.resolver_with(cfg, AnalysisConfig::new());
    let e = r
        .analyze(&Label::parse_canonical("//:lb").unwrap(), &cfg)
        .unwrap_err();
    assert!(e.contains("no such package 'tc'"), "{e}");
    let ct = ws.analyze("//:lb").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
    assert_eq!(content.render(), b"none");
    // cfg = config.exec(exec_group = ...): the tool is built in the exec configuration.
    let ct = ws.analyze("//:u").unwrap();
    assert_eq!(
        ct.default_info.files[0].exec_path.to_string(),
        format!(
            "bazel-out/{}-opt-exec/bin/tool.txt",
            zel_analysis::build_options::auto_host_cpu()
        )
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
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
    assert_eq!(content.render(), b"hello:tool.sh");
    assert_eq!(
        ct.default_info.files[0].exec_path.to_string(),
        "bazel-out/k8-fastbuild/bin/u.out"
    );
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
        let ct = r
            .analyze(&Label::parse_canonical(target).unwrap(), &cfg)
            .unwrap();
        let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
            panic!()
        };
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
        r.analyze(&Label::parse_canonical("//:u").unwrap(), &cfg)
            .map(|ct| ct.default_info.files[0].exec_path.to_string())
    };
    // Toolchain registered by pattern, but no execution platform has the gizmo constraint.
    let mut acfg = AnalysisConfig::new();
    acfg.toolchain_patterns = vec!["//:all".to_string()];
    let e = analyze(acfg).unwrap_err();
    assert!(
        e.contains("No matching toolchains found for target platform"),
        "{e}"
    );
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

#[test]
fn alias_chain_visibility_testonly_and_package_group() {
    let ws = Ws::new(&[
        (
            "a/BUILD",
            r#"
filegroup(name = "priv", srcs = [], testonly = True)
filegroup(name = "pub", srcs = [], visibility = ["//visibility:public"])
alias(name = "mid", actual = ":priv", visibility = ["//visibility:public"])
package_group(name = "pg", packages = ["//..."])
"#,
        ),
        (
            "b/BUILD",
            r#"
alias(name = "top_alias", actual = "//a:mid", visibility = ["//visibility:public"])
filegroup(name = "bad_testonly", srcs = [":top_alias"])
filegroup(name = "bad_pg", srcs = ["//a:pg"])
"#,
        ),
    ]);
    let e = ws.analyze("//b:bad_testonly").unwrap_err();
    assert!(
        e.contains("testonly")
            && e.contains("alias '//b:top_alias' referring to target '//a:priv' through '//a:mid'"),
        "{e}"
    );
    let e_pg = ws.analyze("//b:bad_pg").unwrap_err();
    assert!(e_pg.contains("misplaced package_group"), "{e_pg}");
}

#[test]
fn validation_artifacts_featureset_and_ctx_methods() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _orphan(ctx):
    ctx.actions.declare_file("orphan.txt")
    ctx.actions.do_nothing(mnemonic = "Noop")
    return []
orphan = rule(implementation = _orphan)

def _dir_conflict(ctx):
    f = ctx.actions.declare_file("same")
    d = ctx.actions.declare_directory("same")
    return [DefaultInfo(files = depset([f, d]))]
dir_conflict = rule(implementation = _dir_conflict)

def _feat_and_ctx(ctx):
    out = ctx.actions.declare_file("out.txt")
    args = ctx.actions.args()
    ok_ph = ctx.check_placeholders("hello %{name}", ["name"])
    bad_ph = ctx.check_placeholders("hello %{bad}", ["name"])
    lbl = ctx.package_relative_label(":out.txt")
    _ = ctx.info_file
    _ = ctx.version_file
    ctx.actions.write(
        out,
        ",".join(ctx.features)
            + "|"
            + ",".join(ctx.disabled_features)
            + "|"
            + str(len(ctx.configuration.short_id))
            + "|"
            + str(ok_ph)
            + ","
            + str(bad_ph)
            + "|"
            + str(lbl)
            + "|"
            + str(ctx.toolchains.toolchain_types()),
    )
    return [DefaultInfo(files = depset([out]))]
feat_rule = rule(implementation = _feat_and_ctx)
"#,
        ),
        (
            "BUILD",
            r#"
package(features = ["pkg_on", "layering_check", "pkg_off"])
load(":defs.bzl", "orphan", "dir_conflict", "feat_rule")
orphan(name = "o")
dir_conflict(name = "dc")
feat_rule(name = "f", features = ["no_layering_check", "-pkg_off", "rule_on"])
"#,
        ),
    ]);
    let e_orphan = ws.analyze("//:o").unwrap_err();
    assert!(
        e_orphan.contains("The following files have no generating action:\norphan.txt"),
        "{e_orphan}"
    );
    let e_dc = ws.analyze("//:dc").unwrap_err();
    assert!(
        e_dc.contains("'same' has already been declared as a regular file, not directory."),
        "{e_dc}"
    );

    let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
    cfg.options
        .insert("features".into(), "-pkg_on,cli_on".into());
    let (r, cfg) = ws.resolver_with(cfg, AnalysisConfig::new());
    let ct = r
        .analyze(&Label::parse_canonical("//:f").unwrap(), &cfg)
        .unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
    let text = String::from_utf8(content.render()).unwrap();
    assert_eq!(
        text,
        "cli_on,rule_on|layering_check,pkg_off,pkg_on|7|True,False|@@//:out.txt|[]"
    );
}

#[test]
fn ctx_guards_fragments_toolchains_and_args_validation() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _bad_rule_ctx(ctx):
    return [ctx.rule]
bad_rule = rule(implementation = _bad_rule_ctx)

def _bad_frag(ctx):
    return [ctx.fragments.cpp]
bad_frag = rule(implementation = _bad_frag)

def _bad_tc(ctx):
    return [ctx.toolchains["//:missing"]]
bad_tc = rule(implementation = _bad_tc)

def _bad_args(ctx):
    a = ctx.actions.args()
    a.use_param_file("%%s")
    return []
bad_args = rule(implementation = _bad_args)

def _bad_closure(ctx):
    a = ctx.actions.args()
    a.add_all(["x"], map_each = lambda s: s)
    return []
bad_closure = rule(implementation = _bad_closure)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "bad_rule", "bad_frag", "bad_tc", "bad_args", "bad_closure")
bad_rule(name = "r")
bad_frag(name = "f")
bad_tc(name = "t")
bad_args(name = "a")
bad_closure(name = "c")
"#,
        ),
    ]);
    assert!(ws
        .analyze("//:r")
        .unwrap_err()
        .contains("'rule' is only available in aspect implementations"));
    assert!(ws
        .analyze("//:f")
        .unwrap_err()
        .contains("has to declare 'cpp' as a required fragment"));
    assert!(ws
        .analyze("//:t")
        .unwrap_err()
        .contains("Toolchains are not valid in this context"));
    assert!(ws
        .analyze("//:a")
        .unwrap_err()
        .contains("Expected string with a single \"%s\""));
    assert!(ws
        .analyze("//:c")
        .unwrap_err()
        .contains("must be declared by a top-level def statement"));
}

#[test]
fn aspect_requires_propagation_and_action_conflict() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
BaseInfo = provider(fields = ["n"])
OuterInfo = provider(fields = ["vals"])

def _base_impl(target, ctx):
    n = 1
    for d in ctx.rule.attr.deps:
        if BaseInfo in d:
            n += d[BaseInfo].n
    return [BaseInfo(n = n)]

# Note: base_asp has empty attr_aspects, yet must propagate along outer_asp's attr_aspects.
base_asp = aspect(implementation = _base_impl)

def _outer_impl(target, ctx):
    vals = [target[BaseInfo].n]
    for d in ctx.rule.attr.deps:
        vals.append(d[BaseInfo].n)
        vals.extend(d[OuterInfo].vals)
    return [OuterInfo(vals = vals)]

outer_asp = aspect(implementation = _outer_impl, attr_aspects = ["deps"], requires = [base_asp])

def _lib(ctx):
    return [DefaultInfo()]
lib = rule(implementation = _lib, attrs = {"deps": attr.label_list()})

def _top(ctx):
    out = ctx.actions.declare_file("out.txt")
    vals = []
    for d in ctx.attr.deps:
        vals.extend(d[OuterInfo].vals)
    ctx.actions.write(out, ",".join([str(v) for v in vals]))
    return [DefaultInfo(files = depset([out]))]
top = rule(implementation = _top, attrs = {"deps": attr.label_list(aspects = [outer_asp])})

def _conflict_rule(ctx):
    f = ctx.actions.declare_file("shared.txt")
    ctx.actions.write(f, "from_rule")
    return [DefaultInfo(files = depset([f]))]
c_rule = rule(implementation = _conflict_rule)

def _conflict_asp(target, ctx):
    f = ctx.actions.declare_file("shared.txt")
    ctx.actions.write(f, "from_asp")
    return []
c_asp = aspect(implementation = _conflict_asp)
c_top = rule(implementation = _top, attrs = {"deps": attr.label_list(aspects = [c_asp])})
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "lib", "top", "c_rule", "c_top")
lib(name = "a")
lib(name = "b", deps = [":a"])
top(name = "t", deps = [":b"])
c_rule(name = "cr")
c_top(name = "ct", deps = [":cr"])
"#,
        ),
    ]);
    let ct = ws.analyze("//:t").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!()
    };
    assert_eq!(String::from_utf8(content.render()).unwrap(), "1,1,1");

    let e_conflict = ws.analyze("//:ct").unwrap_err();
    assert!(e_conflict.contains("conflicting actions"), "{e_conflict}");
}

#[test]
fn output_file_executable_and_platform_exec_properties() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _use_exe(ctx):
    return [DefaultInfo(files = depset([ctx.executable.tool]))]
use_exe = rule(implementation = _use_exe, attrs = {"tool": attr.label(executable = True, cfg = "exec")})
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "use_exe")
genrule(name = "gen", outs = ["tool.sh"], cmd = "echo hi > $@", executable = True)
use_exe(name = "u", tool = ":tool.sh")
platform(name = "p1", exec_properties = {"a": "1", "b": "2"})
platform(name = "p2", parents = [":p1"], exec_properties = {"a": "", "c": "3"})
platform(name = "bad_multi", parents = [":p1", ":p2"])
"#,
        ),
    ]);
    let ct = ws.analyze("//:u").unwrap();
    assert!(ct.default_info.files[0]
        .exec_path
        .path_string()
        .ends_with("tool.sh"));

    let p2 = ws.analyze("//:p2").unwrap();
    let Value::ProviderInstance { fields, .. } = &p2.providers["PlatformInfo"] else {
        panic!()
    };
    let Value::Dict(ep) = &fields["exec_properties"] else {
        panic!()
    };
    let items: BTreeMap<String, String> = ep
        .lock()
        .unwrap()
        .items()
        .into_iter()
        .map(|(k, v)| {
            (
                k.as_str().unwrap().to_string(),
                v.as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(!items.contains_key("a"));
    assert_eq!(items.get("b").map(String::as_str), Some("2"));
    assert_eq!(items.get("c").map(String::as_str), Some("3"));

    assert!(ws
        .analyze("//:bad_multi")
        .unwrap_err()
        .contains("at most one parent"));
}

#[test]
fn alias_of_test_is_not_test_and_records_alias_chain() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r##"
def _t_impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(exe, "#!/bin/sh\nexit 0\n", is_executable = True)
    return [DefaultInfo(executable = exe)]
my_test = rule(implementation = _t_impl, test = True)
"##,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "my_test")
my_test(name = "t", testonly = True)
alias(name = "a1", actual = ":t")
alias(name = "a2", actual = ":a1")
"#,
        ),
    ]);
    let (r, cfg) = ws.resolver_with(
        BuildConfigurationKey::target("k8", "fastbuild"),
        AnalysisConfig::new(),
    );
    let real_t = r
        .analyze(&Label::parse_canonical("//:t").unwrap(), &cfg)
        .unwrap();
    assert!(real_t.is_test);
    assert!(real_t.testonly);
    assert!(!real_t.actions.is_empty());

    let a2 = r
        .analyze(&Label::parse_canonical("//:a2").unwrap(), &cfg)
        .unwrap();
    assert!(!a2.is_test, "alias of a test must not be marked is_test");
    assert!(a2.testonly, "alias must inherit testonly from actual");
    assert!(a2.actions.is_empty());
    assert!(a2.typed_actions.is_empty());
    let Some(Value::ProviderInstance { fields, .. }) = a2.providers.get("AliasProvider") else {
        panic!("expected AliasProvider on alias target");
    };
    assert_eq!(
        fields.get("actual_label"),
        Some(&Value::Label(Label::parse_canonical("//:t").unwrap()))
    );
    assert_eq!(
        fields.get("original_label"),
        Some(&Value::Label(Label::parse_canonical("//:a2").unwrap()))
    );
    let Some(Value::List(chain)) = fields.get("alias_chain") else {
        panic!("expected alias_chain list");
    };
    let chain_labels: Vec<String> = chain
        .lock()
        .unwrap()
        .elements
        .iter()
        .map(|v| v.to_string())
        .collect();
    assert_eq!(chain_labels, vec!["@@//:a2", "@@//:a1"]);
    // Actual's actions remain reachable on the resolved target graph.
    assert!(r
        .analyzed_targets()
        .iter()
        .any(|ct| ct.label == real_t.label && !ct.actions.is_empty()));
}

#[test]
fn genrule_toolchains_make_vars_and_tool_runfiles_inputs() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r##"
def _tv_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".dat")
    ctx.actions.write(out, "data")
    return [
        DefaultInfo(files = depset([out])),
        platform_common.TemplateVariableInfo(vars = {"FROM_ATTR": "attr_val"}),
    ]
tv_rule = rule(implementation = _tv_impl)

def _tc_impl(ctx):
    tool = ctx.actions.declare_file("tc_tool")
    ctx.actions.write(tool, "tc", is_executable = True)
    return [
        DefaultInfo(files = depset([tool])),
        platform_common.ToolchainInfo(bin = tool),
        platform_common.TemplateVariableInfo(vars = {"FROM_TC": "tc_val"}),
    ]
my_tc = rule(implementation = _tc_impl)

def _bin_impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(exe, "#!/bin/sh\n", is_executable = True)
    rf_file = ctx.actions.declare_file(ctx.label.name + ".rf")
    ctx.actions.write(rf_file, "rf")
    rf = ctx.runfiles(files = [rf_file])
    return [DefaultInfo(executable = exe, files = depset([exe]), runfiles = rf)]
my_bin = rule(implementation = _bin_impl, executable = True)

def _plain_impl(ctx):
    return []
plain = rule(implementation = _plain_impl)
"##,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "tv_rule", "my_tc", "my_bin", "plain")
toolchain_type(name = "tt")
my_tc(name = "tc_impl")
toolchain(name = "tc", toolchain_type = ":tt", toolchain = ":tc_impl")
tv_rule(name = "tv")
my_bin(name = "tool_bin")
plain(name = "bad_tc")
genrule(
    name = "g",
    outs = ["g.out"],
    tools = [":tool_bin"],
    toolchains = [":tv", ":tt"],
    cmd = "echo $(FROM_ATTR) $(FROM_TC) $(MY_DEF) $(location :tv) $(location :tc_impl) $(location :tool_bin) > $@",
)
genrule(
    name = "bad_g",
    outs = ["bad.out"],
    toolchains = [":bad_tc"],
    cmd = "echo hi > $@",
)
"#,
        ),
    ]);
    let mut acfg = AnalysisConfig::new();
    acfg.registered_toolchains = vec![Label::parse_canonical("//:tc").unwrap()];
    let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
    cfg.user_defines
        .insert("MY_DEF".to_string(), "def_val".to_string());
    let (r, cfg) = ws.resolver_with(cfg, acfg);
    let ct = r
        .analyze(&Label::parse_canonical("//:g").unwrap(), &cfg)
        .unwrap();
    let (_, argv, inputs, _) = spawn_of(&ct, 0);
    assert!(
        argv[2].contains("attr_val tc_val def_val bazel-out/k8-fastbuild/bin/tv.dat bazel-out/k8-fastbuild/bin/tc_tool bazel-out/"),
        "{}",
        argv[2]
    );
    // Tool executable + runfiles manifest (not individual runfiles file) + toolchain tools are inputs.
    assert!(
        inputs
            .iter()
            .any(|p| p.ends_with("tool_bin.runfiles_manifest")),
        "{inputs:?}"
    );
    assert!(
        !inputs.iter().any(|p| p.ends_with("tool_bin.rf")),
        "{inputs:?}"
    );
    assert!(inputs.iter().any(|p| p.ends_with("tv.dat")), "{inputs:?}");
    assert!(inputs.iter().any(|p| p.ends_with("tc_tool")), "{inputs:?}");

    let err = r
        .analyze(&Label::parse_canonical("//:bad_g").unwrap(), &cfg)
        .unwrap_err();
    assert!(
        err.contains("TemplateVariableInfo") && err.contains("ToolchainTypeInfo"),
        "{err}"
    );
}

#[test]
fn external_bzl_helper_rule_resolves_toolchains_in_defining_repo() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let main_root = PathFragment::create("/ws");
    let bt_root = PathFragment::create("/bt");
    let plat_root = PathFragment::create("/platforms");
    for d in [
        "/ws",
        "/bt/tools/launcher",
        "/platforms/host",
        "/platforms/os",
        "/platforms/cpu",
    ] {
        fs.create_directory_and_parents(&PathFragment::create(d))
            .unwrap();
    }
    fs.write_file(
        &PathFragment::create("/platforms/host/BUILD"),
        b"package(default_visibility = ['//visibility:public'])\nplatform(name = 'host')\n",
    )
    .unwrap();
    fs.write_file(&PathFragment::create("/platforms/os/BUILD"), b"")
        .unwrap();
    fs.write_file(&PathFragment::create("/platforms/cpu/BUILD"), b"")
        .unwrap();
    fs.write_file(&PathFragment::create("/bt/tools/BUILD"), b"")
        .unwrap();
    fs.write_file(
        &PathFragment::create("/bt/tools/build_defs.bzl"),
        br#"
def _current_toolchain_base_impl(ctx, *, toolchain_type):
    executable = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.symlink(output = executable, target_file = ctx.toolchains[toolchain_type].binary)
    return DefaultInfo(executable = executable)

def _make_current_toolchain_rule(toolchain_type):
    return rule(
        implementation = lambda ctx: _current_toolchain_base_impl(ctx, toolchain_type = toolchain_type),
        toolchains = [toolchain_type],
        executable = True,
    )

current_launcher_binary = _make_current_toolchain_rule("//tools/launcher:launcher_toolchain_type")

def _tc_impl(ctx):
    return [platform_common.ToolchainInfo(binary = ctx.file.binary)]

tc_rule = rule(implementation = _tc_impl, attrs = {"binary": attr.label(allow_single_file = True, mandatory = True)})
"#,
    )
    .unwrap();
    fs.write_file(
        &PathFragment::create("/bt/tools/launcher/BUILD"),
        br#"
load("//tools:build_defs.bzl", "current_launcher_binary", "tc_rule")
toolchain_type(name = "launcher_toolchain_type")
tc_rule(name = "empty_impl", binary = "empty.sh")
toolchain(name = "3_no_launcher_toolchain", toolchain_type = ":launcher_toolchain_type", toolchain = ":empty_impl")
current_launcher_binary(name = "launcher")
"#,
    )
    .unwrap();
    fs.write_file(
        &PathFragment::create("/bt/tools/launcher/empty.sh"),
        b"#!/bin/sh\n",
    )
    .unwrap();
    fs.write_file(&PathFragment::create("/ws/BUILD"), b"")
        .unwrap();

    let mut repo_resolver = SimpleRepoResolver::new(main_root);
    repo_resolver.external.insert("bazel_tools".into(), bt_root);
    repo_resolver.external.insert("platforms".into(), plat_root);
    repo_resolver
        .apparent
        .insert("bazel_tools".into(), RepositoryName::bazel_tools());
    repo_resolver.apparent.insert(
        "platforms".into(),
        RepositoryName::create("platforms").unwrap(),
    );
    let lc = LoaderConfig::new(fs, Arc::new(repo_resolver));
    let mut acfg = AnalysisConfig::new();
    acfg.registered_toolchains =
        vec![
            Label::parse_canonical("@@bazel_tools//tools/launcher:3_no_launcher_toolchain")
                .unwrap(),
        ];
    let r = TargetResolver::new(PackageLoader::new(lc), acfg);
    let ct = r
        .analyze(
            &Label::parse_canonical("@@bazel_tools//tools/launcher:launcher").unwrap(),
            &BuildConfigurationKey::target("k8", "fastbuild"),
        )
        .unwrap();
    assert_eq!(
        ct.default_info
            .executable
            .as_ref()
            .unwrap()
            .root_relative_path
            .path_string(),
        "external/bazel_tools/tools/launcher/launcher"
    );
}

#[test]
fn string_set_and_transition_validation_parity() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
SetValInfo = provider(fields = ["items", "flag", "i"])

def _ss_impl(ctx):
    return [SetValInfo(items = sorted(list(ctx.build_setting_value)), flag = False, i = 0)]

string_set_flag = rule(
    implementation = _ss_impl,
    build_setting = config.string_set(flag = True),
)

def _b_impl(ctx):
    return [SetValInfo(items = [], flag = ctx.build_setting_value, i = 0)]

bool_flag = rule(implementation = _b_impl, build_setting = config.bool(flag = True))

def _i_impl(ctx):
    return [SetValInfo(items = [], flag = False, i = ctx.build_setting_value)]

int_flag = rule(implementation = _i_impl, build_setting = config.int(flag = True))

def _trans_impl(settings, attr):
    return [{
        "//:ss": set(["b", "a", "b", attr.name]),
        "//:bf": 1,
        "//:iflag": 42,
    }]

my_trans = transition(
    implementation = _trans_impl,
    inputs = [],
    outputs = ["//:ss", "//:bf", "//:iflag"],
)

def _split_list_impl(settings, attr):
    return [
        {"//command_line_option:cpu": "arm64"},
        {"//command_line_option:cpu": "x86_64"},
    ]

split_list_t = transition(
    implementation = _split_list_impl,
    inputs = [],
    outputs = ["//command_line_option:cpu"],
)

def _leaf(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, out.path)
    return [DefaultInfo(files = depset([out]))]

leaf = rule(implementation = _leaf)

def _holder(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".out")
    ss = ctx.attr.ss[SetValInfo].items
    bf = ctx.attr.bf[SetValInfo].flag
    iv = ctx.attr.iflag[SetValInfo].i
    keys = sorted(ctx.split_attr.leaves.keys())
    ctx.actions.write(out, ",".join(ss) + "|" + str(bf) + "|" + str(iv) + "|" + ",".join(keys))
    return [DefaultInfo(files = depset([out]))]

holder = rule(
    implementation = _holder,
    cfg = my_trans,
    attrs = {
        "ss": attr.label(default = "//:ss"),
        "bf": attr.label(default = "//:bf"),
        "iflag": attr.label(default = "//:iflag"),
        "leaves": attr.label(cfg = split_list_t),
    },
)

bad_rule_split = rule(implementation = _leaf, cfg = split_list_t)

def _overflow_impl(settings, attr):
    return {"//:iflag": 1 << 40}

overflow_t = transition(implementation = _overflow_impl, inputs = [], outputs = ["//:iflag"])
overflow_rule = rule(implementation = _leaf, cfg = overflow_t)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "string_set_flag", "bool_flag", "int_flag", "leaf", "holder", "bad_rule_split", "overflow_rule")
string_set_flag(name = "ss", build_setting_default = set(["x"]))
bool_flag(name = "bf", build_setting_default = False)
int_flag(name = "iflag", build_setting_default = 0)
leaf(name = "lf")
holder(name = "h", leaves = ":lf")
bad_rule_split(name = "bad_split")
overflow_rule(name = "bad_int")
"#,
        ),
    ]);
    let ct = ws.analyze("//:h").unwrap();
    let zel_analysis::ActionAnalysisMetadata::FileWriteAction { content, .. } = &ct.actions[0]
    else {
        panic!("expected FileWriteAction");
    };
    assert_eq!(std::str::from_utf8(content).unwrap(), "a,b,h|True|42|0,1");

    let err_split = ws.analyze("//:bad_split").unwrap_err();
    assert!(
        err_split.contains(
            "Rule transition only allowed to return a single transitioned configuration."
        ),
        "{err_split}"
    );
    let err_int = ws.analyze("//:bad_int").unwrap_err();
    assert!(err_int.contains("signed 32-bit int"), "{err_int}");
}

#[test]
fn aspect_rule_attr_params_none_rejection_and_coverage_merge() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
ParamInfo = provider(fields = ["summary"])

def _param_asp_impl(target, ctx):
    cov = coverage_common.instrumented_files_info(
        ctx,
        source_attributes = ["srcs"],
        extensions = [],
    )
    return [
        ParamInfo(summary = ctx.attr.mode + ":" + str(ctx.attr.count)),
        cov,
    ]

param_asp = aspect(
    implementation = _param_asp_impl,
    attrs = {
        "mode": attr.string(default = "slow", values = ["slow", "fast"]),
        "count": attr.int(default = 1, values = [1, 2]),
    },
)

def _none_asp_impl(target, ctx):
    pass

none_asp = aspect(implementation = _none_asp_impl)

def _lib(ctx):
    cov = coverage_common.instrumented_files_info(
        ctx,
        source_attributes = ["srcs"],
        extensions = [],
    )
    return [cov]

lib = rule(implementation = _lib, attrs = {"srcs": attr.label_list(allow_files = True)})

def _consumer(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, ctx.attr.dep[ParamInfo].summary)
    return [DefaultInfo(files = depset([out]))]

consumer = rule(
    implementation = _consumer,
    attrs = {
        "dep": attr.label(aspects = [param_asp]),
        "mode": attr.string(default = "fast"),
        "count": attr.int(default = 2),
    },
)

bad_consumer = rule(
    implementation = _consumer,
    attrs = {"dep": attr.label(aspects = [none_asp])},
)
"#,
        ),
        ("a.cc", "int x = 0;\n"),
        (
            "BUILD",
            r#"
load(":defs.bzl", "lib", "consumer", "bad_consumer")
lib(name = "l", srcs = ["a.cc"])
consumer(name = "c", dep = ":l")
consumer(name = "bad_val", dep = ":l", count = 99)
bad_consumer(name = "bad_none", dep = ":l")
"#,
        ),
    ]);
    let mut cov_cfg = BuildConfigurationKey::target("k8", "fastbuild");
    cov_cfg
        .options
        .insert("collect_code_coverage".into(), "true".into());
    let ct = ws
        .resolver()
        .analyze(&Label::parse_canonical("//:c").unwrap(), &cov_cfg)
        .unwrap();
    let zel_analysis::ActionAnalysisMetadata::FileWriteAction { content, .. } = &ct.actions[0]
    else {
        panic!("expected FileWriteAction");
    };
    assert_eq!(std::str::from_utf8(content).unwrap(), "fast:2");

    let l_ct = ws
        .resolver()
        .analyze(&Label::parse_canonical("//:l").unwrap(), &cov_cfg)
        .unwrap();
    assert!(
        l_ct.instrumented_files
            .as_ref()
            .unwrap()
            .instrumented_files
            .is_empty(),
        "empty extensions = [] must match zero source files"
    );

    let bad_val = ws.analyze("//:bad_val").unwrap_err();
    assert!(bad_val.contains("has to be one of"), "{bad_val}");

    let bad_none = ws.analyze("//:bad_none").unwrap_err();
    assert!(
        bad_none.contains(
            "Aspect implementation should return a list, or a provider instance, but got NoneType"
        ),
        "{bad_none}"
    );
}

#[test]
fn subrule_bin_root_implicit_override_template_dict_and_positional_analysis_test() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _sub_impl(ctx, *, _helper):
    f = ctx.actions.declare_file(ctx.label.name + "_sub.txt")
    ctx.actions.write(f, str(_helper.label))
    return f

my_sub = subrule(
    implementation = _sub_impl,
    attrs = {"_helper": attr.label(default = "//:helper")},
)

def _upper(s):
    return s.upper()

def _gen_rule_impl(ctx):
    sub_f = my_sub()
    td = ctx.actions.template_dict()
    td.add_joined("{ITEMS}", depset(["x", "y", "x"]), join_with = ",", map_each = _upper, uniquify = True)
    out = ctx.actions.declare_file(ctx.label.name + "_main.txt")
    ctx.actions.expand_template(
        template = ctx.file.tpl,
        output = out,
        substitutions = {},
        computed_substitutions = td,
    )
    return [DefaultInfo(files = depset([sub_f, out]))]

gen_rule = rule(
    implementation = _gen_rule_impl,
    output_to_genfiles = True,
    subrules = [my_sub],
    attrs = {"tpl": attr.label(allow_single_file = True)},
)

def _bad_sub_override_impl(ctx):
    my_sub(_helper = None)
    return []

bad_sub_override = rule(implementation = _bad_sub_override_impl, subrules = [my_sub])

def _atest_impl(ctx):
    return [AnalysisTestResultInfo(True, "all good")]

pos_analysis_test = rule(implementation = _atest_impl, analysis_test = True)
"#,
        ),
        ("in.tpl", "vals={ITEMS}\n"),
        (
            "BUILD",
            r#"
load(":defs.bzl", "gen_rule", "bad_sub_override", "pos_analysis_test")
filegroup(name = "helper", srcs = ["in.tpl"])
gen_rule(name = "g", tpl = "in.tpl")
bad_sub_override(name = "bad_override")
pos_analysis_test(name = "at")
"#,
        ),
    ]);

    let ct = ws.analyze("//:g").unwrap();
    assert_eq!(
        ct.default_info.files[0].root,
        zel_analysis::ArtifactRoot::Bin
    );
    assert_eq!(
        ct.default_info.files[1].root,
        zel_analysis::ArtifactRoot::Genfiles
    );

    let err = ws.analyze("//:bad_override").unwrap_err();
    assert!(
        err.contains("implicit dependency and cannot be overridden"),
        "{err}"
    );

    let at = ws.analyze("//:at").unwrap();
    assert!(at.default_info.executable.is_some());
}

#[test]
fn empty_list_transition_drops_attr_dep_and_rejects_rule_transition() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _empty_list_impl(settings, attr):
    return []

empty_list_t = transition(
    implementation = _empty_list_impl,
    inputs = [],
    outputs = ["//command_line_option:cpu"],
)

def _empty_dict_impl(settings, attr):
    return {}

empty_dict_t = transition(
    implementation = _empty_dict_impl,
    inputs = [],
    outputs = ["//command_line_option:cpu"],
)

def _boom_impl(ctx):
    fail("boom_dep should never be analyzed when dropped by [] split transition")

boom_rule = rule(implementation = _boom_impl)

def _holder_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(
        out,
        "{}:{}:{}".format(
            len(ctx.attr.dep),
            len(ctx.split_attr.dep),
            len(ctx.attr.dict_deps),
        ),
    )
    return [DefaultInfo(files = depset([out]))]

holder = rule(
    implementation = _holder_impl,
    cfg = empty_dict_t,
    attrs = {
        "dep": attr.label(cfg = empty_list_t),
        "dict_deps": attr.string_keyed_label_dict(cfg = empty_list_t),
    },
)

bad_empty_rule = rule(implementation = _holder_impl, cfg = empty_list_t)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "boom_rule", "holder", "bad_empty_rule")
boom_rule(name = "boom")
holder(name = "ok_drop", dep = ":boom", dict_deps = {":boom": "x"})
bad_empty_rule(name = "bad_rule")
"#,
        ),
    ]);

    let ct = ws.analyze("//:ok_drop").unwrap();
    let zel_analysis::ActionAnalysisMetadata::FileWriteAction { content, .. } = &ct.actions[0]
    else {
        panic!("expected FileWriteAction");
    };
    assert_eq!(std::str::from_utf8(content).unwrap(), "0:0:0");

    let err = ws.analyze("//:bad_rule").unwrap_err();
    assert!(
        err.contains("Rule transition only allowed to return a single transitioned configuration."),
        "{err}"
    );
}

#[test]
fn filegroup_missing_output_group_returns_empty_and_collects_present_members() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _no_iface_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".out")
    other = ctx.actions.declare_file(ctx.label.name + ".other")
    ctx.actions.write(out, "out")
    ctx.actions.write(other, "other")
    return [
        DefaultInfo(files = depset([out])),
        OutputGroupInfo(other_group = depset([other])),
    ]

def _with_iface_impl(ctx):
    dll = ctx.actions.declare_file(ctx.label.name + ".dll")
    iflib = ctx.actions.declare_file(ctx.label.name + ".if.lib")
    ctx.actions.write(dll, "dll")
    ctx.actions.write(iflib, "iflib")
    return [
        DefaultInfo(files = depset([dll])),
        OutputGroupInfo(interface_library = depset([iflib])),
    ]

no_iface = rule(implementation = _no_iface_impl)
with_iface = rule(implementation = _with_iface_impl)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "no_iface", "with_iface")
no_iface(name = "plain")
with_iface(name = "dll_target")
filegroup(
    name = "missing_only",
    srcs = [":plain"],
    output_group = "interface_library",
)
filegroup(
    name = "mixed_srcs",
    srcs = [":plain", ":dll_target"],
    output_group = "interface_library",
)
"#,
        ),
    ]);

    let missing = ws.analyze("//:missing_only").unwrap();
    assert!(
        missing.default_info.files.is_empty(),
        "expected empty files for missing output_group, got {:?}",
        missing.default_info.files
    );

    let mixed = ws.analyze("//:mixed_srcs").unwrap();
    let paths: Vec<String> = mixed
        .default_info
        .files
        .iter()
        .map(|f| f.root_relative_path.path_string().to_string())
        .collect();
    assert_eq!(paths, vec!["dll_target.if.lib"]);
}

#[test]
fn filegroup_rejects_internal_output_group_suffix() {
    let ws = Ws::new(&[
        ("a.txt", "hello"),
        (
            "BUILD",
            r#"
filegroup(
    name = "fg_hidden",
    srcs = [],
    output_group = "_hidden_top_level_INTERNAL_",
)
filegroup(
    name = "fg_bare_suffix",
    srcs = ["a.txt"],
    output_group = "_INTERNAL_",
)
"#,
        ),
    ]);

    let err_hidden = ws.analyze("//:fg_hidden").unwrap_err();
    assert!(
        err_hidden.contains(
            "in output_group attribute of filegroup rule //:fg_hidden: Output group _hidden_top_level_INTERNAL_ is not permitted for reference in filegroups."
        ),
        "unexpected error: {err_hidden}"
    );

    let err_bare = ws.analyze("//:fg_bare_suffix").unwrap_err();
    assert!(
        err_bare.contains(
            "in output_group attribute of filegroup rule //:fg_bare_suffix: Output group _INTERNAL_ is not permitted for reference in filegroups."
        ),
        "unexpected error: {err_bare}"
    );
}

const CC_COMMON_TEST_HELPERS_BZL: &str = r#"
def make_cc_toolchain(
        ctx,
        *,
        cpu,
        compiler = "clang",
        feature_names = [],
        enabled_features = [],
        action_configs = [],
        artifact_name_patterns = [],
        tools_directory = "external/test_cc_tc",
        if_so_builder = None):
    features = [
        struct(
            name = fname,
            enabled = fname in enabled_features,
            flag_sets = [],
            env_sets = [],
            requires = [],
            implies = [],
            provides = [],
        )
        for fname in feature_names
    ]
    config_info = struct(
        _features_DO_NOT_USE = features,
        _action_configs_DO_NOT_USE = action_configs,
        _artifact_name_patterns_DO_NOT_USE = artifact_name_patterns,
    )
    toolchain_features = cc_common.cc_toolchain_features(
        toolchain_config_info = config_info,
        tools_directory = tools_directory,
    )
    solib_dir = "_solib_" + cpu
    return cc_common.CcToolchainInfo(
        needs_pic_for_dynamic_libraries = lambda *, feature_configuration: cc_common.is_enabled(
            feature_configuration = feature_configuration,
            feature_name = "supports_pic",
        ),
        built_in_include_directories = [],
        all_files = depset(),
        static_runtime_lib = lambda *, feature_configuration: depset(),
        dynamic_runtime_lib = lambda *, feature_configuration: depset(),
        sysroot = None,
        compiler = compiler,
        libc = "local",
        cpu = cpu,
        target_gnu_system_name = cpu,
        toolchain_id = "test-" + cpu,
        dynamic_runtime_solib_dir = ctx.bin_dir.path + "/" + solib_dir,
        objcopy_executable = "",
        compiler_executable = "",
        preprocessor_executable = "",
        nm_executable = "",
        objdump_executable = "",
        ar_executable = "",
        strip_executable = "",
        ld_executable = "",
        gcov_executable = "",
        generate_modmap = None,
        _as_files = depset(),
        _ar_files = depset(),
        _strip_files = depset(),
        _tool_paths = {},
        _solib_dir = solib_dir,
        _linker_files = depset(),
        _coverage_files = depset(),
        _fdo_context = struct(
            branch_fdo_profile = None,
            propeller_optimize_info = None,
            memprof_profile_artifact = None,
            afdo_instrument = None,
            cs_fdo_instrument = None,
        ),
        _compiler_files = depset(),
        _dwp_files = depset(),
        _builtin_include_files = [],
        _legacy_cc_flags_make_variable = "",
        _additional_make_variables = {},
        _all_files_including_libc = depset(),
        _abi = "local",
        _abi_glibc_version = "local",
        _crosstool_top_path = tools_directory,
        _build_info_files = None,
        _supports_header_parsing = False,
        _supports_param_files = False,
        _toolchain_features = toolchain_features,
        _toolchain_label = ctx.label,
        _cpp_configuration = ctx.fragments.cpp,
        _link_dynamic_library_tool = None,
        _grep_includes = None,
        _if_so_builder = if_so_builder,
        _is_tool_configuration = False,
        _is_sibling_repository_layout = ctx.configuration.is_sibling_repository_layout(),
        _stamp_binaries = False,
        _static_runtime_lib_depset = None,
        _dynamic_runtime_lib_depset = None,
        _compiler_files_without_includes = depset(),
        _build_variables_dict = {},
        _build_variables = cc_common.cc_toolchain_variables(vars = {}),
        _allowlist_for_layering_check = None,
        _cc_info = struct(compilation_context = cc_common.create_compilation_context()),
        _objcopy_files = depset(),
        _aggregate_ddi = None,
        _extra_allowlisted_feature_layering_check_macros = [],
        _force_layering_check_features = False,
    )

WINDOWS_ARTIFACT_PATTERNS = [
    struct(category_name = "object_file", prefix = "", extension = ".obj"),
    struct(category_name = "static_library", prefix = "", extension = ".lib"),
    struct(category_name = "alwayslink_static_library", prefix = "", extension = ".lo.lib"),
    struct(category_name = "executable", prefix = "", extension = ".exe"),
    struct(category_name = "dynamic_library", prefix = "", extension = ".dll"),
    struct(category_name = "interface_library", prefix = "", extension = ".if.lib"),
]
"#;

struct LocalWs {
    dir: std::path::PathBuf,
    root: PathFragment,
}

impl LocalWs {
    fn new(tag: &str, files: &[(&str, &str)]) -> Self {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "zel-cc-common-test-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (rel, contents) in files {
            let full = dir.join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&full, contents.as_bytes()).unwrap();
        }
        let root = PathFragment::create(&dir.to_string_lossy());
        Self { dir, root }
    }

    fn resolver_with(
        &self,
        cfg: BuildConfigurationKey,
        acfg: AnalysisConfig,
    ) -> (Arc<TargetResolver>, BuildConfigurationKey) {
        let lc = LoaderConfig::new(
            Arc::new(zel_vfs::LocalFileSystem::default()),
            Arc::new(SimpleRepoResolver::new(self.root.clone())),
        );
        let loader = PackageLoader::new(lc);
        assert_eq!(loader.builtins_error(), None);
        (TargetResolver::new(loader, acfg), cfg)
    }

    fn analyze(&self, label: &str) -> Result<Arc<ConfiguredTarget>, String> {
        let (r, cfg) = self.resolver_with(
            BuildConfigurationKey::target("k8", "fastbuild"),
            AnalysisConfig::new(),
        );
        r.analyze(&Label::parse_canonical(label).unwrap(), &cfg)
    }
}

impl Drop for LocalWs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn builtin_cc_common_create_library_to_link_unix_and_windows() {
    let ws = LocalWs::new(
        "ltl",
        &[
            ("tc_helpers.bzl", CC_COMMON_TEST_HELPERS_BZL),
            (
                "defs.bzl",
                r#"
load(":tc_helpers.bzl", "WINDOWS_ARTIFACT_PATTERNS", "make_cc_toolchain")

LibResultInfo = provider(fields = [
    "dyn_short",
    "resolved_dyn_short",
    "iface_short",
    "resolved_iface_short",
    "alwayslink",
])

def _unix_impl(ctx):
    tc = make_cc_toolchain(
        ctx,
        cpu = "k8",
        feature_names = ["supports_dynamic_linker", "supports_interface_shared_libraries"],
        enabled_features = ["supports_dynamic_linker", "supports_interface_shared_libraries"],
    )
    fc = cc_common.configure_features(
        ctx = ctx,
        cc_toolchain = tc,
        requested_features = ctx.features,
        unsupported_features = ctx.disabled_features,
    )
    static_a = ctx.actions.declare_file("libfoo.a")
    dyn_so = ctx.actions.declare_file("libfoo.so")
    iface_so = ctx.actions.declare_file("libfoo.ifso")
    ctx.actions.write(static_a, "a")
    ctx.actions.write(dyn_so, "so")
    ctx.actions.write(iface_so, "ifso")

    ltl = cc_common.create_library_to_link(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        static_library = static_a,
        dynamic_library = dyn_so,
        interface_library = iface_so,
        alwayslink = True,
    )
    return [
        DefaultInfo(files = depset([ltl.dynamic_library, ltl.interface_library])),
        LibResultInfo(
            dyn_short = ltl.dynamic_library.short_path,
            resolved_dyn_short = ltl.resolved_symlink_dynamic_library.short_path,
            iface_short = ltl.interface_library.short_path,
            resolved_iface_short = ltl.resolved_symlink_interface_library.short_path,
            alwayslink = ltl.alwayslink,
        ),
    ]

def _win_impl(ctx):
    tc = make_cc_toolchain(
        ctx,
        cpu = "x64_windows",
        compiler = "msvc-cl",
        feature_names = [
            "targets_windows",
            "supports_dynamic_linker",
            "supports_interface_shared_libraries",
        ],
        enabled_features = [
            "targets_windows",
            "supports_dynamic_linker",
            "supports_interface_shared_libraries",
        ],
        artifact_name_patterns = WINDOWS_ARTIFACT_PATTERNS,
    )
    fc = cc_common.configure_features(
        ctx = ctx,
        cc_toolchain = tc,
        requested_features = ctx.features,
        unsupported_features = ctx.disabled_features,
    )
    dll = ctx.actions.declare_file("foo.dll")
    iflib = ctx.actions.declare_file("foo.if.lib")
    ctx.actions.write(dll, "dll")
    ctx.actions.write(iflib, "iflib")

    ltl = cc_common.create_library_to_link(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        dynamic_library = dll,
        interface_library = iflib,
    )
    return [
        DefaultInfo(files = depset([ltl.dynamic_library, ltl.interface_library])),
        LibResultInfo(
            dyn_short = ltl.dynamic_library.short_path,
            resolved_dyn_short = "NONE" if ltl.resolved_symlink_dynamic_library == None else ltl.resolved_symlink_dynamic_library.short_path,
            iface_short = ltl.interface_library.short_path,
            resolved_iface_short = "NONE" if ltl.resolved_symlink_interface_library == None else ltl.resolved_symlink_interface_library.short_path,
            alwayslink = ltl.alwayslink,
        ),
    ]

def _bad_ext_impl(ctx):
    tc = make_cc_toolchain(
        ctx,
        cpu = "k8",
        feature_names = ["supports_dynamic_linker"],
        enabled_features = ["supports_dynamic_linker"],
    )
    fc = cc_common.configure_features(ctx = ctx, cc_toolchain = tc)
    bad = ctx.actions.declare_file("libfoo.invalid_ext")
    ctx.actions.write(bad, "bad")
    cc_common.create_library_to_link(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        dynamic_library = bad,
    )
    return []

unix_ltl = rule(implementation = _unix_impl, fragments = ["cpp"])
win_ltl = rule(implementation = _win_impl, fragments = ["cpp"])
bad_ext_ltl = rule(implementation = _bad_ext_impl, fragments = ["cpp"])
"#,
            ),
            (
                "BUILD",
                r#"
load(":defs.bzl", "bad_ext_ltl", "unix_ltl", "win_ltl")
unix_ltl(name = "unix_case")
win_ltl(name = "win_case")
bad_ext_ltl(name = "bad_ext_case")
"#,
            ),
        ],
    );

    let unix_ct = ws.analyze("//:unix_case").unwrap();
    let Value::ProviderInstance { fields, .. } = &unix_ct.providers["//:defs.bzl%LibResultInfo"]
    else {
        panic!("expected LibResultInfo on unix_case");
    };
    assert_eq!(
        fields["dyn_short"].as_str(),
        Some("_solib_k8/_U_S_S_Cunix_Ucase___U/libfoo.so")
    );
    assert_eq!(fields["resolved_dyn_short"].as_str(), Some("libfoo.so"));
    assert_eq!(
        fields["iface_short"].as_str(),
        Some("_solib_k8/_U_S_S_Cunix_Ucase___U/libfoo.ifso")
    );
    assert_eq!(fields["resolved_iface_short"].as_str(), Some("libfoo.ifso"));
    assert_eq!(fields["alwayslink"], Value::Bool(true));
    assert_eq!(
        unix_ct
            .typed_actions
            .iter()
            .filter(|a| matches!(a.kind, ActionKind::Symlink { .. }))
            .count(),
        2
    );

    let win_ct = ws.analyze("//:win_case").unwrap();
    let Value::ProviderInstance {
        fields: wfields, ..
    } = &win_ct.providers["//:defs.bzl%LibResultInfo"]
    else {
        panic!("expected LibResultInfo on win_case");
    };
    assert_eq!(wfields["dyn_short"].as_str(), Some("foo.dll"));
    assert_eq!(wfields["resolved_dyn_short"].as_str(), Some("NONE"));
    assert_eq!(wfields["iface_short"].as_str(), Some("foo.if.lib"));
    assert_eq!(wfields["resolved_iface_short"].as_str(), Some("NONE"));
    assert_eq!(wfields["alwayslink"], Value::Bool(false));
    assert_eq!(
        win_ct
            .typed_actions
            .iter()
            .filter(|a| matches!(a.kind, ActionKind::Symlink { .. }))
            .count(),
        0
    );

    let bad_err = ws.analyze("//:bad_ext_case").unwrap_err();
    assert!(
        bad_err.contains("does not have any of the allowed extensions"),
        "expected extension validation error, got: {bad_err}"
    );
}

#[test]
fn builtin_cc_common_create_library_to_link_rejects_solib_interface_library() {
    let ws = LocalWs::new(
        "solib-iface",
        &[
            ("tc_helpers.bzl", CC_COMMON_TEST_HELPERS_BZL),
            (
                "defs.bzl",
                r#"
load(":tc_helpers.bzl", "make_cc_toolchain")

def _make_tc_and_fc(ctx):
    tc = make_cc_toolchain(
        ctx,
        cpu = "k8",
        feature_names = ["supports_dynamic_linker", "supports_interface_shared_libraries"],
        enabled_features = ["supports_dynamic_linker", "supports_interface_shared_libraries"],
    )
    fc = cc_common.configure_features(ctx = ctx, cc_toolchain = tc)
    return tc, fc

def _solib_iface_only_impl(ctx):
    tc, fc = _make_tc_and_fc(ctx)
    raw_iface = ctx.actions.declare_file("libonly.ifso")
    ctx.actions.write(raw_iface, "ifso")
    first = cc_common.create_library_to_link(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        interface_library = raw_iface,
    )
    # Passing the _solib_ symlink back with dynamic_library = None and custom interface_library_symlink_path.
    cc_common.create_library_to_link(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        interface_library = first.interface_library,
        interface_library_symlink_path = "custom/libonly.ifso",
    )
    return []

def _solib_iface_with_valid_dyn_impl(ctx):
    tc, fc = _make_tc_and_fc(ctx)
    raw_dyn = ctx.actions.declare_file("libvalid.so")
    raw_iface = ctx.actions.declare_file("libbad.ifso")
    ctx.actions.write(raw_dyn, "so")
    ctx.actions.write(raw_iface, "ifso")
    first = cc_common.create_library_to_link(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        interface_library = raw_iface,
    )
    # Both dynamic_library (valid) and interface_library (_solib_ symlink) are set:
    # the error must name interface_library and libbad.ifso, not dynamic_library or libvalid.so.
    cc_common.create_library_to_link(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        dynamic_library = raw_dyn,
        interface_library = first.interface_library,
        interface_library_symlink_path = "custom/libbad.ifso",
    )
    return []

solib_iface_only = rule(implementation = _solib_iface_only_impl, fragments = ["cpp"])
solib_iface_with_valid_dyn = rule(implementation = _solib_iface_with_valid_dyn_impl, fragments = ["cpp"])
"#,
            ),
            (
                "BUILD",
                r#"
load(":defs.bzl", "solib_iface_only", "solib_iface_with_valid_dyn")
solib_iface_only(name = "iface_only")
solib_iface_with_valid_dyn(name = "iface_with_dyn")
"#,
            ),
        ],
    );

    let err_only = ws.analyze("//:iface_only").unwrap_err();
    assert!(
        err_only.contains(
            "interface_library must not be a symbolic link in the solib directory. Got '_solib_k8/_U_S_S_Ciface_Uonly___U/libonly.ifso'"
        ),
        "expected solib interface_library error when dynamic_library is None, got: {err_only}"
    );
    assert!(
        !err_only.contains("NoneType"),
        "unexpected NoneType crash: {err_only}"
    );

    let err_both = ws.analyze("//:iface_with_dyn").unwrap_err();
    assert!(
        err_both.contains(
            "interface_library must not be a symbolic link in the solib directory. Got '_solib_k8/_U_S_S_Ciface_Uwith_Udyn___U/libbad.ifso'"
        ),
        "expected solib interface_library error naming libbad.ifso when both are set, got: {err_both}"
    );
    assert!(
        !err_both.contains("libvalid.so"),
        "error message named dynamic_library instead of interface_library: {err_both}"
    );
}

#[test]
fn builtin_cc_common_link_windows_dll_and_interface_library_workflow() {
    let ws = LocalWs::new(
        "win-dll",
        &[
            ("tc_helpers.bzl", CC_COMMON_TEST_HELPERS_BZL),
            ("hellolib.cc", "int greet() { return 42; }\n"),
            ("hellolib.h", "int greet();\n"),
            (
                "hello.cc",
                "#include \"hellolib.h\"\nint main() { return greet(); }\n",
            ),
            (
                "windows_dll_defs.bzl",
                r#"
load(":tc_helpers.bzl", "WINDOWS_ARTIFACT_PATTERNS", "make_cc_toolchain")

CustomCcInfo = provider(fields = ["compilation_context", "linking_context"])

def _msvc_action_configs():
    cl_flags = struct(
        actions = [],
        flag_groups = [struct(
            flags = ["/c", "%{source_file}", "/Fo%{output_file}"],
            iterate_over = None,
            expand_if_available = None,
            expand_if_not_available = None,
            expand_if_true = None,
            expand_if_false = None,
            expand_if_equal = None,
            flag_groups = [],
        )],
        with_features = [],
    )
    implib_group = struct(
        flags = ["/IMPLIB:%{interface_library_output_path}"],
        iterate_over = None,
        expand_if_available = "interface_library_output_path",
        expand_if_not_available = None,
        expand_if_true = None,
        expand_if_false = None,
        expand_if_equal = None,
        flag_groups = [],
    )
    libs_group = struct(
        flags = ["%{libraries_to_link.name}"],
        iterate_over = "libraries_to_link",
        expand_if_available = "libraries_to_link",
        expand_if_not_available = None,
        expand_if_true = None,
        expand_if_false = None,
        expand_if_equal = None,
        flag_groups = [],
    )
    out_group = struct(
        flags = ["/OUT:%{output_execpath}"],
        iterate_over = None,
        expand_if_available = "output_execpath",
        expand_if_not_available = None,
        expand_if_true = None,
        expand_if_false = None,
        expand_if_equal = None,
        flag_groups = [],
    )
    dll_link_flags = struct(
        actions = [],
        flag_groups = [
            struct(
                flags = ["/DLL"],
                iterate_over = None,
                expand_if_available = None,
                expand_if_not_available = None,
                expand_if_true = None,
                expand_if_false = None,
                expand_if_equal = None,
                flag_groups = [],
            ),
            implib_group,
            libs_group,
            out_group,
        ],
        with_features = [],
    )
    exe_link_flags = struct(
        actions = [],
        flag_groups = [libs_group, out_group],
        with_features = [],
    )
    return [
        struct(
            action_name = "c++-compile",
            enabled = True,
            tools = [struct(path = "C:/MSVC/bin/cl.exe", with_features = [], execution_requirements = [])],
            flag_sets = [cl_flags],
            implies = [],
        ),
        struct(
            action_name = "c++-link-dynamic-library",
            enabled = True,
            tools = [struct(path = "C:/MSVC/bin/link.exe", with_features = [], execution_requirements = [])],
            flag_sets = [dll_link_flags],
            implies = [],
        ),
        struct(
            action_name = "c++-link-executable",
            enabled = True,
            tools = [struct(path = "C:/MSVC/bin/link.exe", with_features = [], execution_requirements = [])],
            flag_sets = [exe_link_flags],
            implies = [],
        ),
    ]

def _win_tc(ctx, support_iface = True):
    fnames = [
        "targets_windows",
        "supports_dynamic_linker",
        "has_configured_linker_path",
        "copy_dynamic_libraries_to_binary",
    ]
    if support_iface:
        fnames.append("supports_interface_shared_libraries")
    ifso_tool = ctx.actions.declare_file(ctx.label.name + "_ifso_builder.exe")
    ctx.actions.write(ifso_tool, "", is_executable = True)
    return make_cc_toolchain(
        ctx,
        cpu = "x64_windows",
        compiler = "msvc-cl",
        feature_names = fnames,
        enabled_features = fnames,
        action_configs = _msvc_action_configs(),
        artifact_name_patterns = WINDOWS_ARTIFACT_PATTERNS,
        if_so_builder = ifso_tool,
    )

def _win_dll_impl(ctx):
    tc = _win_tc(ctx, support_iface = ctx.attr.support_iface)
    fc = cc_common.configure_features(
        ctx = ctx,
        cc_toolchain = tc,
        requested_features = ctx.features,
        unsupported_features = ctx.disabled_features,
    )
    comp_ctx, comp_outputs = cc_common.compile(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        srcs = ctx.files.srcs,
        public_hdrs = ctx.files.hdrs,
        name = ctx.label.name,
    )
    main_out = ctx.actions.declare_file(ctx.label.name) if ctx.label.name.endswith(".dll") else None
    link_outputs = cc_common.link(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        compilation_outputs = comp_outputs,
        name = ctx.label.name,
        output_type = "dynamic_library",
        main_output = main_out,
    )
    ltl = link_outputs.library_to_link
    out_groups = {}
    if ltl != None and ltl.interface_library != None:
        out_groups["interface_library"] = depset([ltl.interface_library])
    return [
        DefaultInfo(files = depset([ltl.dynamic_library])),
        OutputGroupInfo(**out_groups),
        CustomCcInfo(compilation_context = comp_ctx, linking_context = None),
    ]

def _win_import_impl(ctx):
    tc = _win_tc(ctx, support_iface = True)
    fc = cc_common.configure_features(ctx = ctx, cc_toolchain = tc)
    ltl = cc_common.create_library_to_link(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        dynamic_library = ctx.file.shared_library,
        interface_library = ctx.file.interface_library,
    )
    linker_input = cc_common.create_linker_input(
        owner = ctx.label,
        libraries = depset([ltl]),
    )
    linking_ctx = cc_common.create_linking_context(
        linker_inputs = depset([linker_input]),
    )
    comp_ctx, _ = cc_common.compile(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        public_hdrs = ctx.files.hdrs,
        name = ctx.label.name,
    )
    return [
        DefaultInfo(files = depset([ctx.file.shared_library])),
        CustomCcInfo(compilation_context = comp_ctx, linking_context = linking_ctx),
    ]

def _win_exe_impl(ctx):
    tc = _win_tc(ctx, support_iface = True)
    fc = cc_common.configure_features(ctx = ctx, cc_toolchain = tc)
    dep_comp = [d[CustomCcInfo].compilation_context for d in ctx.attr.deps]
    dep_link = [d[CustomCcInfo].linking_context for d in ctx.attr.deps if d[CustomCcInfo].linking_context != None]
    _, comp_outputs = cc_common.compile(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        srcs = ctx.files.srcs,
        compilation_contexts = dep_comp,
        name = ctx.label.name,
    )
    link_outputs = cc_common.link(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        compilation_outputs = comp_outputs,
        linking_contexts = dep_link,
        name = ctx.label.name,
        output_type = "executable",
    )
    return [DefaultInfo(executable = link_outputs.executable, files = depset([link_outputs.executable]))]

win_dll = rule(
    implementation = _win_dll_impl,
    fragments = ["cpp"],
    attrs = {
        "srcs": attr.label_list(allow_files = True),
        "hdrs": attr.label_list(allow_files = True),
        "support_iface": attr.bool(default = True),
    },
)

win_dll_import = rule(
    implementation = _win_import_impl,
    fragments = ["cpp"],
    attrs = {
        "hdrs": attr.label_list(allow_files = True),
        "shared_library": attr.label(allow_single_file = True, mandatory = True),
        "interface_library": attr.label(allow_single_file = True, mandatory = True),
    },
)

win_exe = rule(
    implementation = _win_exe_impl,
    executable = True,
    fragments = ["cpp"],
    attrs = {
        "srcs": attr.label_list(allow_files = True),
        "deps": attr.label_list(providers = [CustomCcInfo]),
    },
)

def windows_dll_library(name, srcs = [], hdrs = []):
    dll_name = name + ".dll"
    import_lib_name = name + "_import_lib"
    win_dll(
        name = dll_name,
        srcs = srcs,
        hdrs = hdrs,
    )
    native.filegroup(
        name = import_lib_name,
        srcs = [":" + dll_name],
        output_group = "interface_library",
    )
    win_dll_import(
        name = name,
        hdrs = hdrs,
        shared_library = ":" + dll_name,
        interface_library = ":" + import_lib_name,
    )
"#,
            ),
            (
                "BUILD",
                r#"
load(":windows_dll_defs.bzl", "win_dll", "win_exe", "windows_dll_library")

windows_dll_library(
    name = "hellolib",
    srcs = ["hellolib.cc"],
    hdrs = ["hellolib.h"],
)

win_exe(
    name = "hello",
    srcs = ["hello.cc"],
    deps = [":hellolib"],
)

win_dll(
    name = "no_iface_feature.dll",
    srcs = ["hellolib.cc"],
    hdrs = ["hellolib.h"],
    support_iface = False,
)

filegroup(
    name = "no_iface_feature_import_lib",
    srcs = [":no_iface_feature.dll"],
    output_group = "interface_library",
)
"#,
            ),
        ],
    );

    // 1. DLL target emits both hellolib.dll and hellolib.dll.if.lib via CppLink with MSVC tool path.
    let dll_ct = ws.analyze("//:hellolib.dll").unwrap();
    let link_action = dll_ct
        .typed_actions
        .iter()
        .find(|a| a.mnemonic == "CppLink")
        .expect("expected CppLink action on //:hellolib.dll");
    let (argv, _) = link_action.command_line();
    assert_eq!(argv[0], "C:/MSVC/bin/link.exe");
    assert!(
        argv.iter()
            .any(|arg| arg.starts_with("/IMPLIB:") && arg.ends_with("hellolib.dll.if.lib")),
        "expected /IMPLIB:...hellolib.dll.if.lib in {argv:?}"
    );
    assert!(
        argv.iter()
            .any(|arg| arg.starts_with("/OUT:") && arg.ends_with("hellolib.dll")),
        "expected /OUT:...hellolib.dll in {argv:?}"
    );
    let link_outs: Vec<String> = link_action
        .outputs
        .iter()
        .map(|o| o.root_relative_path.path_string().to_string())
        .collect();
    assert!(
        link_outs.contains(&"hellolib.dll".to_string()),
        "{link_outs:?}"
    );
    assert!(
        link_outs.contains(&"hellolib.dll.if.lib".to_string()),
        "{link_outs:?}"
    );

    // 2. filegroup(..., output_group = "interface_library") extracts hellolib.dll.if.lib.
    let import_fg = ws.analyze("//:hellolib_import_lib").unwrap();
    let fg_files: Vec<String> = import_fg
        .default_info
        .files
        .iter()
        .map(|f| f.root_relative_path.path_string().to_string())
        .collect();
    assert_eq!(fg_files, vec!["hellolib.dll.if.lib"]);

    // 3. Client executable links against hellolib.dll.if.lib (the import library, not raw hellolib.dll).
    let exe_ct = ws.analyze("//:hello").unwrap();
    assert_eq!(
        exe_ct
            .default_info
            .executable
            .as_ref()
            .unwrap()
            .root_relative_path
            .path_string(),
        "hello.exe"
    );
    let exe_link = exe_ct
        .typed_actions
        .iter()
        .find(|a| a.mnemonic == "CppLink")
        .expect("expected CppLink action on //:hello");
    let (exe_argv, _) = exe_link.command_line();
    let exe_inputs: Vec<String> = exe_link
        .inputs
        .iter()
        .map(|i| i.root_relative_path.path_string().to_string())
        .collect();
    assert!(
        exe_inputs
            .iter()
            .any(|p| p.ends_with("hellolib.dll.if.lib")),
        "expected hellolib.dll.if.lib in exe link inputs: {exe_inputs:?}"
    );
    assert!(
        !exe_inputs.iter().any(|p| p.ends_with("hellolib.dll")),
        "raw DLL should not be passed as linker input when import library is provided: {exe_inputs:?}"
    );
    assert!(
        exe_argv.iter().any(|a| a.ends_with("hellolib.dll.if.lib")),
        "expected hellolib.dll.if.lib on exe link command line: {exe_argv:?}"
    );

    // 4. Without supports_interface_shared_libraries, no interface library is emitted and
    //    filegroup(output_group = "interface_library") cleanly resolves to empty.
    let no_feat_fg = ws.analyze("//:no_iface_feature_import_lib").unwrap();
    assert!(no_feat_fg.default_info.files.is_empty());

    // 5. With --nointerface_shared_objects (options["interface_shared_objects"] = "false"),
    //    hellolib.dll suppresses interface library generation as well.
    let mut no_iface_cfg = BuildConfigurationKey::target("k8", "fastbuild");
    no_iface_cfg
        .options
        .insert("interface_shared_objects".into(), "false".into());
    let (no_iface_resolver, cfg_key) = ws.resolver_with(no_iface_cfg, AnalysisConfig::new());
    let no_iface_fg = no_iface_resolver
        .analyze(
            &Label::parse_canonical("//:hellolib_import_lib").unwrap(),
            &cfg_key,
        )
        .unwrap();
    assert!(no_iface_fg.default_info.files.is_empty());
}

#[test]
fn cc_internal_is_tree_artifact_analysis_time_parity() {
    let ws = LocalWs::new(
        "is-tree-artifact",
        &[
            (
                "defs.bzl",
                r#"
ProbeInfo = provider(fields = ["is_tree", "is_reg"])

def _ok_impl(ctx):
    cc_internal = cc_common.internal_DO_NOT_USE()
    tree_dir = ctx.actions.declare_directory(ctx.label.name + "_tree")
    ctx.actions.run_shell(outputs = [tree_dir], command = "mkdir -p " + tree_dir.path)
    reg_file = ctx.actions.declare_file(ctx.label.name + ".cc")
    ctx.actions.write(output = reg_file, content = "")
    return [
        DefaultInfo(files = depset([tree_dir, reg_file])),
        ProbeInfo(
            is_tree = cc_internal.is_tree_artifact(tree_dir),
            is_reg = cc_internal.is_tree_artifact(reg_file),
        ),
    ]

probe_ok = rule(implementation = _ok_impl)

def _bad_named_artifact_impl(ctx):
    cc_internal = cc_common.internal_DO_NOT_USE()
    tree_dir = ctx.actions.declare_directory(ctx.label.name + "_tree")
    ctx.actions.run_shell(outputs = [tree_dir], command = "mkdir -p " + tree_dir.path)
    cc_internal.is_tree_artifact(artifact = tree_dir)
    return []

probe_bad_named_artifact = rule(implementation = _bad_named_artifact_impl)

def _bad_unknown_kw_impl(ctx):
    cc_internal = cc_common.internal_DO_NOT_USE()
    tree_dir = ctx.actions.declare_directory(ctx.label.name + "_tree")
    ctx.actions.run_shell(outputs = [tree_dir], command = "mkdir -p " + tree_dir.path)
    cc_internal.is_tree_artifact(file = tree_dir)
    return []

probe_bad_unknown_kw = rule(implementation = _bad_unknown_kw_impl)
"#,
            ),
            (
                "BUILD",
                r#"
load(":defs.bzl", "probe_ok", "probe_bad_named_artifact", "probe_bad_unknown_kw")

probe_ok(name = "ok")
probe_bad_named_artifact(name = "bad_named_artifact")
probe_bad_unknown_kw(name = "bad_unknown_kw")
"#,
            ),
        ],
    );

    let ok_ct = ws.analyze("//:ok").unwrap();
    let zel_starlark::Value::ProviderInstance { fields, .. } =
        &ok_ct.providers["//:defs.bzl%ProbeInfo"]
    else {
        panic!("expected ProbeInfo provider");
    };
    assert_eq!(
        fields.get("is_tree"),
        Some(&zel_starlark::Value::Bool(true))
    );
    assert_eq!(
        fields.get("is_reg"),
        Some(&zel_starlark::Value::Bool(false))
    );

    let err_named = ws
        .analyze("//:bad_named_artifact")
        .expect_err("named `artifact = ...` must be rejected as positional-only");
    let msg_named = err_named.to_string();
    assert!(
        msg_named.contains(
            "is_tree_artifact() got named argument for positional-only parameter 'artifact'"
        ),
        "unexpected error for named `artifact`: {msg_named}"
    );

    let err_unknown = ws
        .analyze("//:bad_unknown_kw")
        .expect_err("unknown keyword `file = ...` must be rejected");
    let msg_unknown = err_unknown.to_string();
    assert!(
        msg_unknown.contains("is_tree_artifact() got unexpected keyword argument 'file'"),
        "unexpected error for unknown keyword `file`: {msg_unknown}"
    );
}

#[test]
fn package_specification_info_contains_matches_specs_and_includes() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
CheckInfo = provider(fields = ["results"])

def _check_impl(ctx):
    ps = ctx.attr.group[PackageSpecificationInfo]
    return [CheckInfo(results = [
        ps.contains(Label("//allowed/sub:tgt")),
        ps.contains("//allowed/secret:tgt"),
        ps.contains("//extra:tgt"),
        ps.contains("//other:tgt"),
    ])]

check_group = rule(
    implementation = _check_impl,
    attrs = {"group": attr.label(providers = [PackageSpecificationInfo])},
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "check_group")

package_group(
    name = "inc",
    packages = ["//extra"],
)

package_group(
    name = "grp",
    packages = ["//allowed/...", "-//allowed/secret/..."],
    includes = [":inc"],
)

check_group(name = "chk", group = ":grp")
"#,
        ),
    ]);
    let ct = ws.analyze("//:chk").unwrap();
    let Some(Value::ProviderInstance { fields, .. }) = ct.providers.get("//:defs.bzl%CheckInfo")
    else {
        panic!("expected CheckInfo");
    };
    let got: Vec<bool> = zel_starlark::eval::builtins::value_to_sequence(&fields["results"])
        .unwrap()
        .into_iter()
        .map(|v| matches!(v, Value::Bool(true)))
        .collect();
    assert_eq!(got, vec![true, false, true, false]);
}

#[test]
fn package_specification_info_propagates_cycle_and_load_errors() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _check_impl(ctx):
    _ = ctx.attr.group[PackageSpecificationInfo].contains("//allowed:tgt")
    return [DefaultInfo()]

check_group = rule(
    implementation = _check_impl,
    attrs = {"group": attr.label(providers = [PackageSpecificationInfo])},
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "check_group")

filegroup(name = "not_a_group", srcs = [])

package_group(name = "cyc_a", includes = [":cyc_b"])
package_group(name = "cyc_b", includes = [":cyc_a"])
check_group(name = "chk_cycle", group = ":cyc_a")

package_group(name = "bad_inc", includes = [":not_a_group"])
check_group(name = "chk_bad_inc", group = ":bad_inc")

package_group(name = "inc_err_pkg", includes = ["//broken:grp"])
check_group(name = "chk_err_pkg", group = ":inc_err_pkg")
"#,
        ),
        (
            "broken/BUILD",
            r#"
package_group(name = "grp", packages = ["not_a_valid_spec"])
"#,
        ),
    ]);

    let cycle_err = ws.analyze("//:chk_cycle").unwrap_err();
    assert!(
        cycle_err.contains("cycle in dependency graph"),
        "expected cycle error, got: {cycle_err}"
    );

    let bad_inc_err = ws.analyze("//:chk_bad_inc").unwrap_err();
    assert!(
        bad_inc_err.contains("does not refer to a package group"),
        "expected non-package_group include error, got: {bad_inc_err}"
    );

    let broken_pkg_err = ws.analyze("//:chk_err_pkg").unwrap_err();
    assert!(
        broken_pkg_err.contains("invalid package name 'not_a_valid_spec'"),
        "expected invalid spec package error, got: {broken_pkg_err}"
    );

    let malformed_target = Target::PackageGroup {
        label: Label::parse_canonical("//:inline_bad").unwrap(),
        packages: vec!["invalid_inline_spec".to_string()],
    };
    let inline_cfg = BuildConfigurationKey::target("k8", "fastbuild");
    let inline_err = analyze_target(&malformed_target, &inline_cfg).unwrap_err();
    assert!(
        inline_err.contains("invalid package name 'invalid_inline_spec'"),
        "expected spec parse error from analyze_target, got: {inline_err}"
    );
}

#[test]
fn boolean_build_setting_and_config_setting_accept_short_case_insensitive_tokens() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _bool_flag_impl(ctx):
    return []

bool_flag = rule(
    implementation = _bool_flag_impl,
    build_setting = config.bool(flag = True),
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "bool_flag")
bool_flag(name = "flag", build_setting_default = False)
config_setting(name = "on_t", flag_values = {":flag": "t"})
config_setting(name = "on_upper_y", flag_values = {":flag": "Y"})
config_setting(name = "off_f", flag_values = {":flag": "f"})
config_setting(name = "off_upper_n", flag_values = {":flag": "N"})
filegroup(name = "sel", srcs = select({":on_t": ["on.txt"], ":off_f": ["off.txt"]}))
"#,
        ),
        ("on.txt", ""),
        ("off.txt", ""),
    ]);
    let is_match =
        |ct: &zel_analysis::ConfiguredTarget| match ct.providers.get("ConfigMatchingProvider") {
            Some(Value::ProviderInstance { fields, .. }) => fields.get("matches").cloned(),
            _ => None,
        };
    for raw_true in ["t", "T", "y", "Y", "YES", "True", "1"] {
        let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
        cfg.flag_values
            .insert("//:flag".into(), raw_true.to_string());
        let (r, _) = ws.resolver_with(cfg.clone(), AnalysisConfig::new());
        let ct_t = r
            .analyze(&Label::parse_canonical("//:on_t").unwrap(), &cfg)
            .unwrap();
        let ct_y = r
            .analyze(&Label::parse_canonical("//:on_upper_y").unwrap(), &cfg)
            .unwrap();
        let ct_f = r
            .analyze(&Label::parse_canonical("//:off_f").unwrap(), &cfg)
            .unwrap();
        let sel = r
            .analyze(&Label::parse_canonical("//:sel").unwrap(), &cfg)
            .unwrap();
        assert_eq!(is_match(&ct_t), Some(Value::Bool(true)), "{raw_true}");
        assert_eq!(is_match(&ct_y), Some(Value::Bool(true)), "{raw_true}");
        assert_eq!(is_match(&ct_f), Some(Value::Bool(false)), "{raw_true}");
        assert_eq!(sel.default_info.files[0].exec_path.to_string(), "on.txt");
    }
    for raw_false in ["f", "F", "n", "N", "NO", "False", "0"] {
        let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
        cfg.flag_values
            .insert("//:flag".into(), raw_false.to_string());
        let (r, _) = ws.resolver_with(cfg.clone(), AnalysisConfig::new());
        let ct_f = r
            .analyze(&Label::parse_canonical("//:off_f").unwrap(), &cfg)
            .unwrap();
        let ct_n = r
            .analyze(&Label::parse_canonical("//:off_upper_n").unwrap(), &cfg)
            .unwrap();
        let sel = r
            .analyze(&Label::parse_canonical("//:sel").unwrap(), &cfg)
            .unwrap();
        assert_eq!(is_match(&ct_f), Some(Value::Bool(true)), "{raw_false}");
        assert_eq!(is_match(&ct_n), Some(Value::Bool(true)), "{raw_false}");
        assert_eq!(sel.default_info.files[0].exec_path.to_string(), "off.txt");
    }
}

#[test]
fn external_repo_cc_compile_and_genrule_paths_agree_under_sibling_repository_layout() {
    let dir = std::env::temp_dir().join(format!("zel-sibling-cc-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let main_dir = dir.join("main");
    let ext_dir = dir.join("ext_cc");
    let bt_dir = dir.join("bt");
    std::fs::create_dir_all(&main_dir).unwrap();
    std::fs::create_dir_all(ext_dir.join("pkg/inc")).unwrap();
    std::fs::create_dir_all(bt_dir.join("tools/genrule")).unwrap();

    std::fs::write(main_dir.join("BUILD"), "").unwrap();
    std::fs::write(
        bt_dir.join("tools/genrule/BUILD"),
        "exports_files(['genrule-setup.sh'])\n",
    )
    .unwrap();
    std::fs::write(
        bt_dir.join("tools/genrule/genrule-setup.sh"),
        "#!/bin/bash\n",
    )
    .unwrap();
    std::fs::write(ext_dir.join("tc_helpers.bzl"), CC_COMMON_TEST_HELPERS_BZL).unwrap();
    std::fs::write(ext_dir.join("BUILD"), "").unwrap();
    std::fs::write(
        ext_dir.join("defs.bzl"),
        r#"
load(":tc_helpers.bzl", "make_cc_toolchain")

ProbeCcInfo = provider(fields = [
    "is_sibling",
    "workspace_root",
    "bin_dir",
    "includes",
    "system_includes",
    "quote_includes",
    "hdr_paths",
    "obj_paths",
])

def _ext_cc_impl(ctx):
    tc = make_cc_toolchain(
        ctx,
        cpu = "k8",
        action_configs = [
            struct(
                action_name = "c++-compile",
                enabled = True,
                tools = [struct(path = "/usr/bin/g++", with_features = [], execution_requirements = [])],
                flag_sets = [],
                implies = [],
            ),
        ],
    )
    fc = cc_common.configure_features(
        ctx = ctx,
        cc_toolchain = tc,
        requested_features = ctx.features,
        unsupported_features = ctx.disabled_features,
    )
    cc_ctx, cc_outs = cc_common.compile(
        name = ctx.label.name,
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        srcs = ctx.files.srcs,
        public_hdrs = ctx.files.hdrs,
        strip_include_prefix = ctx.attr.strip_include_prefix,
        include_prefix = ctx.attr.include_prefix,
    )
    return [
        DefaultInfo(files = depset(cc_outs.objects)),
        ProbeCcInfo(
            is_sibling = ctx.configuration.is_sibling_repository_layout(),
            workspace_root = ctx.label.workspace_root,
            bin_dir = ctx.bin_dir.path,
            includes = cc_ctx.includes.to_list(),
            system_includes = cc_ctx.system_includes.to_list(),
            quote_includes = cc_ctx.quote_includes.to_list(),
            hdr_paths = [h.path for h in cc_ctx.headers.to_list()],
            obj_paths = [o.path for o in cc_outs.objects],
        ),
    ]

ext_cc_lib = rule(
    implementation = _ext_cc_impl,
    fragments = ["cpp"],
    attrs = {
        "srcs": attr.label_list(allow_files = True),
        "hdrs": attr.label_list(allow_files = True),
        "includes": attr.string_list(),
        "strip_include_prefix": attr.string(),
        "include_prefix": attr.string(),
    },
)
"#,
    )
    .unwrap();
    std::fs::write(ext_dir.join("pkg/src.cc"), "int f() { return 1; }\n").unwrap();
    std::fs::write(ext_dir.join("pkg/hdr.h"), "#pragma once\n").unwrap();
    std::fs::write(
        ext_dir.join("pkg/BUILD"),
        r#"
load("//:defs.bzl", "ext_cc_lib")

genrule(
    name = "gen_hdr",
    outs = ["gen.h"],
    cmd = "echo '// $(RULEDIR)' > $@",
)

ext_cc_lib(
    name = "lib",
    srcs = ["src.cc"],
    hdrs = ["hdr.h", ":gen.h"],
    includes = ["inc"],
    strip_include_prefix = "/pkg",
    include_prefix = "pref",
)
"#,
    )
    .unwrap();

    let run_with_layout = |sibling: bool| {
        let mut rr = SimpleRepoResolver::new(PathFragment::create(&main_dir.to_string_lossy()));
        rr.external.insert(
            "ext_cc".into(),
            PathFragment::create(&ext_dir.to_string_lossy()),
        );
        rr.external.insert(
            "bazel_tools".into(),
            PathFragment::create(&bt_dir.to_string_lossy()),
        );
        rr.apparent
            .insert("ext_cc".into(), RepositoryName::create("ext_cc").unwrap());
        rr.apparent
            .insert("bazel_tools".into(), RepositoryName::bazel_tools());
        let mut lc = LoaderConfig::new(Arc::new(zel_vfs::LocalFileSystem::default()), Arc::new(rr));
        lc.sibling_repository_layout = sibling;
        let loader = PackageLoader::new(lc);
        assert_eq!(loader.builtins_error(), None);
        let mut acfg = AnalysisConfig::new();
        acfg.sibling_repository_layout = sibling;
        let resolver = TargetResolver::new(loader, acfg);
        let cfg = BuildConfigurationKey::target("k8", "fastbuild");
        let gen_ct = resolver
            .analyze(
                &Label::parse_canonical("@@ext_cc//pkg:gen_hdr").unwrap(),
                &cfg,
            )
            .unwrap();
        let lib_ct = resolver
            .analyze(&Label::parse_canonical("@@ext_cc//pkg:lib").unwrap(), &cfg)
            .unwrap();
        (gen_ct, lib_ct)
    };

    let str_list = |v: &Value| -> Vec<String> {
        zel_starlark::eval::builtins::value_to_sequence(v)
            .unwrap()
            .into_iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect()
    };

    // 1. Default layout (`--noexperimental_sibling_repository_layout`).
    let (gen_def, lib_def) = run_with_layout(false);
    let (_, gen_def_argv, _, gen_def_outs) = spawn_of(&gen_def, 0);
    assert!(
        gen_def_argv[2].starts_with("source external/bazel_tools/tools/genrule/genrule-setup.sh;"),
        "got: {}",
        gen_def_argv[2]
    );
    assert!(
        gen_def_argv[2].contains("bazel-out/k8-fastbuild/bin/external/ext_cc/pkg"),
        "got: {}",
        gen_def_argv[2]
    );
    assert_eq!(
        gen_def_outs,
        vec!["bazel-out/k8-fastbuild/bin/external/ext_cc/pkg/gen.h"]
    );
    let Some(Value::ProviderInstance { fields: f_def, .. }) =
        lib_def.providers.get("@@ext_cc//:defs.bzl%ProbeCcInfo")
    else {
        panic!("expected ProbeCcInfo");
    };
    assert_eq!(f_def["is_sibling"], Value::Bool(false));
    assert_eq!(
        f_def["workspace_root"],
        Value::String("external/ext_cc".into())
    );
    assert_eq!(
        f_def["bin_dir"],
        Value::String("bazel-out/k8-fastbuild/bin".into())
    );
    assert_eq!(
        str_list(&f_def["includes"]),
        vec!["bazel-out/k8-fastbuild/bin/external/ext_cc/pkg/_virtual_includes/lib"]
    );
    let quote_def = str_list(&f_def["quote_includes"]);
    assert!(
        quote_def.contains(&"external/ext_cc".to_string()),
        "{quote_def:?}"
    );
    assert!(
        quote_def.contains(&"bazel-out/k8-fastbuild/bin/external/ext_cc".to_string()),
        "{quote_def:?}"
    );
    let hdrs_def = str_list(&f_def["hdr_paths"]);
    assert!(
        hdrs_def.contains(
            &"bazel-out/k8-fastbuild/bin/external/ext_cc/pkg/_virtual_includes/lib/pref/hdr.h"
                .to_string()
        ),
        "{hdrs_def:?}"
    );
    assert!(
        hdrs_def.contains(
            &"bazel-out/k8-fastbuild/bin/external/ext_cc/pkg/_virtual_includes/lib/pref/gen.h"
                .to_string()
        ),
        "{hdrs_def:?}"
    );
    assert!(
        hdrs_def.contains(&"external/ext_cc/pkg/hdr.h".to_string()),
        "{hdrs_def:?}"
    );
    assert_eq!(
        str_list(&f_def["obj_paths"]),
        vec!["bazel-out/k8-fastbuild/bin/external/ext_cc/pkg/_objs/lib/src.o"]
    );

    // 2. Sibling layout (`--experimental_sibling_repository_layout`).
    let (gen_sib, lib_sib) = run_with_layout(true);
    let (_, gen_sib_argv, _, gen_sib_outs) = spawn_of(&gen_sib, 0);
    assert!(
        gen_sib_argv[2].starts_with("source ../bazel_tools/tools/genrule/genrule-setup.sh;"),
        "got: {}",
        gen_sib_argv[2]
    );
    assert!(
        gen_sib_argv[2].contains("bazel-out/ext_cc/k8-fastbuild/bin/pkg"),
        "got: {}",
        gen_sib_argv[2]
    );
    assert_eq!(
        gen_sib_outs,
        vec!["bazel-out/ext_cc/k8-fastbuild/bin/pkg/gen.h"]
    );
    let Some(Value::ProviderInstance { fields: f_sib, .. }) =
        lib_sib.providers.get("@@ext_cc//:defs.bzl%ProbeCcInfo")
    else {
        panic!("expected ProbeCcInfo");
    };
    assert_eq!(f_sib["is_sibling"], Value::Bool(true));
    assert_eq!(f_sib["workspace_root"], Value::String("../ext_cc".into()));
    assert_eq!(
        f_sib["bin_dir"],
        Value::String("bazel-out/ext_cc/k8-fastbuild/bin".into())
    );
    assert_eq!(
        str_list(&f_sib["includes"]),
        vec!["bazel-out/ext_cc/k8-fastbuild/bin/pkg/_virtual_includes/lib"]
    );
    let quote_sib = str_list(&f_sib["quote_includes"]);
    assert!(
        quote_sib.contains(&"../ext_cc".to_string()),
        "{quote_sib:?}"
    );
    assert!(
        quote_sib.contains(&"bazel-out/ext_cc/k8-fastbuild/bin".to_string()),
        "{quote_sib:?}"
    );
    let hdrs_sib = str_list(&f_sib["hdr_paths"]);
    assert!(
        hdrs_sib.contains(
            &"bazel-out/ext_cc/k8-fastbuild/bin/pkg/_virtual_includes/lib/pref/hdr.h".to_string()
        ),
        "{hdrs_sib:?}"
    );
    assert!(
        hdrs_sib.contains(
            &"bazel-out/ext_cc/k8-fastbuild/bin/pkg/_virtual_includes/lib/pref/gen.h".to_string()
        ),
        "{hdrs_sib:?}"
    );
    assert!(
        hdrs_sib.contains(&"../ext_cc/pkg/hdr.h".to_string()),
        "{hdrs_sib:?}"
    );
    assert_eq!(
        str_list(&f_sib["obj_paths"]),
        vec!["bazel-out/ext_cc/k8-fastbuild/bin/pkg/_objs/lib/src.o"]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn args_and_template_dict_methods_return_self_for_arbitrary_chaining() {
    let ws = Ws::new(&[
        ("tpl.in", "%k1%|%k2%\n"),
        (
            "defs.bzl",
            r#"
def _ident(x):
    return x

def _impl(ctx):
    run_out = ctx.actions.declare_file(ctx.label.name + ".run")
    write_out = ctx.actions.declare_file(ctx.label.name + ".args")
    tpl_out = ctx.actions.declare_file(ctx.label.name + ".tpl")

    other = ctx.actions.args().add("--untouched")
    chained = (
        ctx.actions.args()
        .add_all("--foo", ["a", "b", "a"], uniquify = True)
        .add_all("--omitted", [])
        .add_joined("--bar", depset(["x", "y"]), join_with = ",")
        .add_joined("--omitted_join", [], join_with = ",")
        .add("--baz", "z")
        .use_param_file("--flagfile=%s", use_always = True)
        .set_param_file_format("flag_per_line")
    )
    if chained == other:
        fail("distinct Args instances must not compare equal")
    if chained.add("positional_tail") != chained:
        fail("Args.add must return receiver")

    ctx.actions.run(
        outputs = [run_out],
        executable = "/bin/echo",
        arguments = [chained],
    )

    written = (
        ctx.actions.args()
        .use_param_file("%s", use_always = True)
        .set_param_file_format("multiline")
        .add("line1")
        .add_all(["line2", "line3"])
        .add_joined(["p", "q"], join_with = ":")
    )
    ctx.actions.write(output = write_out, content = written)

    tdict = (
        ctx.actions.template_dict()
        .add("%k1%", "hello")
        .add_joined("%k2%", depset(["u", "v"]), join_with = ";", map_each = _ident)
    )
    ctx.actions.expand_template(
        template = ctx.file.tpl,
        output = tpl_out,
        substitutions = {},
        computed_substitutions = tdict,
    )

    # Verify `other` never received mutations from `chained` or `written`.
    other_out = ctx.actions.declare_file(ctx.label.name + ".other")
    ctx.actions.write(output = other_out, content = other)
    return [DefaultInfo(files = depset([run_out, write_out, tpl_out, other_out]))]

chained_rule = rule(
    implementation = _impl,
    attrs = {"tpl": attr.label(allow_single_file = True, default = "//:tpl.in")},
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "chained_rule")
chained_rule(name = "t")
"#,
        ),
    ]);

    let ct = ws.analyze("//:t").expect("analysis should succeed");
    let spawn = ct
        .typed_actions
        .iter()
        .find(|a| a.outputs.iter().any(|o| o.basename() == "t.run"))
        .expect("spawn action for t.run");
    let (argv, param_files) = spawn.command_line();
    // `flag_per_line` keeps non-flags (`positional_tail`) on the command line alongside `--flagfile=...`.
    assert_eq!(argv[0], "/bin/echo");
    assert!(
        argv.iter().any(|a| a.starts_with("--flagfile=")),
        "expected --flagfile arg in {argv:?}"
    );
    assert!(
        argv.contains(&"positional_tail".to_string()),
        "expected positional_tail on command line in {argv:?}"
    );
    assert_eq!(param_files.len(), 1);
    assert_eq!(param_files[0].1, b"--foo=a b\n--bar=x,y\n--baz=z\n");

    let write_action = ct
        .typed_actions
        .iter()
        .find(|a| a.outputs.iter().any(|o| o.basename() == "t.args"))
        .expect("write action for t.args");
    match &write_action.kind {
        ActionKind::FileWrite { content, .. } => {
            assert_eq!(content.render(), b"line1\nline2\nline3\np:q\n");
        }
        other => panic!("expected FileWrite, got {other:?}"),
    }

    let other_action = ct
        .typed_actions
        .iter()
        .find(|a| a.outputs.iter().any(|o| o.basename() == "t.other"))
        .expect("write action for t.other");
    match &other_action.kind {
        ActionKind::FileWrite { content, .. } => {
            assert_eq!(content.render(), b"--untouched\n");
        }
        other => panic!("expected FileWrite, got {other:?}"),
    }

    let tpl_action = ct
        .typed_actions
        .iter()
        .find(|a| a.outputs.iter().any(|o| o.basename() == "t.tpl"))
        .expect("template expansion action for t.tpl");
    match &tpl_action.kind {
        ActionKind::TemplateExpand {
            computed_substitutions,
            ..
        } => {
            assert_eq!(
                computed_substitutions,
                &vec![
                    ("%k1%".to_string(), "hello".to_string()),
                    ("%k2%".to_string(), "u;v".to_string()),
                ]
            );
        }
        other => panic!("expected TemplateExpand, got {other:?}"),
    }
}

#[test]
fn starlark_actions_inspection_map_directory_and_shadowed_action() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _id(x):
    return x

def _expand_dir(template_ctx, *, input_directories, output_directories, additional_inputs, tools, additional_params):
    in_exp = input_directories["src_dir"]
    out_dir = output_directories["dst_dir"]
    child = template_ctx.declare_file("generated.txt", directory = out_dir)
    a = template_ctx.args()
    a.add("--mode", additional_params["mode"])
    a.add("--dir", in_exp.directory.path)
    template_ctx.run(
        outputs = [child],
        inputs = [in_exp.directory],
        executable = "/bin/cp",
        arguments = [a],
    )

def _impl(ctx):
    w = ctx.actions.declare_file("hello.txt")
    ctx.actions.write(w, "hello")

    tpl = ctx.actions.declare_file("tpl.in")
    ctx.actions.write(tpl, "vals=%VALS%")
    expanded = ctx.actions.declare_file("tpl.out")
    td = ctx.actions.template_dict()
    td.add_joined("%VALS%", depset(["a", "b", "a"]), join_with = ",", map_each = _id, uniquify = True)
    ctx.actions.expand_template(template = tpl, output = expanded, computed_substitutions = td)

    created = ctx.created_actions()
    w_act = created.by_file[w]
    if w_act.content != "hello" or w_act.mnemonic != "FileWrite":
        fail("unexpected write action fields")
    exp_act = created.by_file[expanded]
    if exp_act.substitutions["%VALS%"] != "a,b":
        fail("unexpected template_dict substitutions: " + repr(exp_act.substitutions))

    base_out = ctx.actions.declare_file("base.out")
    ctx.actions.run(
        outputs = [base_out],
        inputs = [w],
        executable = "/bin/true",
        arguments = ["--base"],
        env = {"A": "1", "B": "base"},
    )
    base_act = ctx.created_actions().by_file[base_out]
    shadow_out = ctx.actions.declare_file("shadow.out")
    ctx.actions.run(
        outputs = [shadow_out],
        executable = "/bin/true",
        shadowed_action = base_act,
        env = {"B": "override"},
    )
    sh_act = ctx.created_actions().by_file[shadow_out]
    if sh_act.env["A"] != "1" or sh_act.env["B"] != "override":
        fail("shadowed_action env merge failed: " + repr(sh_act.env))
    if w not in sh_act.inputs.to_list():
        fail("shadowed_action inputs not inherited")

    src_dir = ctx.actions.declare_directory("src_tree")
    ctx.actions.run(outputs = [src_dir], executable = "/bin/mkdir")
    dst_dir = ctx.actions.declare_directory("dst_tree")
    ctx.actions.map_directory(
        input_directories = {"src_dir": src_dir},
        output_directories = {"dst_dir": dst_dir},
        additional_params = {"mode": "fast"},
        implementation = _expand_dir,
    )
    return [
        DefaultInfo(files = depset([w, expanded, shadow_out, dst_dir])),
        OutputGroupInfo(extra = depset([expanded])),
    ]

testable_rule = rule(implementation = _impl, _skylark_testable = True)

def _inspect_impl(ctx):
    dep = ctx.attr.dep
    if len(dep.actions) < 4:
        fail("expected target.actions on dep")
    if "extra" not in dep.output_groups:
        fail("expected extra in OutputGroupInfo iteration")
    out = ctx.actions.declare_file("ok.txt")
    ctx.actions.write(out, "ok")
    return [DefaultInfo(files = depset([out]))]

inspector = rule(implementation = _inspect_impl, attrs = {"dep": attr.label()})
"#,
        ),
        (
            "BUILD",
            "load(\":defs.bzl\", \"testable_rule\", \"inspector\")\ntestable_rule(name = \"t\")\ninspector(name = \"i\", dep = \":t\")\n",
        ),
    ]);
    let ct = ws.analyze("//:i").unwrap();
    assert_eq!(ct.default_info.files.len(), 1);
}

#[test]
fn aspect_propagation_predicate_and_dynamic_attr_aspects() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
SeenInfo = provider(fields = ["labels"])

def _pred(ctx):
    return ctx.rule.qualified_kind.rule_name == "node" and ctx.rule.attr.tag.value != "skip"

def _dyn_attrs(ctx):
    if ctx.attr.follow_extra == "yes":
        return ["deps", "extra"]
    return ["deps"]

def _asp_impl(target, ctx):
    acc = [str(target.label)]
    for d in getattr(ctx.rule.attr, "deps", []):
        if SeenInfo in d:
            acc.extend(d[SeenInfo].labels)
    for d in getattr(ctx.rule.attr, "extra", []):
        if SeenInfo in d:
            acc.extend(d[SeenInfo].labels)
    return [SeenInfo(labels = acc)]

dyn_aspect = aspect(
    implementation = _asp_impl,
    attr_aspects = _dyn_attrs,
    propagation_predicate = _pred,
    attrs = {"follow_extra": attr.string(default = "yes")},
)

def _node_impl(ctx):
    return [DefaultInfo()]

node = rule(
    implementation = _node_impl,
    attrs = {
        "deps": attr.label_list(),
        "extra": attr.label_list(),
        "tag": attr.string(default = ""),
    },
)

def _collect_impl(ctx):
    seen = ctx.attr.root[SeenInfo].labels
    out = ctx.actions.declare_file("seen.txt")
    ctx.actions.write(out, ",".join(seen))
    return [DefaultInfo(files = depset([out]))]

collector = rule(
    implementation = _collect_impl,
    attrs = {"root": attr.label(aspects = [dyn_aspect])},
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "node", "collector")
node(name = "skipped", tag = "skip")
node(name = "extra_leaf")
node(name = "root", deps = [":skipped"], extra = [":extra_leaf"])
collector(name = "c", root = ":root")
"#,
        ),
    ]);
    let ct = ws.analyze("//:c").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    let rendered = String::from_utf8(content.render()).unwrap();
    assert!(rendered.contains("//:root"), "{rendered}");
    assert!(rendered.contains("//:extra_leaf"), "{rendered}");
    assert!(!rendered.contains("skipped"), "{rendered}");
}

#[test]
fn allow_analysis_failure_and_analysis_failure_info() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _bad_impl(ctx):
    fail("boom from bad_rule")

bad_rule = rule(implementation = _bad_impl)

def _wrap_impl(ctx):
    out = ctx.actions.declare_file("never.txt")
    ctx.actions.write(out, "never")
    return [DefaultInfo(files = depset([out]))]

wrap_rule = rule(implementation = _wrap_impl, attrs = {"dep": attr.label()})

def _test_impl(ctx):
    tut = ctx.attr.target_under_test[0]
    ok = AnalysisFailureInfo in tut
    causes = tut[AnalysisFailureInfo].causes.to_list() if ok else []
    msg = causes[0].message if len(causes) == 1 else "wrong causes"
    return [AnalysisTestResultInfo(success = ok and "boom from bad_rule" in msg, message = msg)]

failure_test = rule(
    implementation = _test_impl,
    analysis_test = True,
    attrs = {
        "target_under_test": attr.label(
            cfg = analysis_test_transition(
                settings = {"//command_line_option:allow_analysis_failures": "true"},
            ),
        ),
    },
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "bad_rule", "wrap_rule", "failure_test")
bad_rule(name = "bad")
wrap_rule(name = "wrap", dep = ":bad")
failure_test(name = "check_failure", target_under_test = ":wrap")
"#,
        ),
    ]);
    let ct = ws.analyze("//:check_failure").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!("expected analysis test script");
    };
    let script = String::from_utf8(content.render()).unwrap();
    assert!(script.contains("exit 0"), "{script}");
}

#[test]
fn fragments_configuration_methods_and_string_set_flags() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _flag_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, ",".join(list(ctx.build_setting_value)))
    return [DefaultInfo(files = depset([out]))]

set_flag = rule(
    implementation = _flag_impl,
    build_setting = config.string_set(flag = True),
)

def _disallowed_proto_impl(ctx):
    _ = ctx.fragments.proto.strict_proto_deps()
    return []

disallowed_proto_rule = rule(
    implementation = _disallowed_proto_impl,
    fragments = ["proto"],
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "set_flag", "disallowed_proto_rule")
load("@protobuf//:defs.bzl", "check_rule")
set_flag(name = "my_set", build_setting_default = set(["x"]))
check_rule(name = "chk")
disallowed_proto_rule(name = "bad_proto")
"#,
        ),
    ]);
    let pb_root = PathFragment::create("/pb");
    ws.fs.create_directory_and_parents(&pb_root).unwrap();
    ws.fs
        .write_file(&pb_root.get_relative_str("BUILD"), b"")
        .unwrap();
    ws.fs
        .write_file(
            &pb_root.get_relative_str("defs.bzl"),
            br#"
def _check_impl(ctx):
    df = ctx.configuration.disabled_features()
    ios_min = ctx.fragments.apple.ios_minimum_os_flag
    j_ord = ctx.fragments.java.default_javac_flags_depset
    cov_gen = ctx.fragments.coverage.output_generator
    if cov_gen != None and type(cov_gen) != "Label":
        fail("expected coverage.output_generator to be Label or None, got " + type(cov_gen))
    if ctx.fragments.cpp._fdo_prefetch_hints_label != None:
        fail("expected cpp._fdo_prefetch_hints_label struct field to be None by default")
    strict_deps = ctx.fragments.proto.strict_proto_deps()
    strict_pub = ctx.fragments.proto.strict_public_imports()
    inc_src = ctx.fragments.proto.experimental_proto_descriptorsets_include_source_info()
    out = ctx.actions.declare_file("info.txt")
    ctx.actions.write(
        out,
        "{}|{}|{}|{}|{}|{}|{}".format(
            ",".join(df),
            str(ios_min),
            ",".join(j_ord.to_list()),
            str(cov_gen),
            strict_deps,
            strict_pub,
            str(inc_src),
        ),
    )
    return [DefaultInfo(files = depset([out]))]

check_rule = rule(
    implementation = _check_impl,
    fragments = ["apple", "java", "cpp", "proto"],
)
"#,
        )
        .unwrap();
    let make_resolver = |fs: Arc<InMemoryFileSystem>| {
        let mut rr = SimpleRepoResolver::new(ws.root.clone());
        rr.external.insert("protobuf".into(), pb_root.clone());
        rr.apparent.insert(
            "protobuf".into(),
            RepositoryName::create("protobuf").unwrap(),
        );
        let lc = LoaderConfig::new(fs, Arc::new(rr));
        TargetResolver::new(PackageLoader::new(lc), AnalysisConfig::new())
    };
    let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
    cfg.options.insert("features".into(), "a,-b".into());
    cfg.options.insert("ios_minimum_os".into(), "15.2".into());
    cfg.options.insert(
        "javacopt".into(),
        zel_analysis::build_options::encode_unsplit_list(&[
            "-Xlint:all".to_string(),
            "-source 11".to_string(),
        ]),
    );
    cfg.options
        .insert("collect_code_coverage".into(), "true".into());
    cfg.flag_values.insert("//:my_set".into(), "p,q,p".into());
    let r = make_resolver(ws.fs.clone());
    let flag_ct = r
        .analyze(&Label::parse_canonical("//:my_set").unwrap(), &cfg)
        .unwrap();
    let ActionKind::FileWrite { content, .. } = &flag_ct.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    assert_eq!(String::from_utf8(content.render()).unwrap(), "p,q");

    let bad_err = r
        .analyze(&Label::parse_canonical("//:bad_proto").unwrap(), &cfg)
        .unwrap_err();
    assert!(
        bad_err.contains("cannot use private API"),
        "expected private API error for main-repo rule: {bad_err}"
    );

    let chk_ct = r
        .analyze(&Label::parse_canonical("//:chk").unwrap(), &cfg)
        .unwrap();
    let ActionKind::FileWrite { content, .. } = &chk_ct.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    let text = String::from_utf8(content.render()).unwrap();
    assert_eq!(
        text,
        "b|15.2|-Xlint:all -source 11|@@bazel_tools//tools/test:lcov_merger|ERROR|OFF|False"
    );

    // Without `--collect_code_coverage`, `ctx.fragments.coverage.output_generator` is `None`.
    let mut no_cov_cfg = BuildConfigurationKey::target("k8", "fastbuild");
    no_cov_cfg.options.insert("features".into(), "a,-b".into());
    no_cov_cfg
        .options
        .insert("ios_minimum_os".into(), "15.2".into());
    let r_nocov = make_resolver(ws.fs.clone());
    let chk_nocov = r_nocov
        .analyze(&Label::parse_canonical("//:chk").unwrap(), &no_cov_cfg)
        .unwrap();
    let ActionKind::FileWrite { content, .. } = &chk_nocov.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    let text_nocov = String::from_utf8(content.render()).unwrap();
    assert!(
        text_nocov.contains("|None|ERROR|OFF|False"),
        "expected None output_generator when coverage is off: {text_nocov}"
    );
}

#[test]
fn cc_common_get_tool_for_action_errors_on_unconfigured_and_resolves_disabled_action_config() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _probe_impl(ctx):
    cc_internal = cc_common.internal_DO_NOT_USE()
    cfg_info = struct(
        _features_DO_NOT_USE = [],
        _action_configs_DO_NOT_USE = [
            struct(
                action_name = "c-compile",
                enabled = True,
                tools = [struct(path = "/usr/bin/clang")],
            ),
            struct(
                action_name = "strip",
                enabled = False,
                tools = [struct(path = "/usr/bin/strip", execution_requirements = ["no-remote"])],
            ),
            struct(
                action_name = "empty-tool",
                enabled = True,
                tools = [],
            ),
        ],
        _artifact_name_patterns_DO_NOT_USE = [],
    )
    tf = cc_internal.cc_toolchain_features(toolchain_config_info = cfg_info, tools_directory = "")
    fc = tf.configure_features(requested_features = tf.default_features_and_action_configs())
    if ctx.fragments.cpp.do_not_use_macos_set_install_name != True or type(ctx.fragments.cpp.do_not_use_macos_set_install_name) != "bool":
        fail("expected cpp.do_not_use_macos_set_install_name to be bool True")
    if not cc_common.action_is_enabled(feature_configuration = fc, action_name = "c-compile"):
        fail("expected c-compile to be enabled")
    if cc_common.action_is_enabled(feature_configuration = fc, action_name = "strip"):
        fail("expected strip action_config to be disabled")
    if ctx.attr.mode == "unconfigured":
        cc_common.get_tool_for_action(
            feature_configuration = fc,
            action_name = "unregistered-action",
        )
    if ctx.attr.mode == "no_tool":
        cc_common.get_tool_for_action(
            feature_configuration = fc,
            action_name = "empty-tool",
        )
    strip_tool = cc_common.get_tool_for_action(feature_configuration = fc, action_name = "strip")
    strip_reqs = cc_common.get_execution_requirements(feature_configuration = fc, action_name = "strip")
    compile_tool = cc_common.get_tool_for_action(feature_configuration = fc, action_name = "c-compile")
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, "{}|{}|{}".format(strip_tool, ",".join(strip_reqs), compile_tool))
    return [DefaultInfo(files = depset([out]))]

probe = rule(
    implementation = _probe_impl,
    attrs = {"mode": attr.string(default = "ok")},
    fragments = ["cpp"],
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "probe")
probe(name = "ok")
probe(name = "bad_unconfigured", mode = "unconfigured")
probe(name = "bad_no_tool", mode = "no_tool")
"#,
        ),
    ]);

    let ok_ct = ws.analyze("//:ok").unwrap();
    let ActionKind::FileWrite { content, .. } = &ok_ct.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    assert_eq!(
        String::from_utf8(content.render()).unwrap(),
        "/usr/bin/strip|no-remote|/usr/bin/clang"
    );

    let err = ws.analyze("//:bad_unconfigured").unwrap_err().to_string();
    assert!(
        err.contains(
            "Action unregistered-action does not have an enabled configuration in the toolchain."
        ),
        "unexpected error: {err}"
    );

    let no_tool_err = ws.analyze("//:bad_no_tool").unwrap_err().to_string();
    assert!(
        no_tool_err.contains(
            "Matching tool for action empty-tool not found for given feature configuration"
        ),
        "unexpected error: {no_tool_err}"
    );
}

#[test]
fn coverage_instrumented_respects_flags_filter_test_targets_and_file_targets() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r##"
def _cov_lib_impl(ctx):
    info = coverage_common.instrumented_files_info(ctx, source_attributes = ["srcs"])
    return [DefaultInfo(), info]

cov_lib = rule(
    implementation = _cov_lib_impl,
    attrs = {"srcs": attr.label_list(allow_files = True)},
)

def _plain_impl(ctx):
    return [DefaultInfo()]

plain_lib = rule(implementation = _plain_impl)

def _cov_test_impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(exe, "#!/bin/sh\nexit 0\n", is_executable = True)
    info = coverage_common.instrumented_files_info(ctx, source_attributes = ["srcs"])
    return [DefaultInfo(executable = exe, files = depset([exe])), info]

cov_test = rule(
    implementation = _cov_test_impl,
    test = True,
    attrs = {"srcs": attr.label_list(allow_files = True)},
)

def _probe_impl(ctx):
    if ctx.attr.bad_arg:
        ctx.coverage_instrumented("not_a_target")
    parts = [
        "self=" + str(ctx.coverage_instrumented()),
        "inc_cov=" + str(ctx.coverage_instrumented(ctx.attr.inc_cov)),
        "exc_cov=" + str(ctx.coverage_instrumented(ctx.attr.exc_cov)),
        "inc_fg=" + str(ctx.coverage_instrumented(ctx.attr.inc_fg)),
        "exc_fg=" + str(ctx.coverage_instrumented(ctx.attr.exc_fg)),
        "sub_fg=" + str(ctx.coverage_instrumented(ctx.attr.sub_fg)),
        "inc_plain=" + str(ctx.coverage_instrumented(ctx.attr.inc_plain)),
        "inc_test=" + str(ctx.coverage_instrumented(ctx.attr.inc_test)),
        "alias_test=" + str(ctx.coverage_instrumented(ctx.attr.alias_test)),
        "alias_inc_cov=" + str(ctx.coverage_instrumented(ctx.attr.alias_inc_cov)),
        "alias_exc_cov=" + str(ctx.coverage_instrumented(ctx.attr.alias_exc_cov)),
        "src_file=" + str(ctx.coverage_instrumented(ctx.attr.src_file)),
        "gen_file=" + str(ctx.coverage_instrumented(ctx.attr.gen_file)),
    ]
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, "|".join(parts))
    return [DefaultInfo(files = depset([out]))]

probe = rule(
    implementation = _probe_impl,
    attrs = {
        "inc_cov": attr.label(),
        "exc_cov": attr.label(),
        "inc_fg": attr.label(),
        "exc_fg": attr.label(),
        "sub_fg": attr.label(),
        "inc_plain": attr.label(),
        "inc_test": attr.label(),
        "alias_test": attr.label(),
        "alias_inc_cov": attr.label(),
        "alias_exc_cov": attr.label(),
        "src_file": attr.label(allow_single_file = True),
        "gen_file": attr.label(allow_single_file = True),
        "bad_arg": attr.bool(default = False),
    },
)
"##,
        ),
        ("BUILD", ""),
        ("inc/src.c", "int main() { return 0; }\n"),
        (
            "inc/BUILD",
            r#"
package(default_visibility = ["//visibility:public"])
load("//:defs.bzl", "cov_lib", "plain_lib", "cov_test", "probe")
exports_files(["src.c"])
genrule(name = "gen", outs = ["gen_out.c"], cmd = "echo 'int x = 0;' > $@")
cov_lib(name = "lib", srcs = ["src.c"])
filegroup(name = "fg", srcs = ["src.c"])
filegroup(name = "excluded_fg", srcs = ["src.c"])
plain_lib(name = "plain")
cov_test(name = "my_test", srcs = ["src.c"])
alias(name = "alias_to_exc_lib", actual = "//exc:lib")
probe(
    name = "p",
    testonly = True,
    inc_cov = "//inc:lib",
    exc_cov = "//exc:lib",
    inc_fg = "//inc:fg",
    exc_fg = "//exc:fg",
    sub_fg = "//inc:excluded_fg",
    inc_plain = "//inc:plain",
    inc_test = "//inc:my_test",
    alias_test = "//exc:chain_to_inc_test",
    alias_inc_cov = "//exc:alias_to_inc_lib",
    alias_exc_cov = "//inc:alias_to_exc_lib",
    src_file = "//inc:src.c",
    gen_file = "//inc:gen_out.c",
)
probe(name = "bad", bad_arg = True)
"#,
        ),
        ("exc/src.c", "int y = 1;\n"),
        (
            "exc/BUILD",
            r#"
package(default_visibility = ["//visibility:public"])
load("//:defs.bzl", "cov_lib")
cov_lib(name = "lib", srcs = ["src.c"])
filegroup(name = "fg", srcs = ["src.c"])
alias(name = "alias_to_inc_lib", actual = "//inc:lib")
alias(name = "alias_to_inc_test", actual = "//inc:my_test", testonly = True)
alias(name = "chain_to_inc_test", actual = ":alias_to_inc_test", testonly = True)
"#,
        ),
    ]);

    let run_with = |cov: bool, filter: Option<&str>, inst_tests: bool| -> String {
        let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
        cfg.options.insert(
            "collect_code_coverage".into(),
            if cov { "true" } else { "false" }.into(),
        );
        if let Some(f) = filter {
            cfg.options
                .insert("instrumentation_filter".into(), f.into());
        }
        cfg.options.insert(
            "instrument_test_targets".into(),
            if inst_tests { "true" } else { "false" }.into(),
        );
        let (r, cfg) = ws.resolver_with(cfg, AnalysisConfig::new());
        let ct = r
            .analyze(&Label::parse_canonical("//inc:p").unwrap(), &cfg)
            .unwrap();
        let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
            panic!("expected FileWrite");
        };
        String::from_utf8(content.render()).unwrap()
    };

    // Coverage disabled: everything is False.
    assert_eq!(
        run_with(false, Some("//inc"), true),
        "self=False|inc_cov=False|exc_cov=False|inc_fg=False|exc_fg=False|sub_fg=False|inc_plain=False|inc_test=False|alias_test=False|alias_inc_cov=False|alias_exc_cov=False|src_file=False|gen_file=False"
    );

    // Coverage enabled with filter `^//inc`, test targets excluded:
    // - `//inc:lib`, `//inc:fg`, `//inc:excluded_fg`, `//exc:alias_to_inc_lib`, and self (`//inc:p`) are True.
    // - `//exc:lib`, `//inc:alias_to_exc_lib`, and `//exc:fg` are excluded by actual label -> False!
    // - `//inc:plain` lacks InstrumentedFilesInfo -> False.
    // - `//inc:my_test` and `//exc:chain_to_inc_test` (chained alias of a test) are excluded with --noinstrument_test_targets -> False.
    // - `//inc:src.c` (source file) and `//inc:gen` (rule without InstrumentedFilesInfo) -> False.
    assert_eq!(
        run_with(true, Some("^//inc"), false),
        "self=True|inc_cov=True|exc_cov=False|inc_fg=True|exc_fg=False|sub_fg=True|inc_plain=False|inc_test=False|alias_test=False|alias_inc_cov=True|alias_exc_cov=False|src_file=False|gen_file=False"
    );

    // Coverage enabled with negative filter exclusion `-//inc:excluded_fg` and `--instrument_test_targets`:
    // - `//inc:my_test` and `//exc:chain_to_inc_test` become True.
    // - `//inc:excluded_fg` becomes False despite carrying InstrumentedFilesInfo.
    assert_eq!(
        run_with(true, Some("^//inc,-^//inc:excluded_fg$"), true),
        "self=True|inc_cov=True|exc_cov=False|inc_fg=True|exc_fg=False|sub_fg=False|inc_plain=False|inc_test=True|alias_test=True|alias_inc_cov=True|alias_exc_cov=False|src_file=False|gen_file=False"
    );

    // Invalid target type fails eagerly even when coverage is disabled.
    let err = ws.analyze("//inc:bad").unwrap_err().to_string();
    assert!(
        err.contains("want 'Target or NoneType'"),
        "unexpected error: {err}"
    );
}

#[test]
fn map_directory_analysis_validation_and_orphan_check() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _noop_expand(template_ctx, *, input_directories, output_directories, additional_inputs, tools, additional_params):
    pass

def _impl(ctx):
    src_dir = ctx.actions.declare_directory("src")
    ctx.actions.run(outputs = [src_dir], executable = "/bin/mkdir")
    dst_dir = ctx.actions.declare_directory("dst")
    if ctx.attr.mode == "orphan":
        return [DefaultInfo(files = depset([dst_dir]))]
    if ctx.attr.mode == "empty_in":
        ctx.actions.map_directory(
            input_directories = {},
            output_directories = {"dst": dst_dir},
            implementation = _noop_expand,
        )
    elif ctx.attr.mode == "empty_out":
        ctx.actions.map_directory(
            input_directories = {"src": src_dir},
            output_directories = {},
            implementation = _noop_expand,
        )
    elif ctx.attr.mode == "bad_param":
        ctx.actions.map_directory(
            input_directories = {"src": src_dir},
            output_directories = {"dst": dst_dir},
            additional_params = {"bad": [1, 2]},
            implementation = _noop_expand,
        )
    else:
        ctx.actions.map_directory(
            input_directories = {"src": src_dir},
            output_directories = {"dst": dst_dir},
            additional_params = {"k": "v", "n": 3, "b": True},
            mnemonic = "CustomExpanded",
            implementation = _noop_expand,
        )
    return [DefaultInfo(files = depset([dst_dir]))]

r = rule(implementation = _impl, attrs = {"mode": attr.string(default = "ok")})
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "r")
r(name = "ok", mode = "ok")
r(name = "orphan", mode = "orphan")
r(name = "empty_in", mode = "empty_in")
r(name = "empty_out", mode = "empty_out")
r(name = "bad_param", mode = "bad_param")
"#,
        ),
    ]);

    let ok_ct = ws.analyze("//:ok").unwrap();
    assert!(ok_ct
        .typed_actions
        .iter()
        .any(|a| a.mnemonic == "StarlarkMapActionTemplate"));

    let orphan_err = ws.analyze("//:orphan").unwrap_err().to_string();
    assert!(
        orphan_err.contains("have no generating action"),
        "unexpected error: {orphan_err}"
    );

    let in_err = ws.analyze("//:empty_in").unwrap_err().to_string();
    assert!(
        in_err.contains("requires at least one input"),
        "unexpected error: {in_err}"
    );

    let out_err = ws.analyze("//:empty_out").unwrap_err().to_string();
    assert!(
        out_err.contains("requires at least one output"),
        "unexpected error: {out_err}"
    );

    let param_err = ws.analyze("//:bad_param").unwrap_err().to_string();
    assert!(
        param_err.contains("Expected one of [bool, int, string]"),
        "unexpected error: {param_err}"
    );

    let mut disabled_cfg = BuildConfigurationKey::target("k8", "fastbuild");
    disabled_cfg
        .options
        .insert("experimental_allow_map_directory".into(), "false".into());
    let (res, cfg) = ws.resolver_with(disabled_cfg, AnalysisConfig::new());
    let flag_err = res
        .analyze(&Label::parse_canonical("//:ok").unwrap(), &cfg)
        .unwrap_err()
        .to_string();
    assert!(
        flag_err.contains("--experimental_allow_map_directory"),
        "unexpected error: {flag_err}"
    );
}

#[test]
fn starlark_runfiles_merge_preserves_map_only_and_ordered_symlinks_in_both_directions() {
    struct MapOnlyRunfilesResolver {
        inner: Arc<TargetResolver>,
    }

    impl zel_analysis::starlark_rule::DepResolver for MapOnlyRunfilesResolver {
        fn configured_target(
            &self,
            label: &Label,
            cfg: &BuildConfigurationKey,
        ) -> Result<Arc<ConfiguredTarget>, String> {
            let ct = self.inner.configured_target(label, cfg)?;
            if label.name() == "map_dep" {
                let mut cloned = (*ct).clone();
                let sym_art = zel_analysis::Artifact::source(
                    PathFragment::create("dep_sym.txt"),
                    label.clone(),
                );
                let root_art = zel_analysis::Artifact::source(
                    PathFragment::create("dep_root.txt"),
                    label.clone(),
                );
                let mut rf = zel_analysis::Runfiles::default();
                // Populate ONLY the BTreeMaps (`symlink_entries` / `root_symlink_entries` left empty).
                rf.symlinks.insert("from_map".to_string(), sym_art);
                rf.root_symlinks
                    .insert("root_from_map".to_string(), root_art);
                cloned.providers.insert(
                    "DefaultInfo".to_string(),
                    zel_analysis::starlark_rule::default_info_provider(&[], rf.clone(), rf, None),
                );
                return Ok(Arc::new(cloned));
            }
            Ok(ct)
        }
    }

    let ws = Ws::new(&[
        ("dep_sym.txt", "1"),
        ("dep_root.txt", "2"),
        ("own_sym.txt", "3"),
        ("own_root.txt", "4"),
        (
            "defs.bzl",
            r#"
def _empty_impl(ctx):
    return [DefaultInfo()]

empty_rule = rule(implementation = _empty_impl)

def _merge_check_impl(ctx):
    dep_rf = ctx.attr.dep[DefaultInfo].default_runfiles
    own_rf = ctx.runfiles(
        symlinks = {"from_vec": ctx.file.own_sym},
        root_symlinks = {"root_from_vec": ctx.file.own_root},
    )
    m_own_first = own_rf.merge(dep_rf)
    m_dep_first = dep_rf.merge(own_rf)
    s1 = [e.path for e in m_own_first.symlinks.to_list()]
    r1 = [e.path for e in m_own_first.root_symlinks.to_list()]
    s2 = [e.path for e in m_dep_first.symlinks.to_list()]
    r2 = [e.path for e in m_dep_first.root_symlinks.to_list()]
    out = ctx.actions.declare_file("merged.txt")
    ctx.actions.write(
        out,
        "{}|{}|{}|{}".format(",".join(s1), ",".join(r1), ",".join(s2), ",".join(r2)),
    )
    return [DefaultInfo(files = depset([out]))]

merge_check = rule(
    implementation = _merge_check_impl,
    attrs = {
        "dep": attr.label(),
        "own_sym": attr.label(allow_single_file = True),
        "own_root": attr.label(allow_single_file = True),
    },
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "empty_rule", "merge_check")
empty_rule(name = "map_dep")
merge_check(
    name = "chk",
    dep = ":map_dep",
    own_sym = "own_sym.txt",
    own_root = "own_root.txt",
)
"#,
        ),
    ]);

    let cfg = BuildConfigurationKey::target("k8", "fastbuild");
    let (inner, cfg) = ws.resolver_with(cfg, AnalysisConfig::new());
    let pkg = inner
        .loader()
        .load_package(&zel_cmdline::PackageIdentifier::create_in_main_repo("").unwrap())
        .unwrap();
    let Some(zel_packages::Target::Rule(rule)) = pkg.targets.get("chk") else {
        panic!("expected chk rule");
    };
    let wrapped: Arc<dyn zel_analysis::starlark_rule::DepResolver> =
        Arc::new(MapOnlyRunfilesResolver { inner });
    let ct = zel_analysis::starlark_rule::analyze_starlark_rule(
        rule,
        &cfg,
        zel_analysis::starlark_rule::AnalysisInputs::new(wrapped),
    )
    .unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    let rendered = String::from_utf8(content.render()).unwrap();
    assert_eq!(
        rendered,
        "from_vec,from_map|root_from_vec,root_from_map|from_map,from_vec|root_from_map,root_from_vec"
    );
}

#[test]
fn apple_fragment_watchos_device_and_simulator_cpus_in_starlark_rules() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _impl(ctx):
    a = ctx.fragments.apple
    p = a.single_arch_platform
    cpus = a.apple_cpus
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(
        out,
        "{}|{}|{}|{}|{}|{}".format(
            a.single_arch_cpu,
            p.name,
            p.name_in_plist,
            p.platform_type,
            p.is_device,
            ",".join(list(cpus.watchos_cpus)),
        ),
    )
    return [DefaultInfo(files = depset([out]))]

apple_probe = rule(
    implementation = _impl,
    fragments = ["apple"],
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "apple_probe")
apple_probe(name = "probe")
"#,
        ),
    ]);

    for (cpu, expected) in [
        (
            "watchos_device_arm64",
            "arm64|watchos_device|WatchOS|watchos|True|device_arm64",
        ),
        (
            "watchos_device_arm64e",
            "arm64e|watchos_device|WatchOS|watchos|True|device_arm64e",
        ),
        (
            "watchos_arm64_32",
            "arm64_32|watchos_device|WatchOS|watchos|True|arm64_32",
        ),
        (
            "watchos_arm64",
            "arm64|watchos_simulator|WatchSimulator|watchos|False|arm64",
        ),
    ] {
        let cfg = BuildConfigurationKey::target(cpu, "fastbuild");
        let (r, cfg) = ws.resolver_with(cfg, AnalysisConfig::new());
        let ct = r
            .analyze(&Label::parse_canonical("//:probe").unwrap(), &cfg)
            .unwrap();
        let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
            panic!("expected FileWrite");
        };
        assert_eq!(
            String::from_utf8(content.render()).unwrap(),
            expected,
            "unexpected apple fragment values for {cpu}"
        );
    }
}

#[test]
fn map_directory_fingerprints_cyclic_bzl_globals_and_params_deterministically() {
    let make_bzl = |tag: &str, cyclic: bool| {
        if cyclic {
            format!(
                r#"
LIST = ["{tag}"]
LIST.append(LIST)

DICT = {{"tag": "{tag}"}}
DICT["self"] = DICT

HOLDER = []
WRAPPED = struct(items = HOLDER)
HOLDER.append(WRAPPED)

CYCLE_A = ["{tag}"]
CYCLE_B = [CYCLE_A]
CYCLE_A.append(CYCLE_B)

def _make_helper():
    local_cycle = ["{tag}"]
    local_cycle.append(local_cycle)
    def _helper():
        return local_cycle
    return _helper

_HELPER = _make_helper()

def _expand(template_ctx, *, input_directories, output_directories, additional_inputs, tools, additional_params):
    _ = (LIST, DICT, WRAPPED, CYCLE_A, _HELPER)

def _impl(ctx):
    in_dir = ctx.actions.declare_directory(ctx.label.name + "_in")
    ctx.actions.run(outputs = [in_dir], executable = "/bin/mkdir")
    out_dir = ctx.actions.declare_directory(ctx.label.name + "_out")
    ctx.actions.map_directory(
        input_directories = {{"src": in_dir}},
        output_directories = {{"dst": out_dir}},
        additional_params = {{"mode": "ok"}},
        implementation = _expand,
    )
    return [DefaultInfo(files = depset([out_dir]))]

cyclic_map = rule(implementation = _impl)
"#
            )
        } else {
            format!(
                r#"
LIST = ["{tag}", []]
DICT = {{"tag": "{tag}", "self": {{}}}}
HOLDER = [struct(items = [])]
WRAPPED = struct(items = HOLDER)

CYCLE_A = ["{tag}", [[]]]
CYCLE_B = [CYCLE_A]

def _make_helper():
    local_cycle = ["{tag}", []]
    def _helper():
        return local_cycle
    return _helper

_HELPER = _make_helper()

def _expand(template_ctx, *, input_directories, output_directories, additional_inputs, tools, additional_params):
    _ = (LIST, DICT, WRAPPED, CYCLE_A, _HELPER)

def _impl(ctx):
    in_dir = ctx.actions.declare_directory(ctx.label.name + "_in")
    ctx.actions.run(outputs = [in_dir], executable = "/bin/mkdir")
    out_dir = ctx.actions.declare_directory(ctx.label.name + "_out")
    ctx.actions.map_directory(
        input_directories = {{"src": in_dir}},
        output_directories = {{"dst": out_dir}},
        additional_params = {{"mode": "ok"}},
        implementation = _expand,
    )
    return [DefaultInfo(files = depset([out_dir]))]

cyclic_map = rule(implementation = _impl)
"#
            )
        }
    };

    let analyze_key = |bzl: &str| -> String {
        let ws = Ws::new(&[
            ("defs.bzl", bzl),
            (
                "BUILD",
                "load(\":defs.bzl\", \"cyclic_map\")\ncyclic_map(name = \"m\")\n",
            ),
        ]);
        let ct = ws.analyze("//:m").unwrap();
        let map_action = ct
            .typed_actions
            .iter()
            .find(|a| matches!(a.kind, ActionKind::MapDirectory { .. }))
            .expect("MapDirectory action");
        map_action.key()
    };

    let cyc_v1_run1 = analyze_key(&make_bzl("v1", true));
    let cyc_v1_run2 = analyze_key(&make_bzl("v1", true));
    let cyc_v2 = analyze_key(&make_bzl("v2", true));
    let non_cyc_v1 = analyze_key(&make_bzl("v1", false));

    assert_eq!(
        cyc_v1_run1, cyc_v1_run2,
        "cyclic Starlark globals must produce deterministic action keys across evaluations"
    );
    assert_ne!(
        cyc_v1_run1, cyc_v2,
        "distinct cyclic globals must produce distinct action keys"
    );
    assert_ne!(
        cyc_v1_run1, non_cyc_v1,
        "cyclic vs non-cyclic globals must produce distinct action keys"
    );
}

#[test]
fn composed_split_and_patch_transitions_match_bazel_split_attr_keys() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _split1_impl(settings, attr):
    return {
        "a": {"//command_line_option:copt": ["-DA"]},
        "b": {"//command_line_option:copt": ["-DB"]},
    }

def _split2_impl(settings, attr):
    return {
        "x": {"//command_line_option:cxxopt": ["-DX"]},
        "y": {"//command_line_option:cxxopt": ["-DY"]},
    }

def _patch_impl(settings, attr):
    return {"//command_line_option:conlyopt": ["-DPATCH"]}

split1 = transition(
    implementation = _split1_impl,
    inputs = [],
    outputs = ["//command_line_option:copt"],
)
split2 = transition(
    implementation = _split2_impl,
    inputs = [],
    outputs = ["//command_line_option:cxxopt"],
)
patch = transition(
    implementation = _patch_impl,
    inputs = [],
    outputs = ["//command_line_option:conlyopt"],
)

split_then_split = split1.and_then(split2)
patch_then_split = patch.and_then(split2)
split_then_patch = split1.and_then(patch)
patch_then_patch = patch.and_then(config.target())

def _leaf_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(
        out,
        "{}|{}|{}".format(
            ",".join(ctx.fragments.cpp.copts),
            ",".join(ctx.fragments.cpp.cxxopts),
            ",".join(ctx.fragments.cpp.conlyopts),
        ),
    )
    return [DefaultInfo(files = depset([out]))]

leaf = rule(implementation = _leaf_impl, fragments = ["cpp"])

def _format_split(split_dict):
    entries = []
    for k in sorted(split_dict.keys()):
        entries.append(k + "->" + split_dict[k].files.to_list()[0].path)
    return ";".join(entries)

def _top_impl(ctx):
    out = ctx.actions.declare_file("summary.txt")
    lines = [
        "ss=" + _format_split(ctx.split_attr.ss),
        "ps=" + _format_split(ctx.split_attr.ps),
        "sp=" + _format_split(ctx.split_attr.sp),
        "pp=" + ctx.attr.pp[0].files.to_list()[0].path,
    ]
    ctx.actions.write(out, "\n".join(lines))
    return [DefaultInfo(files = depset([out]))]

top = rule(
    implementation = _top_impl,
    attrs = {
        "ss": attr.label(cfg = split_then_split),
        "ps": attr.label(cfg = patch_then_split),
        "sp": attr.label(cfg = split_then_patch),
        "pp": attr.label(cfg = patch_then_patch),
    },
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "leaf", "top")
leaf(name = "l")
top(name = "t", ss = ":l", ps = ":l", sp = ":l", pp = ":l")
"#,
        ),
    ]);

    let ct = ws.analyze("//:t").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    let text = String::from_utf8(content.render()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");

    let st_ax = zel_analysis::transition_directory_name_fragment([
        "//command_line_option:copt=[-DA]",
        "//command_line_option:cxxopt=[-DX]",
    ]);
    let st_ay = zel_analysis::transition_directory_name_fragment([
        "//command_line_option:copt=[-DA]",
        "//command_line_option:cxxopt=[-DY]",
    ]);
    let st_bx = zel_analysis::transition_directory_name_fragment([
        "//command_line_option:copt=[-DB]",
        "//command_line_option:cxxopt=[-DX]",
    ]);
    let st_by = zel_analysis::transition_directory_name_fragment([
        "//command_line_option:copt=[-DB]",
        "//command_line_option:cxxopt=[-DY]",
    ]);
    assert_eq!(
        lines[0],
        format!(
            "ss=a,x->bazel-out/k8-fastbuild-{st_ax}/bin/l.txt;a,y->bazel-out/k8-fastbuild-{st_ay}/bin/l.txt;b,x->bazel-out/k8-fastbuild-{st_bx}/bin/l.txt;b,y->bazel-out/k8-fastbuild-{st_by}/bin/l.txt"
        ),
        "{text}"
    );

    let st_px = zel_analysis::transition_directory_name_fragment([
        "//command_line_option:conlyopt=[-DPATCH]",
        "//command_line_option:cxxopt=[-DX]",
    ]);
    let st_py = zel_analysis::transition_directory_name_fragment([
        "//command_line_option:conlyopt=[-DPATCH]",
        "//command_line_option:cxxopt=[-DY]",
    ]);
    assert_eq!(
        lines[1],
        format!(
            "ps=x->bazel-out/k8-fastbuild-{st_px}/bin/l.txt;y->bazel-out/k8-fastbuild-{st_py}/bin/l.txt"
        ),
        "{text}"
    );

    let st_ap = zel_analysis::transition_directory_name_fragment([
        "//command_line_option:conlyopt=[-DPATCH]",
        "//command_line_option:copt=[-DA]",
    ]);
    let st_bp = zel_analysis::transition_directory_name_fragment([
        "//command_line_option:conlyopt=[-DPATCH]",
        "//command_line_option:copt=[-DB]",
    ]);
    assert_eq!(
        lines[2],
        format!(
            "sp=a->bazel-out/k8-fastbuild-{st_ap}/bin/l.txt;b->bazel-out/k8-fastbuild-{st_bp}/bin/l.txt"
        ),
        "{text}"
    );

    let st_pp = zel_analysis::transition_directory_name_fragment([
        "//command_line_option:conlyopt=[-DPATCH]",
    ]);
    assert_eq!(
        lines[3],
        format!("pp=bazel-out/k8-fastbuild-{st_pp}/bin/l.txt"),
        "{text}"
    );
}

#[test]
fn composed_exec_transitions_use_resolved_default_and_exec_group_platforms() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
DepInfo = provider(fields = ["plat", "copts", "path", "matched"])

def _tc_impl(ctx):
    return [platform_common.ToolchainInfo(name = ctx.label.name)]

my_tc = rule(implementation = _tc_impl)

def _patch_impl(settings, attr):
    return {"//command_line_option:host_copt": ["-DPATCH"]}

patch = transition(
    implementation = _patch_impl,
    inputs = [],
    outputs = ["//command_line_option:host_copt"],
)

def _read_plat_patch_impl(settings, attr):
    plats = settings["//command_line_option:platforms"]
    first = str(plats[0]) if plats else "none"
    return {"//command_line_option:copt": ["-DFROM=" + first]}

read_plat_patch = transition(
    implementation = _read_plat_patch_impl,
    inputs = ["//command_line_option:platforms"],
    outputs = ["//command_line_option:copt"],
)

def _split_impl(settings, attr):
    return {
        "a": {"//command_line_option:host_copt": ["-DA"]},
        "b": {"//command_line_option:host_copt": ["-DB"]},
    }

split_t = transition(
    implementation = _split_impl,
    inputs = [],
    outputs = ["//command_line_option:host_copt"],
)

patch_then_default_exec = patch.and_then(config.exec())
patch_then_custom_exec = patch.and_then(config.exec(exec_group = "custom"))
custom_exec_then_patch = config.exec(exec_group = "custom").and_then(read_plat_patch)
split_then_default_exec = split_t.and_then(config.exec())
split_then_custom_exec = split_t.and_then(config.exec(exec_group = "custom"))

def _tool_leaf_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, "ok")
    return [
        DefaultInfo(files = depset([out])),
        DepInfo(
            plat = str(ctx.fragments.platform.platform),
            copts = ",".join(ctx.fragments.cpp.copts),
            path = out.path,
            matched = ctx.attr.matched,
        ),
    ]

tool_leaf = rule(
    implementation = _tool_leaf_impl,
    fragments = ["platform", "cpp"],
    attrs = {"matched": attr.string()},
)

def _fmt_dep(t):
    info = t[DepInfo]
    return "{}|{}|{}|{}".format(info.plat, info.matched, info.copts, info.path)

def _fmt_split(split_dict):
    parts = []
    for k in sorted(split_dict.keys()):
        parts.append(k + "=>" + _fmt_dep(split_dict[k]))
    return ";".join(parts)

def _consumer_impl(ctx):
    out = ctx.actions.declare_file("report.txt")
    lines = [
        "pde=" + _fmt_dep(ctx.attr.pde[0]),
        "pce=" + _fmt_dep(ctx.attr.pce[0]),
        "cep=" + _fmt_dep(ctx.attr.cep[0]),
        "sde=" + _fmt_split(ctx.split_attr.sde),
        "sce=" + _fmt_split(ctx.split_attr.sce),
    ]
    ctx.actions.write(out, "\n".join(lines))
    return [DefaultInfo(files = depset([out]))]

consumer = rule(
    implementation = _consumer_impl,
    toolchains = ["//:tt_default"],
    exec_groups = {
        "custom": exec_group(toolchains = ["//:tt_custom"]),
    },
    attrs = {
        "pde": attr.label(cfg = patch_then_default_exec),
        "pce": attr.label(cfg = patch_then_custom_exec),
        "cep": attr.label(cfg = custom_exec_then_patch),
        "sde": attr.label(cfg = split_then_default_exec),
        "sce": attr.label(cfg = split_then_custom_exec),
    },
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "consumer", "my_tc", "tool_leaf")

constraint_setting(name = "exec_kind")
constraint_value(name = "is_default_exec", constraint_setting = ":exec_kind")
constraint_value(name = "is_custom_exec", constraint_setting = ":exec_kind")

platform(name = "default_exec_plat", constraint_values = [":is_default_exec"])
platform(name = "custom_exec_plat", constraint_values = [":is_custom_exec"])

config_setting(name = "on_default_exec", constraint_values = [":is_default_exec"])
config_setting(name = "on_custom_exec", constraint_values = [":is_custom_exec"])

toolchain_type(name = "tt_default")
toolchain_type(name = "tt_custom")
my_tc(name = "tc_default_impl")
my_tc(name = "tc_custom_impl")
toolchain(
    name = "tc_default",
    toolchain_type = ":tt_default",
    toolchain = ":tc_default_impl",
    exec_compatible_with = [":is_default_exec"],
)
toolchain(
    name = "tc_custom",
    toolchain_type = ":tt_custom",
    toolchain = ":tc_custom_impl",
    exec_compatible_with = [":is_custom_exec"],
)

tool_leaf(
    name = "leaf",
    matched = select({
        ":on_default_exec": "default_exec",
        ":on_custom_exec": "custom_exec",
        "//conditions:default": "host_or_other",
    }),
)

consumer(
    name = "c",
    pde = ":leaf",
    pce = ":leaf",
    cep = ":leaf",
    sde = ":leaf",
    sce = ":leaf",
)
"#,
        ),
    ]);

    let mut acfg = AnalysisConfig::new();
    acfg.toolchain_patterns = vec!["//:tc_default".to_string(), "//:tc_custom".to_string()];
    acfg.execution_platform_patterns = vec![
        "//:default_exec_plat".to_string(),
        "//:custom_exec_plat".to_string(),
    ];
    let (r, cfg) = ws.resolver_with(BuildConfigurationKey::target("k8", "fastbuild"), acfg);
    let ct = r
        .analyze(&Label::parse_canonical("//:c").unwrap(), &cfg)
        .unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    let text = String::from_utf8(content.render()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "{text}");

    let pre_exec_mnemonic = |plat: &str, hcopt: &str| -> String {
        let mut expected = cfg.clone();
        let list = Value::List(std::sync::Arc::new(std::sync::Mutex::new(
            zel_starlark::StarlarkList::new(vec![Value::String(hcopt.to_string())]),
        )));
        zel_analysis::transition::set_setting(
            &mut expected,
            "//command_line_option:host_copt",
            &list,
        )
        .unwrap();
        expected.to_exec_for(plat).mnemonic()
    };
    let post_exec_mnemonic = |plat: &str, copt: &str| -> String {
        let mut expected = cfg.to_exec_for(plat);
        let list = Value::List(std::sync::Arc::new(std::sync::Mutex::new(
            zel_starlark::StarlarkList::new(vec![Value::String(copt.to_string())]),
        )));
        zel_analysis::transition::set_setting(&mut expected, "//command_line_option:copt", &list)
            .unwrap();
        expected.mnemonic()
    };
    let exec_copts = |hcopt: &str| -> String {
        if cfg!(windows) {
            hcopt.to_string()
        } else {
            format!("{hcopt},-g0")
        }
    };

    let m_pde = pre_exec_mnemonic("//:default_exec_plat", "-DPATCH");
    let c_pde = exec_copts("-DPATCH");
    assert!(
        m_pde.starts_with("default_exec_plat-opt-exec-ST-"),
        "{m_pde}"
    );
    assert_eq!(
        lines[0],
        format!("pde=@@//:default_exec_plat|default_exec|{c_pde}|bazel-out/{m_pde}/bin/leaf.txt"),
        "{text}"
    );

    let m_pce = pre_exec_mnemonic("//:custom_exec_plat", "-DPATCH");
    assert!(
        m_pce.starts_with("custom_exec_plat-opt-exec-ST-"),
        "{m_pce}"
    );
    assert_eq!(
        lines[1],
        format!("pce=@@//:custom_exec_plat|custom_exec|{c_pde}|bazel-out/{m_pce}/bin/leaf.txt"),
        "{text}"
    );

    let m_cep = post_exec_mnemonic("//:custom_exec_plat", "-DFROM=//:custom_exec_plat");
    assert_eq!(
        lines[2],
        format!(
            "cep=@@//:custom_exec_plat|custom_exec|-DFROM=//:custom_exec_plat|bazel-out/{m_cep}/bin/leaf.txt"
        ),
        "{text}"
    );

    let m_sde_a = pre_exec_mnemonic("//:default_exec_plat", "-DA");
    let m_sde_b = pre_exec_mnemonic("//:default_exec_plat", "-DB");
    let c_a = exec_copts("-DA");
    let c_b = exec_copts("-DB");
    assert_eq!(
        lines[3],
        format!(
            "sde=a=>@@//:default_exec_plat|default_exec|{c_a}|bazel-out/{m_sde_a}/bin/leaf.txt;b=>@@//:default_exec_plat|default_exec|{c_b}|bazel-out/{m_sde_b}/bin/leaf.txt"
        ),
        "{text}"
    );

    let m_sce_a = pre_exec_mnemonic("//:custom_exec_plat", "-DA");
    let m_sce_b = pre_exec_mnemonic("//:custom_exec_plat", "-DB");
    assert_eq!(
        lines[4],
        format!(
            "sce=a=>@@//:custom_exec_plat|custom_exec|{c_a}|bazel-out/{m_sce_a}/bin/leaf.txt;b=>@@//:custom_exec_plat|custom_exec|{c_b}|bazel-out/{m_sce_b}/bin/leaf.txt"
        ),
        "{text}"
    );

    // Composing two exec transitions is rejected.
    let two_exec_ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _p(settings, attr):
    return {}
p = transition(implementation = _p, inputs = [], outputs = [])
bad = config.exec().and_then(p).and_then(config.exec(exec_group = "g"))
r = rule(implementation = lambda ctx: [], attrs = {"d": attr.label(cfg = bad)})
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"x\")\n"),
    ]);
    let err = two_exec_ws.analyze("//:x").unwrap_err();
    assert!(
        err.contains("can't compose two exec transitions"),
        "expected two-exec error, got: {err}"
    );

    // Composing an exec transition into a rule `cfg` is rejected.
    let rule_exec_ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _p(settings, attr):
    return {}
p = transition(implementation = _p, inputs = [], outputs = [])
r = rule(implementation = lambda ctx: [], cfg = p.and_then(config.exec()))
"#,
        ),
        ("BUILD", "load(\":defs.bzl\", \"r\")\nr(name = \"x\")\n"),
    ]);
    let err = rule_exec_ws.analyze("//:x").unwrap_err();
    assert!(
        err.contains(
            "it contains a native transition that can only be used as an attribute transition"
        ),
        "expected rule-cfg exec error, got: {err}"
    );

    // Referencing a non-existent exec group in a composed transition fails analysis.
    let bad_group_ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
def _p(settings, attr):
    return {}
p = transition(implementation = _p, inputs = [], outputs = [])
leaf = rule(implementation = lambda ctx: [])
r = rule(
    implementation = lambda ctx: [],
    attrs = {"bad": attr.label(cfg = p.and_then(config.exec(exec_group = "missing")))},
)
"#,
        ),
        (
            "BUILD",
            "load(\":defs.bzl\", \"leaf\", \"r\")\nleaf(name = \"l\")\nr(name = \"x\", bad = \":l\")\n",
        ),
    ]);
    let err = bad_group_ws.analyze("//:x").unwrap_err();
    assert!(
        err.contains("Attr 'bad' declares a transition for non-existent exec group 'missing'"),
        "expected missing exec_group error, got: {err}"
    );
}

#[test]
fn test_rules_automatically_register_and_resolve_test_exec_group() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let main_root = PathFragment::create("/ws");
    let bt_root = PathFragment::create("/bt");
    let plat_root = PathFragment::create("/plat");
    fs.create_directory_and_parents(&main_root).unwrap();
    fs.create_directory_and_parents(&bt_root.get_relative_str("tools/test"))
        .unwrap();
    fs.create_directory_and_parents(&plat_root.get_relative_str("host"))
        .unwrap();

    fs.write_file(
        &plat_root.get_relative_str("host/BUILD"),
        b"platform(name = \"host\", visibility = [\"//visibility:public\"])\n",
    )
    .unwrap();
    fs.write_file(
        &bt_root.get_relative_str("tools/test/defs.bzl"),
        br#"
def _empty_tc_impl(ctx):
    return [platform_common.ToolchainInfo()]

empty_tc = rule(implementation = _empty_tc_impl)
"#,
    )
    .unwrap();
    fs.write_file(
        &bt_root.get_relative_str("tools/test/BUILD"),
        br#"
package(default_visibility = ["//visibility:public"])
load(":defs.bzl", "empty_tc")

toolchain_type(name = "default_test_toolchain_type")
empty_tc(name = "empty_toolchain")
toolchain(
    name = "default_test_toolchain",
    toolchain_type = ":default_test_toolchain_type",
    toolchain = ":empty_toolchain",
    use_target_platform_constraints = True,
)
"#,
    )
    .unwrap();
    fs.write_file(
        &main_root.get_relative_str("defs.bzl"),
        br#"
DepPlatInfo = provider(fields = ["plat", "copt"])

def _patch_impl(settings, attr):
    return {"//command_line_option:copt": ["-DTEST_EG"]}

patch_t = transition(
    implementation = _patch_impl,
    inputs = [],
    outputs = ["//command_line_option:copt"],
)

def _tool_impl(ctx):
    return [DepPlatInfo(
        plat = str(ctx.fragments.platform.platform),
        copt = ",".join(ctx.fragments.cpp.copts),
    )]

tool_target = rule(
    implementation = _tool_impl,
    fragments = ["platform", "cpp"],
)

def _tc_impl(ctx):
    return [platform_common.ToolchainInfo(marker = ctx.attr.marker)]

marker_tc = rule(implementation = _tc_impl, attrs = {"marker": attr.string()})

def _my_test_impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name + ".exe")
    ctx.actions.write(
        output = exe,
        content = "\n".join([
            "has_test=" + str("test" in ctx.exec_groups),
            "rule_ep=" + str(ctx.exec_groups["test"].exec_platform),
            "merger=" + ctx.attr._lcov_merger[DepPlatInfo].plat,
            "composed=" + ctx.attr._composed_tool[0][DepPlatInfo].plat + "|" + ctx.attr._composed_tool[0][DepPlatInfo].copt,
        ]),
        is_executable = True,
    )
    return [DefaultInfo(executable = exe, files = depset([exe]))]

my_test = rule(
    implementation = _my_test_impl,
    test = True,
    attrs = {
        "_lcov_merger": attr.label(
            default = "//:tool",
            cfg = config.exec("test"),
        ),
        "_composed_tool": attr.label(
            default = "//:tool",
            cfg = config.exec(exec_group = "test").and_then(patch_t),
        ),
    },
)

def _override_test_impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name + ".exe")
    tc = ctx.exec_groups["test"].toolchains["//:tt_custom_test"]
    ctx.actions.write(
        output = exe,
        content = str(ctx.exec_groups["test"].exec_platform) + "|" + tc.marker,
        is_executable = True,
    )
    return [DefaultInfo(executable = exe, files = depset([exe]))]

override_test = rule(
    implementation = _override_test_impl,
    test = True,
    exec_groups = {
        "test": exec_group(toolchains = ["//:tt_custom_test"]),
    },
)
"#,
    )
    .unwrap();
    fs.write_file(&main_root.get_relative_str("lib.rs"), b"fn main() {}\n")
        .unwrap();
    fs.write_file(
        &main_root.get_relative_str("BUILD"),
        br#"
load(":defs.bzl", "marker_tc", "my_test", "override_test", "tool_target")

constraint_setting(name = "env_kind")
constraint_setting(name = "special_kind")
constraint_value(name = "c_default", constraint_setting = ":env_kind")
constraint_value(name = "c_target", constraint_setting = ":env_kind")
constraint_value(name = "c_special", constraint_setting = ":special_kind")
constraint_value(name = "c_unmatched", constraint_setting = ":env_kind")

platform(name = "default_exec", constraint_values = [":c_default"])
platform(name = "target_matching_exec", constraint_values = [":c_target"])
platform(name = "special_exec", constraint_values = [":c_target", ":c_special"])
platform(name = "unmatched_target", constraint_values = [":c_unmatched"])

toolchain_type(name = "tt_custom_test")
marker_tc(name = "custom_test_tc_impl", marker = "custom_override")
toolchain(
    name = "custom_test_tc",
    toolchain_type = ":tt_custom_test",
    toolchain = ":custom_test_tc_impl",
    exec_compatible_with = [":c_special"],
)

tool_target(name = "tool")
my_test(name = "t")
my_test(
    name = "t_ecw",
    exec_group_compatible_with = {"test": [":c_special"]},
)
override_test(name = "t_override")
"#,
    )
    .unwrap();

    let make_resolver = || {
        let mut rr = SimpleRepoResolver::new(main_root.clone());
        rr.external.insert("bazel_tools".into(), bt_root.clone());
        rr.external.insert("platforms".into(), plat_root.clone());
        rr.apparent
            .insert("bazel_tools".into(), RepositoryName::bazel_tools());
        rr.apparent.insert(
            "platforms".into(),
            RepositoryName::create("platforms").unwrap(),
        );
        let lc = LoaderConfig::new(fs.clone(), Arc::new(rr));
        let mut acfg = AnalysisConfig::new();
        acfg.toolchain_patterns = vec![
            "@@bazel_tools//tools/test:all".to_string(),
            "//:custom_test_tc".to_string(),
        ];
        acfg.execution_platform_patterns = vec![
            "//:default_exec".to_string(),
            "//:target_matching_exec".to_string(),
            "//:special_exec".to_string(),
        ];
        TargetResolver::new(PackageLoader::new(lc), acfg)
    };

    // 1. Target platform matches `//:target_matching_exec`: default rule exec platform is
    //    `//:default_exec`, while the built-in `"test"` exec group resolves to
    //    `//:target_matching_exec` via `@bazel_tools//tools/test:default_test_toolchain`.
    let r = make_resolver();
    let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
    cfg.platforms = vec!["//:target_matching_exec".to_string()];
    let ct = r
        .analyze(&Label::parse_canonical("//:t").unwrap(), &cfg)
        .unwrap();
    assert_eq!(ct.exec_platform.as_deref(), Some("//:default_exec"));
    assert_eq!(
        ct.exec_group_platforms.get("test").map(String::as_str),
        Some("//:target_matching_exec")
    );
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    let text = String::from_utf8(content.render()).unwrap();
    assert_eq!(
        text,
        concat!(
            "has_test=True\n",
            "rule_ep=//:target_matching_exec\n",
            "merger=@@//:target_matching_exec\n",
            "composed=@@//:target_matching_exec|-DTEST_EG",
        )
    );

    // 2. `exec_group_compatible_with = {"test": [":c_special"]}` refines the `"test"` exec group.
    let ct_ecw = r
        .analyze(&Label::parse_canonical("//:t_ecw").unwrap(), &cfg)
        .unwrap();
    assert_eq!(
        ct_ecw.exec_group_platforms.get("test").map(String::as_str),
        Some("//:special_exec")
    );

    // 3. Explicit `exec_groups = {"test": ...}` overrides the built-in default `"test"` exec group.
    let ct_override = r
        .analyze(&Label::parse_canonical("//:t_override").unwrap(), &cfg)
        .unwrap();
    let ActionKind::FileWrite { content, .. } = &ct_override.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    assert_eq!(
        String::from_utf8(content.render()).unwrap(),
        "//:special_exec|custom_override"
    );

    // 4. Native test rule (`rust_test`) also registers and resolves the `"test"` exec group.
    let native_rule = zel_packages::RuleInstance {
        label: Label::parse_canonical("//:native_t").unwrap(),
        rule_class: "rust_test".to_string(),
        attrs: Default::default(),
        srcs: vec!["lib.rs".to_string()],
        deps: Vec::new(),
        outs: Vec::new(),
        tools: Vec::new(),
        data: Vec::new(),
        visibility: Vec::new(),
        cmd: None,
        executable: true,
        testonly: true,
        extra: Default::default(),
    };
    let ct_native = analyze_rule_inner(
        &native_rule,
        &cfg,
        r.clone() as Arc<dyn DepResolver>,
        &r.config,
    )
    .unwrap();
    assert_eq!(
        ct_native
            .exec_group_platforms
            .get("test")
            .map(String::as_str),
        Some("//:target_matching_exec")
    );

    // 5. When no execution platform matches `--platforms`, optional `default_test_toolchain_type`
    //    still allows building the test target by falling back to the first compatible platform.
    let r2 = make_resolver();
    let mut unmatched_cfg = BuildConfigurationKey::target("k8", "fastbuild");
    unmatched_cfg.platforms = vec!["//:unmatched_target".to_string()];
    let ct_unmatched = r2
        .analyze(&Label::parse_canonical("//:t").unwrap(), &unmatched_cfg)
        .unwrap();
    assert_eq!(
        ct_unmatched
            .exec_group_platforms
            .get("test")
            .map(String::as_str),
        Some("//:default_exec")
    );
}

#[test]
fn aspect_exec_groups_rule_exec_groups_and_exec_transitions_are_isolated() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
AspectReport = provider(fields = ["text"])
ToolPlat = provider(fields = ["plat", "copt"])

def _tc_impl(ctx):
    return [
        platform_common.ToolchainInfo(tag = ctx.attr.tag),
        platform_common.TemplateVariableInfo({ctx.attr.var_key: ctx.attr.tag}),
    ]

tag_tc = rule(
    implementation = _tc_impl,
    attrs = {
        "tag": attr.string(),
        "var_key": attr.string(),
    },
)

def _patch_impl(settings, attr):
    return {"//command_line_option:copt": ["-DASP_COMPOSED"]}

asp_patch = transition(
    implementation = _patch_impl,
    inputs = [],
    outputs = ["//command_line_option:copt"],
)

def _leaf_impl(ctx):
    return [ToolPlat(
        plat = str(ctx.fragments.platform.platform),
        copt = ",".join(ctx.fragments.cpp.copts),
    )]

tool_leaf = rule(
    implementation = _leaf_impl,
    fragments = ["platform", "cpp"],
)

def _probe_aspect_impl(target, ctx):
    out = ctx.actions.declare_file(target.label.name + ".aspect_report")
    lines = [
        "asp_has_asp_eg=" + str("asp_eg" in ctx.exec_groups),
        "asp_has_asp_copy=" + str("asp_copy" in ctx.exec_groups),
        "asp_has_parent=" + str("parent_only" in ctx.exec_groups),
        "asp_has_child=" + str("child_only" in ctx.exec_groups),
        "asp_eg_plat=" + str(ctx.exec_groups["asp_eg"].exec_platform),
        "asp_eg_tc=" + ctx.exec_groups["asp_eg"].toolchains["//:tt_asp_eg"].tag,
        "asp_copy_plat=" + str(ctx.exec_groups["asp_copy"].exec_platform),
        "asp_default_tc=" + ctx.toolchains["//:tt_asp_default"].tag,
        "asp_var=" + ctx.var.get("ASP_VAR", "missing") + "|" + ctx.var.get("PARENT_VAR", "none"),
        "rule_has_child=" + str("child_only" in ctx.rule.exec_groups),
        "rule_has_parent=" + str("parent_only" in ctx.rule.exec_groups),
        "rule_has_asp=" + str("asp_eg" in ctx.rule.exec_groups),
        "rule_child_plat=" + str(ctx.rule.exec_groups["child_only"].exec_platform),
        "rule_child_eg_tc=" + ctx.rule.exec_groups["child_only"].toolchains["//:tt_child_eg"].tag,
        "rule_default_tc=" + ctx.rule.toolchains["//:tt_child_default"].tag,
        "rule_var=" + ctx.rule.var.get("CHILD_VAR", "missing") + "|" + ctx.rule.var.get("PARENT_VAR", "none"),
        "tool_default=" + ctx.attr._tool_default[ToolPlat].plat,
        "tool_eg=" + ctx.attr._tool_eg[ToolPlat].plat,
        "tool_composed=" + ctx.attr._tool_composed[0][ToolPlat].plat + "|" + ctx.attr._tool_composed[0][ToolPlat].copt,
    ]
    text = "\n".join(lines)
    ctx.actions.write(out, text)
    return [
        AspectReport(text = text),
        OutputGroupInfo(report = depset([out])),
    ]

probe_aspect = aspect(
    implementation = _probe_aspect_impl,
    toolchains = ["//:tt_asp_default"],
    exec_groups = {
        "asp_eg": exec_group(toolchains = ["//:tt_asp_eg"]),
        "asp_copy": exec_group(copy_from_rule = True),
    },
    attrs = {
        "_tool_default": attr.label(
            default = "//:leaf",
            cfg = config.exec(),
        ),
        "_tool_eg": attr.label(
            default = "//:leaf",
            cfg = config.exec(exec_group = "asp_eg"),
        ),
        "_tool_composed": attr.label(
            default = "//:leaf",
            cfg = config.exec(exec_group = "asp_eg").and_then(asp_patch),
        ),
    },
)

def _child_impl(ctx):
    return []

child_rule = rule(
    implementation = _child_impl,
    toolchains = ["//:tt_child_default"],
    exec_groups = {
        "child_only": exec_group(toolchains = ["//:tt_child_eg"]),
    },
)

def _parent_impl(ctx):
    out = ctx.actions.declare_file("parent_report.txt")
    ctx.actions.write(out, ctx.attr.dep[AspectReport].text)
    return [DefaultInfo(files = depset([out]))]

parent_rule = rule(
    implementation = _parent_impl,
    toolchains = ["//:tt_parent"],
    exec_groups = {
        "parent_only": exec_group(toolchains = ["//:tt_parent"]),
    },
    attrs = {
        "dep": attr.label(aspects = [probe_aspect]),
    },
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "child_rule", "parent_rule", "tag_tc", "tool_leaf")

constraint_setting(name = "role")
constraint_value(name = "c_parent", constraint_setting = ":role")
constraint_value(name = "c_child_default", constraint_setting = ":role")
constraint_value(name = "c_child_eg", constraint_setting = ":role")
constraint_value(name = "c_asp_default", constraint_setting = ":role")
constraint_value(name = "c_asp_eg", constraint_setting = ":role")

platform(name = "ep_parent", constraint_values = [":c_parent"])
platform(name = "ep_child_default", constraint_values = [":c_child_default"])
platform(name = "ep_child_eg", constraint_values = [":c_child_eg"])
platform(name = "ep_asp_default", constraint_values = [":c_asp_default"])
platform(name = "ep_asp_eg", constraint_values = [":c_asp_eg"])

toolchain_type(name = "tt_parent")
toolchain_type(name = "tt_child_default")
toolchain_type(name = "tt_child_eg")
toolchain_type(name = "tt_asp_default")
toolchain_type(name = "tt_asp_eg")

tag_tc(name = "tc_parent_impl", tag = "parent_tc", var_key = "PARENT_VAR")
tag_tc(name = "tc_child_default_impl", tag = "child_default_tc", var_key = "CHILD_VAR")
tag_tc(name = "tc_child_eg_impl", tag = "child_eg_tc", var_key = "CHILD_EG_VAR")
tag_tc(name = "tc_asp_default_impl", tag = "asp_default_tc", var_key = "ASP_VAR")
tag_tc(name = "tc_asp_eg_impl", tag = "asp_eg_tc", var_key = "ASP_EG_VAR")

toolchain(
    name = "tc_parent",
    toolchain_type = ":tt_parent",
    toolchain = ":tc_parent_impl",
    exec_compatible_with = [":c_parent"],
)
toolchain(
    name = "tc_child_default",
    toolchain_type = ":tt_child_default",
    toolchain = ":tc_child_default_impl",
    exec_compatible_with = [":c_child_default"],
)
toolchain(
    name = "tc_child_eg",
    toolchain_type = ":tt_child_eg",
    toolchain = ":tc_child_eg_impl",
    exec_compatible_with = [":c_child_eg"],
)
toolchain(
    name = "tc_asp_default",
    toolchain_type = ":tt_asp_default",
    toolchain = ":tc_asp_default_impl",
    exec_compatible_with = [":c_asp_default"],
)
toolchain(
    name = "tc_asp_eg",
    toolchain_type = ":tt_asp_eg",
    toolchain = ":tc_asp_eg_impl",
    exec_compatible_with = [":c_asp_eg"],
)

tool_leaf(name = "leaf")
child_rule(name = "child")
parent_rule(name = "parent", dep = ":child")
"#,
        ),
    ]);

    let make_acfg = || {
        let mut acfg = AnalysisConfig::new();
        acfg.toolchain_patterns = vec![
            "//:tc_parent".to_string(),
            "//:tc_child_default".to_string(),
            "//:tc_child_eg".to_string(),
            "//:tc_asp_default".to_string(),
            "//:tc_asp_eg".to_string(),
        ];
        acfg.execution_platform_patterns = vec![
            "//:ep_parent".to_string(),
            "//:ep_child_default".to_string(),
            "//:ep_child_eg".to_string(),
            "//:ep_asp_default".to_string(),
            "//:ep_asp_eg".to_string(),
        ];
        acfg
    };

    let expected_report = concat!(
        "asp_has_asp_eg=True\n",
        "asp_has_asp_copy=True\n",
        "asp_has_parent=False\n",
        "asp_has_child=False\n",
        "asp_eg_plat=//:ep_asp_eg\n",
        "asp_eg_tc=asp_eg_tc\n",
        "asp_copy_plat=//:ep_asp_default\n",
        "asp_default_tc=asp_default_tc\n",
        "asp_var=asp_default_tc|none\n",
        "rule_has_child=True\n",
        "rule_has_parent=False\n",
        "rule_has_asp=False\n",
        "rule_child_plat=//:ep_child_eg\n",
        "rule_child_eg_tc=child_eg_tc\n",
        "rule_default_tc=child_default_tc\n",
        "rule_var=child_default_tc|none\n",
        "tool_default=@@//:ep_asp_default\n",
        "tool_eg=@@//:ep_asp_eg\n",
        "tool_composed=@@//:ep_asp_eg|-DASP_COMPOSED",
    );

    // 1. Propagated aspect via `parent_rule.dep`: neither `ctx.exec_groups` nor `ctx.rule.*`
    //    nor aspect `config.exec(...)` attributes leak `parent_rule`'s `parent_only` / `ep_parent`.
    let (r, cfg) = ws.resolver_with(
        BuildConfigurationKey::target("k8", "fastbuild"),
        make_acfg(),
    );
    let ct_parent = r
        .analyze(&Label::parse_canonical("//:parent").unwrap(), &cfg)
        .unwrap();
    let ActionKind::FileWrite { content, .. } = &ct_parent.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    assert_eq!(
        String::from_utf8(content.render()).unwrap(),
        expected_report
    );

    // 2. Top-level aspect via `analyze_with_aspects` (where caller inputs are empty) resolves
    //    identical `ctx.exec_groups`, `ctx.rule.exec_groups`, and `config.exec(...)` platforms.
    let (r_top, cfg_top) = ws.resolver_with(
        BuildConfigurationKey::target("k8", "fastbuild"),
        make_acfg(),
    );
    let asp = r_top
        .loader()
        .load_bzl_export(
            &Label::parse_canonical("//:defs.bzl").unwrap(),
            "probe_aspect",
        )
        .unwrap();
    let ct_top = r_top
        .analyze_with_aspects(
            &Label::parse_canonical("//:child").unwrap(),
            &cfg_top,
            std::slice::from_ref(&asp),
            &Default::default(),
        )
        .unwrap();
    let ActionKind::FileWrite {
        content: top_content,
        ..
    } = &ct_top.typed_actions.last().unwrap().kind
    else {
        panic!("expected FileWrite");
    };
    assert_eq!(
        String::from_utf8(top_content.render()).unwrap(),
        expected_report
    );

    // 3. Aspect attribute referencing a target/parent exec group not declared on the aspect fails.
    let bad_asp_ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
leaf = rule(implementation = lambda ctx: [])
bad_aspect = aspect(
    implementation = lambda target, ctx: [],
    attrs = {
        "_bad": attr.label(
            default = "//:leaf",
            cfg = config.exec(exec_group = "parent_or_target_group"),
        ),
    },
)
child = rule(
    implementation = lambda ctx: [],
    exec_groups = {"parent_or_target_group": exec_group()},
)
parent = rule(
    implementation = lambda ctx: [],
    exec_groups = {"parent_or_target_group": exec_group()},
    attrs = {"dep": attr.label(aspects = [bad_aspect])},
)
"#,
        ),
        (
            "BUILD",
            "load(\":defs.bzl\", \"child\", \"leaf\", \"parent\")\nleaf(name = \"leaf\")\nchild(name = \"c\")\nparent(name = \"p\", dep = \":c\")\n",
        ),
    ]);
    let err = bad_asp_ws.analyze("//:p").unwrap_err();
    assert!(
        err.contains(
            "Attr '_bad' declares a transition for non-existent exec group 'parent_or_target_group'"
        ),
        "expected missing aspect exec_group error, got: {err}"
    );

    // 4. Invalid exec group name on `aspect(exec_groups = ...)` is rejected at definition time.
    let invalid_name_ws = Ws::new(&[
        (
            "defs.bzl",
            "bad = aspect(implementation = lambda t, ctx: [], exec_groups = {\"bad name\": exec_group()})\n",
        ),
        ("BUILD", "load(\":defs.bzl\", \"bad\")\n"),
    ]);
    let err = invalid_name_ws.analyze("//:missing").unwrap_err();
    assert!(
        err.contains("Exec group name 'bad name' is not a valid name"),
        "expected invalid exec group name error, got: {err}"
    );
}

#[test]
fn nonsplit_starlark_transition_list_wrapping_matches_bazel_9_3_2() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r##"
MyInfo = provider(fields = ["val"])
AspectInfo = provider(fields = ["summary"])

def _leaf_impl(ctx):
    f = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(f, "#!/bin/sh\necho hi\n", is_executable = True)
    return [
        DefaultInfo(files = depset([f]), executable = f),
        MyInfo(val = ctx.var.get("COMPILATION_MODE", "") + ":" + ctx.label.name),
    ]

leaf = rule(implementation = _leaf_impl, executable = True)

def _t_1to1_impl(settings, attr):
    return {"//command_line_option:compilation_mode": "opt"}

t_1to1 = transition(
    implementation = _t_1to1_impl,
    inputs = [],
    outputs = ["//command_line_option:compilation_mode"],
)

def _t_noop_impl(settings, attr):
    return {}

t_noop = transition(
    implementation = _t_noop_impl,
    inputs = [],
    outputs = [],
)

def _t_1to2_impl(settings, attr):
    return {
        "k_opt": {"//command_line_option:compilation_mode": "opt"},
        "k_dbg": {"//command_line_option:compilation_mode": "dbg"},
    }

t_1to2 = transition(
    implementation = _t_1to2_impl,
    inputs = [],
    outputs = ["//command_line_option:compilation_mode"],
)

def _desc_inner(val):
    t = type(val)
    if t == "list":
        return "list({})[{}]".format(len(val), ",".join([type(x) + ":" + str(getattr(x, "label", x)) for x in val]))
    if t == "Target":
        return "Target(" + str(val.label) + ":" + val[MyInfo].val + ")"
    if t == "File":
        return "File(" + val.path + ")"
    return t + "(" + str(val) + ")"

def _desc(val):
    if type(val) == "dict":
        keys = sorted(val.keys(), key = lambda x: "" if x == None else str(x))
        return "dict{" + ",".join([str(k) + "=" + _desc_inner(val[k]) for k in keys]) + "}"
    return _desc_inner(val)

def _probe_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    lines = []
    for n in [
        "t1_set",
        "t1_unset",
        "t1_file_set",
        "t1_file_unset",
        "t1_exe_set",
        "t1_exe_unset",
        "tnoop_set",
        "t1to2_set",
        "t1to2_unset",
        "t1_ll_set",
        "t1_ll_unset",
        "t1to2_ll_set",
        "t1to2_ll_unset",
        "exec_str_set",
        "exec_str_unset",
        "target_str_set",
        "target_str_unset",
        "cfg_exec_set",
        "cfg_target_set",
        "cfg_none_set",
        "native_composed_set",
        "starlark_composed_set",
        "no_cfg_set",
        "no_cfg_unset",
    ]:
        a = getattr(ctx.attr, n)
        sp = _desc(getattr(ctx.split_attr, n)) if hasattr(ctx.split_attr, n) else "<absent>"
        fls = str(len(getattr(ctx.files, n))) if hasattr(ctx.files, n) else "<absent>"
        fl = _desc(getattr(ctx.file, n)) if hasattr(ctx.file, n) else "<absent>"
        ex = _desc(getattr(ctx.executable, n)) if hasattr(ctx.executable, n) else "<absent>"
        lines.append("{}|{}|{}|{}|{}|{}".format(n, _desc(a), sp, fls, fl, ex))
    # Verify rules_go's exact indexing idiom `ctx.attr.dep[0][Provider]`:
    lines.append("indexed=" + ctx.attr.t1_set[0][MyInfo].val)
    ctx.actions.write(out, "\n".join(lines))
    return [DefaultInfo(files = depset([out]))]

probe = rule(
    implementation = _probe_impl,
    attrs = {
        "t1_set": attr.label(cfg = t_1to1),
        "t1_unset": attr.label(cfg = t_1to1),
        "t1_file_set": attr.label(cfg = t_1to1, allow_single_file = True),
        "t1_file_unset": attr.label(cfg = t_1to1, allow_single_file = True),
        "t1_exe_set": attr.label(cfg = t_1to1, executable = True),
        "t1_exe_unset": attr.label(cfg = t_1to1, executable = True),
        "tnoop_set": attr.label(cfg = t_noop),
        "t1to2_set": attr.label(cfg = t_1to2),
        "t1to2_unset": attr.label(cfg = t_1to2),
        "t1_ll_set": attr.label_list(cfg = t_1to1),
        "t1_ll_unset": attr.label_list(cfg = t_1to1),
        "t1to2_ll_set": attr.label_list(cfg = t_1to2),
        "t1to2_ll_unset": attr.label_list(cfg = t_1to2),
        "exec_str_set": attr.label(cfg = "exec", allow_single_file = True, executable = True),
        "exec_str_unset": attr.label(cfg = "exec", allow_single_file = True, executable = True),
        "target_str_set": attr.label(cfg = "target", allow_single_file = True, executable = True),
        "target_str_unset": attr.label(cfg = "target", allow_single_file = True, executable = True),
        "cfg_exec_set": attr.label(cfg = config.exec()),
        "cfg_target_set": attr.label(cfg = config.target()),
        "cfg_none_set": attr.label(cfg = config.none()),
        "native_composed_set": attr.label(cfg = config.target().and_then(config.exec())),
        "starlark_composed_set": attr.label(cfg = t_1to1.and_then(config.target())),
        "no_cfg_set": attr.label(),
        "no_cfg_unset": attr.label(),
    },
)

def _incoming_impl(ctx):
    out = ctx.actions.declare_file("incoming.txt")
    lines = [
        "dep_set|" + _desc(ctx.attr.dep_set) + "|" + str(hasattr(ctx.split_attr, "dep_set")),
        "dep_unset|" + _desc(ctx.attr.dep_unset) + "|" + str(hasattr(ctx.split_attr, "dep_unset")),
        "dep_t1|" + _desc(ctx.attr.dep_t1) + "|" + _desc(ctx.split_attr.dep_t1),
    ]
    ctx.actions.write(out, "\n".join(lines))
    return [DefaultInfo(files = depset([out]))]

incoming = rule(
    implementation = _incoming_impl,
    cfg = t_1to1,
    attrs = {
        "dep_set": attr.label(),
        "dep_unset": attr.label(),
        "dep_t1": attr.label(cfg = t_1to1),
    },
)

def _asp_impl(target, ctx):
    return [AspectInfo(summary = "{}|{}|{}".format(
        _desc(ctx.rule.attr.t1_set),
        _desc(ctx.rule.attr.t1_unset),
        _desc(ctx.rule.attr.plain),
    ))]

my_aspect = aspect(implementation = _asp_impl)

holder = rule(
    implementation = lambda ctx: [],
    attrs = {
        "t1_set": attr.label(cfg = t_1to1),
        "t1_unset": attr.label(cfg = t_1to1),
        "plain": attr.label(),
    },
)

def _consumer_impl(ctx):
    out = ctx.actions.declare_file("aspect.txt")
    ctx.actions.write(out, ctx.attr.dep[AspectInfo].summary)
    return [DefaultInfo(files = depset([out]))]

consumer = rule(
    implementation = _consumer_impl,
    attrs = {"dep": attr.label(aspects = [my_aspect])},
)
"##,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "leaf", "probe", "incoming", "holder", "consumer")
leaf(name = "l1")
leaf(name = "l2")
probe(
    name = "p",
    t1_set = ":l1",
    t1_file_set = ":l1",
    t1_exe_set = ":l1",
    tnoop_set = ":l1",
    t1to2_set = ":l1",
    t1_ll_set = [":l1", ":l2"],
    t1to2_ll_set = [":l1", ":l2"],
    exec_str_set = ":l1",
    target_str_set = ":l1",
    cfg_exec_set = ":l1",
    cfg_target_set = ":l1",
    cfg_none_set = ":l1",
    native_composed_set = ":l1",
    starlark_composed_set = ":l1",
    no_cfg_set = ":l1",
)
incoming(name = "inc", dep_set = ":l1", dep_t1 = ":l1")
holder(name = "h", t1_set = ":l1", plain = ":l1")
consumer(name = "c", dep = ":h")
"#,
        ),
    ]);

    let ct = ws.analyze("//:p").unwrap();
    let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    let text = String::from_utf8(content.render()).unwrap();
    let by_name: BTreeMap<&str, &str> = text
        .lines()
        .filter_map(|l| l.split_once('|').or_else(|| l.split_once('=')))
        .collect();

    assert_eq!(by_name["indexed"], "opt:l1");
    assert_eq!(
        by_name["t1_set"],
        "list(1)[Target:@@//:l1]|dict{None=Target(@@//:l1:opt:l1)}|1|<absent>|<absent>"
    );
    assert_eq!(by_name["t1_unset"], "list(0)[]|dict{}|0|<absent>|<absent>");
    assert_eq!(
        by_name["t1_file_set"],
        "list(1)[Target:@@//:l1]|dict{None=Target(@@//:l1:opt:l1)}|1|File(bazel-out/k8-opt/bin/l1.sh)|<absent>"
    );
    assert_eq!(
        by_name["t1_file_unset"],
        "list(0)[]|dict{}|0|NoneType(None)|<absent>"
    );
    assert_eq!(
        by_name["t1_exe_set"],
        "list(1)[Target:@@//:l1]|dict{None=Target(@@//:l1:opt:l1)}|1|<absent>|File(bazel-out/k8-opt/bin/l1.sh)"
    );
    assert_eq!(
        by_name["t1_exe_unset"],
        "list(0)[]|dict{}|0|<absent>|NoneType(None)"
    );
    assert_eq!(
        by_name["tnoop_set"],
        "list(1)[Target:@@//:l1]|dict{None=Target(@@//:l1:fastbuild:l1)}|1|<absent>|<absent>"
    );
    assert_eq!(
        by_name["t1to2_set"],
        "list(2)[Target:@@//:l1,Target:@@//:l1]|dict{k_dbg=Target(@@//:l1:dbg:l1),k_opt=Target(@@//:l1:opt:l1)}|2|<absent>|<absent>"
    );
    assert_eq!(
        by_name["t1to2_unset"],
        "list(0)[]|dict{}|0|<absent>|<absent>"
    );
    assert_eq!(
        by_name["t1_ll_set"],
        "list(2)[Target:@@//:l1,Target:@@//:l2]|dict{None=list(2)[Target:@@//:l1,Target:@@//:l2]}|2|<absent>|<absent>"
    );
    assert_eq!(
        by_name["t1_ll_unset"],
        "list(0)[]|dict{}|0|<absent>|<absent>"
    );
    assert_eq!(
        by_name["t1to2_ll_set"],
        "list(4)[Target:@@//:l1,Target:@@//:l2,Target:@@//:l1,Target:@@//:l2]|dict{k_dbg=list(2)[Target:@@//:l1,Target:@@//:l2],k_opt=list(2)[Target:@@//:l1,Target:@@//:l2]}|4|<absent>|<absent>"
    );
    assert_eq!(
        by_name["t1to2_ll_unset"],
        "list(0)[]|dict{}|0|<absent>|<absent>"
    );
    assert!(
        by_name["exec_str_set"].starts_with("Target(@@//:l1:opt:l1)|<absent>|1|File("),
        "{}",
        by_name["exec_str_set"]
    );
    assert_eq!(
        by_name["exec_str_unset"],
        "NoneType(None)|<absent>|0|NoneType(None)|NoneType(None)"
    );
    assert_eq!(
        by_name["target_str_set"],
        "Target(@@//:l1:fastbuild:l1)|<absent>|1|File(bazel-out/k8-fastbuild/bin/l1.sh)|File(bazel-out/k8-fastbuild/bin/l1.sh)"
    );
    assert_eq!(
        by_name["target_str_unset"],
        "NoneType(None)|<absent>|0|NoneType(None)|NoneType(None)"
    );
    assert_eq!(
        by_name["cfg_exec_set"],
        "Target(@@//:l1:opt:l1)|<absent>|1|<absent>|<absent>"
    );
    assert_eq!(
        by_name["cfg_target_set"],
        "Target(@@//:l1:fastbuild:l1)|<absent>|1|<absent>|<absent>"
    );
    assert_eq!(
        by_name["cfg_none_set"],
        "Target(@@//:l1:fastbuild:l1)|<absent>|1|<absent>|<absent>"
    );
    assert_eq!(
        by_name["native_composed_set"],
        "Target(@@//:l1:opt:l1)|<absent>|1|<absent>|<absent>"
    );
    assert_eq!(
        by_name["starlark_composed_set"],
        "list(1)[Target:@@//:l1]|dict{None=Target(@@//:l1:opt:l1)}|1|<absent>|<absent>"
    );
    assert_eq!(
        by_name["no_cfg_set"],
        "Target(@@//:l1:fastbuild:l1)|<absent>|1|<absent>|<absent>"
    );
    assert_eq!(
        by_name["no_cfg_unset"],
        "NoneType(None)|<absent>|0|<absent>|<absent>"
    );

    // Incoming rule transition does NOT list-wrap plain attributes, but still wraps
    // attributes that themselves carry a Starlark outgoing transition.
    let inc = ws.analyze("//:inc").unwrap();
    let ActionKind::FileWrite { content, .. } = &inc.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    let inc_text = String::from_utf8(content.render()).unwrap();
    assert_eq!(
        inc_text,
        "dep_set|Target(@@//:l1:opt:l1)|False\ndep_unset|NoneType(None)|False\ndep_t1|list(1)[Target:@@//:l1]|dict{None=Target(@@//:l1:opt:l1)}"
    );

    // Aspect ctx.rule.attr sees the same list-wrapping on 1:1 Starlark transition attrs.
    let c = ws.analyze("//:c").unwrap();
    let ActionKind::FileWrite { content, .. } = &c.typed_actions[0].kind else {
        panic!("expected FileWrite");
    };
    let asp_text = String::from_utf8(content.render()).unwrap();
    assert_eq!(
        asp_text,
        "list(1)[Target:@@//:l1]|list(0)[]|Target(@@//:l1:fastbuild:l1)"
    );
}

#[test]
fn registered_toolchain_bzl_error_preserves_traceback_and_skips_when_unused() {
    let ws = Ws::new(&[
        (
            "types/BUILD",
            "toolchain_type(name = \"good_type\", visibility = [\"//visibility:public\"])\n",
        ),
        (
            "good_tc/defs.bzl",
            "def _impl(ctx):\n    return [platform_common.ToolchainInfo(val = \"ok\")]\ngood_tc = rule(implementation = _impl)\n",
        ),
        (
            "good_tc/BUILD",
            "load(\":defs.bzl\", \"good_tc\")\ngood_tc(name = \"impl\")\ntoolchain(name = \"tc\", toolchain = \":impl\", toolchain_type = \"//types:good_type\")\n",
        ),
        (
            "bad_tc/defs.bzl",
            "def _impl(ctx):\n    pass\n\nresolved_toolchain = rule(\n    implementation = _impl,\n    toolchains = [\"//types:good_type\"],\n    incompatible_use_toolchain_transition = True,\n)\n",
        ),
        (
            "bad_tc/BUILD",
            "load(\":defs.bzl\", \"resolved_toolchain\")\nresolved_toolchain(name = \"resolved\")\n",
        ),
        (
            "user/defs.bzl",
            r#"
def _needs_good(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, ctx.toolchains["//types:good_type"].val)
    return [DefaultInfo(files = depset([out]))]

needs_good = rule(implementation = _needs_good, toolchains = ["//types:good_type"])

def _no_tc(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, "none")
    return [DefaultInfo(files = depset([out]))]

no_tc = rule(implementation = _no_tc)
"#,
        ),
        (
            "user/BUILD",
            "load(\":defs.bzl\", \"needs_good\", \"no_tc\")\nneeds_good(name = \"use_good\")\nno_tc(name = \"use_none\")\n",
        ),
    ]);

    let make_acfg = || {
        let mut acfg = AnalysisConfig::new();
        acfg.toolchain_patterns = vec!["//good_tc:all".to_string(), "//bad_tc:all".to_string()];
        acfg
    };

    // Target requiring no toolchains succeeds even when a broken toolchain is registered.
    let (r_ok, cfg_ok) = ws.resolver_with(
        BuildConfigurationKey::target("k8", "fastbuild"),
        make_acfg(),
    );
    assert!(r_ok
        .analyze(&Label::parse_canonical("//user:use_none").unwrap(), &cfg_ok)
        .is_ok());

    // Target requiring a toolchain validates registered toolchains and preserves the Starlark
    // traceback for `rule() got unexpected keyword argument 'incompatible_use_toolchain_transition'`.
    let (r_bad, cfg_bad) = ws.resolver_with(
        BuildConfigurationKey::target("k8", "fastbuild"),
        make_acfg(),
    );
    let err = r_bad
        .analyze(
            &Label::parse_canonical("//user:use_good").unwrap(),
            &cfg_bad,
        )
        .unwrap_err();
    assert!(
        err.contains("Traceback (most recent call last):"),
        "expected Starlark traceback, got:\n{err}"
    );
    assert!(
        err.contains(
            "Error in rule: rule() got unexpected keyword argument 'incompatible_use_toolchain_transition'"
        ),
        "expected rule() kwarg error, got:\n{err}"
    );
    assert!(
        err.contains(
            "invalid registered toolchain '//bad_tc:all': error loading package 'bad_tc': initialization of module 'bad_tc/defs.bzl' failed"
        ),
        "expected invalid registered toolchain summary, got:\n{err}"
    );

    // `analysis_failure` renders each line cleanly without double `ERROR: ERROR:` prefixes.
    let rep = crate::failure::analysis_failure(
        &err,
        &Label::parse_canonical("//user:use_good").unwrap(),
        &cfg_bad.short_hash(),
        &|_| Some("/ws/user/BUILD:2:11".to_string()),
    );
    let rendered = rep.events.join("\n");
    assert!(!rendered.contains("ERROR: ERROR:"), "{rendered}");
    assert!(
        rendered.contains("ERROR: Traceback (most recent call last):"),
        "{rendered}"
    );
    assert!(
        rendered.contains("ERROR: invalid registered toolchain '//bad_tc:all'"),
        "{rendered}"
    );
}

#[test]
fn forwarded_default_info_allows_files_to_run_while_direct_kwarg_still_fails() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r##"
def _sdk_impl(ctx):
    f = ctx.actions.declare_file(ctx.label.name + ".sdk")
    rf = ctx.actions.declare_file(ctx.label.name + ".rf")
    drf = ctx.actions.declare_file(ctx.label.name + ".drf")
    ctx.actions.write(f, "sdk")
    ctx.actions.write(rf, "rf")
    ctx.actions.write(drf, "drf")
    return [DefaultInfo(
        files = depset([f]),
        default_runfiles = ctx.runfiles(files = [rf]),
        data_runfiles = ctx.runfiles(files = [drf]),
    )]

sdk_rule = rule(implementation = _sdk_impl)

def _bin_impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(exe, "#!/bin/sh\nexit 0\n", is_executable = True)
    return [DefaultInfo(executable = exe)]

bin_rule = rule(implementation = _bin_impl, executable = True)

def _toolchain_impl(ctx):
    return [
        ctx.attr.sdk[DefaultInfo],
        platform_common.ToolchainInfo(sdk = ctx.attr.sdk),
    ]

go_toolchain_like = rule(
    implementation = _toolchain_impl,
    attrs = {"sdk": attr.label(mandatory = True)},
)

exec_forwarding = rule(
    implementation = _toolchain_impl,
    executable = True,
    attrs = {"sdk": attr.label(mandatory = True)},
)

def _direct_files_to_run_impl(ctx):
    return [DefaultInfo(files_to_run = ctx.attr.sdk[DefaultInfo].files_to_run)]

direct_files_to_run = rule(
    implementation = _direct_files_to_run_impl,
    attrs = {"sdk": attr.label(mandatory = True)},
)
"##,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "sdk_rule", "bin_rule", "go_toolchain_like", "exec_forwarding", "direct_files_to_run")
sdk_rule(name = "sdk")
bin_rule(name = "bin")
go_toolchain_like(name = "tc_from_sdk", sdk = ":sdk")
go_toolchain_like(name = "tc_from_bin", sdk = ":bin")
exec_forwarding(name = "bad_exec_fwd", sdk = ":bin")
direct_files_to_run(name = "bad_direct", sdk = ":sdk")
"#,
        ),
    ]);

    let tc = ws.analyze("//:tc_from_sdk").unwrap();
    assert_eq!(tc.default_info.files.len(), 1);
    assert!(tc.default_info.files[0]
        .exec_path
        .path_string()
        .ends_with("sdk.sdk"));
    assert_eq!(tc.default_info.default_runfiles.len(), 1);
    assert!(tc.default_info.default_runfiles[0]
        .exec_path
        .path_string()
        .ends_with("sdk.rf"));
    assert_eq!(tc.default_info.data_runfiles.len(), 1);
    assert!(tc.default_info.data_runfiles[0]
        .exec_path
        .path_string()
        .ends_with("sdk.drf"));
    assert!(tc.default_info.executable.is_none());
    assert!(tc.providers.contains_key("ToolchainInfo"));

    // Forwarding DefaultInfo from an executable dependency to a non-executable rule must not
    // fail executable ownership validation and preserves files/runfiles with executable=None.
    let tc_bin = ws.analyze("//:tc_from_bin").unwrap();
    assert_eq!(tc_bin.default_info.files.len(), 1);
    assert!(tc_bin.default_info.files[0]
        .exec_path
        .path_string()
        .ends_with("bin.sh"));
    assert!(tc_bin.default_info.executable.is_none());

    // An executable rule still cannot satisfy its executable requirement by forwarding a dep's DefaultInfo.
    let err_exec = ws.analyze("//:bad_exec_fwd").unwrap_err();
    assert!(
        err_exec.contains("needs to create an executable File"),
        "{err_exec}"
    );

    // Direct Starlark `DefaultInfo(files_to_run = ...)` constructor calls must still fail.
    let err_direct = ws.analyze("//:bad_direct").unwrap_err();
    assert!(
        err_direct.contains("DefaultInfo() got unexpected keyword argument 'files_to_run'"),
        "{err_direct}"
    );
}

#[test]
fn alias_to_source_file_accepted_by_executable_and_file_attrs() {
    let ws = Ws::new(&[
        ("bin/tool.sh", "#!/bin/sh\necho hi\n"),
        (
            "defs.bzl",
            r#"
MyInfo = provider()

def _non_exec_impl(ctx):
    f = ctx.actions.declare_file("data.txt")
    ctx.actions.write(f, "data")
    return [DefaultInfo(files = depset([f]))]

non_exec = rule(implementation = _non_exec_impl)

def _consumer_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".out")
    node_path = ctx.executable.node.path if ctx.executable.node else "none"
    single_path = ctx.file.single.path if ctx.file.single else "none"
    ctx.actions.write(out, node_path + "|" + single_path)
    return [DefaultInfo(files = depset([out]))]

consumer = rule(
    implementation = _consumer_impl,
    attrs = {
        "node": attr.label(executable = True, cfg = "exec", mandatory = True),
        "single": attr.label(allow_single_file = [".sh"], providers = [MyInfo]),
    },
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "non_exec", "consumer")
alias(name = "node_bin", actual = "bin/tool.sh")
alias(name = "node_bin_chain", actual = ":node_bin")
non_exec(name = "plain")
alias(name = "plain_alias", actual = ":plain")
consumer(name = "ok_single_alias", node = ":node_bin", single = ":node_bin")
consumer(name = "ok_chained_alias", node = ":node_bin_chain", single = ":node_bin_chain")
consumer(name = "bad_non_exec_alias", node = ":plain_alias")
"#,
        ),
    ]);

    let src_alias = ws.analyze("//:node_bin").unwrap();
    assert_eq!(
        src_alias
            .default_info
            .executable
            .as_ref()
            .map(|a| a.exec_path.path_string()),
        Some("bin/tool.sh")
    );

    for target in ["//:ok_single_alias", "//:ok_chained_alias"] {
        let ct = ws.analyze(target).unwrap();
        let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
            panic!("expected FileWrite action for {target}");
        };
        let text = String::from_utf8(content.render()).unwrap();
        assert_eq!(text, "bin/tool.sh|bin/tool.sh");
    }

    let err = ws.analyze("//:bad_non_exec_alias").unwrap_err();
    assert!(
        err.contains("does not have mandatory executable output"),
        "{err}"
    );
}

#[test]
fn analysis_test_expect_failure_with_aspect_and_mandatory_dep_providers() {
    let ws = Ws::new(&[
        (
            "defs.bzl",
            r#"
MyInfo = provider(fields = ["val"])
ActionsInfo = provider(fields = ["actions"])

def _bad_dep_impl(ctx):
    fail("Non-deferred itest_service cannot depend on deferred itest_service")

bad_dep = rule(implementation = _bad_dep_impl)

def _no_provider_impl(ctx):
    return [DefaultInfo()]

no_provider = rule(implementation = _no_provider_impl)

def _mid_impl(ctx):
    return [MyInfo(val = ctx.attr.dep[MyInfo].val)]

mid_rule = rule(
    implementation = _mid_impl,
    attrs = {"dep": attr.label(providers = [MyInfo], mandatory = True)},
)

def _action_retrieving_aspect_impl(target, ctx):
    if ctx.rule.attr.dep:
        pass
    return [ActionsInfo(actions = target.actions)]

_action_retrieving_aspect = aspect(
    implementation = _action_retrieving_aspect_impl,
    attr_aspects = ["*"],
)

def _failure_test_impl(ctx):
    tut = ctx.attr.target_under_test[0]
    ok = AnalysisFailureInfo in tut
    causes = tut[AnalysisFailureInfo].causes.to_list() if ok else []
    msg = causes[0].message if len(causes) == 1 else "unexpected causes: " + str(len(causes))
    matched = ok and "Non-deferred itest_service cannot depend on deferred itest_service" in msg
    return [AnalysisTestResultInfo(success = matched, message = msg)]

failure_test = rule(
    implementation = _failure_test_impl,
    analysis_test = True,
    attrs = {
        "target_under_test": attr.label(
            cfg = analysis_test_transition(
                settings = {"//command_line_option:allow_analysis_failures": "true"},
            ),
            aspects = [_action_retrieving_aspect],
        ),
    },
)
"#,
        ),
        (
            "BUILD",
            r#"
load(":defs.bzl", "bad_dep", "no_provider", "mid_rule", "failure_test")
bad_dep(name = "deferred_fail")
no_provider(name = "missing_p")
mid_rule(name = "depends_on_failing", dep = ":deferred_fail")
mid_rule(name = "depends_on_missing", dep = ":missing_p")
failure_test(name = "test_direct_fail", target_under_test = ":deferred_fail")
failure_test(name = "test_transitive_fail", target_under_test = ":depends_on_failing")
"#,
        ),
    ]);

    // Both direct failure and transitive failure through mandatory provider attr must succeed
    // with the aspect attached and preserve the original failure cause message.
    for test_target in ["//:test_direct_fail", "//:test_transitive_fail"] {
        let ct = ws.analyze(test_target).unwrap();
        let ActionKind::FileWrite { content, .. } = &ct.typed_actions[0].kind else {
            panic!("expected FileWrite script for {test_target}");
        };
        let script = String::from_utf8(content.render()).unwrap();
        assert!(script.contains("exit 0"), "{test_target}:\n{script}");
    }

    // Without allow_analysis_failures, mandatory provider validation still rejects missing providers.
    let err = ws.analyze("//:depends_on_missing").unwrap_err();
    assert!(
        err.contains("does not have mandatory providers: 'MyInfo'"),
        "{err}"
    );
}

#[test]
fn starlark_doc_extract_emits_binaryproto_and_textproto_and_extracts_symbols() {
    let ws = Ws::new(&[
        ("pkg/BUILD", r#"exports_files(["defs.bzl"])"#),
        (
            "pkg/defs.bzl",
            r#""""Module summary docstring.

    More module details.
"""

def _private_helper():
    """Should not be exported."""
    pass

def greet(who, greeting = "hello", *args, loud = False, **kwargs):
    """Greets someone.

    Args:
        who: The recipient name.
        greeting: Greeting prefix.
        loud: Whether to shout.
        *args: Extra positional args.
        **kwargs: Extra keyword args.

    Returns:
        The formatted greeting string.

    Deprecated:
        Use greet_v2 instead.
    """
    pass

MyInfo = provider(
    doc = "Carries build metadata.",
    fields = {
        "files": "Output files.",
        "tag": "Optional tag.",
    },
)

my_test = rule(
    implementation = _private_helper,
    doc = "Runs a custom test.",
    test = True,
    provides = [MyInfo],
    attrs = {
        "srcs": attr.label_list(doc = "Source files.", mandatory = True),
        "mode": attr.string(doc = "Execution mode.", default = "fast", values = ["fast", "full"]),
        "verbose": attr.bool(doc = "Enable verbose logging."),
        "out": attr.output(doc = "Explicit output."),
        "_tool": attr.label(default = "//pkg:defs.bzl"),
    },
)

my_aspect = aspect(
    implementation = _private_helper,
    doc = "Collects metadata across deps.",
    attr_aspects = ["deps", "srcs"],
    attrs = {
        "flag": attr.int(doc = "Numeric flag.", default = 7),
    },
)

my_repo = repository_rule(
    implementation = _private_helper,
    doc = "Fetches an external archive.",
    environ = ["MY_ENV_VAR"],
    attrs = {
        "url": attr.string(doc = "Download URL.", mandatory = True),
    },
)

my_ext = module_extension(
    implementation = _private_helper,
    doc = "Configures toolchains.",
    tag_classes = {
        "install": tag_class(
            doc = "Installs a version.",
            attrs = {
                "version": attr.string(doc = "Tool version.", mandatory = True),
            },
        ),
    },
)

my_macro = macro(
    implementation = _private_helper,
    doc = "Wraps my_test.",
    finalizer = True,
    attrs = {
        "extra": attr.string_list(doc = "Extra tags."),
    },
)

#: Default timeout in seconds.
DEFAULT_TIMEOUT = 30
"#,
        ),
        (
            "docs/BUILD",
            r#"
starlark_doc_extract(
    name = "api.extract",
    src = "//pkg:defs.bzl",
)
"#,
        ),
    ]);

    let r = ws.resolver();
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");
    let ct = r
        .analyze(&Label::parse_canonical("//docs:api.extract").unwrap(), &cfg)
        .unwrap();

    // DefaultInfo.files contains only %{name}.binaryproto (matching StarlarkDocExtract.java).
    assert_eq!(ct.default_info.files.len(), 1);
    assert!(
        ct.default_info.files[0]
            .root_relative_path
            .path_string()
            .ends_with("docs/api.extract.binaryproto"),
        "unexpected default output: {:?}",
        ct.default_info.files[0].root_relative_path
    );
    assert_eq!(ct.output_groups.groups["binaryproto"].len(), 1);
    assert_eq!(ct.output_groups.groups["textproto"].len(), 1);
    assert!(ct.output_groups.groups["textproto"][0]
        .root_relative_path
        .path_string()
        .ends_with("docs/api.extract.textproto"));

    // Both implicit output file targets resolve cleanly through Target::OutputFile.
    let bin_ct = r
        .analyze(
            &Label::parse_canonical("//docs:api.extract.binaryproto").unwrap(),
            &cfg,
        )
        .unwrap();
    assert_eq!(bin_ct.default_info.files.len(), 1);
    let txt_ct = r
        .analyze(
            &Label::parse_canonical("//docs:api.extract.textproto").unwrap(),
            &cfg,
        )
        .unwrap();
    assert_eq!(txt_ct.default_info.files.len(), 1);

    // Extract the binaryproto and textproto payloads from the registered FileWrite actions.
    let bin_bytes = ct
        .typed_actions
        .iter()
        .find_map(|a| {
            if a.outputs
                .iter()
                .any(|o| o.root_relative_path.path_string().ends_with(".binaryproto"))
            {
                if let ActionKind::FileWrite {
                    content: zel_analysis::FileContent::Bytes(b),
                    ..
                } = &a.kind
                {
                    return Some(b.clone());
                }
            }
            None
        })
        .expect("missing binaryproto FileWrite action");

    let txt_str = ct
        .typed_actions
        .iter()
        .find_map(|a| {
            if a.outputs
                .iter()
                .any(|o| o.root_relative_path.path_string().ends_with(".textproto"))
            {
                if let ActionKind::FileWrite {
                    content: zel_analysis::FileContent::Literal(s),
                    ..
                } = &a.kind
                {
                    return Some(s.clone());
                }
            }
            None
        })
        .expect("missing textproto FileWrite action");

    let decoded = zel_proto::dynamic::Msg::decode("stardoc_output.ModuleInfo", &bin_bytes).unwrap();
    assert_eq!(decoded.to_text(), txt_str);
    assert_eq!(decoded.get_str("file"), "//pkg:defs.bzl");
    assert_eq!(
        decoded.get_str("module_docstring"),
        "Module summary docstring.\n\nMore module details."
    );

    // Function check (private helper excluded; params ordered ordinary -> kw_only -> *args -> **kwargs).
    let funcs = decoded.get_msgs("func_info");
    assert_eq!(funcs.len(), 1);
    assert_eq!(funcs[0].get_str("function_name"), "greet");
    assert_eq!(funcs[0].get_str("doc_string"), "Greets someone.");
    assert_eq!(
        funcs[0].get_msg("return").unwrap().get_str("doc_string"),
        "The formatted greeting string."
    );
    assert_eq!(
        funcs[0]
            .get_msg("deprecated")
            .unwrap()
            .get_str("doc_string"),
        "Use greet_v2 instead."
    );
    let params = funcs[0].get_msgs("parameter");
    let param_summary: Vec<(&str, &str, bool, String)> = params
        .iter()
        .map(|p| {
            (
                p.get_str("name"),
                p.get_str("default_value"),
                p.get_bool("mandatory"),
                p.get_enum_name("role"),
            )
        })
        .collect();
    assert_eq!(
        param_summary,
        vec![
            ("who", "", true, "PARAM_ROLE_ORDINARY".to_string()),
            (
                "greeting",
                "\"hello\"",
                false,
                "PARAM_ROLE_ORDINARY".to_string()
            ),
            (
                "loud",
                "False",
                false,
                "PARAM_ROLE_KEYWORD_ONLY".to_string()
            ),
            ("args", "", false, "PARAM_ROLE_VARARGS".to_string()),
            ("kwargs", "", false, "PARAM_ROLE_KWARGS".to_string()),
        ]
    );
    assert_eq!(params[0].get_str("doc_string"), "The recipient name.");

    // Rule check (private _tool excluded; implicit name attr first; test=true implies executable=true).
    let rules = decoded.get_msgs("rule_info");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].get_str("rule_name"), "my_test");
    assert_eq!(rules[0].get_str("doc_string"), "Runs a custom test.");
    assert!(rules[0].get_bool("test"));
    assert!(rules[0].get_bool("executable"));
    let attrs = rules[0].get_msgs("attribute");
    let attr_names: Vec<&str> = attrs.iter().map(|a| a.get_str("name")).collect();
    assert_eq!(attr_names, vec!["name", "srcs", "mode", "verbose", "out"]);
    assert_eq!(attrs[0].get_enum_name("type"), "NAME");
    assert!(attrs[0].get_bool("natively_defined"));
    assert_eq!(attrs[1].get_enum_name("type"), "LABEL_LIST");
    assert!(attrs[1].get_bool("mandatory"));
    assert_eq!(attrs[2].get_enum_name("type"), "STRING");
    assert_eq!(attrs[2].get_str("default_value"), "\"fast\"");
    assert_eq!(attrs[3].get_enum_name("type"), "BOOLEAN");
    assert_eq!(attrs[3].get_str("default_value"), "False");
    assert_eq!(attrs[4].get_enum_name("type"), "OUTPUT");
    assert!(attrs[4].get_bool("nonconfigurable"));

    // Provider check.
    let provs = decoded.get_msgs("provider_info");
    assert_eq!(provs.len(), 1);
    assert_eq!(provs[0].get_str("provider_name"), "MyInfo");
    assert_eq!(provs[0].get_str("doc_string"), "Carries build metadata.");
    let pfields = provs[0].get_msgs("field_info");
    assert_eq!(pfields.len(), 2);
    assert_eq!(pfields[0].get_str("name"), "files");
    assert_eq!(pfields[0].get_str("doc_string"), "Output files.");

    // Aspect check.
    let aspects = decoded.get_msgs("aspect_info");
    assert_eq!(aspects.len(), 1);
    assert_eq!(aspects[0].get_str("aspect_name"), "my_aspect");
    assert_eq!(aspects[0].values("aspect_attribute").len(), 2);

    // Repository rule, module extension, macro, and other-symbol doc comment checks.
    let repo_rules = decoded.get_msgs("repository_rule_info");
    assert_eq!(repo_rules.len(), 1);
    assert_eq!(repo_rules[0].get_str("rule_name"), "my_repo");
    assert_eq!(repo_rules[0].values("environ").len(), 1);

    let exts = decoded.get_msgs("module_extension_info");
    assert_eq!(exts.len(), 1);
    assert_eq!(exts[0].get_str("extension_name"), "my_ext");
    assert_eq!(exts[0].get_msgs("tag_class").len(), 1);
    assert_eq!(
        exts[0].get_msgs("tag_class")[0].get_str("tag_name"),
        "install"
    );

    let macros = decoded.get_msgs("macro_info");
    assert_eq!(macros.len(), 1);
    assert_eq!(macros[0].get_str("macro_name"), "my_macro");
    assert!(macros[0].get_bool("finalizer"));

    let others = decoded.get_msgs("starlark_other_symbol_info");
    assert_eq!(others.len(), 1);
    assert_eq!(others[0].get_str("name"), "DEFAULT_TIMEOUT");
    assert_eq!(others[0].get_str("doc"), "Default timeout in seconds.");
    assert_eq!(others[0].get_str("type_name"), "int");
}

#[test]
fn starlark_doc_extract_symbol_names_and_render_main_repo_name() {
    let ws = Ws::new(&[
        ("pkg/BUILD", r#"exports_files(["defs.bzl"])"#),
        (
            "pkg/defs.bzl",
            r#""Public API"

def alpha():
    """Alpha doc."""
    pass

def beta():
    """Beta doc."""
    pass

def _secret():
    """Secret doc."""
    pass
"#,
        ),
        (
            "docs/BUILD",
            r#"
starlark_doc_extract(
    name = "filtered",
    src = "//pkg:defs.bzl",
    symbol_names = ["alpha", "_secret"],
    render_main_repo_name = True,
)
"#,
        ),
    ]);

    let mut repo_resolver = SimpleRepoResolver::new(ws.root.clone());
    repo_resolver
        .apparent
        .insert("my_rules".to_string(), RepositoryName::main());
    let lc = LoaderConfig::new(ws.fs.clone(), Arc::new(repo_resolver));
    let r = TargetResolver::new(PackageLoader::new(lc), AnalysisConfig::new());
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");

    let ct = r
        .analyze(&Label::parse_canonical("//docs:filtered").unwrap(), &cfg)
        .unwrap();
    let bin_bytes = ct
        .typed_actions
        .iter()
        .find_map(|a| match &a.kind {
            ActionKind::FileWrite {
                content: zel_analysis::FileContent::Bytes(b),
                ..
            } => Some(b.clone()),
            _ => None,
        })
        .unwrap();

    let decoded = zel_proto::dynamic::Msg::decode("stardoc_output.ModuleInfo", &bin_bytes).unwrap();
    assert_eq!(decoded.get_str("file"), "@my_rules//pkg:defs.bzl");
    let func_names: Vec<&str> = decoded
        .get_msgs("func_info")
        .iter()
        .map(|f| f.get_str("function_name"))
        .collect();
    assert_eq!(func_names, vec!["_secret", "alpha"]);
    assert_eq!(
        decoded.get_msgs("func_info")[1]
            .get_msg("origin_key")
            .unwrap()
            .get_str("file"),
        "@my_rules//pkg:defs.bzl"
    );
}

#[test]
fn starlark_doc_extract_reexports_and_downstream_consumer() {
    let ws = Ws::new(&[
        ("pkg/BUILD", r#"exports_files(["defs.bzl", "impl.bzl"])"#),
        (
            "pkg/impl.bzl",
            r#"
def _impl(ctx):
    pass

impl_rule = rule(
    implementation = _impl,
    doc = "Rule defined in impl.bzl.",
)

def impl_fn(x):
    """Function defined in impl.bzl."""
    pass
"#,
        ),
        (
            "pkg/defs.bzl",
            r#""Re-exports."
load(":impl.bzl", _impl_fn = "impl_fn", _impl_rule = "impl_rule")

public_rule = _impl_rule
api = struct(fn = _impl_fn)
"#,
        ),
        (
            "docs/render.bzl",
            r#"
def _render_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".md")
    ctx.actions.run_shell(
        inputs = [ctx.file.src],
        outputs = [out],
        command = "cp $1 $2",
        arguments = [ctx.file.src.path, out.path],
    )
    return [DefaultInfo(files = depset([out]))]

render_docs = rule(
    implementation = _render_impl,
    attrs = {
        "src": attr.label(allow_single_file = True, mandatory = True),
    },
)
"#,
        ),
        (
            "docs/BUILD",
            r#"
load(":render.bzl", "render_docs")

starlark_doc_extract(
    name = "rules.extract",
    src = "//pkg:defs.bzl",
)

render_docs(
    name = "rules",
    src = ":rules.extract.binaryproto",
)
"#,
        ),
    ]);

    let r = ws.resolver();
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");

    // Downstream rule depending on `:rules.extract.binaryproto` succeeds in analysis!
    let render_ct = r
        .analyze(&Label::parse_canonical("//docs:rules").unwrap(), &cfg)
        .unwrap();
    assert_eq!(render_ct.default_info.files.len(), 1);
    assert_eq!(render_ct.typed_actions.len(), 1);
    assert!(render_ct.typed_actions[0].inputs.iter().any(|i| i
        .root_relative_path
        .path_string()
        .ends_with("docs/rules.extract.binaryproto")));

    // Re-exported rule and struct-nested function preserve their origin_key from impl.bzl.
    let extract_ct = r
        .analyze(
            &Label::parse_canonical("//docs:rules.extract").unwrap(),
            &cfg,
        )
        .unwrap();
    let bin_bytes = extract_ct
        .typed_actions
        .iter()
        .find_map(|a| match &a.kind {
            ActionKind::FileWrite {
                content: zel_analysis::FileContent::Bytes(b),
                ..
            } => Some(b.clone()),
            _ => None,
        })
        .unwrap();
    let decoded = zel_proto::dynamic::Msg::decode("stardoc_output.ModuleInfo", &bin_bytes).unwrap();
    let rules = decoded.get_msgs("rule_info");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].get_str("rule_name"), "public_rule");
    assert_eq!(
        rules[0].get_msg("origin_key").unwrap().get_str("name"),
        "impl_rule"
    );
    assert_eq!(
        rules[0].get_msg("origin_key").unwrap().get_str("file"),
        "//pkg:impl.bzl"
    );

    let funcs = decoded.get_msgs("func_info");
    assert_eq!(funcs.len(), 1);
    assert_eq!(funcs[0].get_str("function_name"), "api.fn");
    assert_eq!(
        funcs[0].get_msg("origin_key").unwrap().get_str("name"),
        "impl_fn"
    );
    assert_eq!(
        funcs[0].get_msg("origin_key").unwrap().get_str("file"),
        "//pkg:impl.bzl"
    );
}

#[test]
fn starlark_doc_extract_generated_src_handled_gracefully() {
    let ws = Ws::new(&[(
        "docs/BUILD",
        r#"
genrule(
    name = "gen_bzl",
    outs = ["generated.bzl"],
    cmd = "echo 'def f(): pass' > $@",
)

starlark_doc_extract(
    name = "gen.extract",
    src = ":generated.bzl",
)
"#,
    )]);

    let r = ws.resolver();
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");
    let ct = r
        .analyze(&Label::parse_canonical("//docs:gen.extract").unwrap(), &cfg)
        .unwrap();
    assert_eq!(ct.default_info.files.len(), 1);
    assert_eq!(ct.typed_actions.len(), 2);
    let bin_bytes = ct
        .typed_actions
        .iter()
        .find_map(|a| match &a.kind {
            ActionKind::FileWrite {
                content: zel_analysis::FileContent::Bytes(b),
                ..
            } => Some(b.clone()),
            _ => None,
        })
        .unwrap();
    let decoded = zel_proto::dynamic::Msg::decode("stardoc_output.ModuleInfo", &bin_bytes).unwrap();
    assert_eq!(decoded.get_str("file"), "//docs:generated.bzl");
}

#[test]
fn starlark_doc_extract_dedupes_loaded_struct_symbols_reexported_doc_comments_and_unicode_indent() {
    let ws = Ws::new(&[
        (
            "pkg/BUILD",
            r#"
exports_files(["impl.bzl", "api.bzl"])
starlark_doc_extract(
    name = "api.extract",
    src = "api.bzl",
    symbol_names = ["ns.a", "ns.b", "ALIASED_CONST", "unicode_doc"],
)
"#,
        ),
        (
            "pkg/impl.bzl",
            r#"
def _a():
    """Function A."""
    pass

def _b():
    """Function B."""
    pass

ns = struct(
    a = _a,
    b = _b,
)

#: Original documented constant.
ORIG_CONST = 42
"#,
        ),
        (
            "pkg/api.bzl",
            "load(\"//pkg:impl.bzl\", \"ns\", ALIASED_CONST = \"ORIG_CONST\")\n\ndef unicode_doc():\n    \"\"\"Header.\n\u{3000}Body line 1.\n    Body line 2.\n    \"\"\"\n    pass\n",
        ),
    ]);

    let r = ws.resolver();
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");
    let ct = r
        .analyze(&Label::parse_canonical("//pkg:api.extract").unwrap(), &cfg)
        .unwrap();
    let bin_bytes = ct
        .typed_actions
        .iter()
        .find_map(|a| match &a.kind {
            ActionKind::FileWrite {
                content: zel_analysis::FileContent::Bytes(b),
                ..
            } => Some(b.clone()),
            _ => None,
        })
        .unwrap();
    let decoded = zel_proto::dynamic::Msg::decode("stardoc_output.ModuleInfo", &bin_bytes).unwrap();

    // Multiple dotted symbol_names under the same loaded struct (`ns.a`, `ns.b`) must not duplicate entries.
    let func_names: Vec<&str> = decoded
        .get_msgs("func_info")
        .iter()
        .map(|f| f.get_str("function_name"))
        .collect();
    assert_eq!(func_names, vec!["ns.a", "ns.b", "unicode_doc"]);

    // Unicode whitespace indentation must not panic and should strip cleanly.
    let unicode_fn = decoded
        .get_msgs("func_info")
        .into_iter()
        .find(|f| f.get_str("function_name") == "unicode_doc")
        .unwrap();
    assert!(
        unicode_fn.get_str("doc_string").contains("Body line 1."),
        "doc_string={}",
        unicode_fn.get_str("doc_string")
    );

    // Re-exported `#:` doc comment is found via origin name (`ORIG_CONST`) under export name (`ALIASED_CONST`).
    let others = decoded.get_msgs("starlark_other_symbol_info");
    assert_eq!(others.len(), 1);
    assert_eq!(others[0].get_str("name"), "ALIASED_CONST");
    assert_eq!(others[0].get_str("doc"), "Original documented constant.");
}
