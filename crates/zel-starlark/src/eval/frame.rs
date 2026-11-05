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

use crate::syntax::location::Location;

/// Activation frame representing a function call on the call stack.
#[derive(Debug, Clone)]
pub struct CallFrame {
    pub function_name: String,
    /// Identity of the running function (address of its definition), 0 if unknown.
    pub function_id: usize,
    pub location: Location,
    pub locals: crate::syntax::ast::Scope,
    /// Captured scopes of enclosing functions, innermost first.
    pub parents: Vec<crate::syntax::ast::Scope>,
    /// Defining module of the running function when it differs from the module being evaluated;
    /// free variables then resolve against that module's globals.
    pub module: Option<std::sync::Arc<crate::syntax::ast::ModuleEnv>>,
}

impl CallFrame {
    pub fn new(function_name: impl Into<String>, location: Location) -> Self {
        Self {
            function_name: function_name.into(),
            function_id: 0,
            location,
            locals: Default::default(),
            parents: Vec::new(),
            module: None,
        }
    }
}
