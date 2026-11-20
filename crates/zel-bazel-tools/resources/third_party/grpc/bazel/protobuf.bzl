# Copyright 2026 The Bazel Authors. All rights reserved.
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#    http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Helper functions for running protoc and gRPC codegen plugins."""

load("@com_google_protobuf//bazel/common:proto_info.bzl", _ProtobufProtoInfo = "ProtoInfo")

_VIRTUAL_IMPORTS = "/_virtual_imports/"
_PROTO_EXTENSION = ".proto"

def extract_proto_info(target):
    """Extracts ProtoInfo from a target."""
    if _ProtobufProtoInfo in target:
        return target[_ProtobufProtoInfo]
    fail("Target %s does not provide ProtoInfo" % target.label)

def well_known_proto_libs():
    """Returns the standard well-known protobuf proto_library labels."""
    return [
        Label("@com_google_protobuf//:any_proto"),
        Label("@com_google_protobuf//:api_proto"),
        Label("@com_google_protobuf//:compiler_plugin_proto"),
        Label("@com_google_protobuf//:descriptor_proto"),
        Label("@com_google_protobuf//:duration_proto"),
        Label("@com_google_protobuf//:empty_proto"),
        Label("@com_google_protobuf//:field_mask_proto"),
        Label("@com_google_protobuf//:source_context_proto"),
        Label("@com_google_protobuf//:struct_proto"),
        Label("@com_google_protobuf//:timestamp_proto"),
        Label("@com_google_protobuf//:type_proto"),
        Label("@com_google_protobuf//:wrappers_proto"),
    ]

def get_proto_root(workspace_root):
    """Computes the root protobuf directory prefix for `workspace_root`."""
    if workspace_root:
        return "/" + workspace_root
    return ""

def proto_path_to_generated_filename(proto_path, fmt_str):
    """Strips a trailing `.proto` suffix from `proto_path` and formats `fmt_str`."""
    if not proto_path.endswith(_PROTO_EXTENSION):
        fail("proto_path (" + proto_path + ") does not end with " + _PROTO_EXTENSION)
    return fmt_str.format(proto_path[:-len(_PROTO_EXTENSION)])

def is_in_virtual_imports(source_file, virtual_folder = _VIRTUAL_IMPORTS):
    """Returns True if `source_file` is a generated file under `_virtual_imports`."""
    return (not source_file.is_source) and (virtual_folder in source_file.path)

def get_include_directory(source_file):
    """Returns the `--proto_path` include directory for `source_file`."""
    if is_in_virtual_imports(source_file):
        prefix, rest = source_file.path.split(_VIRTUAL_IMPORTS, 1)
        first_seg = rest.split("/", 1)[0]
        return prefix + _VIRTUAL_IMPORTS + first_seg
    if not source_file.is_source:
        return source_file.root.path
    if source_file.path.startswith("external/"):
        parts = source_file.path.split("/", 2)
        if len(parts) >= 2:
            return "external/" + parts[1]
    return source_file.root.path if source_file.root.path else "."

def get_plugin_args(plugin, flags, dir_out, generate_mocks, plugin_name = "PLUGIN"):
    """Constructs `--plugin=protoc-gen-<name>=...` and `--<name>_out=...` arguments."""
    augmented_flags = list(flags)
    if generate_mocks:
        augmented_flags.append("generate_mock_code=true")
    plugin_flags = (",".join(augmented_flags) + ":") if augmented_flags else ""
    return [
        "--plugin=protoc-gen-" + plugin_name + "=" + plugin.path,
        "--" + plugin_name + "_out=" + plugin_flags + dir_out,
    ]

def protos_from_context(context):
    """Collects direct `.proto` files from `context.attr.srcs`, staging cross-package files."""
    protos = []
    for dep in context.attr.srcs:
        pi = extract_proto_info(dep)
        direct = pi.check_deps_sources.to_list() if hasattr(pi, "check_deps_sources") else pi.direct_sources
        for src in direct:
            if (not is_in_virtual_imports(src)) and src.owner and src.owner.package != context.label.package:
                staged = context.actions.declare_file(src.basename)
                context.actions.run_shell(
                    inputs = [src],
                    outputs = [staged],
                    command = "cp %s %s" % (src.path, staged.path),
                    mnemonic = "CopySourceProto",
                )
                protos.append(staged)
            else:
                protos.append(src)
    return protos

def includes_from_deps(deps):
    """Collects transitive `.proto` import files from `deps`."""
    sets = []
    for dep in deps:
        pi = extract_proto_info(dep)
        if hasattr(pi, "transitive_imports"):
            sets.append(pi.transitive_imports)
        else:
            sets.append(pi.transitive_sources)
    return depset(transitive = sets).to_list()

def get_out_dir(protos, context):
    """Computes the `--<lang>_out` output directory and optional virtual `import_path`."""
    virtual = [p for p in protos if is_in_virtual_imports(p)]
    real = [p for p in protos if not is_in_virtual_imports(p)]
    if virtual and real:
        fail("Proto sources must be either all virtual imports or all real")
    if virtual:
        inc = get_include_directory(virtual[0])
        ws_root = context.label.workspace_root
        if ws_root and inc.endswith("/" + ws_root):
            out_dir = inc[:-len(ws_root) - 1]
        elif ws_root and inc.endswith(ws_root):
            out_dir = inc[:-len(ws_root)].rstrip("/")
        else:
            out_dir = inc
        _, after = inc.split("_virtual_imports", 1)
        return struct(
            path = out_dir,
            import_path = "_virtual_imports" + after,
        )
    return struct(
        path = context.genfiles_dir.path + get_proto_root(context.label.workspace_root),
        import_path = None,
    )

def _package_relative_proto_path(proto, context):
    if is_in_virtual_imports(proto):
        return "_virtual_imports/" + proto.path.split(_VIRTUAL_IMPORTS, 1)[1]
    pkg = context.label.package
    ws = context.label.workspace_root
    prefix_parts = [p for p in [ws, pkg] if p]
    prefix = "/".join(prefix_parts)
    rel = proto.path
    if proto.root.path and rel.startswith(proto.root.path + "/"):
        rel = rel[len(proto.root.path) + 1:]
    if prefix:
        if rel.startswith(prefix + "/"):
            return rel[len(prefix) + 1:]
        fail("Proto source '%s' is not in package '%s'" % (proto.path, prefix))
    return rel

def declare_out_files(protos, context, templates):
    """Declares generated output files for each `(proto, template)` pair."""
    out_files = []
    for proto in protos:
        rel_path = _package_relative_proto_path(proto, context)
        for tmpl in templates:
            out_files.append(
                context.actions.declare_file(proto_path_to_generated_filename(rel_path, tmpl)),
            )
    return out_files

def get_proto_arguments(protos, genfiles_dir_path):
    """Formats `.proto` arguments passed on the `protoc` command line."""
    args = []
    for proto in protos:
        if is_in_virtual_imports(proto):
            inc = get_include_directory(proto)
            args.append(proto.path[len(inc) + 1:])
        elif proto.path.startswith(genfiles_dir_path + "/"):
            args.append(proto.path[len(genfiles_dir_path) + 1:])
        elif proto.root.path and proto.path.startswith(proto.root.path + "/"):
            args.append(proto.path[len(proto.root.path) + 1:])
        else:
            args.append(proto.path)
    return args
