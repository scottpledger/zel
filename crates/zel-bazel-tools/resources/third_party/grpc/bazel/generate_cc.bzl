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

"""Generates C++ protobuf or gRPC source and header files from `proto_library` targets."""

load("@com_google_protobuf//bazel/common:proto_info.bzl", _ProtobufProtoInfo = "ProtoInfo")
load(
    ":protobuf.bzl",
    "declare_out_files",
    "get_include_directory",
    "get_out_dir",
    "get_plugin_args",
    "get_proto_arguments",
    "includes_from_deps",
    "protos_from_context",
    "well_known_proto_libs",
)

def _uniq(items):
    seen = {}
    out = []
    for item in items:
        if item not in seen:
            seen[item] = True
            out.append(item)
    return out

def _well_known_include_dir(proto_file):
    if proto_file.dirname.endswith("/google/protobuf"):
        return proto_file.dirname[:-len("/google/protobuf")]
    if proto_file.dirname.endswith("/google/protobuf/compiler"):
        return proto_file.dirname[:-len("/google/protobuf/compiler")]
    return get_include_directory(proto_file)

def _generate_cc_impl(ctx):
    protos = protos_from_context(ctx)
    if not protos:
        fail("ProtoInfo in 'srcs' contained no direct .proto sources", "srcs")

    includes = list(includes_from_deps(ctx.attr.srcs))
    extra_wkt_dirs = []
    for wkt in ctx.attr.well_known_protos:
        if _ProtobufProtoInfo in wkt:
            includes.extend(includes_from_deps([wkt]))
        else:
            for wkt_file in wkt.files.to_list():
                includes.append(wkt_file)
                extra_wkt_dirs.append(_well_known_include_dir(wkt_file))

    out_dir = get_out_dir(protos, ctx)

    if ctx.executable.plugin:
        templates = ["{}.grpc.pb.h", "{}.grpc.pb.cc"]
        if ctx.attr.generate_mocks:
            templates.append("{}_mock.grpc.pb.h")
        plugin_or_cpp_args = get_plugin_args(
            ctx.executable.plugin,
            ctx.attr.flags,
            out_dir.path,
            ctx.attr.generate_mocks,
        )
        tools = [ctx.executable.plugin]
    else:
        templates = ["{}.pb.h", "{}.pb.cc"]
        flag_prefix = (",".join(ctx.attr.flags) + ":") if ctx.attr.flags else ""
        plugin_or_cpp_args = ["--cpp_out=" + flag_prefix + out_dir.path]
        tools = []

    out_files = declare_out_files(protos, ctx, templates)

    proto_paths = _uniq(
        [get_include_directory(imp) for imp in includes] +
        extra_wkt_dirs +
        [out_dir.path],
    )
    arguments = list(plugin_or_cpp_args)
    for inc_dir in proto_paths:
        arguments.append("--proto_path=" + inc_dir)
    arguments.extend(get_proto_arguments(protos, ctx.genfiles_dir.path))

    inputs = depset(direct = protos, transitive = [depset(includes)])
    ctx.actions.run(
        inputs = inputs,
        tools = tools,
        outputs = out_files,
        executable = ctx.executable.protoc,
        arguments = arguments,
        mnemonic = "CppGrpcGenSource",
        progress_message = "Generating C++ gRPC/proto sources for %s" % ctx.label,
        use_default_shell_env = True,
    )

    return [DefaultInfo(files = depset(out_files))]

_generate_cc = rule(
    implementation = _generate_cc_impl,
    attrs = {
        "srcs": attr.label_list(
            mandatory = True,
            allow_empty = False,
            providers = [_ProtobufProtoInfo],
        ),
        "plugin": attr.label(
            executable = True,
            cfg = "exec",
        ),
        "flags": attr.string_list(
            mandatory = False,
            allow_empty = True,
        ),
        "well_known_protos": attr.label_list(
            default = [],
            mandatory = False,
            allow_files = [".proto"],
        ),
        "generate_mocks": attr.bool(
            default = False,
            mandatory = False,
        ),
        "protoc": attr.label(
            default = Label("//third_party/protobuf:protoc"),
            executable = True,
            cfg = "exec",
        ),
    },
    output_to_genfiles = True,
)

def generate_cc(well_known_protos = False, **kwargs):
    if type(well_known_protos) == "bool":
        wkt_deps = well_known_proto_libs() if well_known_protos else []
    elif type(well_known_protos) == "list":
        wkt_deps = well_known_protos
    elif well_known_protos:
        wkt_deps = [well_known_protos]
    else:
        wkt_deps = []
    _generate_cc(
        well_known_protos = wkt_deps,
        **kwargs
    )
