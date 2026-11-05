def _gen(rctx):
    rctx.file("BUILD.bazel", "")
r = repository_rule(implementation=_gen)
def _impl(ctx):
    r(name="buildozer_binary")
    return ctx.extension_metadata(reproducible=True)
buildozer_binary = module_extension(implementation=_impl)
