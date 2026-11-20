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
"""The cc_common.link function."""

load(":common/cc/compile/cc_compilation_outputs.bzl", "EMPTY_COMPILATION_OUTPUTS")
load(":common/cc/link/cc_linking_helper.bzl", "create_cc_link_actions")
load(":common/cc/link/target_types.bzl", "LINKING_MODE", "LINK_TARGET_TYPE")

_cc_internal = _builtins.internal.cc_internal

_TARGET_TYPE = {
    ("cpp", "executable"): (None, LINK_TARGET_TYPE.EXECUTABLE),
    ("cpp", "dynamic_library"): (None, LINK_TARGET_TYPE.DYNAMIC_LIBRARY),
    ("objc", "executable"): (None, LINK_TARGET_TYPE.OBJC_EXECUTABLE),
    ("objcpp", "executable"): (None, LINK_TARGET_TYPE.OBJC_EXECUTABLE),
    ("objc", "archive"): (LINK_TARGET_TYPE.OBJC_FULLY_LINKED_ARCHIVE, None),
}

def link(
        *,
        actions,
        name,
        feature_configuration,
        cc_toolchain,
        language = "c++",
        output_type = "executable",
        link_deps_statically = True,
        compilation_outputs = EMPTY_COMPILATION_OUTPUTS,
        linking_contexts = [],
        user_link_flags = [],
        stamp = 0,
        additional_inputs = [],
        additional_outputs = [],
        variables_extension = {},
        use_test_only_flags = False,
        never_link = False,
        test_only_target = False,
        native_deps = False,
        whole_archive = False,
        additional_linkstamp_defines = [],
        always_link = False,
        link_artifact_name_suffix = "",
        main_output = None,
        use_shareable_artifact_factory = False,
        build_config = None,
        emit_interface_shared_library = False):
    """Used for C++ transitive linking."""
    if output_type not in ["executable", "dynamic_library", "archive"]:
        fail("Output type '%s' is not supported" % output_type)
    language = language.replace("++", "pp")
    if language not in ["cpp", "objc", "objcpp"]:
        fail("Language '%s' is not supported" % language)
    if (language, output_type) not in _TARGET_TYPE:
        fail("Language '%s' does not support %s" % (language, output_type))

    static_link_type, dynamic_link_type = _TARGET_TYPE[(language, output_type)]

    if type(additional_inputs) == type([]):
        additional_inputs = depset(additional_inputs)

    if static_link_type == LINK_TARGET_TYPE.STATIC_LIBRARY and always_link:
        static_link_type = LINK_TARGET_TYPE.ALWAYS_LINK_STATIC_LIBRARY

    linking_mode = LINKING_MODE.STATIC if link_deps_statically else LINKING_MODE.DYNAMIC

    actions = _cc_internal.wrap_link_actions(actions, build_config, use_shareable_artifact_factory)

    return create_cc_link_actions(
        actions,
        name,
        static_link_type,
        dynamic_link_type,
        linking_mode,
        feature_configuration,
        cc_toolchain,
        compilation_outputs,
        linking_contexts,
        user_link_flags,
        stamp,
        additional_inputs,
        additional_outputs,
        variables_extension,
        use_test_only_flags,
        never_link,
        test_only_target,
        whole_archive,
        native_deps,
        additional_linkstamp_defines,
        always_link,
        link_artifact_name_suffix,
        main_output,
        emit_interface_shared_libraries = dynamic_link_type == LINK_TARGET_TYPE.DYNAMIC_LIBRARY and
                                          (emit_interface_shared_library or
                                           feature_configuration.is_enabled("targets_windows")),
    )
