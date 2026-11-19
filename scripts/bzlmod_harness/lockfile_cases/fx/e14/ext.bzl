def _r(rctx):
    rctx.file("BUILD.bazel", "")
r = repository_rule(implementation = _r, attrs = {"v": attr.string(), "l": attr.string_list()})

def _impl(mctx):
    for m in mctx.modules:
        for t in m.tags.mk:
            r(name = t.name, v = "x")

mk = tag_class(attrs = {"name": attr.string(), "opt": attr.string_list(), "n": attr.int(), "b": attr.bool(), "d": attr.string_dict(), "lab": attr.label()})
ex = module_extension(implementation = _impl, tag_classes = {"mk": mk})
