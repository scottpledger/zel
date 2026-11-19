def _gen(rctx):
    rctx.file("BUILD.bazel", "")
r = repository_rule(implementation=_gen)
def _impl(ctx):
    r(name="local_config_xcode")
    return ctx.extension_metadata(reproducible=True)
xcode_configure_extension = module_extension(implementation=_impl)
