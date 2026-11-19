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

"""gRPC build helper rules for @bazel_tools//third_party/grpc."""

load("@rules_cc//cc:cc_library.bzl", "cc_library")

def _grpc_plugin_binary_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(
        output = out,
        content = "#!/usr/bin/env bash\nexec %s \"$@\"\n" % ctx.attr.tool_name,
        is_executable = True,
    )
    return [DefaultInfo(executable = out, files = depset([out]))]

grpc_plugin_binary = rule(
    implementation = _grpc_plugin_binary_impl,
    executable = True,
    attrs = {
        "tool_name": attr.string(default = "grpc_cpp_plugin"),
    },
)

def grpc_cc_library(
        name,
        srcs = [],
        public_hdrs = [],
        hdrs = [],
        external_deps = [],
        deps = [],
        standalone = False,
        language = "c++",
        testonly = False,
        visibility = None,
        data = [],
        alwayslink = 0,
        **kwargs):
    """Wrapper around cc_library matching Bazel's third_party/grpc/build_defs.bzl."""
    _ = standalone
    copts = []
    if language.lower() == "c":
        copts = select({
            "//src/conditions:windows": ["/we4013"],
            "//conditions:default": ["-std=c99", "-Wimplicit-function-declaration"],
        })
    cc_library(
        name = name,
        srcs = srcs,
        hdrs = hdrs + public_hdrs,
        deps = deps + ["//third_party/" + dep for dep in external_deps],
        copts = copts,
        defines = ["GRPC_ARES=0"],
        includes = [".", "include"],
        linkopts = select({
            "//src/conditions:windows": [],
            "//conditions:default": ["-pthread"],
        }),
        data = data,
        testonly = testonly,
        visibility = visibility,
        alwayslink = alwayslink,
        **kwargs
    )
