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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorElement {
    Select(Vec<(String, Value)>),
    Native(Value),
}

/// Represents a configurable attribute value (`select({...}) + [...]` or `select({...}) | {...}`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SelectorList {
    pub elements: Vec<SelectorElement>,
}

impl SelectorList {
    pub fn from_select(entries: Vec<(String, Value)>) -> Self {
        Self {
            elements: vec![SelectorElement::Select(entries)],
        }
    }

    /// Resolves the `SelectorList` against a set of active `config_setting` conditions
    /// (falling back to `//conditions:default`).
    pub fn resolve(&self, active_conditions: &[String]) -> Result<Value, StarlarkError> {
        let mut acc: Option<Value> = None;
        for elem in &self.elements {
            let chosen = match elem {
                SelectorElement::Native(v) => v.clone(),
                SelectorElement::Select(branches) => {
                    let mut matched = None;
                    for cond in active_conditions {
                        if let Some((_, v)) = branches.iter().find(|(k, _)| k == cond) {
                            matched = Some(v.clone());
                            break;
                        }
                    }
                    if matched.is_none() {
                        matched = branches
                            .iter()
                            .find(|(k, _)| k == "//conditions:default")
                            .map(|(_, v)| v.clone());
                    }
                    match matched {
                        Some(val) => val,
                        None => {
                            return Err(StarlarkError(
                                "Configurable attribute doesn't match this configuration (no matching condition)".to_string(),
                            ));
                        }
                    }
                }
            };
            acc = Some(match acc {
                None => chosen,
                Some(prev) => match (&prev, &chosen) {
                    (Value::Dict(_), Value::Dict(_)) => prev.bitor(&chosen)?,
                    _ => prev.add(&chosen)?,
                },
            });
        }
        Ok(acc.unwrap_or(Value::None))
    }
}
