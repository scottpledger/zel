"""Exposes zel-bazel-tools/resources and builtins without subpackage BUILD boundaries."""

def _walk_and_link(rctx, root_dir, root_prefix):
    stack = [(root_dir, root_prefix)]
    for _ in range(100000):
        if not stack:
            break
        src_dir, dst_prefix = stack.pop()
        for entry in src_dir.readdir():
            rel = dst_prefix + "/" + entry.basename
            if entry.is_dir:
                stack.append((entry, rel))
            else:
                # Suffix files so nested BUILD / BUILD.bazel files do not create subpackages.
                rctx.symlink(entry, rel + ".zel_data")

def _zel_bazel_tools_data_impl(rctx):
    cargo_toml = rctx.path(Label("//:Cargo.toml"))
    crate_dir = cargo_toml.dirname.get_child("crates").get_child("zel-bazel-tools")
    _walk_and_link(rctx, crate_dir.get_child("resources"), "zel_res")
    _walk_and_link(rctx, crate_dir.get_child("builtins"), "zel_blt")
    rctx.file(
        "BUILD.bazel",
        'filegroup(name = "srcs", srcs = glob(["**"]), visibility = ["//visibility:public"])\n',
    )

zel_bazel_tools_data = repository_rule(
    implementation = _zel_bazel_tools_data_impl,
    local = True,
)
