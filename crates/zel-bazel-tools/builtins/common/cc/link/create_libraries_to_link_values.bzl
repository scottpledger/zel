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
"""Goes over LibraryToLinks and produces LibraryToLinkValue-s."""

load(":common/cc/cc_helper_internal.bzl", "is_shared_library", "is_versioned_shared_library", "root_relative_path")

_TYPE = struct(
    STATIC_LIBRARY = "static_library",
    DYNAMIC_LIBRARY = "dynamic_library",
    INTERFACE_LIBRARY = "interface_library",
    OBJECT_FILE = "object_file",
    OBJECT_FILE_GROUP = "object_file_group",
    VERSIONED_DYNAMIC_LIBRARY = "versioned_dynamic_library",
)

_NamedLibraryInfo = provider(
    "NamedLibraryInfo represents static_library, dynamic_library, interface_library, and object_file.",
    fields = ["type", "name", "is_whole_archive"],
)
_ObjectFileGroupInfo = provider(
    "ObjectFileGroupInfo.",
    fields = ["type", "object_files", "is_whole_archive"],
)
_VersionedLibraryInfo = provider(
    "VersionedLibraryInfo.",
    fields = ["type", "name", "path", "is_whole_archive"],
)

def add_object_files_to_link(object_files, libraries_to_link_values):
    """Adds object files to libraries_to_link_values."""
    for object_file in object_files:
        if object_file.is_directory:
            libraries_to_link_values.append(
                _ObjectFileGroupInfo(
                    type = _TYPE.OBJECT_FILE_GROUP,
                    object_files = [object_file],
                    is_whole_archive = False,
                ),
            )
        else:
            libraries_to_link_values.append(
                _NamedLibraryInfo(
                    type = _TYPE.OBJECT_FILE,
                    name = object_file.path,
                    is_whole_archive = False,
                ),
            )

def add_libraries_to_link(
        libraries,
        prefer_static_libs,
        prefer_pic_libs,
        use_start_end_lib,
        need_whole_archive,
        lto_map,
        allow_lto_indexing,
        shared_non_lto_obj_root_prefix,
        feature_configuration,
        expanded_linker_inputs,
        libraries_to_link_values):
    """Converts libraries from LibraryToLink to LibraryToLinkValue."""
    static_library_files = set()

    windows_with_interface_shared_libraries = (
        feature_configuration.is_enabled("targets_windows") and
        feature_configuration.is_enabled("supports_interface_shared_libraries")
    )

    for library in libraries:
        static_lib = (prefer_static_libs and
                      (library.static_library != None or library.pic_static_library != None) or
                      (library.interface_library == None and library.dynamic_library == None))
        if static_lib:
            pic = (prefer_pic_libs and library.pic_static_library != None) or \
                  library.static_library == None
            library_file = library.pic_static_library if pic else library.static_library
            if library_file in static_library_files:
                continue
            static_library_files.add(library_file)
            _add_static_library_to_link(
                library,
                prefer_pic_libs,
                use_start_end_lib,
                need_whole_archive,
                lto_map,
                allow_lto_indexing,
                shared_non_lto_obj_root_prefix,
                expanded_linker_inputs,
                libraries_to_link_values,
            )
        else:
            _add_dynamic_library_to_link(
                library,
                windows_with_interface_shared_libraries,
                expanded_linker_inputs,
                libraries_to_link_values,
            )

def _add_static_library_to_link(
        library,
        prefer_pic_libs,
        use_start_end_lib,
        need_whole_archive,
        lto_map,
        allow_lto_indexing,
        shared_non_lto_obj_root_prefix,
        expanded_linker_inputs,
        libraries_to_link_values):
    input_is_whole_archive = not library._disable_whole_archive and (
        library.alwayslink or need_whole_archive
    )

    pic = (prefer_pic_libs and library.pic_static_library != None) or \
          library.static_library == None
    library_file = library.pic_static_library if pic else library.static_library
    objects = library.pic_objects if pic else library.objects

    if use_start_end_lib and library._contains_objects:
        objects = process_objects_for_lto(
            objects,
            lto_map,
            allow_lto_indexing,
            shared_non_lto_obj_root_prefix,
            expanded_linker_inputs,
        )

        if input_is_whole_archive:
            for object in objects:
                if object.is_directory:
                    libraries_to_link_values.append(
                        _ObjectFileGroupInfo(
                            type = _TYPE.OBJECT_FILE_GROUP,
                            object_files = [object],
                            is_whole_archive = True,
                        ),
                    )
                else:
                    libraries_to_link_values.append(
                        _NamedLibraryInfo(
                            type = _TYPE.OBJECT_FILE,
                            name = object.path,
                            is_whole_archive = True,
                        ),
                    )
        elif objects:
            libraries_to_link_values.append(
                _ObjectFileGroupInfo(
                    type = _TYPE.OBJECT_FILE_GROUP,
                    object_files = objects,
                    is_whole_archive = False,
                ),
            )
    else:
        libraries_to_link_values.append(
            _NamedLibraryInfo(
                type = _TYPE.STATIC_LIBRARY,
                name = library_file.path,
                is_whole_archive = input_is_whole_archive,
            ),
        )
        expanded_linker_inputs.append(library_file)

def _add_dynamic_library_to_link(
        library,
        windows_with_interface_shared_libraries,
        expanded_linker_inputs,
        libraries_to_link_values):
    input_file = library.interface_library or library.dynamic_library

    expanded_linker_inputs.append(input_file)

    shared_library = is_shared_library(input_file)
    if windows_with_interface_shared_libraries and shared_library:
        return

    name = input_file.basename

    has_compatible_name = (
        name.startswith("lib") or
        (not name.endswith(".so") and not name.endswith(".dylib") and
         not name.endswith(".dll") and not name.endswith(".pyd"))
    )
    if shared_library and has_compatible_name:
        lib_name = name.removeprefix("lib").removesuffix(".so").removesuffix(".dylib") \
            .removesuffix(".dll").removesuffix(".pyd")
        libraries_to_link_values.append(
            _NamedLibraryInfo(
                type = _TYPE.DYNAMIC_LIBRARY,
                name = lib_name,
                is_whole_archive = False,
            ),
        )
    elif shared_library or is_versioned_shared_library(input_file):
        libraries_to_link_values.append(
            _VersionedLibraryInfo(
                type = _TYPE.VERSIONED_DYNAMIC_LIBRARY,
                name = name,
                path = input_file.path,
                is_whole_archive = False,
            ),
        )
    else:
        libraries_to_link_values.append(
            _NamedLibraryInfo(
                type = _TYPE.INTERFACE_LIBRARY,
                name = input_file.path,
                is_whole_archive = False,
            ),
        )

def process_objects_for_lto(
        object_files,
        lto_map,
        allow_lto_indexing,
        shared_non_lto_obj_root_prefix,
        expanded_linker_artifacts):
    """Processes and returns the subset of object files not handled by LTO."""
    if allow_lto_indexing:
        mapped_object_files = []
        remaining_object_files = []
        for orig_object in object_files:
            object = lto_map.pop(orig_object, orig_object)
            mapped_object_files.append(object)
            if object == orig_object or root_relative_path(object).startswith(shared_non_lto_obj_root_prefix):
                remaining_object_files.append(object)
    else:
        mapped_object_files = [lto_map.pop(obj, obj) for obj in object_files] if lto_map else object_files
        remaining_object_files = mapped_object_files

    expanded_linker_artifacts.extend(mapped_object_files)

    return remaining_object_files
