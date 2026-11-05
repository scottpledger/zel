load("@rules_shell//shell:sh_binary.bzl", "sh_binary")
load("@rules_shell//shell:sh_test.bzl", "sh_test")
load("@rules_shell//shell:sh_library.bzl", "sh_library")
load("//:rules.bzl", "asp", "cat", "wr", "tdep", "sdep")
sh_binary(name = "bin", srcs = ["bin.sh"], deps = [], data = ["//lib:l2", "//lib:fg"])
sh_test(name = "app_test", srcs = ["bin.sh"], data = [":bin", "//lib:w1"], args = ["--x", "$(location :bin)"], env = {"E": "1"})
cat(name = "uses_priv", srcs = ["//lib:l1"])
cat(name = "uses_data", srcs = ["//data:d", "data_local.txt"], tool = "//tools:t")
tdep(name = "td", dep = ["//lib:l1", "//lib:l3"])
sdep(name = "sd", dep = ["//lib:l1"])
filegroup(name = "sel", srcs = select({
    "//cfg:opt": ["opt.txt"],
    "//cfg:dbg": ["dbg.txt"],
    "//cfg:defined": ["def.txt"],
    "//cfg:cpu_k8": ["k8.txt"],
    "//conditions:default": ["dflt.txt"],
}))
filegroup(name = "sel_deps", srcs = select({"//cfg:opt": ["//lib:l1"], "//conditions:default": ["//lib:l2"]}), data = select({"//cfg:strv": [":sel"], "//conditions:default": []}))
alias(name = "al", actual = select({"//cfg:opt": ":bin", "//conditions:default": ":uses_data"}))
cat(name = "tc", srcs = [":sel"], target_compatible_with = select({"//cfg:opt": [], "//conditions:default": ["@platforms//:incompatible"]}))
cat(name = "tc_ok", srcs = [":sel"], target_compatible_with = ["//cfg:always"])
filegroup(name = "vis_bad", srcs = ["//lib:fg"])
filegroup(name = "fg_empty")
genrule(name = "cfg_gen", outs = ["cg.txt"], cmd = "echo $(COMPILATION_MODE) > $@", toolchains = [])
