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

//! Static checks applied to parsed files before execution.
//!
//! * [`check_dot_bazel_file_syntax`] ports `DotBazelFileSyntaxChecker.java` (the restricted
//!   syntax of `BUILD`, `WORKSPACE`, `REPO.bazel` and `MODULE.bazel` files).
//! * [`check_undefined_names`] mirrors the resolver's `name 'x' is not defined` errors,
//!   including `SpellChecker.didYouMean` suggestions.

use crate::syntax::ast::{pattern_names, AssignTarget, CompClause, Expr, Stmt};
use crate::syntax::location::Location;
use std::collections::HashSet;

/// A static (parse/resolve time) error with its source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub location: Location,
    pub message: String,
}

impl std::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.location, self.message)
    }
}

/// Port of `DotBazelFileSyntaxChecker`: rejects `def`/`lambda`, `for`/`if` statements, `*args` and
/// `**kwargs` call arguments, and (optionally) `load()` statements.
///
/// `where_` describes the file kind (e.g. `"BUILD files"`).
pub fn check_dot_bazel_file_syntax(
    stmts: &[Stmt],
    where_: &str,
    can_load_bzl: bool,
    allow_literal_star_star_args: bool,
) -> Vec<SyntaxError> {
    let mut c = DotBazelChecker {
        where_: where_.to_string(),
        can_load_bzl,
        allow_literal_star_star_args,
        errors: Vec::new(),
        module_file: where_ == "MODULE.bazel files",
        include_assigned: false,
    };
    for s in stmts {
        c.stmt(s);
    }
    c.errors
}

struct DotBazelChecker {
    where_: String,
    can_load_bzl: bool,
    allow_literal_star_star_args: bool,
    errors: Vec<SyntaxError>,
    /// Checking a `MODULE.bazel` file: apply `CompiledModuleFile.SyntaxChecker`'s `include` rules.
    module_file: bool,
    include_assigned: bool,
}

const INCLUDE_MSG: &str = "the `include` directive MUST be called with one positional string literal argument and, optionally, `dev_dependency` as a boolean literal";

fn is_valid_include_call(pos_args: &[Expr], kw_args: &[(String, Expr)], var_args: &Option<Box<Expr>>, kw_var_args: &Option<Box<Expr>>) -> bool {
    if var_args.is_some() || kw_var_args.is_some() || pos_args.len() != 1 {
        return false;
    }
    if !matches!(&pos_args[0], Expr::Literal(crate::types::value::Value::String(_))) {
        return false;
    }
    match kw_args {
        [] => true,
        [(k, v)] => {
            k == "dev_dependency"
                && matches!(v, Expr::Ident(n, _) if n == "True" || n == "False"
                    ) || (k == "dev_dependency" && matches!(v, Expr::Literal(crate::types::value::Value::Bool(_))))
        }
        _ => false,
    }
}

impl DotBazelChecker {
    fn err(&mut self, location: &Location, message: String) {
        self.errors.push(SyntaxError {
            location: location.clone(),
            message,
        });
    }

    fn functions_msg(&self) -> String {
        format!(
            "functions may not be defined in {}{}",
            self.where_,
            if self.can_load_bzl {
                ". You may move the function to a .bzl file and load it."
            } else {
                "."
            }
        )
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Def(f) => {
                let m = self.functions_msg();
                self.err(&f.location, m);
            }
            Stmt::For { location, .. } => {
                let m = format!(
                    "`for` statements are not allowed in {}. You may inline the loop{} or as a last resort use a list comprehension.",
                    self.where_,
                    if self.can_load_bzl {
                        ", move it to a function definition (in a .bzl file),"
                    } else {
                        ""
                    }
                );
                self.err(location, m);
            }
            Stmt::If { location, .. } => {
                let m = format!(
                    "`if` statements are not allowed in {}. You may{} use an `if` expression for simple cases.",
                    self.where_,
                    if self.can_load_bzl {
                        " move conditional logic to a function definition (in a .bzl file), or"
                    } else {
                        ""
                    }
                );
                self.err(location, m);
            }
            Stmt::Load(_, location) => {
                if !self.can_load_bzl {
                    let m = format!("`load` statements may not be used in {}", self.where_);
                    self.err(location, m);
                }
            }
            Stmt::Expr(Expr::Call { callee, pos_args, kw_args, var_args, kw_var_args, .. })
                if self.module_file
                    && !self.include_assigned
                    && matches!(&**callee, Expr::Ident(n, _) if n == "include") =>
            {
                if !is_valid_include_call(pos_args, kw_args, var_args, kw_var_args) {
                    let loc = callee.location();
                    self.err(&loc, INCLUDE_MSG.to_string());
                }
            }
            Stmt::Assign { target: AssignTarget::Ident(n), value, .. }
                if self.module_file && !self.include_assigned && n == "include" =>
            {
                self.expr(value);
                self.include_assigned = true;
            }
            Stmt::Assign { target, value, .. } => {
                if let AssignTarget::Index { target, index } = target {
                    self.expr(target);
                    self.expr(index);
                }
                if let AssignTarget::Attr { target, .. } = target {
                    self.expr(target);
                }
                self.expr(value);
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::Return(Some(e), _) => self.expr(e),
            _ => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Lambda { location, .. } => {
                let m = self.functions_msg();
                self.err(location, m);
            }
            Expr::Call {
                callee,
                pos_args,
                kw_args,
                var_args,
                kw_var_args,
                location,
            } => {
                if let Some(ka) = kw_var_args {
                    if !self.allow_literal_star_star_args {
                        let m = format!(
                            "**kwargs arguments are not allowed in {}. Pass the arguments in explicitly.",
                            self.where_
                        );
                        self.err(location, m);
                    }
                    if !matches!(**ka, Expr::Dict(..)) {
                        let m = format!(
                            "**kwargs arguments must be a literal dict in {}.",
                            self.where_
                        );
                        self.err(location, m);
                    }
                }
                if var_args.is_some() {
                    let m = format!(
                        "*args arguments are not allowed in {}. Pass the arguments in explicitly.",
                        self.where_
                    );
                    self.err(location, m);
                }
                self.expr(callee);
                for a in pos_args {
                    self.expr(a);
                }
                for (_, a) in kw_args {
                    self.expr(a);
                }
                if let Some(a) = var_args {
                    self.expr(a);
                }
                if let Some(a) = kw_var_args {
                    self.expr(a);
                }
            }
            Expr::List(items, _) | Expr::Tuple(items, _) => items.iter().for_each(|i| self.expr(i)),
            Expr::Dict(pairs, _, _) => {
                for (k, v) in pairs {
                    self.expr(k);
                    self.expr(v);
                }
            }
            Expr::ListComp { body, clauses, .. } => {
                self.expr(body);
                self.clauses(clauses);
            }
            Expr::DictComp {
                key,
                value,
                clauses,
                ..
            } => {
                self.expr(key);
                self.expr(value);
                self.clauses(clauses);
            }
            Expr::UnaryOp { expr, .. } => self.expr(expr),
            Expr::BinaryOp { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Compare {
                lhs, comparisons, ..
            } => {
                self.expr(lhs);
                for (_, r) in comparisons {
                    self.expr(r);
                }
            }
            Expr::IfExpr {
                test, body, orelse, ..
            } => {
                self.expr(test);
                self.expr(body);
                self.expr(orelse);
            }
            Expr::Attr { target, .. } => self.expr(target),
            Expr::Index { target, index, .. } => {
                self.expr(target);
                self.expr(index);
            }
            Expr::Slice {
                target,
                start,
                end,
                step,
                ..
            } => {
                self.expr(target);
                for x in [start, end, step].into_iter().flatten() {
                    self.expr(x);
                }
            }
            Expr::Ident(n, loc) => {
                if self.module_file && !self.include_assigned && n == "include" {
                    let m = "the `include` directive MUST be called directly at the top-level".to_string();
                    self.err(loc, m);
                }
            }
            Expr::Literal(_) => {}
        }
    }

    fn clauses(&mut self, clauses: &[CompClause]) {
        for c in clauses {
            match c {
                CompClause::For { iter, .. } => self.expr(iter),
                CompClause::If(e) => self.expr(e),
            }
        }
    }
}

/// Names that are always available (Starlark universe).
pub const UNIVERSE: &[&str] = &[
    "None", "True", "False", "abs", "all", "any", "bool", "dict", "dir", "enumerate", "fail",
    "float", "getattr", "hasattr", "hash", "int", "len", "list", "max", "min", "print", "range",
    "repr", "reversed", "set", "sorted", "str", "tuple", "type", "zip", "chr", "ord",
];

/// Reports every reference to a name that is neither bound in an enclosing scope, nor a
/// module-level binding, nor `predeclared`/universal. Mirrors Bazel's resolver, which reports
/// these statically (so unreachable code is checked too).
pub fn check_undefined_names(stmts: &[Stmt], predeclared: &[String]) -> Vec<SyntaxError> {
    let mut module = HashSet::new();
    for s in stmts {
        collect_stmt_bindings(s, &mut module, true);
    }
    let mut w = NameWalker {
        scopes: vec![module.clone()],
        predeclared: predeclared.iter().cloned().collect(),
        candidates: {
            let mut c: Vec<String> = module.into_iter().collect();
            c.extend(predeclared.iter().cloned());
            c.extend(UNIVERSE.iter().map(|s| s.to_string()));
            c
        },
        errors: Vec::new(),
    };
    for s in stmts {
        w.stmt(s);
    }
    w.errors
}

fn collect_target(t: &AssignTarget, out: &mut HashSet<String>) {
    match t {
        AssignTarget::Ident(n) => {
            out.insert(n.clone());
        }
        AssignTarget::Tuple(ns) => out.extend(pattern_names(ns)),
        _ => {}
    }
}

/// Collects names bound by `s` in the *current* scope (not entering nested `def`s).
fn collect_stmt_bindings(s: &Stmt, out: &mut HashSet<String>, include_load_def: bool) {
    match s {
        Stmt::Assign { target, .. } => collect_target(target, out),
        Stmt::Def(f) if include_load_def => {
            out.insert(f.name.clone());
        }
        Stmt::Def(f) => {
            out.insert(f.name.clone());
        }
        Stmt::Load(l, _) => out.extend(l.symbols.iter().map(|(local, _)| local.clone())),
        Stmt::For {
            var_names, body, ..
        } => {
            out.extend(pattern_names(var_names));
            for b in body {
                collect_stmt_bindings(b, out, include_load_def);
            }
        }
        Stmt::If {
            branches,
            else_body,
            ..
        } => {
            for (_, body) in branches {
                for b in body {
                    collect_stmt_bindings(b, out, include_load_def);
                }
            }
            for b in else_body {
                collect_stmt_bindings(b, out, include_load_def);
            }
        }
        _ => {}
    }
}

struct NameWalker {
    scopes: Vec<HashSet<String>>,
    predeclared: HashSet<String>,
    candidates: Vec<String>,
    errors: Vec<SyntaxError>,
}

impl NameWalker {
    fn defined(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(name))
            || self.predeclared.contains(name)
            || UNIVERSE.contains(&name)
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Assign { target, value, .. } => {
                self.expr(value);
                match target {
                    AssignTarget::Index { target, index } => {
                        self.expr(target);
                        self.expr(index);
                    }
                    AssignTarget::Attr { target, .. } => self.expr(target),
                    AssignTarget::Ident(n) => {
                        // Augmented assignment reads the target; plain assignment binds it.
                        let _ = n;
                    }
                    AssignTarget::Tuple(_) => {}
                }
            }
            Stmt::Def(f) => {
                for p in &f.params {
                    if let Some(d) = &p.default {
                        self.expr(d);
                    }
                }
                let mut locals: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
                for b in &f.body {
                    collect_stmt_bindings(b, &mut locals, true);
                }
                self.scopes.push(locals);
                for b in &f.body {
                    self.stmt(b);
                }
                self.scopes.pop();
            }
            Stmt::If {
                branches,
                else_body,
                ..
            } => {
                for (c, body) in branches {
                    self.expr(c);
                    body.iter().for_each(|b| self.stmt(b));
                }
                else_body.iter().for_each(|b| self.stmt(b));
            }
            Stmt::For { iter, body, .. } => {
                self.expr(iter);
                body.iter().for_each(|b| self.stmt(b));
            }
            Stmt::Return(Some(e), _) => self.expr(e),
            Stmt::Expr(e) => self.expr(e),
            _ => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Ident(name, loc) => {
                if !self.defined(name) {
                    let suggestion = did_you_mean(
                        name,
                        self.candidates
                            .iter()
                            .map(|s| s.as_str())
                            .chain(self.scopes.iter().flat_map(|scope| scope.iter().map(|s| s.as_str()))),
                    );
                    self.errors.push(SyntaxError {
                        location: loc.clone(),
                        message: format!("name '{name}' is not defined{suggestion}"),
                    });
                }
            }
            Expr::Literal(_) => {}
            Expr::List(items, _) | Expr::Tuple(items, _) => items.iter().for_each(|i| self.expr(i)),
            Expr::Dict(pairs, _, _) => {
                for (k, v) in pairs {
                    self.expr(k);
                    self.expr(v);
                }
            }
            Expr::ListComp { body, clauses, .. } => {
                self.comprehension(clauses, |w| w.expr(body));
            }
            Expr::DictComp {
                key,
                value,
                clauses,
                ..
            } => {
                self.comprehension(clauses, |w| {
                    w.expr(key);
                    w.expr(value);
                });
            }
            Expr::UnaryOp { expr, .. } => self.expr(expr),
            Expr::BinaryOp { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Compare {
                lhs, comparisons, ..
            } => {
                self.expr(lhs);
                comparisons.iter().for_each(|(_, r)| self.expr(r));
            }
            Expr::IfExpr {
                test, body, orelse, ..
            } => {
                self.expr(test);
                self.expr(body);
                self.expr(orelse);
            }
            Expr::Call {
                callee,
                pos_args,
                kw_args,
                var_args,
                kw_var_args,
                ..
            } => {
                self.expr(callee);
                pos_args.iter().for_each(|a| self.expr(a));
                kw_args.iter().for_each(|(_, a)| self.expr(a));
                if let Some(a) = var_args {
                    self.expr(a);
                }
                if let Some(a) = kw_var_args {
                    self.expr(a);
                }
            }
            Expr::Attr { target, .. } => self.expr(target),
            Expr::Index { target, index, .. } => {
                self.expr(target);
                self.expr(index);
            }
            Expr::Slice {
                target,
                start,
                end,
                step,
                ..
            } => {
                self.expr(target);
                for x in [start, end, step].into_iter().flatten() {
                    self.expr(x);
                }
            }
            Expr::Lambda { params, body, .. } => {
                for p in params {
                    if let Some(d) = &p.default {
                        self.expr(d);
                    }
                }
                self.scopes
                    .push(params.iter().map(|p| p.name.clone()).collect());
                self.expr(body);
                self.scopes.pop();
            }
        }
    }

    fn comprehension(&mut self, clauses: &[CompClause], body: impl FnOnce(&mut Self)) {
        let mut vars = HashSet::new();
        for c in clauses {
            if let CompClause::For { var_names, .. } = c {
                vars.extend(pattern_names(var_names));
            }
        }
        // The first `for` iterable is evaluated in the enclosing scope.
        let mut first = true;
        for c in clauses {
            match c {
                CompClause::For { iter, .. } if first => {
                    self.expr(iter);
                    first = false;
                    self.scopes.push(vars.clone());
                }
                CompClause::For { iter, .. } => self.expr(iter),
                CompClause::If(e) => {
                    if first {
                        self.scopes.push(vars.clone());
                        first = false;
                    }
                    self.expr(e)
                }
            }
        }
        if first {
            self.scopes.push(vars);
        }
        body(self);
        self.scopes.pop();
    }
}

/// Port of `SpellChecker.editDistance`.
fn edit_distance(s1: &str, s2: &str, max: usize) -> Option<usize> {
    if s1 == s2 {
        return Some(0);
    }
    let a: Vec<char> = s1.chars().collect();
    let b: Vec<char> = s2.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        row[0] = i;
        let mut best = row[0];
        let mut previous = i - 1;
        for j in 1..=b.len() {
            let old = row[j];
            row[j] = std::cmp::min(
                previous + usize::from(a[i - 1] != b[j - 1]),
                1 + std::cmp::min(row[j - 1], row[j]),
            );
            previous = old;
            best = best.min(row[j]);
        }
        if best > max {
            return None;
        }
    }
    (row[b.len()] <= max).then_some(row[b.len()])
}

/// Port of `SpellChecker.didYouMean`: `" (did you mean 'x'?)"` or `""`.
pub fn did_you_mean<'a>(input: &str, words: impl IntoIterator<Item = &'a str>) -> String {
    let mut best: Option<&str> = None;
    let mut best_distance = std::cmp::min(5, (input.chars().count() + 1) / 2);
    let lower = input.to_lowercase();
    for cand in words {
        if let Some(d) = edit_distance(&lower, &cand.to_lowercase(), best_distance) {
            if d < best_distance {
                best_distance = d;
                best = Some(cand);
            }
        }
    }
    match best {
        Some(s) => format!(" (did you mean '{s}'?)"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::lexer::Lexer;
    use crate::syntax::parser::Parser;

    fn parse(src: &str) -> Vec<Stmt> {
        let toks = Lexer::new("BUILD", src).lex_all().unwrap();
        Parser::new(toks).parse_module().unwrap()
    }

    #[test]
    fn build_file_restrictions() {
        let errs = check_dot_bazel_file_syntax(&parse("def f():\n  pass\n"), "BUILD files", true, false);
        assert_eq!(errs.len(), 1);
        assert_eq!(
            errs[0].message,
            "functions may not be defined in BUILD files. You may move the function to a .bzl file and load it."
        );
        let errs = check_dot_bazel_file_syntax(&parse("for x in [1]:\n  pass\n"), "BUILD files", true, false);
        assert!(errs[0].message.starts_with("`for` statements are not allowed in BUILD files."));
        let errs = check_dot_bazel_file_syntax(&parse("if True:\n  pass\n"), "BUILD files", true, false);
        assert!(errs[0].message.starts_with("`if` statements are not allowed in BUILD files."));
        let errs = check_dot_bazel_file_syntax(&parse("x = lambda: 1\n"), "BUILD files", true, false);
        assert!(errs[0].message.starts_with("functions may not be defined in BUILD files"));
        let errs = check_dot_bazel_file_syntax(&parse("f(*a)\nf(**k)\n"), "BUILD files", true, false);
        assert_eq!(errs.len(), 3);
        assert!(check_dot_bazel_file_syntax(&parse("x = [1 for y in [2] if y]\nz = 1 if x else 2\n"), "BUILD files", true, false).is_empty());
        let errs = check_dot_bazel_file_syntax(&parse("load('//:a.bzl', 'a')\n"), "MODULE.bazel files", false, false);
        assert_eq!(errs[0].message, "`load` statements may not be used in MODULE.bazel files");
    }

    #[test]
    fn undefined_names() {
        let stmts = parse("x = foo\ndef f(a):\n  b = a\n  return b + c\ny = [i for i in [1] if i]\nz = undefined_name(1)\n");
        let errs = check_undefined_names(&stmts, &[]);
        let msgs: Vec<_> = errs.iter().map(|e| e.message.clone()).collect();
        assert_eq!(
            msgs,
            vec![
                "name 'foo' is not defined".to_string(),
                "name 'c' is not defined".to_string(),
                "name 'undefined_name' is not defined".to_string()
            ]
        );
        let errs = check_undefined_names(&parse("filegroop(name='a')\n"), &["filegroup".to_string()]);
        assert_eq!(errs[0].message, "name 'filegroop' is not defined (did you mean 'filegroup'?)");
    }
}
