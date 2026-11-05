def _gen(rctx):
    rctx.file("BUILD.bazel", "")
r = repository_rule(implementation=_gen)
def _impl(ctx):
    r(name="local_jdk"); r(name="remote_java_tools")
    return ctx.extension_metadata(reproducible=True)
toolchains = module_extension(implementation=_impl)
