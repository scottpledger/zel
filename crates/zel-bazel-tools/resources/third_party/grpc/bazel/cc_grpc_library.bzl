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

"""Generates C++ gRPC service stubs and wraps them in a `cc_library`."""

load("@rules_cc//cc:defs.bzl", "cc_library")
load(":generate_cc.bzl", "generate_cc")

def cc_grpc_library(
        name,
        srcs,
        deps,
        proto_only = False,
        well_known_protos = False,
        generate_mocks = False,
        use_external = False,
        grpc_only = False,
        extra_cc_library_kwargs = {},
        **kwargs):
    """Generates C++ gRPC classes from a single `proto_library` target and compiles a `cc_library`."""
    _ = (proto_only, use_external, grpc_only)
    if len(srcs) != 1:
        fail("Only one srcs value supported", "srcs")

    codegen_grpc_target = "_" + name + "_grpc_codegen"
    generate_cc(
        name = codegen_grpc_target,
        srcs = srcs,
        plugin = Label("//third_party/grpc:cpp_plugin"),
        protoc = Label("//third_party/protobuf:protoc"),
        well_known_protos = well_known_protos,
        generate_mocks = generate_mocks,
        **kwargs
    )

    cc_kwargs = dict(extra_cc_library_kwargs)
    cc_kwargs.update(kwargs)
    cc_library(
        name = name,
        srcs = [":" + codegen_grpc_target],
        hdrs = [":" + codegen_grpc_target],
        deps = deps + [Label("//third_party/grpc:grpc++_codegen_proto")],
        **cc_kwargs
    )
