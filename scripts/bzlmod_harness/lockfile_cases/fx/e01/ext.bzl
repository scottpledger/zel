def _r(rctx):
    rctx.file("BUILD.bazel", "")
r = repository_rule(implementation = _r)
def _impl(mctx):
    for m in mctx.modules:
        for t in m.tags.mk:
            r(name = t.name)
mk = tag_class(attrs = {"name": attr.string()})
ex = module_extension(implementation = _impl, tag_classes = {"mk": mk})
