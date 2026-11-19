# Copyright 2023 The Bazel Authors. All rights reserved.
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
# limitations under the License
"""Definition of CcInfo provider."""

load(":common/cc/cc_helper_internal.bzl", "check_private_api")
load(":common/cc/link/create_extra_link_time_library.bzl", "create_extra_link_time_libraries", "merge_extra_link_time_libraries")
load(":common/paths.bzl", "paths")

_cc_internal = _builtins.internal.cc_internal

CcCompilationContextInfo = provider(
    "CcCompilationContext",
    fields = {
        "includes": "Search paths for header files referenced both by angle bracket and quotes.",
        "quote_includes": "Search paths for header files referenced by quotes.",
        "system_includes": "Search paths for header files referenced by angle brackets.",
        "framework_includes": "Search paths for framework header files.",
        "external_includes": "Search paths for external header files referenced by angle bracket.",
        "defines": "Defines needed to compile this target and transitive dependents.",
        "local_defines": "Defines needed to compile this target only.",
        "headers": "Headers needed to compile this target.",
        "validation_artifacts": "Set of validation artifacts.",
        "_direct_module_maps": "Internal.",
        "_exporting_module_maps": "Internal",
        "_exporting_module_map_files": "Internal",
        "_non_code_inputs": "Internal.",
        "_transitive_modules": "Internal",
        "_transitive_pic_modules": "Internal",
        "_module_map": "Internal",
        "_virtual_to_original_headers": "Internal",
        "_modules_info_files": "Internal",
        "_pic_modules_info_files": "Internal",
        "_module_files": "Internal",
        "_pic_module_files": "Internal",
        "direct_headers": "Modular headers declared by this target.",
        "direct_public_headers": "Modular public headers declared by this target.",
        "direct_private_headers": "Modular private headers declared by this target.",
        "direct_textual_headers": "Textual headers declared by this target.",
        "_header_info": "Internal",
    },
)

CcLinkingContextInfo = provider(
    "CcLinkingContextInfo",
    fields = {
        "linker_inputs": "A depset of linker inputs.",
        "_extra_link_time_libraries": "Extra link time libraries.",
    },
)

CcNativeLibraryInfo = provider(
    "CcNativeLibraryInfo",
    fields = ["libraries_to_link"],
)

CcDebugContextInfo = provider(
    doc = "C++ debug related objects, specifically when fission is used.",
    fields = {
        "files": "The .dwo files for non-PIC compilation.",
        "pic_files": "The .dwo files for PIC compilation.",
    },
)

EMPTY_COMPILATION_CONTEXT = CcCompilationContextInfo(
    defines = depset(),
    local_defines = depset(),
    headers = depset(),
    direct_headers = [],
    direct_public_headers = [],
    direct_private_headers = [],
    direct_textual_headers = [],
    includes = depset(),
    quote_includes = depset(),
    system_includes = depset(),
    framework_includes = depset(),
    external_includes = depset(),
    validation_artifacts = depset(),
    _virtual_to_original_headers = depset(),
    _module_map = None,
    _exporting_module_maps = depset(),
    _exporting_module_map_files = depset(),
    _non_code_inputs = depset(),
    _transitive_modules = depset(),
    _transitive_pic_modules = depset(),
    _direct_module_maps = depset(),
    _modules_info_files = depset(),
    _pic_modules_info_files = depset(),
    _module_files = depset(),
    _pic_module_files = depset(),
    _header_info = _cc_internal.create_header_info(),
)

_EMPTY_LINKING_CONTEXT = CcLinkingContextInfo(
    linker_inputs = depset(),
    _extra_link_time_libraries = None,
)

_EMPTY_DEBUG_CONTEXT = CcDebugContextInfo(
    files = depset(),
    pic_files = depset(),
)

_ModuleMapInfo = provider(
    "ModuleMapInfo",
    fields = {
        "file": "The module map file.",
        "name": "The name of the module.",
    },
)

def create_module_map(*, file, name):
    check_private_api()
    return _ModuleMapInfo(file = file, name = name)

def create_separate_module_map(module_map):
    return _ModuleMapInfo(file = module_map.file, name = module_map.name + ".sep")

def create_linking_context(
        *,
        linker_inputs,
        extra_link_time_library = None):
    return CcLinkingContextInfo(
        linker_inputs = linker_inputs,
        _extra_link_time_libraries = create_extra_link_time_libraries(extra_link_time_library),
    )

def merge_linking_contexts(*, linking_contexts):
    linker_inputs = depset(transitive = [ctx.linker_inputs for ctx in linking_contexts], order = "topological")
    extra_link_time_libraries = merge_extra_link_time_libraries([ctx._extra_link_time_libraries for ctx in linking_contexts if ctx._extra_link_time_libraries != None])
    return CcLinkingContextInfo(
        linker_inputs = linker_inputs,
        _extra_link_time_libraries = extra_link_time_libraries,
    )

def create_debug_context(compilation_outputs):
    check_private_api()
    return CcDebugContextInfo(
        files = depset(compilation_outputs._dwo_files),
        pic_files = depset(compilation_outputs._pic_dwo_files),
    )

def merge_debug_context(debug_contexts = []):
    check_private_api()
    if not debug_contexts:
        return _EMPTY_DEBUG_CONTEXT

    transitive_dwo_files = []
    transitive_pic_dwo_files = []

    for ctx in debug_contexts:
        transitive_dwo_files.append(ctx.files)
        transitive_pic_dwo_files.append(ctx.pic_files)

    return CcDebugContextInfo(
        files = depset(transitive = transitive_dwo_files),
        pic_files = depset(transitive = transitive_pic_dwo_files),
    )

def _create_cc_info(
        *,
        compilation_context = None,
        linking_context = None,
        debug_context = None,
        cc_native_library_info = None):
    return dict(
        compilation_context = compilation_context or EMPTY_COMPILATION_CONTEXT,
        linking_context = linking_context or _EMPTY_LINKING_CONTEXT,
        _debug_context = debug_context or _EMPTY_DEBUG_CONTEXT,
        _legacy_transitive_native_libraries = cc_native_library_info.libraries_to_link if cc_native_library_info else depset(),
    )

CcInfo, _ = provider(
    doc = "Provider for C++ compilation and linking information.",
    fields = {
        "compilation_context": "A `CcCompilationContext`.",
        "linking_context": "A `CcLinkingContext`.",
        "_debug_context": "A `CcDebugInfoContext`.",
        "_legacy_transitive_native_libraries": "A `CcNativeLibraryInfo`.",
    },
    init = _create_cc_info,
)

def _normalize_paths(paths_depset):
    if not paths_depset:
        return depset()
    return depset([paths.normalize(p) for p in paths_depset.to_list()])

def create_compilation_context(
        *,
        headers = None,
        includes = None,
        quote_includes = None,
        system_includes = None,
        framework_includes = None,
        external_includes = None,
        defines = None,
        local_defines = None,
        direct_textual_headers = [],
        direct_public_headers = [],
        direct_private_headers = [],
        dependent_cc_compilation_contexts = [],
        exported_dependent_cc_compilation_contexts = [],
        non_code_inputs = [],
        module_map = None,
        virtual_to_original_headers = None,
        pic_header_module = None,
        header_module = None,
        separate_module_headers = [],
        separate_module = None,
        separate_pic_module = None,
        add_public_headers_to_modular_headers = True):
    if headers != None and type(headers) != type(depset()):
        fail("for headers, got list, want a depset of File")

    modular_public_hdrs_list = list(direct_public_headers)
    if add_public_headers_to_modular_headers and headers:
        modular_public_hdrs_list.extend(headers.to_list())

    header_info = _cc_internal.create_header_info(
        header_module = header_module,
        pic_header_module = pic_header_module,
        modular_public_headers = modular_public_hdrs_list,
        modular_private_headers = direct_private_headers,
        textual_headers = direct_textual_headers,
        separate_module_headers = separate_module_headers,
        separate_module = separate_module,
        separate_pic_module = separate_pic_module,
    )

    includes = _normalize_paths(includes)
    quote_includes = _normalize_paths(quote_includes)
    system_includes = _normalize_paths(system_includes)
    framework_includes = _normalize_paths(framework_includes)
    external_includes = _normalize_paths(external_includes)

    single_compilation_context = CcCompilationContextInfo(
        defines = defines if defines else depset(),
        local_defines = local_defines if local_defines else depset(),
        headers = headers if headers else depset(),
        direct_headers = _cc_internal.freeze(header_info.modular_public_headers + header_info.modular_private_headers + header_info.separate_module_headers),
        direct_public_headers = header_info.modular_public_headers,
        direct_private_headers = header_info.modular_private_headers,
        direct_textual_headers = header_info.textual_headers,
        includes = includes,
        quote_includes = quote_includes,
        system_includes = system_includes,
        framework_includes = framework_includes,
        external_includes = external_includes,
        validation_artifacts = depset(),
        _virtual_to_original_headers = virtual_to_original_headers if virtual_to_original_headers else depset(),
        _module_map = module_map,
        _exporting_module_maps = depset(),
        _exporting_module_map_files = depset(),
        _non_code_inputs = depset(non_code_inputs),
        _transitive_modules = depset(),
        _transitive_pic_modules = depset(),
        _direct_module_maps = depset(),
        _header_info = header_info,
        _modules_info_files = depset(),
        _pic_modules_info_files = depset(),
        _module_files = depset(),
        _pic_module_files = depset(),
    )

    return _merge_compilation_contexts(
        compilation_context = single_compilation_context,
        exported_deps = exported_dependent_cc_compilation_contexts,
        deps = dependent_cc_compilation_contexts,
    )

def _flat_depset(*, transitive = []):
    largest_depset = depset()
    largest_depset_list = []
    for t in transitive:
        t_list = t.to_list()
        if len(t_list) > len(largest_depset_list):
            largest_depset_list = t_list
            largest_depset = t

    all = depset(transitive = transitive)
    if all.to_list() == largest_depset_list:
        return largest_depset
    return all

def _merge_compilation_contexts(*, compilation_context = EMPTY_COMPILATION_CONTEXT, exported_deps = [], deps = []):
    exporting_module_maps = depset(
        direct = [dep._module_map for dep in exported_deps if dep._module_map],
        transitive = [dep._exporting_module_maps for dep in exported_deps],
    )
    exporting_module_map_files = depset(
        direct = [dep._module_map.file for dep in exported_deps if dep._module_map],
        transitive = [dep._exporting_module_map_files for dep in exported_deps],
    )
    all_deps = exported_deps + deps
    direct_module_maps = depset(
        direct = [dep._module_map.file for dep in all_deps if dep._module_map],
        transitive = [dep._exporting_module_map_files for dep in all_deps],
    )

    dep_header_infos = [dep._header_info for dep in all_deps]
    merged_header_infos = [dep._header_info for dep in exported_deps]

    compilation_context_header_info = compilation_context._header_info
    header_info = _cc_internal.create_header_info_with_deps(
        header_info = compilation_context_header_info,
        deps = dep_header_infos,
        merged_deps = merged_header_infos,
    )

    transitive_modules_artifacts = []
    transitive_pic_modules_artifacts = []
    for dep in all_deps:
        dep_header_info = dep._header_info
        if dep_header_info.header_module:
            transitive_modules_artifacts.append(dep_header_info.header_module)
        if dep_header_info.separate_module:
            transitive_modules_artifacts.append(dep_header_info.separate_module)
        if dep_header_info.pic_header_module:
            transitive_pic_modules_artifacts.append(dep_header_info.pic_header_module)
        if dep_header_info.separate_pic_module:
            transitive_pic_modules_artifacts.append(dep_header_info.separate_pic_module)

    return CcCompilationContextInfo(
        includes = _flat_depset(
            transitive = [compilation_context.includes] + [dep.includes for dep in all_deps],
        ),
        quote_includes = _flat_depset(
            transitive = [compilation_context.quote_includes] + [dep.quote_includes for dep in all_deps],
        ),
        system_includes = _flat_depset(
            transitive = [compilation_context.system_includes] + [dep.system_includes for dep in all_deps],
        ),
        framework_includes = _flat_depset(
            transitive = [compilation_context.framework_includes] + [dep.framework_includes for dep in all_deps],
        ),
        external_includes = _flat_depset(
            transitive = [compilation_context.external_includes] + [dep.external_includes for dep in all_deps],
        ),
        defines = _flat_depset(
            transitive = [dep.defines for dep in all_deps] + [compilation_context.defines],
        ),
        local_defines = compilation_context.local_defines,
        headers = depset(
            direct = compilation_context.headers.to_list(),
            transitive = [dep.headers for dep in all_deps],
        ),
        direct_headers = _cc_internal.freeze(header_info.modular_public_headers + header_info.modular_private_headers + header_info.separate_module_headers),
        direct_public_headers = header_info.modular_public_headers,
        direct_private_headers = header_info.modular_private_headers,
        direct_textual_headers = header_info.textual_headers,
        _direct_module_maps = direct_module_maps,
        _module_map = compilation_context._module_map,
        _exporting_module_maps = exporting_module_maps,
        _exporting_module_map_files = exporting_module_map_files,
        _non_code_inputs = depset(
            direct = compilation_context._non_code_inputs.to_list(),
            transitive = [dep._non_code_inputs for dep in all_deps],
        ),
        _virtual_to_original_headers = depset(
            transitive = [compilation_context._virtual_to_original_headers] + [dep._virtual_to_original_headers for dep in all_deps],
        ),
        validation_artifacts = depset(
            transitive = [compilation_context.validation_artifacts] + [dep.validation_artifacts for dep in all_deps],
        ),
        _header_info = header_info,
        _transitive_modules = depset(
            transitive_modules_artifacts,
            transitive = [dep._transitive_modules for dep in all_deps],
        ),
        _transitive_pic_modules = depset(
            transitive_pic_modules_artifacts,
            transitive = [dep._transitive_pic_modules for dep in all_deps],
        ),
        _modules_info_files = depset(
            transitive = [compilation_context._modules_info_files] + [dep._modules_info_files for dep in all_deps],
        ),
        _pic_modules_info_files = depset(
            transitive = [compilation_context._pic_modules_info_files] + [dep._pic_modules_info_files for dep in all_deps],
        ),
        _module_files = depset(
            transitive = [compilation_context._module_files] + [dep._module_files for dep in all_deps],
        ),
        _pic_module_files = depset(
            transitive = [compilation_context._pic_module_files] + [dep._pic_module_files for dep in all_deps],
        ),
    )

def merge_compilation_contexts(*, compilation_contexts = []):
    return _merge_compilation_contexts(exported_deps = compilation_contexts)

def create_compilation_context_with_extra_header_tokens(
        *,
        cc_compilation_context,
        extra_header_tokens):
    return CcCompilationContextInfo(
        defines = cc_compilation_context.defines,
        local_defines = cc_compilation_context.local_defines,
        headers = cc_compilation_context.headers,
        direct_headers = cc_compilation_context.direct_headers,
        direct_public_headers = cc_compilation_context.direct_public_headers,
        direct_private_headers = cc_compilation_context.direct_private_headers,
        direct_textual_headers = cc_compilation_context.direct_textual_headers,
        includes = cc_compilation_context.includes,
        quote_includes = cc_compilation_context.quote_includes,
        system_includes = cc_compilation_context.system_includes,
        framework_includes = cc_compilation_context.framework_includes,
        external_includes = cc_compilation_context.external_includes,
        validation_artifacts = depset(extra_header_tokens, transitive = [cc_compilation_context.validation_artifacts]),
        _virtual_to_original_headers = cc_compilation_context._virtual_to_original_headers,
        _module_map = cc_compilation_context._module_map,
        _exporting_module_maps = cc_compilation_context._exporting_module_maps,
        _exporting_module_map_files = cc_compilation_context._exporting_module_map_files,
        _non_code_inputs = cc_compilation_context._non_code_inputs,
        _transitive_modules = cc_compilation_context._transitive_modules,
        _transitive_pic_modules = cc_compilation_context._transitive_pic_modules,
        _direct_module_maps = cc_compilation_context._direct_module_maps,
        _header_info = cc_compilation_context._header_info,
        _modules_info_files = cc_compilation_context._modules_info_files,
        _pic_modules_info_files = cc_compilation_context._pic_modules_info_files,
        _module_files = cc_compilation_context._module_files,
        _pic_module_files = cc_compilation_context._pic_module_files,
    )

def create_cc_compilation_context_with_cpp20_modules(
        *,
        cc_compilation_context,
        cpp_module_files,
        pic_cpp_module_files,
        cpp_modules_info_file,
        pic_cpp_modules_info_file):
    return CcCompilationContextInfo(
        defines = cc_compilation_context.defines,
        local_defines = cc_compilation_context.local_defines,
        headers = cc_compilation_context.headers,
        direct_headers = cc_compilation_context.direct_headers,
        direct_public_headers = cc_compilation_context.direct_public_headers,
        direct_private_headers = cc_compilation_context.direct_private_headers,
        direct_textual_headers = cc_compilation_context.direct_textual_headers,
        includes = cc_compilation_context.includes,
        quote_includes = cc_compilation_context.quote_includes,
        system_includes = cc_compilation_context.system_includes,
        framework_includes = cc_compilation_context.framework_includes,
        external_includes = cc_compilation_context.external_includes,
        validation_artifacts = cc_compilation_context.validation_artifacts,
        _virtual_to_original_headers = cc_compilation_context._virtual_to_original_headers,
        _module_map = cc_compilation_context._module_map,
        _exporting_module_maps = cc_compilation_context._exporting_module_maps,
        _exporting_module_map_files = cc_compilation_context._exporting_module_map_files,
        _non_code_inputs = cc_compilation_context._non_code_inputs,
        _transitive_modules = cc_compilation_context._transitive_modules,
        _transitive_pic_modules = cc_compilation_context._transitive_pic_modules,
        _direct_module_maps = cc_compilation_context._direct_module_maps,
        _header_info = cc_compilation_context._header_info,
        _module_files = depset(cpp_module_files, transitive = [cc_compilation_context._module_files]),
        _pic_module_files = depset(pic_cpp_module_files, transitive = [cc_compilation_context._pic_module_files]),
        _modules_info_files = depset([cpp_modules_info_file], transitive = [cc_compilation_context._modules_info_files]),
        _pic_modules_info_files = depset([pic_cpp_modules_info_file], transitive = [cc_compilation_context._pic_modules_info_files]),
    )

def merge_cc_infos(*, direct_cc_infos = [], cc_infos = []):
    cc_linking_contexts = []
    cc_debug_info_contexts = []
    transitive_native_cc_libraries = []

    for cc_info in direct_cc_infos:
        cc_linking_contexts.append(getattr(cc_info, "linking_context", _EMPTY_LINKING_CONTEXT))
        cc_debug_info_contexts.append(getattr(cc_info, "_debug_context", _EMPTY_DEBUG_CONTEXT))
        transitive_native_cc_libraries.append(getattr(cc_info, "_legacy_transitive_native_libraries", depset()))

    for cc_info in cc_infos:
        cc_linking_contexts.append(getattr(cc_info, "linking_context", _EMPTY_LINKING_CONTEXT))
        cc_debug_info_contexts.append(getattr(cc_info, "_debug_context", _EMPTY_DEBUG_CONTEXT))
        transitive_native_cc_libraries.append(getattr(cc_info, "_legacy_transitive_native_libraries", depset()))

    return CcInfo(
        compilation_context = _merge_compilation_contexts(
            compilation_context = EMPTY_COMPILATION_CONTEXT,
            exported_deps = [getattr(cc_info, "compilation_context", EMPTY_COMPILATION_CONTEXT) for cc_info in direct_cc_infos],
            deps = [getattr(cc_info, "compilation_context", EMPTY_COMPILATION_CONTEXT) for cc_info in cc_infos],
        ),
        linking_context = merge_linking_contexts(linking_contexts = cc_linking_contexts),
        debug_context = merge_debug_context(cc_debug_info_contexts),
        cc_native_library_info = CcNativeLibraryInfo(libraries_to_link = depset(order = "topological", transitive = transitive_native_cc_libraries)),
    )
