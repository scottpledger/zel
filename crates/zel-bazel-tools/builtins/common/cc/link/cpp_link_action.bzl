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
"""Functions that create C++ link action."""

load(":common/cc/cc_helper_internal.bzl", artifact_category = "artifact_category_names")
load(":common/cc/link/finalize_link_action.bzl", "finalize_link_action")
load(":common/cc/link/link_build_variables.bzl", "setup_linking_variables")
load(":common/cc/link/lto_backends.bzl", "create_shared_non_lto_artifacts")
load(":common/cc/link/target_types.bzl", "LINK_TARGET_TYPE", "USE_ARCHIVER", "USE_LINKER", "is_dynamic_library")
load(":common/paths.bzl", "paths")

_cc_internal = _builtins.internal.cc_internal
_cc_common_internal = _builtins.internal.cc_common

def link_action(
        *,
        actions,
        mnemonic,
        library_identifier,
        link_type,
        linking_mode,
        use_pic,
        stamping,
        feature_configuration,
        cc_toolchain,
        compilation_outputs,
        additional_object_files = [],
        libraries_to_link,
        linkstamps,
        linkopts,
        non_code_inputs,
        additional_linker_inputs,
        link_action_outputs,
        variables_extensions,
        output,
        interface_output,
        dynamic_library_solib_symlink_output,
        use_test_only_flags,
        whole_archive,
        native_deps,
        additional_linkstamp_defines,
        thinlto_param_file,
        all_lto_artifacts = [],
        allow_lto_indexing = False):
    """Creates a C++ linking action."""
    if bool(library_identifier) == link_type.executable:
        fail("Executables can't have library identifier", library_identifier, link_type.executable)
    if interface_output and not is_dynamic_library(link_type):
        fail("Interface output can only be used with DYNAMIC_LIBRARY targets")
    if not _cc_common_internal.action_is_enabled(
        feature_configuration = feature_configuration,
        action_name = link_type.action_name,
    ):
        fail("Expected action_config for '%s' to be configured" % link_type.action_name)

    object_files = compilation_outputs.pic_objects if use_pic else compilation_outputs.objects
    object_files = object_files + additional_object_files

    propeller_optimize_info = getattr(cc_toolchain._fdo_context, "propeller_optimize_info", None)
    if not cc_toolchain._is_tool_configuration and propeller_optimize_info and \
       propeller_optimize_info.ld_profile:
        non_code_inputs.append(propeller_optimize_info.ld_profile)

    lto_mapping = {}
    if all_lto_artifacts:
        for lto_artifact in all_lto_artifacts:
            lto_mapping[lto_artifact._bitcode_file] = lto_artifact._object_file

    if thinlto_param_file:
        non_code_inputs.append(thinlto_param_file)

    linkstamp_map = _map_linkstamps_to_outputs(actions, linkstamps, output)
    linkstamp_object_file_inputs = linkstamp_map.values()
    object_file_inputs = object_files

    object_artifacts = [lto_mapping.get(obj, obj) for obj in object_files]
    linkstamp_object_artifacts = [lto_mapping.get(obj, obj) for obj in linkstamp_map.values()]

    combined_object_artifacts = object_artifacts + linkstamp_object_artifacts

    output_library = None
    lto_compilation_context = compilation_outputs._lto_compilation_context
    if not link_type.executable:
        use_archiver = link_type.linker_or_archiver == USE_ARCHIVER
        output_library = struct(
            file = output,
            artifact_category = link_type.linker_output,
            library_identifier = library_identifier,
            object_files = combined_object_artifacts if use_archiver else [],
            lto_compilation_context = lto_compilation_context if use_archiver else [],
            shared_non_lto_backends = create_shared_non_lto_artifacts(
                actions,
                lto_compilation_context,
                link_type.linker_or_archiver == USE_LINKER,
                feature_configuration,
                cc_toolchain,
                use_pic,
                object_file_inputs,
            ),
            must_keep_debug = False,
        )

    interface_output_library = None
    if interface_output:
        interface_output_library = struct(
            file = interface_output,
            artifact_category = artifact_category.DYNAMIC_LIBRARY,
            library_identifier = library_identifier,
            object_files = combined_object_artifacts,
            lto_compilation_context = lto_compilation_context,
            shared_non_lto_backends = None,
            must_keep_debug = False,
        )

    action_outputs = [output] + link_action_outputs + ([interface_output] if interface_output else [])

    build_variables = variables_extensions | setup_linking_variables(
        cc_toolchain,
        feature_configuration,
        output,
        _cc_internal.dynamic_library_soname(
            actions,
            output.short_path,
            link_type != LINK_TARGET_TYPE.NODEPS_DYNAMIC_LIBRARY,
        ),
        interface_output,
        thinlto_param_file,
    )

    user_link_flags = linkopts + cc_toolchain._cpp_configuration.linkopts

    finalize_link_action(
        actions,
        mnemonic if mnemonic else "CppLink",
        link_type.action_name,
        link_type,
        linking_mode,
        stamping,
        feature_configuration,
        cc_toolchain,
        "Linking %{output}",
        object_file_inputs,
        non_code_inputs,
        libraries_to_link,
        linkstamp_map,
        linkstamp_object_artifacts,
        linkstamp_object_file_inputs,
        user_link_flags,
        additional_linker_inputs,
        build_variables,
        output,
        interface_output,
        dynamic_library_solib_symlink_output,
        action_outputs,
        use_test_only_flags,
        whole_archive,
        native_deps,
        additional_linkstamp_defines,
        lto_mapping,
        allow_lto_indexing,
    )

    return output_library, interface_output_library

def _map_linkstamps_to_outputs(actions, linkstamps, output):
    map = {}
    stamp_output_dir = paths.join(paths.dirname(output.short_path), "_objs", output.basename)
    linkstamps = set(linkstamps)
    seen_linkstamp_sources = set()
    for linkstamp in linkstamps:
        if linkstamp.file() in seen_linkstamp_sources:
            continue
        seen_linkstamp_sources.add(linkstamp.file())
        linkstamp_file = linkstamp.file()
        stamp_output_path = paths.join(
            stamp_output_dir,
            paths.replace_extension(linkstamp_file.short_path, ".o"),
        )
        stamp_output_file = actions.declare_shareable_artifact(stamp_output_path)
        map[linkstamp] = stamp_output_file
    return map
