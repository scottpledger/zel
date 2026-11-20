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
"""A module to create C/C++ link actions in a consistent way."""

load(
    ":common/cc/cc_helper_internal.bzl",
    "wrap_with_check_private_api",
    _use_pic_for_binaries = "use_pic_for_binaries",
    _use_pic_for_dynamic_libs = "use_pic_for_dynamic_libs",
    artifact_category = "artifact_category_names",
)
load(":common/cc/compile/cc_compilation_outputs.bzl", "EMPTY_COMPILATION_OUTPUTS")
load(":common/cc/link/cpp_link_action.bzl", "link_action")
load(":common/cc/link/create_library_to_link.bzl", "make_library_to_link")
load(":common/cc/link/lto_indexing_action.bzl", "create_lto_artifacts_and_lto_indexing_action")
load(":common/cc/link/target_types.bzl", "LINKING_MODE", "LINK_TARGET_TYPE", "USE_ARCHIVER", "is_dynamic_library")
load(":common/paths.bzl", "paths")

_cc_internal = _builtins.internal.cc_internal

def create_cc_link_actions(
        actions,
        name,
        static_link_type,
        dynamic_link_type,
        linking_mode,
        feature_configuration,
        cc_toolchain,
        compilation_outputs,
        linking_contexts = [],
        linkopts = [],
        stamp = 0,
        additional_linker_inputs = [],
        linker_outputs = [],
        variables_extension = {},
        use_test_only_flags = False,
        neverlink = False,
        test_only_target = False,
        whole_archive = False,
        native_deps = False,
        additional_linkstamp_defines = [],
        alwayslink = False,
        link_artifact_name_suffix = "",
        linker_output_artifact = None,
        emit_interface_shared_libraries = True,
        linked_dll_name_suffix = ""):
    """Constructs the C++ linker actions."""
    if stamp not in [-1, 0, 1]:
        fail("stamp value %d is not supported, must be 0 (disabled), 1 (enabled), or -1 (default)" % stamp)
    stamping = False if cc_toolchain._is_tool_configuration else (stamp == 1 or (stamp == -1 and cc_toolchain._stamp_binaries))
    if not compilation_outputs:
        compilation_outputs = EMPTY_COMPILATION_OUTPUTS
    linkopts = list(linkopts)

    cpp_config = cc_toolchain._cpp_configuration
    use_pic_for_binaries = _use_pic_for_binaries(cpp_config, feature_configuration)
    use_pic_for_dynamic_libs = _use_pic_for_dynamic_libs(cpp_config, feature_configuration)

    link_action_kwargs = dict(
        actions = actions,
        stamping = stamping,
        feature_configuration = feature_configuration,
        cc_toolchain = cc_toolchain,
        compilation_outputs = compilation_outputs,
        linkopts = [],
        non_code_inputs = [],
        additional_linker_inputs = additional_linker_inputs,
        link_action_outputs = linker_outputs,
        variables_extensions = variables_extension,
        use_test_only_flags = use_test_only_flags,
        whole_archive = whole_archive,
        native_deps = native_deps,
        additional_linkstamp_defines = additional_linkstamp_defines,
    )

    static_library = {}
    if static_link_type:
        static_library = _create_no_pic_and_pic_static_libs_actions(
            actions,
            name,
            static_link_type,
            cc_toolchain,
            link_artifact_name_suffix,
            use_pic_for_binaries,
            use_pic_for_dynamic_libs,
            link_action_kwargs,
        )

    dynamic_library = {}
    all_lto_artifacts = []
    if dynamic_link_type:
        if dynamic_link_type.executable:
            use_pic = use_pic_for_binaries
        else:
            use_pic = use_pic_for_dynamic_libs

        link_action_kwargs["non_code_inputs"] = list(compilation_outputs._header_tokens)
        link_action_kwargs["linkopts"] = linkopts

        dynamic_library, all_lto_artifacts, linker_output_artifact = \
            _create_dynamic_link_actions(
                actions,
                name,
                dynamic_link_type,
                linking_mode,
                use_pic,
                feature_configuration,
                cc_toolchain,
                linking_contexts,
                linker_output_artifact,
                neverlink,
                test_only_target,
                link_artifact_name_suffix,
                emit_interface_shared_libraries,
                linked_dll_name_suffix,
                link_action_kwargs,
            )

    if static_link_type == LINK_TARGET_TYPE.ALWAYS_LINK_STATIC_LIBRARY and \
       feature_configuration.is_enabled("disable_whole_archive_for_static_lib"):
        fail("Attribute alwayslink: alwayslink should not be True for a target" +
             " with the disable_whole_archive_for_static_lib feature enabled.")

    library_to_link = None
    if dynamic_library | static_library:
        library_to_link = make_library_to_link(
            **(dynamic_library | static_library |
               dict(
                   _disable_whole_archive =
                       feature_configuration.is_enabled("disable_whole_archive_for_static_lib"),
                   alwayslink = alwayslink,
                   _contains_objects = True,
               ))
        )

    return struct(
        library_to_link = library_to_link,
        all_lto_artifacts = wrap_with_check_private_api(all_lto_artifacts),
        executable = linker_output_artifact if dynamic_link_type and dynamic_link_type.executable else None,
    )

def _create_dynamic_link_actions(
        actions,
        name,
        dynamic_link_type,
        linking_mode,
        use_pic,
        feature_configuration,
        cc_toolchain,
        linking_contexts,
        linker_output_artifact,
        neverlink,
        test_only_target,
        link_artifact_name_suffix,
        emit_interface_shared_libraries,
        linked_dll_name_suffix,
        link_action_kwargs):
    if linker_output_artifact:
        linker_output = linker_output_artifact
        main_library_identifier = paths.replace_extension(linker_output_artifact.path, "")
    else:
        linker_output, main_library_identifier = _get_linked_artifact(
            actions,
            name,
            dynamic_link_type,
            cc_toolchain,
            link_artifact_name_suffix,
            linked_dll_name_suffix,
        )

    so_interface = None

    emit_interface_shared_libraries = emit_interface_shared_libraries and (
        feature_configuration.is_enabled("supports_interface_shared_libraries") and
        cc_toolchain._cpp_configuration.interface_shared_objects()
    )
    if emit_interface_shared_libraries:
        so_interface, _ = _get_linked_artifact(actions, name, LINK_TARGET_TYPE.INTERFACE_DYNAMIC_LIBRARY, cc_toolchain, link_artifact_name_suffix)

        if not feature_configuration.is_enabled("targets_windows"):
            link_action_kwargs["linkopts"].append("-Wl,-soname=" + _cc_internal.dynamic_library_soname(
                actions,
                linker_output.short_path,
                False,
            ))

    mnemonic = "ObjcLink" if dynamic_link_type == LINK_TARGET_TYPE.OBJC_EXECUTABLE else None

    if linking_mode == LINKING_MODE.DYNAMIC:
        toolchain_libraries = [make_library_to_link(
            _library_identifier = lib.path,
            dynamic_library = lib,
            _disable_whole_archive = True,
        ) for lib in cc_toolchain.dynamic_runtime_lib(feature_configuration = feature_configuration).to_list()]
    else:
        toolchain_libraries = [make_library_to_link(
            _library_identifier = lib.path,
            static_library = lib,
            _disable_whole_archive = True,
        ) for lib in cc_toolchain.static_runtime_lib(feature_configuration = feature_configuration).to_list()]

    libraries_to_link, linkstamps, linkopts, non_code_inputs = \
        _maybe_link_transitively(feature_configuration, dynamic_link_type, linking_mode, linking_contexts)
    libraries_to_link.extend(toolchain_libraries)
    if linkopts:
        link_action_kwargs["linkopts"].extend(linkopts)
    if non_code_inputs:
        link_action_kwargs["non_code_inputs"].extend(non_code_inputs)

    all_lto_artifacts, allow_lto_indexing, thinlto_param_file, additional_object_files = \
        _maybe_do_lto_indexing(
            link_type = dynamic_link_type,
            linking_mode = linking_mode,
            use_pic = use_pic,
            libraries_to_link = libraries_to_link,
            output = linker_output,
            test_only_target = test_only_target,
            **link_action_kwargs
        )

    impl_library_link_artifact = None
    if dynamic_link_type == LINK_TARGET_TYPE.DYNAMIC_LIBRARY and not neverlink and \
       not feature_configuration.is_enabled("copy_dynamic_libraries_to_binary"):
        impl_library_link_artifact = _cc_internal.dynamic_library_symlink(
            actions,
            linker_output,
            cc_toolchain._solib_dir,
            True,
            False,
        )

    dynamic_library, interface_library = link_action(
        mnemonic = mnemonic,
        library_identifier = main_library_identifier if not dynamic_link_type.executable else None,
        link_type = dynamic_link_type,
        linking_mode = linking_mode,
        use_pic = use_pic,
        additional_object_files = additional_object_files,
        libraries_to_link = libraries_to_link,
        linkstamps = linkstamps,
        output = linker_output,
        interface_output = so_interface,
        dynamic_library_solib_symlink_output = impl_library_link_artifact,
        all_lto_artifacts = all_lto_artifacts,
        thinlto_param_file = thinlto_param_file,
        allow_lto_indexing = allow_lto_indexing,
        **link_action_kwargs
    )

    library_to_link = _construct_dynamic_library_to_link(
        actions,
        dynamic_link_type,
        interface_library,
        dynamic_library,
        impl_library_link_artifact,
        neverlink,
        feature_configuration,
        cc_toolchain,
    )

    return library_to_link, all_lto_artifacts, linker_output

def _maybe_link_transitively(feature_configuration, dynamic_link_type, linking_mode, linking_contexts):  # @unused
    should_link_transitively = (feature_configuration.is_enabled("targets_windows") or
                                dynamic_link_type != LINK_TARGET_TYPE.NODEPS_DYNAMIC_LIBRARY)
    if not should_link_transitively:
        return [], [], [], []

    linker_inputs = depset(
        transitive = [linking_context.linker_inputs for linking_context in linking_contexts],
        order = "topological",
    ).to_list()
    if dynamic_link_type != LINK_TARGET_TYPE.NODEPS_DYNAMIC_LIBRARY:
        linkstamps = [stamp for linker_input in linker_inputs for stamp in linker_input.linkstamps]
    else:
        linkstamps = []
    linkopts = [flag for linker_input in linker_inputs for flag in linker_input.user_link_flags]
    non_code_inputs = \
        [input for linker_input in linker_inputs for input in linker_input.additional_inputs]

    libraries_to_link = depset(
        [lib for linker_input in linker_inputs for lib in linker_input.libraries],
        order = "topological",
    ).to_list()

    return libraries_to_link, linkstamps, linkopts, non_code_inputs

def _maybe_do_lto_indexing(*, link_type, linking_mode, compilation_outputs, libraries_to_link, feature_configuration, **link_action_kwargs):
    all_lto_artifacts = []
    thinlto_param_file = None
    additional_object_files = []
    allow_lto_indexing = False

    if not feature_configuration.is_enabled("thin_lto"):
        return all_lto_artifacts, allow_lto_indexing, thinlto_param_file, additional_object_files

    lto_compilation_context = compilation_outputs._lto_compilation_context
    has_lto_bitcode_inputs = lto_compilation_context.lto_bitcode_inputs

    prefer_static_libs = linking_mode == LINKING_MODE.STATIC or \
                         not feature_configuration.is_enabled("supports_dynamic_linker")
    prefer_pic_libs = is_dynamic_library(link_type)

    static_libraries_to_link = []
    for lib in libraries_to_link:
        if prefer_static_libs:
            if lib.static_library != None or lib.pic_static_library != None:
                static_libraries_to_link.append(lib)
        elif lib.interface_library == None and lib.dynamic_library == None:
            static_libraries_to_link.append(lib)

    if not has_lto_bitcode_inputs:
        for lib in static_libraries_to_link:
            pic = (prefer_pic_libs and lib.pic_static_library != None) or lib.static_library == None
            context = lib._pic_lto_compilation_context if pic else lib._lto_compilation_context
            if context and context.lto_bitcode_inputs:
                has_lto_bitcode_inputs = True
                break

    if has_lto_bitcode_inputs:
        if not feature_configuration.is_enabled("supports_start_end_lib"):
            fail("When using LTO. The feature supports_start_end_lib must be enabled.")

        all_lto_artifacts, allow_lto_indexing, thinlto_param_file, thinlto_merged_object_file = \
            create_lto_artifacts_and_lto_indexing_action(
                link_type = link_type,
                linking_mode = linking_mode,
                compilation_outputs = compilation_outputs,
                libraries_to_link = libraries_to_link,
                static_libraries_to_link = static_libraries_to_link,
                prefer_pic_libs = prefer_pic_libs,
                feature_configuration = feature_configuration,
                lto_compilation_context = lto_compilation_context,
                **link_action_kwargs
            )
        if thinlto_merged_object_file:
            additional_object_files = [thinlto_merged_object_file]
    return all_lto_artifacts, allow_lto_indexing, thinlto_param_file, additional_object_files

def _construct_dynamic_library_to_link(
        actions,
        dynamic_link_type,
        interface_library,
        dynamic_library,
        impl_library_link_artifact,
        neverlink,
        feature_configuration,
        cc_toolchain):
    library_to_link = {}
    if dynamic_library:
        library_to_link = dict(_library_identifier = dynamic_library.library_identifier)
        if neverlink or feature_configuration.is_enabled("copy_dynamic_libraries_to_binary"):
            if interface_library:
                library_to_link["interface_library"] = interface_library.file
            library_to_link["dynamic_library"] = dynamic_library.file
        else:
            if dynamic_link_type == LINK_TARGET_TYPE.NODEPS_DYNAMIC_LIBRARY:
                impl_library_link_artifact = _cc_internal.dynamic_library_symlink(
                    actions,
                    dynamic_library.file,
                    cc_toolchain._solib_dir,
                    False,
                    False,
                )
            library_to_link["dynamic_library"] = impl_library_link_artifact
            library_to_link["resolved_symlink_dynamic_library"] = dynamic_library.file

            if interface_library:
                library_link_artifact = _cc_internal.dynamic_library_symlink(
                    actions,
                    interface_library.file,
                    cc_toolchain._solib_dir,
                    dynamic_link_type != LINK_TARGET_TYPE.NODEPS_DYNAMIC_LIBRARY,
                    False,
                )
                library_to_link["interface_library"] = library_link_artifact
                library_to_link["resolved_symlink_interface_library"] = interface_library.file
    return library_to_link

def _get_linked_artifact(actions, name, link_target_type, cc_toolchain, link_artifact_name_suffix, linked_dll_name_suffix = ""):
    maybe_pic_name = name + link_artifact_name_suffix
    if link_target_type.is_pic:
        maybe_pic_name = _cc_internal.get_artifact_name_for_category(
            cc_toolchain = cc_toolchain,
            category = artifact_category.PIC_FILE,
            output_name = maybe_pic_name,
        )

    linked_name = maybe_pic_name
    if link_target_type == LINK_TARGET_TYPE.NODEPS_DYNAMIC_LIBRARY:
        linked_name += linked_dll_name_suffix
    linked_name = _cc_internal.get_artifact_name_for_category(
        cc_toolchain = cc_toolchain,
        category = link_target_type.linker_output,
        output_name = linked_name,
    )

    if link_target_type == LINK_TARGET_TYPE.OBJC_FULLY_LINKED_ARCHIVE:
        if linked_name.startswith("lib"):
            linked_name = linked_name[3:]

    file = actions.declare_file(linked_name)
    return file, file.short_path[:-len(linked_name)] + "lib" + name

def _create_no_pic_and_pic_static_libs_actions(
        actions,
        name,
        static_link_type,
        cc_toolchain,
        link_artifact_name_suffix,
        use_pic_for_binaries,
        use_pic_for_dynamic_libs,
        link_action_kwargs):
    if static_link_type.linker_or_archiver != USE_ARCHIVER:
        fail("Only archiving of static libraries is supported, not linking.")

    create_no_pic_action = not use_pic_for_binaries
    create_pic_action = use_pic_for_binaries or use_pic_for_dynamic_libs

    no_pic_library_to_link = {}
    if create_no_pic_action:
        no_pic_library_to_link = _create_action_for_static_library(
            actions,
            name,
            static_link_type,
            False,
            link_artifact_name_suffix,
            cc_toolchain,
            link_action_kwargs,
        )

    pic_library_to_link = {}
    if create_pic_action:
        if not create_no_pic_action:
            link_target_type_used_for_naming = static_link_type
        elif static_link_type == LINK_TARGET_TYPE.ALWAYS_LINK_STATIC_LIBRARY:
            link_target_type_used_for_naming = LINK_TARGET_TYPE.ALWAYS_LINK_PIC_STATIC_LIBRARY
        else:
            link_target_type_used_for_naming = LINK_TARGET_TYPE.PIC_STATIC_LIBRARY

        pic_library_to_link = _create_action_for_static_library(
            actions,
            name,
            link_target_type_used_for_naming,
            True,
            link_artifact_name_suffix,
            cc_toolchain,
            link_action_kwargs,
        )

    return no_pic_library_to_link | pic_library_to_link

def _create_action_for_static_library(
        actions,
        name,
        link_target_type_used_for_naming,
        use_pic,
        link_artifact_name_suffix,
        cc_toolchain,
        link_action_kwargs):
    linked_artifact, library_identifier = _get_linked_artifact(actions, name, link_target_type_used_for_naming, cc_toolchain, link_artifact_name_suffix)

    output_library, _ = link_action(
        mnemonic = "CppArchive",
        library_identifier = library_identifier,
        link_type = link_target_type_used_for_naming,
        linking_mode = LINKING_MODE.STATIC,
        use_pic = use_pic,
        linkstamps = [],
        libraries_to_link = [],
        output = linked_artifact,
        interface_output = None,
        dynamic_library_solib_symlink_output = None,
        all_lto_artifacts = [],
        thinlto_param_file = None,
        allow_lto_indexing = False,
        **link_action_kwargs
    )

    if not use_pic:
        return dict(
            _library_identifier = library_identifier,
            static_library = output_library.file,
            objects = output_library.object_files,
            _lto_compilation_context = output_library.lto_compilation_context,
            _shared_non_lto_backends = output_library.shared_non_lto_backends,
            alwayslink = output_library.artifact_category == artifact_category.ALWAYSLINK_STATIC_LIBRARY,
        )
    else:
        return dict(
            _library_identifier = library_identifier,
            pic_static_library = output_library.file,
            pic_objects = output_library.object_files,
            _pic_lto_compilation_context = output_library.lto_compilation_context,
            _pic_shared_non_lto_backends = output_library.shared_non_lto_backends,
            alwayslink = output_library.artifact_category == artifact_category.ALWAYSLINK_STATIC_LIBRARY,
        )
