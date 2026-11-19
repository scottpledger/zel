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
"""Common functions that create C++ link and LTO indexing action."""

load(":common/cc/compile/linkstamp_compile.bzl", "register_linkstamp_compile_action")
load(":common/cc/link/collect_solib_dirs.bzl", "collect_solib_dirs")
load(":common/cc/link/create_libraries_to_link_values.bzl", "add_libraries_to_link", "add_object_files_to_link", "process_objects_for_lto")
load(":common/cc/link/link_build_variables.bzl", "setup_common_linking_variables")
load(":common/cc/link/target_types.bzl", "LINKING_MODE", "LINK_TARGET_TYPE", "USE_ARCHIVER", "USE_LINKER", "is_dynamic_library")
load(":common/cc/semantics.bzl", "semantics")

_cc_internal = _builtins.internal.cc_internal
_cc_common_internal = _builtins.internal.cc_common

def finalize_link_action(
        actions,
        mnemonic,
        action_name,
        link_type,
        linking_mode,
        stamping,
        feature_configuration,
        cc_toolchain,
        progress_message,
        object_file_inputs,
        non_code_inputs,
        libraries_to_link,
        linkstamp_map,
        linkstamp_object_artifacts,
        linkstamp_object_file_inputs,
        user_link_flags,
        additional_linker_inputs,
        additional_build_variables,
        output,
        interface_output,
        dynamic_library_solib_symlink_output,
        action_outputs,
        use_test_only_flags,
        whole_archive,
        native_deps,
        additional_linkstamp_defines,
        lto_mapping,
        allow_lto_indexing):
    """Creates C++ linking or LTO indexing, and linkstamp compile actions."""
    need_whole_archive = whole_archive or _need_whole_archive(
        feature_configuration,
        linking_mode,
        link_type,
        user_link_flags,
        cc_toolchain._cpp_configuration,
    )

    must_keep_debug = any([lib._must_keep_debug for lib in libraries_to_link])

    toolchain_libraries_solib_dir = ""
    if feature_configuration.is_enabled("static_link_cpp_runtimes"):
        toolchain_libraries_solib_dir = cc_toolchain.dynamic_runtime_solib_dir

    solib_dir = output.root.path + "/" + cc_toolchain._solib_dir
    libraries_to_link_values = []
    expanded_linker_artifacts = []
    lto_map = dict(lto_mapping)

    if feature_configuration.is_enabled("use_lto_native_object_directory"):
        shared_non_lto_obj_root_prefix = "shared.nonlto-obj"
    else:
        shared_non_lto_obj_root_prefix = "shared.nonlto"
    object_file_inputs = process_objects_for_lto(
        object_file_inputs,
        lto_map,
        allow_lto_indexing,
        shared_non_lto_obj_root_prefix,
        expanded_linker_artifacts,
    )
    add_object_files_to_link(object_file_inputs, libraries_to_link_values)
    add_object_files_to_link(linkstamp_object_file_inputs, libraries_to_link_values)

    use_start_end_lib = (cc_toolchain._cpp_configuration.start_end_lib() and
                         feature_configuration.is_enabled("supports_start_end_lib"))

    prefer_static_libs = linking_mode == LINKING_MODE.STATIC or \
                         not feature_configuration.is_enabled("supports_dynamic_linker")

    prefer_pic_libs = is_dynamic_library(link_type)

    add_libraries_to_link(
        libraries_to_link,
        prefer_static_libs,
        prefer_pic_libs,
        use_start_end_lib,
        need_whole_archive,
        lto_map,
        allow_lto_indexing,
        shared_non_lto_obj_root_prefix,
        feature_configuration,
        expanded_linker_artifacts,
        libraries_to_link_values,
    )

    libraries_to_link_values = _cc_internal.intern_seq(libraries_to_link_values)

    if lto_map:
        fail("Still have LTO objects left: %s" % lto_map)
    expanded_linker_artifacts = depset([
        lto_mapping.get(li, li)
        for li in expanded_linker_artifacts
    ])

    library_search_directories, all_runtime_library_search_directories = collect_solib_dirs(
        libraries_to_link,
        cc_toolchain,
        feature_configuration,
        prefer_static_libs,
        output,
        dynamic_library_solib_symlink_output,
        link_type,
        linking_mode,
        native_deps,
        solib_dir,
        toolchain_libraries_solib_dir,
        _cc_internal.actions2ctx_cheat(actions).workspace_name,
    )

    build_variables = setup_common_linking_variables(
        cc_toolchain = cc_toolchain,
        feature_configuration = feature_configuration,
        is_using_linker = link_type.linker_or_archiver == USE_LINKER,
        is_linking_dynamic_library = link_type == LINK_TARGET_TYPE.DYNAMIC_LIBRARY,
        param_file = "LINKER_PARAM_FILE_PLACEHOLDER",
        must_keep_debug = must_keep_debug,
        use_test_only_flags = use_test_only_flags,
        user_link_flags = user_link_flags,
        runtime_library_search_directories = all_runtime_library_search_directories,
        libraries_to_link = libraries_to_link_values,
        library_search_directories = library_search_directories,
    )

    build_variables = build_variables | additional_build_variables

    if link_type == LINK_TARGET_TYPE.INTERFACE_DYNAMIC_LIBRARY:
        fail("you can't link an interface dynamic library directly")
    if not is_dynamic_library(link_type):
        if interface_output:
            fail("interface output may only be non-null for dynamic library links")
    if link_type.linker_or_archiver == USE_ARCHIVER:
        toolchain_libraries_solib_dir = None

        if linking_mode != LINKING_MODE.STATIC:
            fail("static library link must be static")
        if native_deps:
            fail("the native deps flag must be false for static links")
        if need_whole_archive:
            fail("the need whole archive flag must be false for static links")

    should_use_link_dynamic_library_tool = (
        is_dynamic_library(link_type) and
        feature_configuration.is_enabled("supports_interface_shared_libraries") and
        not feature_configuration.is_enabled("has_configured_linker_path")
    )
    if should_use_link_dynamic_library_tool:
        tool_path = cc_toolchain._link_dynamic_library_tool.path
    else:
        tool_path = _cc_common_internal.get_tool_for_action(
            feature_configuration = feature_configuration,
            action_name = link_type.action_name,
        )

    if cc_toolchain._cpp_configuration.incompatible_use_specific_tool_files() and \
       link_type.linker_or_archiver == USE_ARCHIVER:
        linker_files = cc_toolchain._ar_files
    else:
        linker_files = cc_toolchain._linker_files

    dependency_inputs = depset(
        direct = [cc_toolchain._link_dynamic_library_tool] if should_use_link_dynamic_library_tool else [],
        transitive = [additional_linker_inputs, linker_files],
    )

    non_code_inputs = depset(non_code_inputs)

    inputs = depset(transitive = [expanded_linker_artifacts, non_code_inputs, dependency_inputs])

    if linkstamp_map:
        needs_pic = (cc_toolchain._cpp_configuration.force_pic() or
                     (is_dynamic_library(link_type) and feature_configuration.is_enabled("supports_pic")))

        for linkstamp, artifact in linkstamp_map.items():
            register_linkstamp_compile_action(
                actions = actions,
                cc_toolchain = cc_toolchain,
                feature_configuration = feature_configuration,
                source_file = linkstamp.file(),
                output_file = artifact,
                compilation_inputs = linkstamp.hdrs(),
                inputs_for_validation = inputs,
                label_replacement = _quote_replacement(output.path if native_deps and cc_toolchain._cpp_configuration.share_native_deps() else str(output.owner)),
                output_replacement = _quote_replacement(output.path),
                needs_pic = needs_pic,
                stamping = stamping,
                additional_linkstamp_defines = additional_linkstamp_defines,
            )

        inputs = depset(
            direct = linkstamp_map.values(),
            transitive = [
                expanded_linker_artifacts,
                non_code_inputs,
                dependency_inputs,
                depset(linkstamp_object_artifacts),
            ],
        )

    _create_action(
        actions,
        action_name,
        feature_configuration,
        cc_toolchain,
        build_variables,
        mnemonic,
        tool_path,
        inputs,
        action_outputs,
        progress_message,
        link_type,
        interface_output,
    )

def _create_action(
        actions,
        action_name,
        feature_configuration,
        cc_toolchain,
        build_variables,
        mnemonic,
        tool_path,
        inputs,
        outputs,
        progress_message,
        link_type,
        interface_output):
    parameter_file_type = None
    if _can_split_command_line(
        link_type,
        cc_toolchain,
        feature_configuration,
        interface_output,
    ):
        if feature_configuration.is_enabled("gcc_quoting_for_param_files"):
            parameter_file_type = "GCC_QUOTED"
        elif feature_configuration.is_enabled("windows_quoting_for_param_files"):
            parameter_file_type = "WINDOWS"
        else:
            parameter_file_type = "UNQUOTED"

    execution_info = {}
    for req in _cc_common_internal.get_execution_requirements(feature_configuration = feature_configuration, action_name = action_name):
        execution_info[req] = ""

    build_variables = _cc_internal.cc_toolchain_variables(vars = build_variables)
    link_args = _cc_internal.get_link_args(
        feature_configuration = feature_configuration,
        action_name = action_name,
        build_variables = build_variables,
        parameter_file_type = parameter_file_type,
    )
    env = _cc_common_internal.get_environment_variables(
        feature_configuration = feature_configuration,
        action_name = action_name,
        variables = build_variables,
    )
    if "requires_darwin" not in execution_info:
        env = env | {"PWD": "/proc/self/cwd"}
    exec_group = None
    toolchain = None

    if "cpp_link" in _cc_internal.actions2ctx_cheat(actions).exec_groups:
        exec_group = "cpp_link"
    elif "@@bazel_tools//tools/cpp:toolchain_type" in _cc_internal.actions2ctx_cheat(actions).toolchains:
        toolchain = semantics.toolchain

    execution_info = semantics.get_cc_link_memlimit(
        cc_toolchain._cpp_configuration.compilation_mode(),
        execution_info,
    )
    actions.run(
        mnemonic = mnemonic,
        executable = tool_path,
        arguments = [link_args],
        inputs = inputs,
        outputs = outputs,
        progress_message = progress_message,
        resource_set = _resource_set,
        env = env,
        use_default_shell_env = True,
        execution_requirements = execution_info,
        toolchain = toolchain,
        exec_group = exec_group,
    )

def _can_split_command_line(
        link_type,
        cc_toolchain,
        feature_configuration,
        interface_output):
    if not cc_toolchain._supports_param_files:
        return False
    elif is_dynamic_library(link_type):
        if interface_output:
            return feature_configuration.is_enabled("targets_windows")
        else:
            return True
    elif link_type.linker_or_archiver == USE_LINKER:
        return True
    elif link_type.linker_or_archiver == USE_ARCHIVER:
        return feature_configuration.is_enabled("archive_param_file")
    return False

def _need_whole_archive(feature_configuration, linking_mode, link_type, linkopts, cpp_config):
    shared_linkopts = is_dynamic_library(link_type) or "-shared" in linkopts or "-shared" in cpp_config.linkopts
    if not shared_linkopts:
        return False
    if feature_configuration.is_requested("force_no_whole_archive"):
        return False
    if cpp_config.incompatible_remove_legacy_whole_archive():
        return False
    if linking_mode != LINKING_MODE.STATIC:
        return False
    if feature_configuration.is_requested("legacy_whole_archive"):
        return True
    if cpp_config.legacy_whole_archive():
        return True
    return False

def _quote_replacement(s):
    if "\\" not in s and "$" not in s:
        return s
    return s.replace("\\", "\\\\").replace("$", "\\$")

def _resource_set(os, inputs):
    if os == "osx":
        return {"memory": 15 + 0.05 * inputs, "cpu": 1}
    elif os == "linux":
        return {"memory": max(50, -100 + 0.1 * inputs), "cpu": 1}
    else:
        return {"memory": 1500 + inputs, "cpu": 1}
