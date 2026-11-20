# Copyright 2024 The Bazel Authors. All rights reserved.
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
"""Functions that create LTO indexing action."""

load(":common/cc/cc_helper_internal.bzl", "root_relative_path")
load(":common/cc/compile/lto_compilation_context.bzl", "get_minimized_bitcode_or_self")
load(":common/cc/link/finalize_link_action.bzl", "finalize_link_action")
load(":common/cc/link/link_build_variables.bzl", "setup_lto_indexing_variables")
load(":common/cc/link/lto_backends.bzl", "create_lto_backends")
load(":common/cc/link/target_types.bzl", "LINKING_MODE", "LINK_TARGET_TYPE", "is_dynamic_library")

_cc_internal = _builtins.internal.cc_internal

def create_lto_artifacts_and_lto_indexing_action(
        actions,
        link_type,
        linking_mode,
        use_pic,
        feature_configuration,
        cc_toolchain,
        compilation_outputs,
        libraries_to_link,
        static_libraries_to_link,
        prefer_pic_libs,
        linkopts,
        output,
        test_only_target,
        lto_compilation_context,
        variables_extensions,
        **link_action_args):
    """Creates all LTO artifacts and if allowed the LTO indexing action."""
    if not feature_configuration.is_enabled("supports_start_end_lib"):
        fail("When using LTO. The feature supports_start_end_lib must be enabled.")
    can_include_any_link_static_in_lto_indexing = \
        not feature_configuration.is_enabled("thin_lto_all_linkstatic_use_shared_nonlto_backends")
    can_include_any_link_static_test_target_in_lto_indexing = \
        not feature_configuration.is_enabled("thin_lto_linkstatic_tests_use_shared_nonlto_backends")
    include_link_static_in_lto_indexing = can_include_any_link_static_in_lto_indexing and (
        can_include_any_link_static_test_target_in_lto_indexing or not test_only_target
    )
    allow_lto_indexing = include_link_static_in_lto_indexing or (
        linking_mode == LINKING_MODE.DYNAMIC and bool(lto_compilation_context.lto_bitcode_inputs)
    )

    lto_output_root_prefix = root_relative_path(output) + ".lto" if allow_lto_indexing else "shared.nonlto"
    lto_obj_root_prefix = lto_output_root_prefix
    if feature_configuration.is_enabled("use_lto_native_object_directory"):
        lto_obj_root_prefix = lto_output_root_prefix + "-obj"
    object_file_inputs = compilation_outputs.pic_objects if use_pic else compilation_outputs.objects

    all_lto_artifacts = create_lto_backends(
        actions,
        lto_compilation_context,
        feature_configuration,
        cc_toolchain,
        use_pic,
        object_file_inputs,
        lto_output_root_prefix,
        lto_obj_root_prefix,
        static_libraries_to_link,
        allow_lto_indexing,
        include_link_static_in_lto_indexing,
        prefer_pic_libs,
    )
    if allow_lto_indexing:
        thinlto_param_file, thinlto_merged_object_file = _lto_indexing_action(
            actions = actions,
            cc_toolchain = cc_toolchain,
            compilation_outputs = compilation_outputs,
            feature_configuration = feature_configuration,
            linkopts = linkopts,
            variables_extensions = variables_extensions,
            output = output,
            link_type = link_type,
            libraries_to_link = libraries_to_link,
            static_libraries_to_link = static_libraries_to_link,
            prefer_pic_libs = prefer_pic_libs,
            use_pic = use_pic,
            linking_mode = linking_mode,
            all_lto_artifacts = all_lto_artifacts,
            include_link_static_in_lto_indexing = include_link_static_in_lto_indexing,
            lto_output_root_prefix = lto_output_root_prefix,
            lto_obj_root_prefix = lto_obj_root_prefix,
            **link_action_args
        )
    else:
        thinlto_param_file, thinlto_merged_object_file = None, None

    return all_lto_artifacts, allow_lto_indexing, thinlto_param_file, thinlto_merged_object_file

def _lto_indexing_action(
        actions,
        linking_mode,
        cc_toolchain,
        all_lto_artifacts,
        libraries_to_link,
        static_libraries_to_link,
        prefer_pic_libs,
        include_link_static_in_lto_indexing,
        compilation_outputs,
        output,
        feature_configuration,
        link_type,
        linkopts,
        use_pic,
        lto_output_root_prefix,
        lto_obj_root_prefix,
        stamping,  # @unused
        link_action_outputs,  # @unused
        variables_extensions,
        **link_action_args):
    lto_compilation_context = compilation_outputs._lto_compilation_context
    object_file_inputs = [
        get_minimized_bitcode_or_self(lto_compilation_context, input)
        for input in (compilation_outputs.pic_objects if use_pic else compilation_outputs.objects)
    ]

    lto_mapping = {}
    static_library_artifacts = set()
    for lib in static_libraries_to_link:
        if not lib._contains_objects:
            continue
        pic = (prefer_pic_libs and lib.pic_static_library != None) or lib.static_library == None
        if pic:
            library_artifact = lib.pic_static_library
            objects = lib.pic_objects
            shared_non_lto_backends = lib._pic_shared_non_lto_backends
            lib_lto_compilation_context = lib._pic_lto_compilation_context
        else:
            library_artifact = lib.static_library
            objects = lib.objects
            shared_non_lto_backends = lib._shared_non_lto_backends
            lib_lto_compilation_context = lib._lto_compilation_context
        if library_artifact in static_library_artifacts:
            continue
        static_library_artifacts.add(library_artifact)
        for a in objects:
            if not include_link_static_in_lto_indexing:
                lto_artifacts = shared_non_lto_backends.get(a, None)
                if lto_artifacts:
                    lto_mapping[a] = lto_artifacts._object_file
                    continue
                elif get_minimized_bitcode_or_self(lib_lto_compilation_context, a) != a:
                    fail(("For artifact '%s' in library '%s': unexpectedly has a shared LTO artifact for " +
                          "bitcode") % (a, lib.file))
            if lib_lto_compilation_context:
                lto_mapping[a] = get_minimized_bitcode_or_self(lib_lto_compilation_context, a)

    thinlto_param_file = \
        actions.declare_shareable_artifact(root_relative_path(output) + "-lto-final.params")

    thinlto_merged_object_file = \
        actions.declare_shareable_artifact(root_relative_path(output) + ".lto.merged.o")

    action_outputs = \
        ([lto_artifact.imports for lto_artifact in all_lto_artifacts if lto_artifact.index] +
         [lto_artifact.index for lto_artifact in all_lto_artifacts if lto_artifact.index] +
         [thinlto_param_file, thinlto_merged_object_file])

    build_variables = variables_extensions | setup_lto_indexing_variables(
        cc_toolchain,
        feature_configuration,
        _cc_internal.actions2ctx_cheat(actions).bin_dir.path,
        thinlto_param_file.path,
        thinlto_merged_object_file.path,
        lto_output_root_prefix,
        lto_obj_root_prefix,
    )

    cpp_config = cc_toolchain._cpp_configuration
    user_link_flags = linkopts + cpp_config.linkopts + cpp_config.lto_index_options()

    if not link_type.executable and not is_dynamic_library(link_type):
        fail("Can only do LTO on executables and dynamic libraries")

    if link_type.executable:
        action_name = "lto-index-for-executable"
    elif link_type == LINK_TARGET_TYPE.DYNAMIC_LIBRARY:
        action_name = "lto-index-for-dynamic-library"
    else:
        action_name = "lto-index-for-nodeps-dynamic-library"

    finalize_link_action(
        actions,
        "CppLTOIndexing",
        action_name,
        link_type,
        linking_mode,
        False,
        feature_configuration,
        cc_toolchain,
        "LTO indexing %{output}",
        object_file_inputs = object_file_inputs,
        libraries_to_link = libraries_to_link,
        linkstamp_map = {},
        linkstamp_object_artifacts = [],
        linkstamp_object_file_inputs = [],
        user_link_flags = user_link_flags,
        additional_build_variables = build_variables,
        output = output,
        interface_output = None,
        dynamic_library_solib_symlink_output = None,
        action_outputs = action_outputs,
        lto_mapping = lto_mapping,
        allow_lto_indexing = False,
        **link_action_args
    )

    return thinlto_param_file, thinlto_merged_object_file
