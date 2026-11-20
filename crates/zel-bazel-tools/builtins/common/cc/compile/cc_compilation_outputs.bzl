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
"""The CcCompilationOutputs provider."""

load(":common/cc/cc_helper_internal.bzl", "wrap_with_check_private_api", _PRIVATE_STARLARKIFICATION_ALLOWLIST = "PRIVATE_STARLARKIFICATION_ALLOWLIST")
load(":common/cc/compile/lto_compilation_context.bzl", "EMPTY_LTO_COMPILATION_CONTEXT", "merge_lto_compilation_contexts")

_cc_internal = _builtins.internal.cc_internal

_UnboundValueProviderDoNotUse = provider("Unique symbol to distinguish between bound and unbound Starlark values.", fields = [])
_UNBOUND = _UnboundValueProviderDoNotUse()

CcCompilationOutputsInfo = provider(
    doc = "The outputs of a C++ compilation.",
    fields = {
        "objects": "(list[File]) All .o files built by the target.",
        "pic_objects": "(list[File]) All .pic.o files built by the target.",
        "_lto_compilation_context": "(LTOCompilationContext) Maps ThinLTO bitcode files.",
        "_dwo_files": "(list[File]) All .dwo files built by the target.",
        "_pic_dwo_files": "(list[File]) All .pic.dwo files built by the target.",
        "_gcno_files": "(list[File]) All .gcno files built by the target.",
        "_pic_gcno_files": "(list[File]) All .pic.gcno files built by the target.",
        "temps": "(() -> depset[File]) Temporary artifacts.",
        "_header_tokens": "(list[File]) Processed header tokens.",
        "_module_files": "(list[File]) All .pcm files built by the target.",
        "cpp_module_files": "(list[File]) C++20 module files.",
        "pic_cpp_module_files": "(list[File]) PIC C++20 module files.",
        "cpp_modules_info_file": "(File) .CXXModules.json file.",
        "pic_cpp_modules_info_file": "(File) PIC .CXXModules.json file.",
    },
)

def create_compilation_outputs_internal(
        *,
        objects = [],
        pic_objects = [],
        lto_compilation_context = None,
        dwo_files = [],
        pic_dwo_files = [],
        gcno_files = [],
        pic_gcno_files = [],
        temps = [],
        header_tokens = [],
        module_files = [],
        cpp_module_files = [],
        pic_cpp_module_files = [],
        cpp_modules_info_file = None,
        pic_cpp_modules_info_file = None):
    if lto_compilation_context == None:
        lto_compilation_context = EMPTY_LTO_COMPILATION_CONTEXT
    return CcCompilationOutputsInfo(
        objects = _cc_internal.freeze(objects),
        pic_objects = _cc_internal.freeze(pic_objects),
        temps = wrap_with_check_private_api(depset(temps)),
        _header_tokens = _cc_internal.freeze(header_tokens),
        _module_files = _cc_internal.freeze(module_files),
        _lto_compilation_context = lto_compilation_context,
        _gcno_files = _cc_internal.freeze(gcno_files),
        _pic_gcno_files = _cc_internal.freeze(pic_gcno_files),
        _dwo_files = _cc_internal.freeze(dwo_files),
        _pic_dwo_files = _cc_internal.freeze(pic_dwo_files),
        cpp_module_files = _cc_internal.freeze(cpp_module_files),
        pic_cpp_module_files = _cc_internal.freeze(pic_cpp_module_files),
        cpp_modules_info_file = cpp_modules_info_file,
        pic_cpp_modules_info_file = pic_cpp_modules_info_file,
    )

EMPTY_COMPILATION_OUTPUTS = create_compilation_outputs_internal()

def _validate_extensions(param_name, files, valid_extensions):
    for file in files:
        if type(file) != "File":
            fail("for '%s', got a depset of '%s', expected a depset of 'File'" % (param_name, type(file)))
        if file.is_directory:
            continue
        if file.extension not in valid_extensions:
            fail(
                "'%s' has wrong extension. The list of possible extensions for '%s' is: %s" % (
                    file.path,
                    param_name,
                    ", ".join(valid_extensions),
                ),
            )

def _to_list(data):
    if not data:
        return []
    if type(data) == type(depset()):
        return data.to_list()
    if type(data) == type([]):
        return data
    fail("Expected depset or list for artifacts, got " + type(data))

def create_compilation_outputs(
        *,
        objects = None,
        pic_objects = None,
        lto_compilation_context = _UNBOUND,
        dwo_objects = _UNBOUND,
        pic_dwo_objects = _UNBOUND):
    if lto_compilation_context != _UNBOUND or dwo_objects != _UNBOUND or pic_dwo_objects != _UNBOUND:
        _cc_internal.check_private_api(allowlist = _PRIVATE_STARLARKIFICATION_ALLOWLIST)
    if lto_compilation_context == _UNBOUND:
        lto_compilation_context = None
    if dwo_objects == _UNBOUND:
        dwo_objects = []
    if pic_dwo_objects == _UNBOUND:
        pic_dwo_objects = []

    objects = _to_list(objects)
    pic_objects = _to_list(pic_objects)
    dwo_objects = _to_list(dwo_objects)
    pic_dwo_objects = _to_list(pic_dwo_objects)

    obj_extensions = ["o", "obj", "opb", "bc"]
    _validate_extensions("objects", objects, obj_extensions)
    _validate_extensions("pic_objects", pic_objects, obj_extensions)

    return create_compilation_outputs_internal(
        objects = objects,
        pic_objects = pic_objects,
        lto_compilation_context = lto_compilation_context,
        dwo_files = dwo_objects,
        pic_dwo_files = pic_dwo_objects,
    )

def merge_compilation_outputs(*, compilation_outputs):
    objects = []
    pic_objects = []
    dwo_files = []
    pic_dwo_files = []
    gcno_files = []
    pic_gcno_files = []
    lto_compilation_contexts = []
    transitive_temps = []
    header_tokens = []
    module_files = []

    for co in compilation_outputs:
        objects.extend(co.objects)
        pic_objects.extend(co.pic_objects)
        dwo_files.extend(co._dwo_files)
        pic_dwo_files.extend(co._pic_dwo_files)
        gcno_files.extend(co._gcno_files)
        pic_gcno_files.extend(co._pic_gcno_files)
        transitive_temps.append(co.temps())
        header_tokens.extend(co._header_tokens)
        module_files.extend(co._module_files)
        if co._lto_compilation_context:
            lto_compilation_contexts.append(co._lto_compilation_context)

    return CcCompilationOutputsInfo(
        objects = objects,
        pic_objects = pic_objects,
        _lto_compilation_context = merge_lto_compilation_contexts(lto_compilation_contexts = lto_compilation_contexts),
        _dwo_files = dwo_files,
        _pic_dwo_files = pic_dwo_files,
        _gcno_files = gcno_files,
        _pic_gcno_files = pic_gcno_files,
        temps = wrap_with_check_private_api(depset(transitive = transitive_temps)),
        _header_tokens = header_tokens,
        _module_files = module_files,
    )
