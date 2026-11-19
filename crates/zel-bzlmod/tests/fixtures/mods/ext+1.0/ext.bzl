def _gen_r(rctx):
    rctx.file("BUILD.bazel", "filegroup(name='g', visibility=['//visibility:public'])\n")
    rctx.file("v.txt", rctx.attr.v)
gen = repository_rule(implementation=_gen_r, attrs={"v":attr.string()})

def _impl(ctx):
    for m in ctx.modules:
        for t in m.tags.mk:
            gen(name=t.name, v=m.name)
    return ctx.extension_metadata(reproducible=True)

mk = tag_class(attrs={"name":attr.string()})
ext = module_extension(implementation=_impl, tag_classes={"mk":mk})
