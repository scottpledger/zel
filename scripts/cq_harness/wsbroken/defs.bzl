def _impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ins = [f for s in ctx.attr.srcs for f in s.files.to_list()]
    ctx.actions.run_shell(inputs = ins, outputs=[out], command = "cat %s > %s" % (" ".join([i.path for i in ins]) or "/dev/null", out.path), mnemonic="CatIt", progress_message="Cat %s" % ctx.label)
    return [DefaultInfo(files=depset([out]))]
catrule = rule(_impl, attrs={"srcs": attr.label_list(allow_files=True), "tag_x": attr.string(default="d"), "opt": attr.string_list()})

def _asp_impl(target, ctx):
    out = ctx.actions.declare_file(target.label.name + ".asp")
    ctx.actions.write(out, "x")
    return [OutputGroupInfo(asp=depset([out]))]
asp = aspect(_asp_impl, attr_aspects=["srcs","deps"])

def _smacro(name, visibility, srcs, **kw):
    native.filegroup(name = name + "_fg", srcs = srcs, visibility=visibility)
    catrule(name = name + "_cat", srcs = [":" + name + "_fg"], visibility=visibility)
smacro = macro(implementation=_smacro, attrs={"srcs": attr.label_list(allow_files=True)})

def lmacro(name, **kw):
    native.genrule(name=name, outs=[name+".o"], cmd="echo "+name+" > $@", **kw)
    native.filegroup(name=name+"_grp", srcs=[":"+name])

def _flag_impl(ctx):
    return [DefaultInfo()]
def _t_impl(settings, attr):
    return {"//command_line_option:compilation_mode": "opt"}
t = transition(implementation=_t_impl, inputs=[], outputs=["//command_line_option:compilation_mode"])
trule = rule(_flag_impl, attrs={"dep": attr.label(cfg=t), "_allowlist_function_transition": attr.label(default="@bazel_tools//tools/allowlists/function_transition_allowlist")})

def _tool_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(out, "#!/bin/sh\ntrue\n", is_executable=True)
    return [DefaultInfo(executable=out, runfiles=ctx.runfiles(files=ctx.files.data))]
xbin = rule(_tool_impl, executable=True, attrs={"srcs": attr.label_list(allow_files=True), "deps": attr.label_list(), "data": attr.label_list(allow_files=True)})
x_test = rule(_tool_impl, test=True, attrs={"srcs": attr.label_list(allow_files=True), "deps": attr.label_list(), "data": attr.label_list(allow_files=True)})
xlib = rule(lambda ctx: [DefaultInfo()], attrs={"srcs": attr.label_list(allow_files=True), "deps": attr.label_list(), "data": attr.label_list(allow_files=True)})
