def _cat_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".out")
    ins = []
    for s in ctx.attr.srcs:
        ins += s.files.to_list()
    args = ctx.actions.args()
    args.add_all(ins)
    args.add(out)
    ctx.actions.run_shell(inputs = ins, outputs = [out], command = "cat \"$@\" > /dev/null; echo done > " + out.path, arguments = [args], mnemonic = "Cat7", progress_message = "Catting %{label}", env = {"K": ctx.attr.k})
    return [DefaultInfo(files = depset([out]), runfiles = ctx.runfiles(files = ins))]

cat = rule(_cat_impl, attrs = {
    "srcs": attr.label_list(allow_files = True),
    "k": attr.string(default = "v"),
    "opts": attr.string_list(default = ["a", "b"]),
    "m": attr.string_dict(),
    "n": attr.int(default = 3),
    "flag": attr.bool(default = False),
    "tool": attr.label(cfg = "exec", allow_files = True),
    "out2": attr.output(),
}, doc = "cat rule")

def _write_impl(ctx):
    f = ctx.actions.declare_file(ctx.attr.name + ".txt")
    ctx.actions.write(f, ctx.attr.text)
    e = ctx.actions.declare_file(ctx.attr.name + ".sh")
    ctx.actions.write(e, "#!/bin/sh\necho hi\n", is_executable = True)
    d = ctx.actions.declare_directory(ctx.attr.name + "_dir")
    ctx.actions.run_shell(outputs = [d], command = "mkdir -p " + d.path + " && echo a > " + d.path + "/a", mnemonic = "MkDir7")
    return [DefaultInfo(files = depset([f, d]), executable = e)]
wr = rule(_write_impl, attrs = {"text": attr.string(default = "x"), "deps": attr.label_list()}, executable = True)

def _t_impl(settings, attr):
    return {"//command_line_option:compilation_mode": "opt", "//cfg:str": "viaT"}
tr = transition(implementation = _t_impl, inputs = [], outputs = ["//command_line_option:compilation_mode", "//cfg:str"])

def _split_impl(settings, attr):
    return {"one": {"//cfg:str": "s1"}, "two": {"//cfg:str": "s2"}}
split = transition(implementation = _split_impl, inputs = [], outputs = ["//cfg:str"])

def _tdep(ctx):
    return [DefaultInfo(files = depset(transitive = [d[DefaultInfo].files for d in ctx.attr.dep]))]
tdep = rule(_tdep, attrs = {"dep": attr.label_list(cfg = tr, allow_files = True)})
sdep = rule(_tdep, attrs = {"dep": attr.label_list(cfg = split, allow_files = True)})

def _flag_impl(ctx):
    return [DefaultInfo(), config_common.FeatureFlagInfo(value = "x")] if False else [DefaultInfo()]
strflag = rule(_flag_impl, build_setting = config.string(flag = True))
intflag = rule(_flag_impl, build_setting = config.int(flag = True))

def _asp_impl(target, ctx):
    o = ctx.actions.declare_file(target.label.name + ".asp7")
    ctx.actions.write(o, "asp")
    return [OutputGroupInfo(aspout = depset([o]))]
asp = aspect(_asp_impl, attr_aspects = ["srcs", "deps", "dep"])

def _sm(name, visibility, srcs, **kw):
    native.filegroup(name = name + "_fg", srcs = srcs, visibility = visibility)
    cat(name = name + "_cat", srcs = [":" + name + "_fg"], visibility = visibility)
sm = macro(implementation = _sm, attrs = {"srcs": attr.label_list(allow_files = True)})

def lm(name, n = 2, **kw):
    for i in range(n):
        native.genrule(name = "%s_%d" % (name, i), outs = ["%s_%d.o" % (name, i)], cmd = "echo %d > $@" % i, **kw)
    native.filegroup(name = name, srcs = [":%s_%d" % (name, i) for i in range(n)])
