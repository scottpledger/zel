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
"""The cc_common.create_linking_context_from_compilation_outputs function."""

load(":common/cc/cc_info.bzl", "create_linking_context", "merge_linking_contexts")
load(":common/cc/link/cc_linking_helper.bzl", "create_cc_link_actions")
load(":common/cc/link/create_linker_input.bzl", "create_linker_input")
load(":common/cc/link/target_types.bzl", "LINKING_MODE", "LINK_TARGET_TYPE")

_cc_internal = _builtins.internal.cc_internal

def create_linking_context_from_compilation_outputs(
        *,
        actions,
        name,
        feature_configuration,
        cc_toolchain,
        language = "c++",  # @unused
        disallow_static_libraries = False,
        disallow_dynamic_library = False,
        compilation_outputs,
        linking_contexts = [],
        user_link_flags = [],
        alwayslink = False,
        additional_inputs = [],
        variables_extension = {},
        stamp = 0,
        linked_dll_name_suffix = "",
        test_only_target = False):
    """Links a single library."""
    if alwayslink and _cc_internal.rule_class(_cc_internal.actions2ctx_cheat(actions)) != "swift_library":
        static_link_type = LINK_TARGET_TYPE.ALWAYS_LINK_STATIC_LIBRARY
    else:
        static_link_type = LINK_TARGET_TYPE.STATIC_LIBRARY
    if type(additional_inputs) == type([]):
        additional_inputs = depset(additional_inputs)

    cc_linking_outputs = create_cc_link_actions(
        _cc_internal.wrap_link_actions(actions),
        name,
        None if disallow_static_libraries else static_link_type,
        None if disallow_dynamic_library else LINK_TARGET_TYPE.NODEPS_DYNAMIC_LIBRARY,
        LINKING_MODE.DYNAMIC,
        feature_configuration,
        cc_toolchain,
        compilation_outputs,
        linking_contexts,
        user_link_flags,
        stamp,
        additional_inputs,
        [],
        variables_extension,
        alwayslink = alwayslink,
        test_only_target = test_only_target,
        linked_dll_name_suffix = linked_dll_name_suffix,
    )

    linker_input = create_linker_input(
        owner = _cc_internal.actions2ctx_cheat(actions).label.same_package_label(name),
        libraries = depset([cc_linking_outputs.library_to_link]) if cc_linking_outputs.library_to_link else depset(),
        user_link_flags = user_link_flags,
        additional_inputs = additional_inputs,
    )
    direct_linking_context = create_linking_context(
        linker_inputs = depset([linker_input]),
    )
    linking_context = merge_linking_contexts(
        linking_contexts = [direct_linking_context] + linking_contexts,
    )
    return linking_context, cc_linking_outputs
