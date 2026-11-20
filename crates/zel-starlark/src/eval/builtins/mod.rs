// Copyright 2026 The Bazel Authors. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod string_methods;
pub mod universal;

pub use string_methods::{eval_string_method, eval_string_method_in};
pub use universal::{
    compare_values, dir_attrs, dotted_version_canonical_components, dotted_version_cmp,
    dotted_version_compare_to, eval_universal_function, eval_universal_function_in,
    eval_universal_function_with_semantics, fallible_merge_sort, is_callable, merge_sort_keyed,
    parse_dotted_version, to_int32, unpack_dict_pairs, value_to_sequence, DICT_METHODS,
    LABEL_FIELDS, LABEL_METHODS, LIST_METHODS, SET_METHODS, STRING_METHODS,
};
