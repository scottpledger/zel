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

use crate::syntax::ast::*;
use crate::types::list::StarlarkError;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Module,
    Function,
    Comprehension,
}

pub struct Resolver {
    errors: Vec<StarlarkError>,
    loop_depth: usize,
    /// `load()` may follow other statements (BUILD files); `.bzl` files forbid it.
    pub allow_late_loads: bool,
}

impl Resolver {
    pub fn new() -> Self {
        Self {
            errors: Vec::new(),
            loop_depth: 0,
            allow_late_loads: false,
        }
    }

    pub fn resolve_module(&mut self, stmts: &[Stmt]) -> Result<(), StarlarkError> {
        let mut saw_non_load = false;
        let mut loaded_names = HashSet::new();

        for stmt in stmts {
            match stmt {
                Stmt::Load(load, loc) => {
                    if saw_non_load && !self.allow_late_loads {
                        return Err(StarlarkError(format!(
                            "{loc}: load statements must appear before any other statement"
                        )));
                    }
                    for (local_name, _) in &load.symbols {
                        if !loaded_names.insert(local_name.clone()) {
                            return Err(StarlarkError(format!(
                                "{loc}: load statement defines '{local_name}' more than once"
                            )));
                        }
                    }
                }
                Stmt::Pass(_) => {}
                // A leading docstring (string-literal expression statement) is not a statement
                // for this check.
                Stmt::Expr(Expr::Literal(crate::types::value::Value::String(_))) if !saw_non_load => {}
                _ => {
                    saw_non_load = true;
                }
            }
        }

        let mut globals = HashSet::new();
        // Register top-level function names and global assignments
        for stmt in stmts {
            self.collect_module_bindings(stmt, &mut globals);
        }

        // Validate statements
        for stmt in stmts {
            self.resolve_stmt(stmt, ScopeKind::Module, &mut globals, false)?;
        }

        if let Some(err) = self.errors.first() {
            return Err(err.clone());
        }
        Ok(())
    }

    fn collect_module_bindings(&self, stmt: &Stmt, globals: &mut HashSet<String>) {
        match stmt {
            Stmt::Def(func) => {
                globals.insert(func.name.clone());
            }
            Stmt::Assign { target, .. } => {
                self.collect_target_bindings(target, globals);
            }
            Stmt::Load(load, _) => {
                for (sym, _) in &load.symbols {
                    globals.insert(sym.clone());
                }
            }
            _ => {}
        }
    }

    fn collect_target_bindings(&self, target: &AssignTarget, names: &mut HashSet<String>) {
        match target {
            AssignTarget::Ident(name) => {
                names.insert(name.clone());
            }
            AssignTarget::Tuple(targets) => {
                for t in pattern_names(targets) {
                    names.insert(t);
                }
            }
            _ => {}
        }
    }

    fn resolve_stmt(
        &mut self,
        stmt: &Stmt,
        scope: ScopeKind,
        bound_vars: &mut HashSet<String>,
        in_function: bool,
    ) -> Result<(), StarlarkError> {
        match stmt {
            Stmt::Load(_, loc) => {
                if scope != ScopeKind::Module {
                    return Err(StarlarkError(format!(
                        "{loc}: load statement not at top level"
                    )));
                }
                Ok(())
            }
            Stmt::Return(opt_expr, loc) => {
                if !in_function {
                    return Err(StarlarkError(format!(
                        "{loc}: return statements must be inside a function"
                    )));
                }
                if let Some(e) = opt_expr {
                    self.resolve_expr(e, scope, bound_vars)?;
                }
                Ok(())
            }
            Stmt::Break(loc) => {
                if self.loop_depth == 0 {
                    return Err(StarlarkError(format!(
                        "{loc}: break statement outside loop"
                    )));
                }
                Ok(())
            }
            Stmt::Continue(loc) => {
                if self.loop_depth == 0 {
                    return Err(StarlarkError(format!(
                        "{loc}: continue statement outside loop"
                    )));
                }
                Ok(())
            }
            Stmt::Pass(_) => Ok(()),
            Stmt::Expr(e) => self.resolve_expr(e, scope, bound_vars),
            Stmt::Assign {
                target,
                op,
                value,
                location,
                ..
            } => {
                if *op != AssignOp::Assign {
                    // Augmented assign reads target first
                    if let AssignTarget::Ident(ref name) = target {
                        if scope == ScopeKind::Function && !bound_vars.contains(name) {
                            return Err(StarlarkError(format!(
                                "{location}: local variable '{name}' is referenced before assignment"
                            )));
                        }
                    }
                }
                self.resolve_expr(value, scope, bound_vars)?;
                self.collect_target_bindings(target, bound_vars);
                Ok(())
            }
            Stmt::If {
                branches,
                else_body,
                ..
            } => {
                for (cond, body) in branches {
                    self.resolve_expr(cond, scope, bound_vars)?;
                    for s in body {
                        self.resolve_stmt(s, scope, bound_vars, in_function)?;
                    }
                }
                for s in else_body {
                    self.resolve_stmt(s, scope, bound_vars, in_function)?;
                }
                Ok(())
            }
            Stmt::For {
                var_names,
                iter,
                body,
                ..
            } => {
                self.resolve_expr(iter, scope, bound_vars)?;
                for v in pattern_names(var_names) {
                    bound_vars.insert(v);
                }
                self.loop_depth += 1;
                for s in body {
                    self.resolve_stmt(s, scope, bound_vars, in_function)?;
                }
                self.loop_depth -= 1;
                Ok(())
            }
            Stmt::Def(func) => {
                // Check duplicate parameters
                let mut param_names = HashSet::new();
                for param in &func.params {
                    if !param_names.insert(param.name.clone()) {
                        return Err(StarlarkError(format!(
                            "{}: duplicate parameter: {}",
                            func.location, param.name
                        )));
                    }
                    if let Some(ref def_expr) = param.default {
                        self.resolve_expr(def_expr, scope, bound_vars)?;
                    }
                }

                // Collect local variables within function
                let mut locals = param_names;
                for s in &func.body {
                    self.collect_function_locals(s, &mut locals);
                }

                // Resolve function body with local scope
                let mut assigned = HashSet::new();
                for param in &func.params {
                    assigned.insert(param.name.clone());
                }

                for s in &func.body {
                    self.resolve_stmt(s, ScopeKind::Function, &mut assigned, true)?;
                }
                Ok(())
            }
        }
    }

    fn collect_function_locals(&self, stmt: &Stmt, locals: &mut HashSet<String>) {
        match stmt {
            Stmt::Assign { target, .. } => {
                self.collect_target_bindings(target, locals);
            }
            Stmt::For {
                var_names, body, ..
            } => {
                for v in pattern_names(var_names) {
                    locals.insert(v);
                }
                for s in body {
                    self.collect_function_locals(s, locals);
                }
            }
            Stmt::If {
                branches,
                else_body,
                ..
            } => {
                for (_, body) in branches {
                    for s in body {
                        self.collect_function_locals(s, locals);
                    }
                }
                for s in else_body {
                    self.collect_function_locals(s, locals);
                }
            }
            _ => {}
        }
    }

    fn resolve_expr(
        &mut self,
        expr: &Expr,
        scope: ScopeKind,
        bound_vars: &mut HashSet<String>,
    ) -> Result<(), StarlarkError> {
        match expr {
            Expr::Literal(_) => Ok(()),
            Expr::Ident(name, _loc) => {
                if scope == ScopeKind::Function && !bound_vars.contains(name) && is_local_var_candidate(name) {
                    // Check if variable is defined as local anywhere in the function
                    // Note: if it's universal (like len) or global, it can be read.
                    // But if it is assigned in the function, it is local, and reading before assignment is an error!
                }
                Ok(())
            }
            Expr::List(items, _) | Expr::Tuple(items, _) => {
                for item in items {
                    self.resolve_expr(item, scope, bound_vars)?;
                }
                Ok(())
            }
            Expr::Dict(pairs, _, _) => {
                for (k, v) in pairs {
                    self.resolve_expr(k, scope, bound_vars)?;
                    self.resolve_expr(v, scope, bound_vars)?;
                }
                Ok(())
            }
            Expr::ListComp {
                body,
                clauses,
                location: _,
            } => {
                self.resolve_comprehension(clauses, body, scope, bound_vars)
            }
            Expr::DictComp {
                key,
                value,
                clauses,
                location: _,
            } => {
                self.resolve_comprehension_dict(clauses, key, value, scope, bound_vars)
            }
            Expr::UnaryOp { expr, .. } => self.resolve_expr(expr, scope, bound_vars),
            Expr::BinaryOp { lhs, rhs, .. } => {
                self.resolve_expr(lhs, scope, bound_vars)?;
                self.resolve_expr(rhs, scope, bound_vars)?;
                Ok(())
            }
            Expr::Compare {
                lhs, comparisons, ..
            } => {
                self.resolve_expr(lhs, scope, bound_vars)?;
                for (_, rhs) in comparisons {
                    self.resolve_expr(rhs, scope, bound_vars)?;
                }
                Ok(())
            }
            Expr::IfExpr {
                test, body, orelse, ..
            } => {
                self.resolve_expr(test, scope, bound_vars)?;
                self.resolve_expr(body, scope, bound_vars)?;
                self.resolve_expr(orelse, scope, bound_vars)?;
                Ok(())
            }
            Expr::Call {
                callee,
                pos_args,
                kw_args,
                var_args,
                kw_var_args,
                ..
            } => {
                self.resolve_expr(callee, scope, bound_vars)?;
                for arg in pos_args {
                    self.resolve_expr(arg, scope, bound_vars)?;
                }
                for (_, arg) in kw_args {
                    self.resolve_expr(arg, scope, bound_vars)?;
                }
                if let Some(va) = var_args {
                    self.resolve_expr(va, scope, bound_vars)?;
                }
                if let Some(ka) = kw_var_args {
                    self.resolve_expr(ka, scope, bound_vars)?;
                }
                Ok(())
            }
            Expr::Attr { target, .. } => self.resolve_expr(target, scope, bound_vars),
            Expr::Index { target, index, .. } => {
                self.resolve_expr(target, scope, bound_vars)?;
                self.resolve_expr(index, scope, bound_vars)?;
                Ok(())
            }
            Expr::Slice {
                target,
                start,
                end,
                step,
                ..
            } => {
                self.resolve_expr(target, scope, bound_vars)?;
                if let Some(s) = start {
                    self.resolve_expr(s, scope, bound_vars)?;
                }
                if let Some(e) = end {
                    self.resolve_expr(e, scope, bound_vars)?;
                }
                if let Some(st) = step {
                    self.resolve_expr(st, scope, bound_vars)?;
                }
                Ok(())
            }
            Expr::Lambda { params, body, .. } => {
                let mut lambda_bound = bound_vars.clone();
                for p in params {
                    lambda_bound.insert(p.name.clone());
                }
                self.resolve_expr(body, ScopeKind::Function, &mut lambda_bound)
            }
        }
    }

    fn resolve_comprehension(
        &mut self,
        clauses: &[CompClause],
        body: &Expr,
        scope: ScopeKind,
        outer_bound: &HashSet<String>,
    ) -> Result<(), StarlarkError> {
        // Comprehensions create an isolated local scope.
        // The first `for` clause's iter expression is resolved in the outer scope!
        let mut comp_scope = outer_bound.clone();

        for (i, clause) in clauses.iter().enumerate() {
            match clause {
                CompClause::For { var_names, iter } => {
                    if i == 0 {
                        self.resolve_expr(iter, scope, &mut outer_bound.clone())?;
                    } else {
                        self.resolve_expr(iter, ScopeKind::Comprehension, &mut comp_scope)?;
                    }
                    for v in pattern_names(var_names) {
                        comp_scope.insert(v);
                    }
                }
                CompClause::If(cond) => {
                    self.resolve_expr(cond, ScopeKind::Comprehension, &mut comp_scope)?;
                }
            }
        }
        self.resolve_expr(body, ScopeKind::Comprehension, &mut comp_scope)
    }

    fn resolve_comprehension_dict(
        &mut self,
        clauses: &[CompClause],
        key: &Expr,
        value: &Expr,
        scope: ScopeKind,
        outer_bound: &HashSet<String>,
    ) -> Result<(), StarlarkError> {
        let mut comp_scope = outer_bound.clone();

        for (i, clause) in clauses.iter().enumerate() {
            match clause {
                CompClause::For { var_names, iter } => {
                    if i == 0 {
                        self.resolve_expr(iter, scope, &mut outer_bound.clone())?;
                    } else {
                        self.resolve_expr(iter, ScopeKind::Comprehension, &mut comp_scope)?;
                    }
                    for v in pattern_names(var_names) {
                        comp_scope.insert(v);
                    }
                }
                CompClause::If(cond) => {
                    self.resolve_expr(cond, ScopeKind::Comprehension, &mut comp_scope)?;
                }
            }
        }
        self.resolve_expr(key, ScopeKind::Comprehension, &mut comp_scope)?;
        self.resolve_expr(value, ScopeKind::Comprehension, &mut comp_scope)
    }
}

fn is_local_var_candidate(_name: &str) -> bool {
    true
}
