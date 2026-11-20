# Copyright 2025 The Bazel Authors. All rights reserved.
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
"""Collects solib search directories and runtime rpath directories."""

load(":common/cc/cc_helper_internal.bzl", "is_shared_library")
load(":common/cc/link/target_types.bzl", "LINKING_MODE", "LINK_TARGET_TYPE", "is_dynamic_library")
load(":common/paths.bzl", "paths")

def collect_solib_dirs(
        libraries,
        cc_toolchain,
        feature_configuration,
        prefer_static_libs,
        output,
        dynamic_library_solib_symlink_output,
        link_type,
        linking_mode,
        is_native_deps,
        solib_dir,
        toolchain_libraries_solib_dir,
        workspace_name):
    """Collects library_search_directories and runtime_library_search_directories."""
    need_toolchain_libraries_rpath = (
        toolchain_libraries_solib_dir and
        (is_dynamic_library(link_type) or
         (link_type == LINK_TARGET_TYPE.EXECUTABLE and linking_mode == LINKING_MODE.DYNAMIC))
    )

    library_search_directories = []
    rpath_roots_for_explicit_so_deps = {}

    solib_dir_path_string = cc_toolchain._solib_dir
    if is_native_deps and cc_toolchain._cpp_configuration.share_native_deps():
        potential_solib_parents = []
        rpath_roots = [solib_dir_path_string + "/"]
    else:
        potential_solib_parents = _find_potential_solib_parents(output, dynamic_library_solib_symlink_output, workspace_name)
        rpath_roots = [exec_root + solib_dir_path_string + "/" for exec_root in potential_solib_parents]

    include_solib_dir, include_toolchain_libraries_solib_dir = _collect_solib_dirs_from_libraries(
        libraries,
        prefer_static_libs,
        feature_configuration,
        solib_dir,
        toolchain_libraries_solib_dir,
        rpath_roots,
        library_search_directories,
        rpath_roots_for_explicit_so_deps,
    )

    rpath_roots_for_explicit_so_deps = rpath_roots_for_explicit_so_deps.keys()

    direct_runtime_library_search_directories = []
    if include_solib_dir:
        direct_runtime_library_search_directories.extend(rpath_roots)
    direct_runtime_library_search_directories.extend(rpath_roots_for_explicit_so_deps)

    transitive_runtime_library_search_directories = []
    if include_toolchain_libraries_solib_dir:
        transitive_runtime_library_search_directories = [
            _collect_toolchain_runtime_library_search_directories(
                cc_toolchain,
                output,
                potential_solib_parents,
                need_toolchain_libraries_rpath,
                toolchain_libraries_solib_dir,
                is_native_deps,
                workspace_name,
            ),
        ]
    all_runtime_library_search_directories = depset(
        order = "topological",
        direct = direct_runtime_library_search_directories,
        transitive = transitive_runtime_library_search_directories,
    )

    return depset(library_search_directories), all_runtime_library_search_directories

def _collect_solib_dirs_from_libraries(
        libraries,
        prefer_static_libs,
        feature_configuration,
        solib_dir,
        toolchain_libraries_solib_dir,
        rpath_roots,
        library_search_directories,
        rpath_roots_for_explicit_so_deps):
    include_solib_dir, include_toolchain_libraries_solib_dir = False, False
    linked_libraries_paths = {}

    dont_copy_dynamic_libraries_to_binary = not feature_configuration.is_enabled("copy_dynamic_libraries_to_binary")
    windows_shared_libraries = (feature_configuration.is_enabled("targets_windows") and
                                feature_configuration.is_enabled("supports_interface_shared_libraries"))
    solib_dir_split = reversed(solib_dir.split("/"))

    for library in libraries:
        static_lib = (prefer_static_libs and
                      (library.static_library != None or library.pic_static_library != None) or
                      (library.interface_library == None and library.dynamic_library == None))
        if static_lib:
            continue
        if library.interface_library:
            input_file = library.interface_library
            original_file = library.resolved_symlink_interface_library or input_file
        else:
            input_file = library.dynamic_library
            original_file = library.resolved_symlink_dynamic_library or input_file

        original_lib_dir = original_file.dirname
        library_identifier = library._library_identifier
        previous_lib_dir = linked_libraries_paths.setdefault(library_identifier, original_lib_dir)

        if previous_lib_dir != original_lib_dir:
            fail(("You are trying to link the same dynamic library %s built in a different" +
                  " configuration. Previously registered instance had path %s, current one" +
                  " has path %s") %
                 (library_identifier, previous_lib_dir, original_lib_dir))

        lib_dir = input_file.dirname

        if dont_copy_dynamic_libraries_to_binary:
            if not include_solib_dir and lib_dir.split("/")[2:] == solib_dir.split("/")[2:]:
                include_solib_dir = True
            if lib_dir == toolchain_libraries_solib_dir:
                include_toolchain_libraries_solib_dir = True

        if windows_shared_libraries:
            if is_shared_library(input_file):
                continue

        lib_dir = input_file.dirname
        if lib_dir != solib_dir and (not toolchain_libraries_solib_dir or toolchain_libraries_solib_dir != lib_dir):
            dotdots = ""
            common_parent = solib_dir
            for seg in solib_dir_split:
                if lib_dir.startswith(common_parent + "/"):
                    break
                dotdots += "../"
                common_parent = common_parent[:-len(seg) - 1]

            for rpath_root in rpath_roots:
                normalized_path_to_root = paths.normalize(rpath_root + dotdots + paths.relativize(lib_dir, common_parent))
                rpath_roots_for_explicit_so_deps[normalized_path_to_root] = None

            if input_file.short_path.startswith("_solib_"):
                artifact_path_under_solib = "/".join(input_file.short_path.split("/")[1:-1])
                for rpath_root in rpath_roots:
                    rpath_roots_for_explicit_so_deps[rpath_root + artifact_path_under_solib] = None

        library_search_directories.append(lib_dir)

    return include_solib_dir, include_toolchain_libraries_solib_dir

def _collect_toolchain_runtime_library_search_directories(
        cc_toolchain,
        output,
        potential_solib_parents,
        need_toolchain_libraries_rpath,
        toolchain_libraries_solib_dir,
        is_native_deps,
        workspace_name):
    if not need_toolchain_libraries_rpath:
        return depset()

    runtime_library_search_directories = []
    toolchain_libraries_solib_name = paths.basename(toolchain_libraries_solib_dir)
    if not (is_native_deps and cc_toolchain._cpp_configuration.share_native_deps()):
        for potential_exec_root in _find_toolchain_solib_parents(cc_toolchain, output, potential_solib_parents, toolchain_libraries_solib_dir, workspace_name):
            runtime_library_search_directories.append(potential_exec_root + toolchain_libraries_solib_name + "/")

    if is_native_deps:
        runtime_library_search_directories.append("../" + toolchain_libraries_solib_name + "/")
        runtime_library_search_directories.append(".")

    runtime_library_search_directories.append(toolchain_libraries_solib_name + "/")

    return depset(runtime_library_search_directories, order = "topological")

def _find_potential_solib_parents(output, dynamic_library_solib_symlink_output, workspace_name):
    solib_parents = []
    outputs = [output]
    if dynamic_library_solib_symlink_output:
        outputs.append(dynamic_library_solib_symlink_output)

    for output in outputs:
        path = output.short_path
        is_external = path.startswith("../")
        uses_legacy_repository_layout = is_external and output.path.split("/")[3] == "external"

        solib_parents.append("../" * (len(path.split("/")) - 1 - (2 if is_external and not uses_legacy_repository_layout else 0)))

        if is_external and not uses_legacy_repository_layout:
            solib_repository_name = path.split("/")[1]
        else:
            solib_repository_name = workspace_name

        solib_parents.append(output.basename + ".runfiles/" + solib_repository_name + "/")
        if is_external and uses_legacy_repository_layout:
            solib_parents.append(
                "../" * (len(output.short_path.split("/")) - 2) + workspace_name + "/",
            )

    return solib_parents

def _find_toolchain_solib_parents(cc_toolchain, output, potential_solib_parents, toolchain_libraries_solib_dir, workspace_name):
    uses_legacy_repository_layout = not cc_toolchain._is_sibling_repository_layout
    if uses_legacy_repository_layout:
        return potential_solib_parents

    solib_parents = []

    toolchain_bin_exec_path = paths.dirname(toolchain_libraries_solib_dir)
    binary_origin_exec_path = output.dirname
    solib_parents.append(
        _get_relative(binary_origin_exec_path, toolchain_bin_exec_path) + "/",
    )

    toolchain_runfiles_repo_name = _get_runfiles_repo_name(cc_toolchain._toolchain_label.repo_name, workspace_name)
    solib_parents.append(
        output.basename + ".runfiles/" + toolchain_runfiles_repo_name + "/",
    )

    binary_repo_name = _get_runfiles_repo_name(output.owner.repo_name, workspace_name)
    toolchain_bin_runfiles_path = toolchain_runfiles_repo_name
    binary_origin_runfiles_path = binary_repo_name + "/" + output.dirname[len(output.root.path):].removeprefix("/")
    solib_parents.append(
        _get_relative(binary_origin_runfiles_path, toolchain_bin_runfiles_path) + "/",
    )

    return solib_parents

def _get_runfiles_repo_name(repo_name, workspace_name):
    return repo_name or workspace_name

def _get_relative(start, to):
    common_parent = start
    dotdots = ""
    for seg in reversed(common_parent.split("/")):
        if paths.starts_with(to, common_parent):
            break
        dotdots += "../"
        common_parent = common_parent[:-len(seg) - 1]

    return dotdots + paths.relativize(to, common_parent)
