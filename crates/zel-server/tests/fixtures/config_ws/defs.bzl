def _tool_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(out, "#!/bin/sh\necho hi\n", is_executable = True)
    return [DefaultInfo(executable = out)]

tool = rule(implementation = _tool_impl, executable = True)

def _user_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.run_shell(
        outputs = [out],
        tools = [ctx.executable.tool],
        command = "%s > %s" % (ctx.executable.tool.path, out.path),
    )
    return [DefaultInfo(files = depset([out]))]

user = rule(
    implementation = _user_impl,
    attrs = {"tool": attr.label(cfg = "exec", executable = True)},
)

def _test_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(out, "#!/bin/sh\nexit 0\n", is_executable = True)
    return [DefaultInfo(executable = out)]

my_test = rule(implementation = _test_impl, test = True)

def _flag_impl(ctx):
    return []

string_flag = rule(implementation = _flag_impl, build_setting = config.string(flag = True))

def _bflag_impl(ctx):
    return []

bool_flag = rule(implementation = _bflag_impl, build_setting = config.bool(flag = True))
int_flag = rule(implementation = _bflag_impl, build_setting = config.int(flag = True))
list_flag = rule(implementation = _bflag_impl, build_setting = config.string_list(flag = True, repeatable = True))
