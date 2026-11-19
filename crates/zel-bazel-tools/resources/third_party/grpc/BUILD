load("@rules_cc//cc:cc_library.bzl", "cc_library")
load(":build_defs.bzl", "grpc_plugin_binary")

package(default_visibility = ["//visibility:public"])

exports_files(["build_defs.bzl"])

grpc_plugin_binary(
    name = "cpp_plugin",
    tool_name = "grpc_cpp_plugin",
)

cc_library(
    name = "grpc++_codegen_proto",
)
