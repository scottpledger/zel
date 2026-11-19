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
"""The cc_common.create_library_to_link function."""

load(":common/cc/cc_helper_internal.bzl", "is_versioned_shared_library", "path_contains_up_level_references")
load(":common/cc/compile/lto_compilation_context.bzl", _EMPTY_LTO = "EMPTY_LTO_COMPILATION_CONTEXT")
load(":common/cc/link/lto_backends.bzl", "create_shared_non_lto_artifacts")
load(":common/paths.bzl", "paths")

_cc_internal = _builtins.internal.cc_internal

_warning = "Internal field."

LibraryToLinkInfo = provider(
    "A library the user can link against.",
    fields = {
        "static_library": "Artifact of static library to be linked.",
        "pic_static_library": "Artifact of pic static library to be linked.",
        "dynamic_library": "Artifact of dynamic library to be linked.",
        "interface_library": "Artifact of interface library to be linked.",
        "resolved_symlink_dynamic_library": "Resolved Artifact of the dynamic library if symlinked.",
        "resolved_symlink_interface_library": "Resolved Artifact of the interface library if symlinked.",
        "objects": "List of object files in the library.",
        "pic_objects": "List of pic object files in the library.",
        "alwayslink": "Whether to link the static library/objects in the --whole_archive block.",
        "lto_bitcode_files": "List of LTO bitcode files in the library.",
        "pic_lto_bitcode_files": "List of pic LTO bitcode files in the library.",
        "_library_identifier": _warning,
        "_contains_objects": _warning,
        "_disable_whole_archive": _warning,
        "_must_keep_debug": _warning,
        "_lto_compilation_context": _warning,
        "_pic_lto_compilation_context": _warning,
        "_shared_non_lto_backends": _warning,
        "_pic_shared_non_lto_backends": _warning,
    },
)

def make_library_to_link(
        *,
        static_library = None,
        pic_static_library = None,
        dynamic_library = None,
        interface_library = None,
        resolved_symlink_dynamic_library = None,
        resolved_symlink_interface_library = None,
        objects = [],
        pic_objects = [],
        alwayslink = False,
        _library_identifier,
        _contains_objects = False,
        _disable_whole_archive = False,
        _must_keep_debug = False,
        _lto_compilation_context = None,
        _pic_lto_compilation_context = None,
        _shared_non_lto_backends = {},
        _pic_shared_non_lto_backends = {}):
    """Constructs a frozen LibraryToLink."""
    if not (static_library or pic_static_library or dynamic_library or interface_library):
        fail("At least on library artifact must be non null")
    if resolved_symlink_dynamic_library and not dynamic_library:
        fail("resolved_symlink_dynamic_library without dynamic_library")
    if resolved_symlink_interface_library and not interface_library:
        fail("resolved_symlink_interface_library without interface_library")
    return LibraryToLinkInfo(
        static_library = static_library,
        pic_static_library = pic_static_library,
        interface_library = interface_library,
        dynamic_library = dynamic_library,
        resolved_symlink_dynamic_library = resolved_symlink_dynamic_library,
        resolved_symlink_interface_library = resolved_symlink_interface_library,
        objects = _cc_internal.freeze(objects),
        pic_objects = _cc_internal.freeze(pic_objects),
        alwayslink = alwayslink,
        lto_bitcode_files = _cc_internal.freeze(_lto_compilation_context.lto_bitcode_inputs.keys() if _lto_compilation_context else []),
        pic_lto_bitcode_files = _cc_internal.freeze(_pic_lto_compilation_context.lto_bitcode_inputs.keys() if _pic_lto_compilation_context else []),
        _library_identifier = _library_identifier,
        _contains_objects = _contains_objects,
        _must_keep_debug = _must_keep_debug,
        _disable_whole_archive = _disable_whole_archive,
        _lto_compilation_context = _lto_compilation_context,
        _pic_lto_compilation_context = _pic_lto_compilation_context,
        _shared_non_lto_backends = _cc_internal.freeze(_shared_non_lto_backends),
        _pic_shared_non_lto_backends = _cc_internal.freeze(_pic_shared_non_lto_backends),
    )

def create_library_to_link(
        *,
        actions,
        feature_configuration = None,
        cc_toolchain = None,
        static_library = None,
        pic_static_library = None,
        dynamic_library = None,
        interface_library = None,
        pic_objects = None,
        objects = None,
        lto_compilation_context = None,
        pic_lto_compilation_context = None,
        alwayslink = False,
        dynamic_library_symlink_path = "",
        interface_library_symlink_path = "",
        must_keep_debug = False):
    """Creates a `LibraryToLink` struct with information for linking a single library."""
    errors = []
    _fail = lambda error: errors.append(error)
    _validate_ext = lambda *args, **kwargs: _validate_extension(fail = _fail, *args, **kwargs)

    if dynamic_library_symlink_path:
        _validate_symlink_path("dynamic_library_symlink_path", dynamic_library_symlink_path)
        _validate_ext(
            dynamic_library_symlink_path,
            [".so", ".dylib", ".dll", ".pyd", ".wasm", ".tgt", ".vpi"],
            is_versioned_shared_library,
            empty_ext = True,
        )

    if interface_library_symlink_path:
        _validate_symlink_path("interface_library_symlink_path", interface_library_symlink_path)
        _validate_ext(interface_library_symlink_path, [".ifso", ".tbd", ".lib", ".dll.a"])

    if static_library:
        if alwayslink:
            _validate_ext(static_library, [".a", ".lib", ".rlib"] + [".lo"], not_ext = [".pic.lo", ".if.lib"], empty_ext = True)
        else:
            _validate_ext(static_library, [".a", ".lib", ".rlib"], not_ext = [".lo.lib", ".if.lib"], empty_ext = True)

    if pic_static_library:
        if alwayslink:
            _validate_ext(pic_static_library, [".a", ".lib", ".rlib"] + [".lo"], not_ext = [".if.lib"], empty_ext = True)
        else:
            _validate_ext(pic_static_library, [".a", ".lib", ".rlib"], not_ext = [".lo.lib", ".if.lib"], empty_ext = True)

    resolved_symlink_dynamic_library = None
    if dynamic_library:
        _validate_ext(dynamic_library, [".so", ".dylib", ".dll", ".pyd", ".wasm", ".tgt", ".vpi"], is_versioned_shared_library, empty_ext = True)
        if not cc_toolchain:
            fail("If you pass 'dynamic_library', you must also pass a 'cc_toolchain'")
        if not feature_configuration:
            fail("If you pass 'dynamic_library', you must also pass a 'feature_configuration'")
        if not feature_configuration.is_enabled("targets_windows"):
            resolved_symlink_dynamic_library = dynamic_library
            if dynamic_library_symlink_path:
                if dynamic_library.short_path.startswith("_solib_"):
                    fail("dynamic_library must not be a symbolic link in the solib directory. Got '%s'" % dynamic_library.short_path)
                dynamic_library = _cc_internal.dynamic_library_symlink2(actions, dynamic_library, cc_toolchain._solib_dir, dynamic_library_symlink_path)
            else:
                dynamic_library = _cc_internal.dynamic_library_symlink(actions, dynamic_library, cc_toolchain._solib_dir, True, True)

    resolved_symlink_interface_library = None
    if interface_library:
        _validate_ext(interface_library, [".ifso", ".tbd", ".lib", ".dll.a", ".so", ".dylib"])
        if not cc_toolchain:
            fail("If you pass 'interface_library', you must also pass a 'cc_toolchain'")
        if not feature_configuration:
            fail("If you pass 'interface_library', you must also pass a 'feature_configuration'")
        if not feature_configuration.is_enabled("targets_windows"):
            resolved_symlink_interface_library = interface_library
            if interface_library_symlink_path:
                if interface_library.short_path.startswith("_solib_"):
                    fail("dynamic_library must not be a symbolic link in the solib directory. Got '%s'" % dynamic_library.short_path)
                interface_library = _cc_internal.dynamic_library_symlink2(actions, interface_library, cc_toolchain._solib_dir, interface_library_symlink_path)
            else:
                interface_library = _cc_internal.dynamic_library_symlink(actions, interface_library, cc_toolchain._solib_dir, True, True)

    if errors:
        fail("\n".join(errors))

    identifier = static_library or pic_static_library or dynamic_library or interface_library
    if not identifier:
        fail("Must pass at least one of the following parameters: static_library, pic_static_library, " +
             "dynamic_library and interface_library.")

    library_identifier = identifier.short_path.removesuffix(".pic.a").removesuffix(".nopic.a").removesuffix(".pic.lo")
    library_identifier = paths.replace_extension(library_identifier, "")

    objects = objects or []
    pic_objects = pic_objects or []

    if lto_compilation_context and static_library and objects != None:
        shared_non_lto_backends = create_shared_non_lto_artifacts(
            actions,
            lto_compilation_context,
            False,
            feature_configuration,
            cc_toolchain,
            False,
            objects,
        )
    else:
        shared_non_lto_backends = None
    lto_compilation_context = lto_compilation_context or _EMPTY_LTO
    pic_lto_compilation_context = pic_lto_compilation_context or _EMPTY_LTO

    return make_library_to_link(
        static_library = static_library,
        pic_static_library = pic_static_library,
        objects = objects,
        pic_objects = pic_objects,
        dynamic_library = dynamic_library,
        resolved_symlink_dynamic_library = resolved_symlink_dynamic_library,
        interface_library = interface_library,
        resolved_symlink_interface_library = resolved_symlink_interface_library,
        alwayslink = alwayslink,
        _library_identifier = library_identifier,
        _must_keep_debug = must_keep_debug,
        _lto_compilation_context = lto_compilation_context,
        _pic_lto_compilation_context = pic_lto_compilation_context,
        _shared_non_lto_backends = shared_non_lto_backends,
        _contains_objects = bool(objects) or bool(pic_objects),
    )

def _validate_symlink_path(attr, path):
    if not path or paths.is_absolute(path) or path_contains_up_level_references(path):
        fail("%s must be a relative file path. Got '%s" % (attr, path))

def _validate_extension(path, extensions, func = None, not_ext = [], fail = fail, empty_ext = False):
    path = getattr(path, "basename", path)
    for ext in not_ext:
        if path.endswith(ext):
            fail("'%s' does not have any of the allowed extensions %s" % (path, ", ".join(extensions)))
    for ext in extensions:
        if path.endswith(ext):
            return
    if empty_ext:
        _, actual_ext = paths.split_extension(path)
        if actual_ext == "":
            return
    if func and func(struct(basename = path)):
        return
    fail("'%s' does not have any of the allowed extensions %s" % (path, ", ".join(extensions)))
