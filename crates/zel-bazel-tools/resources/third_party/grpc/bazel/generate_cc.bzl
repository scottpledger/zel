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
)

def _uniq(items):
    seen = {}
    out = []
    for item in items:
        if item not in seen:
            seen[item] = True
            out.append(item)
    return out

def _generate_cc_impl(ctx):
    protos = protos_from_context(ctx)
    if not protos:
        fail("ProtoInfo in 'srcs' contained no direct .proto sources", "srcs")

    includes = includes_from_deps(ctx.attr.srcs)
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

    proto_paths = _uniq([get_include_directory(imp) for imp in includes] + [out_dir.path])
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

generate_cc = rule(
    implementation = _generate_cc_impl,
    attrs = {
        "srcs": attr.label_list(
            mandatory = True,
            allow_empty = False,
            providers = [[_ProtobufProtoInfo], [ProtoInfo]],
        ),
        "plugin": attr.label(
            executable = True,
            cfg = "exec",
        ),
        "flags": attr.string_list(
            mandatory = False,
            allow_empty = True,
        ),
        "well_known_protos": attr.bool(
            default = False,
            mandatory = False,
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
