# Copyright 2020 The Bazel Authors. All rights reserved.
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
"""Functions that populate link build variables."""

load(":common/cc/cc_helper_internal.bzl", "get_relative_path", "should_create_per_object_debug_info", artifact_category = "artifact_category_names")

_cc_internal = _builtins.internal.cc_internal

LINK_BUILD_VARIABLES = struct(
    OUTPUT_EXECPATH = "output_execpath",
    LIBRARIES_TO_LINK = "libraries_to_link",
    RUNTIME_LIBRARY_SEARCH_DIRECTORIES = "runtime_library_search_directories",
    LIBRARY_SEARCH_DIRECTORIES = "library_search_directories",
    RUNTIME_SOLIB_NAME = "runtime_solib_name",
    GENERATE_INTERFACE_LIBRARY = "generate_interface_library",
    INTERFACE_LIBRARY_BUILDER = "interface_library_builder_path",
    INTERFACE_LIBRARY_INPUT = "interface_library_input_path",
    INTERFACE_LIBRARY_OUTPUT = "interface_library_output_path",
    USER_LINK_FLAGS = "user_link_flags",
    FORCE_PIC = "force_pic",
    STRIP_DEBUG_SYMBOLS = "strip_debug_symbols",
    IS_CC_TEST = "is_cc_test",
    IS_USING_FISSION = "is_using_fission",
    LINKER_PARAM_FILE = "linker_param_file",
    THINLTO_PARAM_FILE = "thinlto_param_file",
    THINLTO_OPTIONAL_PARAMS_FILE = "thinlto_optional_params_file",
    THINLTO_INDEXING_PARAM_FILE = "thinlto_indexing_param_file",
    THINLTO_PREFIX_REPLACE = "thinlto_prefix_replace",
    THINLTO_OBJECT_SUFFIX_REPLACE = "thinlto_object_suffix_replace",
    THINLTO_MERGED_OBJECT_FILE = "thinlto_merged_object_file",
    FDO_INSTRUMENT_PATH = "fdo_instrument_path",
    CS_FDO_INSTRUMENT_PATH = "cs_fdo_instrument_path",
    PROPELLER_OPTIMIZE_LD_PATH = "propeller_optimize_ld_path",
)

def create_link_variables(
        *,
        cc_toolchain,
        feature_configuration,
        output_file = None,
        runtime_library_search_directories = [],
        library_search_directories = [],
        user_link_flags = [],
        param_file = None,
        is_using_linker = True,
        is_linking_dynamic_library = False,
        must_keep_debug = True,
        use_test_only_flags = False,
        is_static_linking_mode = None):
    """Returns common link build variables used for both linking and thin LTO indexing actions."""
    if feature_configuration.is_enabled("fdo_instrument"):
        fail("FDO instrumentation not supported")

    runtime_library_search_directories = runtime_library_search_directories or []
    library_search_directories = library_search_directories or []
    user_link_flags = user_link_flags or []
    use_test_only_flags = use_test_only_flags or False

    vars = setup_common_linking_variables(
        cc_toolchain,
        feature_configuration,
        [],
        runtime_library_search_directories,
        library_search_directories,
        user_link_flags,
        param_file,
        is_using_linker,
        is_linking_dynamic_library,
        must_keep_debug,
        use_test_only_flags,
        is_static_linking_mode,
    )

    if output_file:
        if type(output_file) != type(""):
            fail("Parameter 'output_file' expected String, got '%s'" % type(output_file))
        vars["output_execpath"] = output_file
    return _cc_internal.cc_toolchain_variables(vars = vars)

def setup_common_linking_variables(
        cc_toolchain,
        feature_configuration,
        libraries_to_link = [],
        runtime_library_search_directories = [],
        library_search_directories = [],
        user_link_flags = [],
        param_file = None,
        is_using_linker = True,
        is_linking_dynamic_library = False,
        must_keep_debug = True,
        use_test_only_flags = False,
        is_static_linking_mode = None):  # @unused
    """Returns common link build variables used for both linking and thin LTO indexing actions."""
    vars = dict(cc_toolchain._build_variables_dict)
    cpp_config = cc_toolchain._cpp_configuration

    if cpp_config.force_pic():
        vars[LINK_BUILD_VARIABLES.FORCE_PIC] = ""

    if not must_keep_debug and cpp_config.should_strip_binaries():
        vars[LINK_BUILD_VARIABLES.STRIP_DEBUG_SYMBOLS] = ""

    if (is_using_linker and
        should_create_per_object_debug_info(feature_configuration, cpp_config)):
        vars[LINK_BUILD_VARIABLES.IS_USING_FISSION] = ""

    vars[LINK_BUILD_VARIABLES.IS_CC_TEST] = use_test_only_flags
    vars[LINK_BUILD_VARIABLES.RUNTIME_LIBRARY_SEARCH_DIRECTORIES] = runtime_library_search_directories
    vars[LINK_BUILD_VARIABLES.LIBRARIES_TO_LINK] = libraries_to_link
    vars[LINK_BUILD_VARIABLES.LIBRARY_SEARCH_DIRECTORIES] = library_search_directories

    if param_file:
        vars[LINK_BUILD_VARIABLES.LINKER_PARAM_FILE] = param_file

    if feature_configuration.is_enabled("fdo_instrument"):
        if getattr(cc_toolchain._fdo_context, "branch_fdo_profile", None):
            fail("Can't use --feature=fdo_instrument together with --fdo_profile")
        if not cpp_config.fdo_instrument():
            fail("When using --feature=fdo_instrument, you need to set --fdo_instrument as well")
        vars[LINK_BUILD_VARIABLES.FDO_INSTRUMENT_PATH] = cpp_config.fdo_instrument()
    elif feature_configuration.is_enabled("cs_fdo_instrument"):
        if not cpp_config.cs_fdo_instrument():
            fail("When using --feature=cs_fdo_instrument, you need to set --cs_fdo_instrument as well")
        vars[LINK_BUILD_VARIABLES.CS_FDO_INSTRUMENT_PATH] = cpp_config.cs_fdo_instrument()

    user_link_flags = user_link_flags if is_using_linker else []
    if is_linking_dynamic_library:
        vars[LINK_BUILD_VARIABLES.USER_LINK_FLAGS] = _remove_pie(user_link_flags)
    else:
        vars[LINK_BUILD_VARIABLES.USER_LINK_FLAGS] = user_link_flags
    return vars

def _remove_pie(flags):
    return [flag for flag in flags if flag != "-pie" and flag != "-Wl,-pie"]

_DONT_GENERATE_INTERFACE_LIBRARY = {
    LINK_BUILD_VARIABLES.GENERATE_INTERFACE_LIBRARY: "no",
    LINK_BUILD_VARIABLES.INTERFACE_LIBRARY_BUILDER: "ignored",
    LINK_BUILD_VARIABLES.INTERFACE_LIBRARY_INPUT: "ignored",
    LINK_BUILD_VARIABLES.INTERFACE_LIBRARY_OUTPUT: "ignored",
}

def setup_linking_variables(
        cc_toolchain,
        feature_configuration,
        output_file,
        runtime_solib_name,
        interface_library_output,
        thinlto_param_file):
    """Returns additional build variables used by regular linking action."""
    vars = {}

    if thinlto_param_file:
        vars[LINK_BUILD_VARIABLES.THINLTO_PARAM_FILE] = thinlto_param_file

    vars[LINK_BUILD_VARIABLES.OUTPUT_EXECPATH] = output_file
    vars[LINK_BUILD_VARIABLES.RUNTIME_SOLIB_NAME] = runtime_solib_name

    fdo_context = cc_toolchain._fdo_context
    if (not cc_toolchain._is_tool_configuration and
        fdo_context and
        feature_configuration.is_enabled("propeller_optimize") and
        fdo_context.propeller_optimize_info and
        fdo_context.propeller_optimize_info.ld_profile):
        vars[LINK_BUILD_VARIABLES.PROPELLER_OPTIMIZE_LD_PATH] = fdo_context.propeller_optimize_info.ld_profile

    should_generate_interface_library = output_file and cc_toolchain._if_so_builder and interface_library_output
    if should_generate_interface_library:
        vars[LINK_BUILD_VARIABLES.GENERATE_INTERFACE_LIBRARY] = "yes"
        vars[LINK_BUILD_VARIABLES.INTERFACE_LIBRARY_BUILDER] = cc_toolchain._if_so_builder
        vars[LINK_BUILD_VARIABLES.INTERFACE_LIBRARY_INPUT] = output_file
        vars[LINK_BUILD_VARIABLES.INTERFACE_LIBRARY_OUTPUT] = interface_library_output
    else:
        vars = vars | _DONT_GENERATE_INTERFACE_LIBRARY

    return vars

def setup_lto_indexing_variables(
        cc_toolchain,
        feature_configuration,
        bin_directory_path,
        thinlto_param_file,
        thinlto_merged_object_file,
        lto_output_root_prefix,
        lto_obj_root_prefix):
    """Returns additional build variables used by LTO indexing actions."""
    vars = {}

    vars[LINK_BUILD_VARIABLES.THINLTO_INDEXING_PARAM_FILE] = thinlto_param_file
    vars[LINK_BUILD_VARIABLES.THINLTO_OPTIONAL_PARAMS_FILE] = "=" + thinlto_param_file

    if lto_output_root_prefix != lto_obj_root_prefix:
        vars[LINK_BUILD_VARIABLES.THINLTO_PREFIX_REPLACE] = (
            ";" + get_relative_path(bin_directory_path, lto_output_root_prefix) + "/;" +
            get_relative_path(bin_directory_path, lto_obj_root_prefix) + "/"
        )
    else:
        vars[LINK_BUILD_VARIABLES.THINLTO_PREFIX_REPLACE] = (
            ";" + get_relative_path(bin_directory_path, lto_output_root_prefix) + "/"
        )

    if not feature_configuration.is_enabled("no_use_lto_indexing_bitcode_file"):
        object_file_extension = _cc_internal.get_artifact_name_extension_for_category(
            cc_toolchain,
            artifact_category.OBJECT_FILE,
        )
        vars[LINK_BUILD_VARIABLES.THINLTO_OBJECT_SUFFIX_REPLACE] = ".indexing.o;" + object_file_extension

    vars[LINK_BUILD_VARIABLES.THINLTO_MERGED_OBJECT_FILE] = thinlto_merged_object_file
    vars = vars | _DONT_GENERATE_INTERFACE_LIBRARY

    return vars
