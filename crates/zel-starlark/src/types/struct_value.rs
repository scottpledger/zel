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

use crate::types::list::StarlarkError;
use crate::types::value::Value;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StructValue {
    pub fields: BTreeMap<String, Value>,
    pub is_attr: bool,
    /// Java class name Bazel would print for this value (`<unknown object CLASS>`) when it is an
    /// opaque host object that merely behaves like a struct here.
    pub opaque_class: Option<String>,
}

impl StructValue {
    pub fn new(fields: BTreeMap<String, Value>) -> Self {
        Self { fields, is_attr: false, opaque_class: None }
    }

    /// The `attr` of a `repository_ctx`: an opaque `RepoDefinition` for Bazel.
    pub fn new_attr(fields: BTreeMap<String, Value>) -> Self {
        Self {
            fields,
            is_attr: true,
            opaque_class: Some("com.google.devtools.build.lib.bazel.repository.RepoDefinition".to_string()),
        }
    }

    /// A struct-like value that Bazel represents as an opaque object of the given Java class.
    pub fn new_opaque(fields: BTreeMap<String, Value>, class: &str) -> Self {
        Self { fields, is_attr: false, opaque_class: Some(class.to_string()) }
    }

    /// `repr`/`str` of this struct (`struct(a = "x")`, with strings quoted like Bazel does).
    pub fn repr(&self) -> String {
        if let Some(c) = &self.opaque_class {
            return format!("<unknown object {c}>");
        }
        let fields: Vec<String> = self.fields.iter().map(|(k, v)| format!("{k} = {}", v.repr())).collect();
        format!("struct({})", fields.join(", "))
    }

    pub fn get(&self, attr: &str) -> Result<Value, StarlarkError> {
        if let Some(val) = self.fields.get(attr) {
            Ok(val.clone())
        } else if self.is_attr {
            Err(StarlarkError(format!("unknown attribute {attr}")))
        } else {
            let available = self
                .fields
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            Err(StarlarkError(format!(
                "'struct' value has no field or method '{attr}'\nAvailable attributes: {available}"
            )))
        }
    }

    pub fn add(&self, other: &Self) -> Result<Self, StarlarkError> {
        let mut fields = self.fields.clone();
        for (k, v) in &other.fields {
            if fields.contains_key(k) {
                return Err(StarlarkError(format!(
                    "cannot add structs: field '{k}' appears in both operands"
                )));
            }
            fields.insert(k.clone(), v.clone());
        }
        Ok(Self { fields, is_attr: self.is_attr, opaque_class: self.opaque_class.clone() })
    }
}

impl fmt::Display for StructValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.repr())
    }
}
