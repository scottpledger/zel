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

//! Starlark Interpreter & Bazel Dialect (`Depset`, `SelectorList`, `Provider`, `Struct`,
//! `BUILD` / `.bzl` / `MODULE.bazel` evaluation).
//! Mirrors `src/main/java/net/starlark/java` and `build/lib/packages`.

pub mod bigint;
pub mod eval;
pub mod semantics;
pub mod syntax;
pub mod types;

pub use bigint::BigInt;
pub use eval::format::{percent_format, str_format};
pub use eval::frame::CallFrame;
pub use eval::modules::json::{json_decode, json_encode, json_indent};
pub use eval::modules::proto::proto_encode_text;
pub use eval::thread::{register_source, source_line, StarlarkThread};
pub use semantics::StarlarkSemantics;
pub use syntax::ast::{
    AssignOp, AssignTarget, BinaryOpKind, CmpOp, CompClause, Expr, LoadStatement,
    ClosureEnv, ModuleEnv, ParamDef, Pattern, Scope, Stmt, UnaryOpKind, UserFunctionDef,
};
pub use syntax::checks::{check_dot_bazel_file_syntax, check_undefined_names, did_you_mean, SyntaxError};
pub use syntax::lexer::{Lexer, LexerError};
pub use syntax::location::Location;
pub use syntax::parser::Parser;
pub use syntax::resolver::Resolver;
pub use syntax::token::{Token, TokenKind};
pub use types::depset::{Depset, DepsetOrder};
pub use types::dict::StarlarkDict;
pub use types::list::{StarlarkError, StarlarkList};
pub use types::range::RangeList;
pub use types::select::{SelectorElement, SelectorList};
pub use types::set::StarlarkSet;
pub use types::struct_value::StructValue;
pub use types::value::{format_float, int_equals_float, Value, HOST_REPR};

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use zel_cmdline::Label;
use zel_vfs::{evaluate_glob, FileSystem, PathFragment};

/// Recorded rule call during `BUILD` file evaluation (e.g. `cc_binary(name = "foo", ...)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRuleCall {
    pub rule_class: String,
    pub kwargs: BTreeMap<String, Value>,
}

/// Host callback used by embedders (Bzlmod, repository rules) to provide native functions and
/// opaque objects. Invoked with a dispatch name (`host:<fn>` builtins have the `host:` prefix
/// stripped; Host objects use `attr:<kind>.<name>` / `method:<kind>.<name>` / `call:<kind>`),
/// positional args (the receiver is the first arg for attr/method/call) and keyword args.
pub type HostFn = Arc<
    dyn Fn(&mut EvalContext, &str, Vec<Value>, BTreeMap<String, Value>) -> Result<Value, StarlarkError>
        + Send
        + Sync,
>;

/// Callback resolving `load("<label>", ...)` into the exported symbols of the target module.
pub type LoadHook = Arc<dyn Fn(&str) -> Result<BTreeMap<String, Value>, String> + Send + Sync>;

/// A top-level `load()` statement as seen before evaluation: `(label, (line, column))`, the
/// position being that of the label string literal.
pub type PreloadItem = (String, (usize, usize));

/// Why [`PreloadHook`] failed: error lines to report (already `file:line:col: message`), then
/// the message of the exception that aborts loading the file.
#[derive(Debug, Clone, Default)]
pub struct PreloadFailure {
    pub events: Vec<String>,
    pub fatal: String,
}

/// Callback resolving every top-level `load()` of a file before it is resolved and evaluated
/// (Bazel loads all `.bzl` dependencies of a file up front).
/// On success it returns the canonical form of each label, in order.
pub type PreloadHook = Arc<dyn Fn(&[PreloadItem]) -> Result<Vec<String>, PreloadFailure> + Send + Sync>;

enum ControlFlow {
    None,
    Return(Value),
    Break,
    Continue,
}

/// Evaluation context for a Starlark module (`BUILD`, `.bzl`, or `MODULE.bazel`).
pub struct EvalContext {
    pub file_name: String,
    pub globals: BTreeMap<String, Value>,
    pub rule_calls: Vec<RecordedRuleCall>,
    pub loads: Vec<LoadStatement>,
    pub exports_files: Vec<String>,
    pub package_dir: PathFragment,
    pub fs: Option<Arc<dyn FileSystem>>,
    pub semantics: StarlarkSemantics,
    pub thread: StarlarkThread,
    pub host: Option<HostFn>,
    /// Location of the call currently being dispatched to the host (`file:line:col`).
    pub call_location: String,
    /// Location of the `=` of the assignment statement being executed (Bazel reports argument-binding
    /// errors of calls inside an assignment there).
    pub assign_stmt_location: Option<Location>,
    pub load_hook: Option<LoadHook>,
    /// Resolves all top-level loads before name resolution (see [`PreloadHook`]).
    pub preload_hook: Option<PreloadHook>,
    /// Set by [`EvalContext::eval_file_checked`] when the preload hook failed: the message of
    /// the fatal exception (the returned errors are the events to print before it).
    pub fatal_error: Option<String>,
    /// `(load label, symbol name)` → `(line, column)` of the symbol in the file being evaluated
    /// (set by [`EvalContext::eval_file_checked`]); used to locate missing-symbol errors.
    pub load_symbol_positions: BTreeMap<(String, String), (usize, usize)>,
    /// `true` if the last [`EvalContext::eval_file_checked`] failed before executing any
    /// statement (Bazel: "compilation" failed, as opposed to "initialization").
    pub static_failure: bool,
    /// Skip the Bazel scanner/parser port (which, like Bazel without
    /// `--experimental_starlark_type_syntax`, rejects type annotations): set for `@_builtins`,
    /// which Bazel always parses with type syntax enabled.
    pub allow_type_syntax: bool,
    /// Canonical label of each `load()` label string (filled by the preload hook).
    pub canonical_loads: BTreeMap<String, String>,
    /// Globals of this module as seen by functions defined here but called from other modules.
    /// Populated by [`EvalContext::publish_module`] once evaluation of the module finishes.
    pub module_env: Arc<ModuleEnv>,
    /// When set, referencing an undefined identifier is an error (`name 'x' is not defined`)
    /// instead of silently evaluating to a rule callable.
    pub strict_names: bool,
    /// Like [`EvalContext::strict_names`] for the static (resolver) check of
    /// [`EvalContext::eval_file_checked`] only: undefined identifiers are rejected before
    /// execution, but evaluation itself stays lenient. `static_universe` lists additional
    /// predeclared names (Bazel's `.bzl` builtins) that the resolver accepts.
    pub static_names: bool,
    /// Repository mapping (apparent -> canonical) of the main repository, used by `print` to show
    /// labels in their shorthand display form like Bazel's `Label.debugPrint`.
    pub main_repo_mapping: Option<Arc<BTreeMap<String, String>>>,
    pub static_universe: Vec<String>,
    /// Location of the most recent call dispatched to the host callback.
    pub last_call_loc: Location,
    /// Keyword argument names of the most recent call in source order (`**` entries last).
    pub call_kw_order: Vec<String>,
    /// A `**` argument repeated a keyword (`name`): reported when the call is bound.
    pub pending_dup_kw: Option<String>,
    /// When `Some`, `print()` output is captured here instead of written to stderr.
    pub captured_prints: Option<Vec<(Location, String)>>,
    /// Custom handler for `print()` output.
    pub print_handler: Option<Arc<dyn Fn(&Location, &str) + Send + Sync>>,
    /// Prefix errors that carry no `file:line:col:` position with the failing statement's.
    pub locate_errors: bool,
    /// `(function, message)` of the most recent error raised by a universal builtin such as `hash`
    /// (lets embedders print `Error in hash: ...`).
    pub builtin_error: Option<(String, String)>,
    /// Location the current frame is "at": the `(` of the call being evaluated, left stale by nested calls in its
    /// arguments (Starlark's `Frame.setLocation`).
    pub cur_call_loc: Location,
    /// `cur_call_loc` as it stood right after the arguments of the most recent call were evaluated; argument
    /// binding errors raised while processing keyword arguments are reported there.
    pub arg_eval_loc: Location,
    /// Location of the `.` of the attribute selector of the method call being dispatched.
    pub attr_loc: Location,
    /// `cur_call_loc` as it was before the most recent call expression started evaluating.
    pub pre_call_loc: Location,
}

impl Default for EvalContext {
    fn default() -> Self {
        Self::new(PathFragment::empty(), None)
    }
}

impl EvalContext {
    pub fn new(package_dir: PathFragment, fs: Option<Arc<dyn FileSystem>>) -> Self {
        let semantics = StarlarkSemantics::default();
        let thread = StarlarkThread::new(semantics.clone());
        let mut ctx = Self {
            file_name: "BUILD".to_string(),
            globals: BTreeMap::new(),
            rule_calls: Vec::new(),
            loads: Vec::new(),
            exports_files: Vec::new(),
            package_dir,
            fs,
            semantics,
            thread,
            host: None,
            call_location: String::new(),
            assign_stmt_location: None,
            load_hook: None,
            preload_hook: None,
            fatal_error: None,
            load_symbol_positions: BTreeMap::new(),
            static_failure: false,
            allow_type_syntax: false,
            canonical_loads: BTreeMap::new(),
            module_env: Arc::new(ModuleEnv::default()),
            strict_names: false,
            static_names: false,
            main_repo_mapping: None,
            static_universe: Vec::new(),
            last_call_loc: Location::default(),
            call_kw_order: Vec::new(),
            pending_dup_kw: None,
            captured_prints: None,
            print_handler: None,
            locate_errors: std::env::var("ZEL_LOCATE_ERRORS").is_ok(),
            builtin_error: None,
            cur_call_loc: Location::default(),
            arg_eval_loc: Location::default(),
            attr_loc: Location::default(),
            pre_call_loc: Location::default(),
        };
        ctx.init_builtins();
        ctx
    }

    fn init_builtins(&mut self) {
        // Builtin modules
        let mut json_fields = BTreeMap::new();
        json_fields.insert("encode".to_string(), Value::BuiltinFunction("json.encode".to_string()));
        json_fields.insert("decode".to_string(), Value::BuiltinFunction("json.decode".to_string()));
        json_fields.insert("indent".to_string(), Value::BuiltinFunction("json.indent".to_string()));
        json_fields.insert(
            "encode_indent".to_string(),
            Value::BuiltinFunction("json.encode_indent".to_string()),
        );
        self.globals.insert(
            "json".to_string(),
            Value::Struct(Arc::new(StructValue::new(json_fields))),
        );

        let mut proto_fields = BTreeMap::new();
        proto_fields.insert(
            "encode_text".to_string(),
            Value::BuiltinFunction("proto.encode_text".to_string()),
        );
        self.globals.insert(
            "proto".to_string(),
            Value::Struct(Arc::new(StructValue::new(proto_fields))),
        );
    }

    pub fn eval_str(&mut self, source: &str) -> Result<(), StarlarkError> {
        let tokens = Lexer::new(&self.file_name, source)
            .lex_all()
            .map_err(|e| StarlarkError(e.to_string()))?;
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse_module()?;

        if self.strict_names {
            let predeclared: Vec<String> = self.globals.keys().cloned().collect();
            let errs = syntax::checks::check_undefined_names(&stmts, &predeclared);
            if let Some(first) = errs.into_iter().next() {
                return Err(StarlarkError(first.message));
            }
        }

        let mut resolver = Resolver::new();
        resolver.allow_late_loads = !self.file_name.ends_with(".bzl");
        resolver.resolve_module(&stmts)?;

        for stmt in &stmts {
            match self.exec_stmt(stmt)? {
                ControlFlow::Return(_v) => return Ok(()),
                ControlFlow::Break | ControlFlow::Continue => {
                    return Err(StarlarkError("flow statement outside loop".to_string()));
                }
                ControlFlow::None => {}
            }
        }
        self.publish_module();
        Ok(())
    }

    /// Parses and executes `source` like [`EvalContext::eval_str`], but first applies Bazel's
    /// static checks. `dot_bazel` is `Some((where, can_load_bzl))` for files subject to
    /// `DotBazelFileSyntaxChecker` (e.g. `("BUILD files", true)`). When `self.strict_names`
    /// is set, undefined identifiers are reported statically (like Bazel's resolver).
    ///
    /// All static errors are returned (each already prefixed with `file:line:col:`); execution
    /// only starts if there are none.
    pub fn eval_file_checked(
        &mut self,
        source: &str,
        dot_bazel: Option<(&str, bool)>,
    ) -> Result<(), Vec<String>> {
        // Everything up to the evaluation of the first statement is static ("compilation").
        self.static_failure = true;
        self.load_symbol_positions.clear();
        // Bazel's scanner/parser errors (recovering, capped at five parser errors) come first.
        let parsed = if self.allow_type_syntax {
            syntax::bazel_syntax::ParseOutput::empty()
        } else {
            syntax::bazel_syntax::parse(&self.file_name, source)
        };
        for l in &parsed.loads {
            for (name, line, col) in &l.symbols {
                self.load_symbol_positions
                    .entry((l.label.clone(), name.clone()))
                    .or_insert((*line, *col));
            }
        }
        if !parsed.errors.is_empty() {
            let mut errs: Vec<String> = parsed.errors.iter().map(|e| e.to_string()).collect();
            // `Program.compileFile` runs the resolver on the recovered tree of a `.bzl` file: its errors
            // (including "contains syntax errors" for every error expression) follow the parse errors.
            if self.static_names && self.file_name.ends_with(".bzl") {
                errs.extend(parsed.resolver_errors.iter().map(|e| e.error.to_string()));
            }
            return Err(errs);
        }
        // `DotBazelFileSyntaxChecker` runs on the parsed file; its errors end loading before the
        // resolver (name resolution, argument order, ...) ever runs. `MODULE.bazel` has extra
        // `include` rules that only the AST-based checker knows.
        let module_file = matches!(dot_bazel, Some(("MODULE.bazel files", _)));
        if let (Some((where_, can_load)), false) = (dot_bazel, module_file) {
            let errors: Vec<String> = parsed
                .dot_bazel_errors(&self.file_name, where_, can_load)
                .iter()
                .map(|e| e.to_string())
                .collect();
            if !errors.is_empty() {
                return Err(errors);
            }
        }
        let resolver_errors_only = || -> Vec<String> {
            parsed.resolver_errors.iter().map(|e| e.error.to_string()).collect()
        };
        let tokens = match Lexer::new(&self.file_name, source).lex_all() {
            Ok(t) => t,
            Err(e) => {
                let r = resolver_errors_only();
                return Err(if r.is_empty() { vec![e.to_string()] } else { r });
            }
        };
        let mut parser = Parser::new(tokens);
        let stmts = match parser.parse_module() {
            Ok(s) => s,
            Err(e) => {
                // Syntax the evaluator's own parser rejects but Bazel's parser accepts (e.g.
                // invalid assignment targets) is a resolver error in Bazel.
                let r = resolver_errors_only();
                return Err(if r.is_empty() { vec![e.0] } else { r });
            }
        };
        if let (Some((where_, can_load)), true) = (dot_bazel, module_file) {
            let errors: Vec<String> = check_dot_bazel_file_syntax(&stmts, where_, can_load, true)
                .iter()
                .map(|e| e.to_string())
                .collect();
            if !errors.is_empty() {
                return Err(errors);
            }
        }
        // Resolver errors, in traversal order: structural ones (from the Bazel parser port) are
        // interleaved with undefined names by the source position at which the resolver reports.
        let mut undefined: Vec<((usize, usize), String)> = Vec::new();
        if self.static_names && self.file_name.ends_with(".bzl") {
            // Bazel's resolver forbids top-level `if` and `for` statements in `.bzl` files.
            for st in &stmts {
                let msg = match st {
                    Stmt::If { .. } => "if statements are not allowed at the top level. You may move it inside a function or use an if expression (x if condition else y).",
                    Stmt::For { .. } => "for loops are not allowed at the top level. You may move it inside a function or use a comprehension, [f(x) for x in sequence]",
                    _ => continue,
                };
                let loc = st.location();
                undefined.push(((loc.line, loc.column), format!("{loc}: {msg}")));
            }
        }
        if self.strict_names || self.static_names {
            let mut predeclared: Vec<String> = self.globals.keys().cloned().collect();
            predeclared.extend(self.static_universe.iter().cloned());
            for e in check_undefined_names(&stmts, &predeclared) {
                undefined.push(((e.location.line, e.location.column), e.to_string()));
            }
        }
        undefined.sort_by_key(|(k, _)| *k);
        let mut errors: Vec<String> = Vec::new();
        let mut next_undefined = 0;
        for e in &parsed.resolver_errors {
            while next_undefined < undefined.len() && undefined[next_undefined].0 < e.key {
                errors.push(undefined[next_undefined].1.clone());
                next_undefined += 1;
            }
            errors.push(e.error.to_string());
        }
        errors.extend(undefined[next_undefined..].iter().map(|(_, m)| m.clone()));
        if !errors.is_empty() {
            return Err(errors);
        }
        // Bazel resolves names first, then loads every `.bzl` dependency up front.
        if let Some(hook) = self.preload_hook.clone() {
            if !parsed.loads.is_empty() {
                let items: Vec<PreloadItem> =
                    parsed.loads.iter().map(|l| (l.label.clone(), (l.line, l.column))).collect();
                match hook(&items) {
                    Ok(canonical) => {
                        for ((raw, _), c) in items.iter().zip(canonical) {
                            self.canonical_loads.insert(raw.clone(), c);
                        }
                    }
                    Err(f) => {
                        self.fatal_error = Some(f.fatal);
                        return Err(f.events);
                    }
                }
            }
        }
        let mut resolver = Resolver::new();
        resolver.allow_late_loads = !self.file_name.ends_with(".bzl");
        resolver.resolve_module(&stmts).map_err(|e| vec![e.0])?;
        self.static_failure = false;
        for stmt in &stmts {
            match self.exec_stmt(stmt).map_err(|e| vec![e.0])? {
                ControlFlow::Return(_) => break,
                ControlFlow::Break | ControlFlow::Continue => {
                    return Err(vec!["flow statement outside loop".to_string()]);
                }
                ControlFlow::None => {}
            }
        }
        self.publish_module();
        Ok(())
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> Result<ControlFlow, StarlarkError> {
        let r = self.exec_stmt_inner(stmt);
        if self.locate_errors {
            if let Err(e) = &r {
                if !has_location_prefix(&e.0) {
                    let loc = stmt.location();
                    return Err(StarlarkError(format!("{loc}: {}", e.0)));
                }
            }
        }
        r
    }

    fn exec_stmt_inner(&mut self, stmt: &Stmt) -> Result<ControlFlow, StarlarkError> {
        match stmt {
            Stmt::Pass(_) => Ok(ControlFlow::None),
            Stmt::Break(_) => Ok(ControlFlow::Break),
            Stmt::Continue(_) => Ok(ControlFlow::Continue),
            Stmt::Return(opt_expr, _) => {
                let val = match opt_expr {
                    Some(e) => self.eval_expr(e)?,
                    None => Value::None,
                };
                Ok(ControlFlow::Return(val))
            }
            Stmt::Load(load, loc) => {
                self.loads.push(load.clone());
                if let Some(hook) = self.load_hook.clone() {
                    let exports = hook(&load.module_label).map_err(|e| StarlarkError(format!("{loc}: {e}")))?;
                    for (local_name, exported_name) in &load.symbols {
                        let v = exports.get(exported_name).ok_or_else(|| {
                            let shown = self
                                .canonical_loads
                                .get(&load.module_label)
                                .unwrap_or(&load.module_label);
                            let what = format!(
                                "file '{shown}' does not contain symbol '{exported_name}'"
                            );
                            match self
                                .load_symbol_positions
                                .get(&(load.module_label.clone(), exported_name.clone()))
                            {
                                // Bazel reports it as an evaluation error at the symbol.
                                Some((l, c)) => {
                                    let f = &self.file_name;
                                    let mut m = format!(
                                        "{f}:{l}:{c}: Traceback (most recent call last):\n\tFile \"{f}\", line {l}, column {c}, in <toplevel>\n"
                                    );
                                    if let Some(src) = source_line(f, *l) {
                                        m.push_str(&format!("\t\t{src}\n"));
                                    }
                                    m.push_str(&format!("Error: {what}"));
                                    StarlarkError(m)
                                }
                                None => StarlarkError(format!("{loc}: {what}")),
                            }
                        })?;
                        self.globals.insert(local_name.clone(), v.clone());
                    }
                    // Helper functions referenced by loaded user functions resolve against their
                    // defining module (see `ModuleEnv`), so nothing else leaks into this module.
                    return Ok(ControlFlow::None);
                }
                for (local_name, exported_name) in &load.symbols {
                    if !self.globals.contains_key(local_name) {
                        self.globals.insert(
                            local_name.clone(),
                            Value::RuleCallable {
                                rule_class: exported_name.clone(),
                                executable: exported_name.ends_with("_binary"),
                                test: exported_name.ends_with("_test"),
                            },
                        );
                    }
                }
                Ok(ControlFlow::None)
            }
            Stmt::Assign {
                target,
                op,
                value,
                location,
                op_location,
            } => {
                let saved_assign_loc = self.assign_stmt_location.replace(op_location.clone());
                let val = self.eval_expr(value);
                self.assign_stmt_location = saved_assign_loc;
                let val = val?;
                match target {
                    AssignTarget::Ident(name) => {
                        let final_val = if *op == AssignOp::Assign {
                            val
                        } else {
                            let prev = self.lookup_var(name).ok_or_else(|| {
                                StarlarkError(format!("{location}: name '{name}' is not defined"))
                            })?;
                            self.apply_assign_op(*op, &prev, &val, location)?
                        };
                        self.set_var(name, final_val);
                    }
                    AssignTarget::Tuple(names) => {
                        if *op != AssignOp::Assign {
                            return Err(StarlarkError(format!(
                                "{location}: augmented assignment to tuple not allowed"
                            )));
                        }
                        self.bind_patterns(names, val, &location.to_string())?;
                    }
                    AssignTarget::Index {
                        target: index_target,
                        index: index_expr,
                    } => {
                        let target_val = self.eval_expr(index_target)?;
                        let key_val = self.eval_expr(index_expr)?;
                        match target_val {
                            Value::List(l) => {
                                let mut guard = l.lock().unwrap();
                                guard.check_mutable()?;
                                let len = guard.elements.len() as i64;
                                let idx = key_val.as_i64().ok_or_else(|| {
                                    StarlarkError(format!(
                                        "{location}: list index must be integer, not {}",
                                        key_val.type_name()
                                    ))
                                })?;
                                let actual = if idx < 0 { len + idx } else { idx };
                                if actual < 0 || actual >= len {
                                    return Err(StarlarkError(format!(
                                        "{location}: list index out of range"
                                    )));
                                }
                                if *op == AssignOp::Assign {
                                    guard.elements[actual as usize] = val;
                                } else {
                                    let prev = guard.elements[actual as usize].clone();
                                    guard.elements[actual as usize] =
                                        self.apply_assign_op(*op, &prev, &val, location)?;
                                }
                            }
                            Value::Dict(d) => {
                                let mut guard = d.lock().unwrap();
                                guard.check_mutable()?;
                                if *op == AssignOp::Assign {
                                    guard.insert(key_val, val)?;
                                } else {
                                    let prev = guard.get(&key_val).cloned().ok_or_else(|| {
                                        StarlarkError(format!(
                                            "{location}: key {} not found in dictionary",
                                            key_val.repr()
                                        ))
                                    })?;
                                    let next_val =
                                        self.apply_assign_op(*op, &prev, &val, location)?;
                                    guard.insert(key_val, next_val)?;
                                }
                            }
                            other => {
                                return Err(StarlarkError(format!(
                                    "{location}: type '{}' does not support item assignment",
                                    other.type_name()
                                )))
                            }
                        }
                    }
                    AssignTarget::Attr { .. } => {
                        return Err(StarlarkError(format!(
                            "{location}: cannot assign to attribute"
                        )));
                    }
                }
                Ok(ControlFlow::None)
            }
            Stmt::Expr(e) => {
                let _ = self.eval_expr(e)?;
                Ok(ControlFlow::None)
            }
            Stmt::Def(func) => {
                // Evaluate parameter defaults at definition time and freeze them
                let mut evaluated_params = Vec::new();
                for p in &func.params {
                    let default_val = if let Some(ref def_expr) = p.default {
                        let v = self.eval_expr(def_expr)?;
                        v.freeze();
                        Some(Expr::Literal(v))
                    } else {
                        None
                    };
                    evaluated_params.push(ParamDef {
                        name: p.name.clone(),
                        default: default_val,
                        is_args: p.is_args,
                        is_kwargs: p.is_kwargs,
                        is_kw_only: p.is_kw_only,
                    });
                }

                let new_def = Arc::new(UserFunctionDef {
                    name: func.name.clone(),
                    params: evaluated_params,
                    body: func.body.clone(),
                    location: func.location.clone(),
                    module: Some(self.current_module_env()),
                    closure: self.capture_closure(),
                });

                self.set_var(&func.name, Value::UserFunction(new_def));
                Ok(ControlFlow::None)
            }
            Stmt::If {
                branches,
                else_body,
                ..
            } => {
                for (cond, body) in branches {
                    if self.eval_expr(cond)?.is_truthy() {
                        for s in body {
                            match self.exec_stmt(s)? {
                                ControlFlow::None => {}
                                other => return Ok(other),
                            }
                        }
                        return Ok(ControlFlow::None);
                    }
                }
                for s in else_body {
                    match self.exec_stmt(s)? {
                        ControlFlow::None => {}
                        other => return Ok(other),
                    }
                }
                Ok(ControlFlow::None)
            }
            Stmt::For {
                var_names,
                iter,
                body,
                ..
            } => {
                let iter_val = self.eval_expr(iter)?;
                // Depset direct iteration is forbidden in Bazel Starlark!
                if matches!(iter_val, Value::Depset(_)) {
                    return Err(StarlarkError("iteration over depset is forbidden".to_string()));
                }

                // Increment iteration count on iterable
                match &iter_val {
                    Value::List(l) => l.lock().unwrap().iter_count += 1,
                    Value::Dict(d) => d.lock().unwrap().iter_count += 1,
                    Value::Set(s) => s.lock().unwrap().iter_count += 1,
                    _ => {}
                }

                // Ranges iterate lazily (`range(2147483647)` with an early `break` is common).
                let elements: Box<dyn Iterator<Item = Value>> = if let Value::Range(r) = &iter_val {
                    let (start, step, len) = (r.start, r.step, r.len);
                    Box::new((0..len).map(move |i| Value::Int(BigInt::from_i64(start + i as i64 * step))))
                } else {
                    match eval::builtins::value_to_sequence(&iter_val) {
                        Ok(seq) => Box::new(seq.into_iter()),
                        Err(e) => {
                            match &iter_val {
                                Value::List(l) => l.lock().unwrap().iter_count -= 1,
                                Value::Dict(d) => d.lock().unwrap().iter_count -= 1,
                                Value::Set(s) => s.lock().unwrap().iter_count -= 1,
                                _ => {}
                            }
                            let _ = e;
                            return Err(StarlarkError(format!("{}: type '{}' is not iterable", iter.location(), iter_val.type_name())));
                        }
                    }
                };

                let mut flow = ControlFlow::None;
                let mut broke = false;
                for elem in elements {
                    self.bind_loop_vars(var_names, elem)?;
                    for s in body {
                        match self.exec_stmt(s)? {
                            ControlFlow::None => {}
                            ControlFlow::Continue => break,
                            ControlFlow::Break => {
                                flow = ControlFlow::None;
                                broke = true;
                                break;
                            }
                            ControlFlow::Return(v) => {
                                flow = ControlFlow::Return(v);
                                break;
                            }
                        }
                    }
                    if broke || matches!(flow, ControlFlow::Return(_)) {
                        break;
                    }
                }

                // Decrement iteration count
                match &iter_val {
                    Value::List(l) => l.lock().unwrap().iter_count -= 1,
                    Value::Dict(d) => d.lock().unwrap().iter_count -= 1,
                    Value::Set(s) => s.lock().unwrap().iter_count -= 1,
                    _ => {}
                }

                Ok(flow)
            }
        }
    }

    fn apply_assign_op(
        &self,
        op: AssignOp,
        prev: &Value,
        rhs: &Value,
        loc: &Location,
    ) -> Result<Value, StarlarkError> {
        match op {
            AssignOp::Assign => Ok(rhs.clone()),
            AssignOp::Add => prev.add(rhs),
            AssignOp::Sub => self.eval_binop(BinaryOpKind::Sub, prev, rhs, loc),
            AssignOp::Mul => self.eval_binop(BinaryOpKind::Mul, prev, rhs, loc),
            AssignOp::Div => self.eval_binop(BinaryOpKind::Div, prev, rhs, loc),
            AssignOp::FloorDiv => self.eval_binop(BinaryOpKind::FloorDiv, prev, rhs, loc),
            AssignOp::Mod => self.eval_binop(BinaryOpKind::Mod, prev, rhs, loc),
            AssignOp::BitAnd => self.eval_binop(BinaryOpKind::BitAnd, prev, rhs, loc),
            AssignOp::BitOr => prev.bitor(rhs),
            AssignOp::BitXor => self.eval_binop(BinaryOpKind::BitXor, prev, rhs, loc),
            AssignOp::Shl => self.eval_binop(BinaryOpKind::Shl, prev, rhs, loc),
            AssignOp::Shr => self.eval_binop(BinaryOpKind::Shr, prev, rhs, loc),
        }
    }

    /// Publishes this module's current globals so that functions defined in it (and called from
    /// other modules) can resolve their free variables. Call once evaluation has finished.
    pub fn publish_module(&self) {
        *self.module_env.globals.lock().unwrap() = self.globals.clone();
    }

    fn lookup_var(&self, name: &str) -> Option<Value> {
        if let Some(frame) = self.thread.current_frame() {
            if let Some(val) = frame.locals.lock().unwrap().get(name) {
                return Some(val.clone());
            }
            for p in &frame.parents {
                if let Some(val) = p.lock().unwrap().get(name) {
                    return Some(val.clone());
                }
            }
            if let Some(module) = &frame.module {
                if let Some(val) = module.globals.lock().unwrap().get(name) {
                    return Some(val.clone());
                }
                return None;
            }
        }
        self.globals.get(name).cloned()
    }

    /// The package and repository mapping of the `.bzl` whose function is running (or being
    /// evaluated), if it came from a `.bzl` file.
    pub fn current_label_ctx(&self) -> Option<(zel_cmdline::PackageIdentifier, zel_cmdline::RepositoryMapping)> {
        self.current_module_env().label_ctx.lock().unwrap().clone()
    }

    /// `hasattr` / `getattr` on a host object (`repository_ctx`, `ctx`, ...): the host resolves
    /// the attribute; a failing lookup means it does not exist.
    fn probe_host_attr(
        &mut self,
        name: &str,
        pos: &[Value],
        kw: &BTreeMap<String, Value>,
    ) -> Option<Result<Value, StarlarkError>> {
        if name != "hasattr" && name != "getattr" {
            return None;
        }
        let Some(Value::Host(kind, id)) = pos.first() else { return None };
        let attr = pos.get(1).and_then(|v| v.as_str())?.to_string();
        let host = self.host.clone()?;
        let r = host(
            self,
            &format!("attr:{kind}.{attr}"),
            vec![Value::Host(kind.clone(), id.clone())],
            BTreeMap::new(),
        );
        let r = r.map(|v| match &v {
            Value::BuiltinFunction(n) if n.starts_with("host:method:") => {
                self.bound_method(Value::Host(kind.clone(), id.clone()), &attr, &Location::default())
            }
            _ => v,
        });
        Some(match (name, r) {
            ("hasattr", r) => Ok(Value::Bool(r.is_ok())),
            (_, Ok(v)) => Ok(v),
            (_, Err(e)) => match pos.get(2).cloned().or_else(|| kw.get("default").cloned()) {
                Some(d) => Ok(d),
                None => Err(e),
            },
        })
    }

    /// The module whose globals free variables currently resolve against: the defining module of
    /// the running function, else the module being evaluated.
    fn current_module_env(&self) -> Arc<ModuleEnv> {
        match self.thread.current_frame().and_then(|f| f.module.as_ref()) {
            Some(m) => Arc::clone(m),
            None => Arc::clone(&self.module_env),
        }
    }

    /// Scopes a function created right now would close over: the running function's locals and
    /// the scopes it itself captured. Empty at module level.
    fn capture_closure(&self) -> ClosureEnv {
        match self.thread.current_frame() {
            Some(f) => {
                let mut v = vec![f.locals.clone()];
                v.extend(f.parents.iter().cloned());
                ClosureEnv(v)
            }
            None => ClosureEnv::default(),
        }
    }

    fn set_var(&mut self, name: &str, val: Value) {
        if let Some(frame) = self.thread.current_frame_mut() {
            frame.locals.lock().unwrap().insert(name.to_string(), val);
        } else {
            self.globals.insert(name.to_string(), val);
        }
    }

    fn bind_loop_vars(&mut self, var_names: &[Pattern], elem: Value) -> Result<(), StarlarkError> {
        if var_names.len() == 1 {
            if let Pattern::Name(n) = &var_names[0] {
                self.set_var(n, elem);
                return Ok(());
            }
        }
        self.bind_patterns(var_names, elem, "")
    }

    /// Unpacks `val` (a sequence) into `pats`, recursing into nested tuple patterns.
    fn bind_patterns(&mut self, pats: &[Pattern], val: Value, loc: &str) -> Result<(), StarlarkError> {
        let items = eval::builtins::value_to_sequence(&val)?;
        if items.len() != pats.len() {
            let prefix = if loc.is_empty() { String::new() } else { format!("{loc}: ") };
            return Err(StarlarkError(format!(
                "{prefix}cannot unpack {} values into {} variables",
                items.len(),
                pats.len()
            )));
        }
        for (p, item) in pats.iter().zip(items) {
            match p {
                Pattern::Name(n) => self.set_var(n, item),
                Pattern::Tuple(inner) => self.bind_patterns(inner, item, loc)?,
            }
        }
        Ok(())
    }

    pub fn eval_expr(&mut self, expr: &Expr) -> Result<Value, StarlarkError> {
        match expr {
            Expr::Literal(v) => Ok(v.clone()),
            Expr::Ident(name, _loc) => {
                if let Some(v) = self.lookup_var(name) {
                    return Ok(v);
                }
                // Universe functions are first-class values (`f = fail`, `key = len`).
                if syntax::checks::UNIVERSE.contains(&name.as_str()) && !matches!(name.as_str(), "None" | "True" | "False") {
                    return Ok(Value::BuiltinFunction(name.clone()));
                }
                // Universal / Builtin functions
                if let Some(v) = eval::builtins::eval_universal_function(name, &[], &BTreeMap::new())? {
                    return Ok(v);
                }
                if self.strict_names {
                    return Err(StarlarkError(format!(
                        "{_loc}: name '{name}' is not defined"
                    )));
                }
                // Legacy permissive mode (MODULE.bazel / embedders): unknown identifiers evaluate
                // to rule callables.
                Ok(Value::RuleCallable {
                    rule_class: name.clone(),
                    executable: name.ends_with("_binary"),
                    test: name.ends_with("_test"),
                })
            }
            Expr::List(items, _) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.eval_expr(item)?);
                }
                Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(out)))))
            }
            Expr::Tuple(items, _) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.eval_expr(item)?);
                }
                Ok(Value::Tuple(out))
            }
            Expr::Dict(pairs, loc, colons) => {
                let mut dict = StarlarkDict::new();
                for (i, (k_expr, v_expr)) in pairs.iter().enumerate() {
                    let k = self.eval_expr(k_expr)?;
                    let v = self.eval_expr(v_expr)?;
                    // `DictExpression`: insertion errors are reported at the entry's colon.
                    let at = colons.get(i).unwrap_or(loc);
                    if dict.contains_key(&k) {
                        return Err(with_loc(
                            StarlarkError(format!("dictionary expression has duplicate key: {}", k.repr())),
                            at,
                        ));
                    }
                    dict.insert(k, v).map_err(|e| with_loc(e, at))?;
                }
                Ok(Value::Dict(Arc::new(Mutex::new(dict))))
            }
            Expr::ListComp { body, clauses, .. } => {
                let mut out = Vec::new();
                self.eval_list_comp_clauses(clauses, 0, body, &mut out)?;
                Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(out)))))
            }
            Expr::DictComp {
                key,
                value,
                clauses,
                ..
            } => {
                let mut dict = StarlarkDict::new();
                self.eval_dict_comp_clauses(clauses, 0, key, value, &mut dict)?;
                Ok(Value::Dict(Arc::new(Mutex::new(dict))))
            }
            Expr::UnaryOp {
                op,
                expr,
                location: loc,
            } => {
                let val = self.eval_expr(expr)?;
                match op {
                    UnaryOpKind::Not => Ok(Value::Bool(!val.is_truthy())),
                    UnaryOpKind::Neg => match val {
                        Value::Int(i) => Ok(Value::Int(i.neg())),
                        Value::Float(f) => Ok(Value::Float(-f)),
                        other => Err(StarlarkError(format!(
                            "{loc}: unsupported unary -: {}",
                            other.type_name()
                        ))),
                    },
                    UnaryOpKind::Pos => match val {
                        Value::Int(i) => Ok(Value::Int(i)),
                        Value::Float(f) => Ok(Value::Float(f)),
                        other => Err(StarlarkError(format!(
                            "{loc}: unsupported unary +: {}",
                            other.type_name()
                        ))),
                    },
                    UnaryOpKind::Invert => match val {
                        Value::Int(i) => Ok(Value::Int(i.bit_not())),
                        other => Err(StarlarkError(format!(
                            "{loc}: unsupported unary operation: ~{}",
                            other.type_name()
                        ))),
                    },
                }
            }
            Expr::BinaryOp {
                op,
                lhs,
                rhs,
                location: loc,
            } => {
                if *op == BinaryOpKind::And {
                    let l = self.eval_expr(lhs)?;
                    if !l.is_truthy() {
                        return Ok(l);
                    }
                    return self.eval_expr(rhs);
                }
                if *op == BinaryOpKind::Or {
                    let l = self.eval_expr(lhs)?;
                    if l.is_truthy() {
                        return Ok(l);
                    }
                    return self.eval_expr(rhs);
                }
                let l = self.eval_expr(lhs)?;
                let r = self.eval_expr(rhs)?;
                self.eval_binop(*op, &l, &r, loc)
            }
            Expr::Compare {
                lhs,
                comparisons,
                location: cmp_loc,
            } => {
                let mut cur = self.eval_expr(lhs)?;
                for (op, rhs_expr) in comparisons {
                    let rhs_val = self.eval_expr(rhs_expr)?;
                    let matches = match op {
                        CmpOp::Eq => cur == rhs_val,
                        CmpOp::Ne => cur != rhs_val,
                        CmpOp::Lt => eval::builtins::compare_values(&cur, &rhs_val).map_err(|e| with_loc(e, cmp_loc))? == Ordering::Less,
                        CmpOp::Le => {
                            let ord = eval::builtins::compare_values(&cur, &rhs_val).map_err(|e| with_loc(e, cmp_loc))?;
                            ord == Ordering::Less || ord == Ordering::Equal
                        }
                        CmpOp::Gt => {
                            eval::builtins::compare_values(&cur, &rhs_val).map_err(|e| with_loc(e, cmp_loc))? == Ordering::Greater
                        }
                        CmpOp::Ge => {
                            let ord = eval::builtins::compare_values(&cur, &rhs_val).map_err(|e| with_loc(e, cmp_loc))?;
                            ord == Ordering::Greater || ord == Ordering::Equal
                        }
                        CmpOp::In => self.check_contains(&rhs_val, &cur)?,
                        CmpOp::NotIn => !self.check_contains(&rhs_val, &cur)?,
                    };
                    if !matches {
                        return Ok(Value::Bool(false));
                    }
                    cur = rhs_val;
                }
                Ok(Value::Bool(true))
            }
            Expr::IfExpr {
                test, body, orelse, ..
            } => {
                if self.eval_expr(test)?.is_truthy() {
                    self.eval_expr(body)
                } else {
                    self.eval_expr(orelse)
                }
            }
            Expr::Call {
                callee,
                pos_args,
                kw_args,
                var_args,
                kw_var_args,
                location: loc,
            } => self.eval_call(callee, pos_args, kw_args, var_args, kw_var_args, loc),
            Expr::Attr {
                target,
                attr,
                location: loc,
            } => {
                let recv = self.eval_expr(target)?;
                match recv {
                    Value::Struct(s) => s.get(attr).map_err(|e| StarlarkError(format!("{loc}: {}", e.0))),
                    Value::Host(kind, id) => {
                        let host = self.host.clone().ok_or_else(|| {
                            StarlarkError(format!("{loc}: no host to resolve '{attr}'"))
                        })?;
                        self.call_location = loc.to_string();

                        let r = host(
                            self,
                            &format!("attr:{kind}.{attr}"),
                            vec![Value::Host(kind.clone(), id.clone())],
                            BTreeMap::new(),
                        )
                        .map_err(|e| StarlarkError(format!("{loc}: {}", e.0)))?;
                        // A method read without calling it (`f = ctx.path`) stays bound to its receiver.
                        Ok(match &r {
                            Value::BuiltinFunction(n) if n.starts_with("host:method:") => {
                                self.bound_method(Value::Host(kind, id), attr, loc)
                            }
                            _ => r,
                        })
                    }
                    Value::ProviderInstance { fields, .. } => fields
                        .get(attr)
                        .cloned()
                        .ok_or_else(|| StarlarkError(format!("{loc}: field '{attr}' not found"))),
                    Value::Label(l) => match attr.as_str() {
                        "name" => Ok(Value::String(l.name().to_string())),
                        "package" => Ok(Value::String(l.package_name().to_string())),
                        "repo_name" | "workspace_name" => {
                            Ok(Value::String(l.repository().name().to_string()))
                        }
                        "workspace_root" => Ok(Value::String(if l.repository().is_main() {
                            String::new()
                        } else {
                            format!("external/{}", l.repository().name())
                        })),
                        "same_package_label" | "relative" | "package_relative" => Ok(self.bound_method(Value::Label(l.clone()), attr, loc)),
                        _ => Err(StarlarkError(format!("{loc}: Label has no attribute '{attr}'"))),
                    },
                    Value::Dict(d) => {
                        let found = d.lock().unwrap().get(&Value::String(attr.clone())).cloned();
                        match found {
                            Some(v) => Ok(v),
                            None if matches!(
                                attr.as_str(),
                                "get" | "items" | "keys" | "values" | "pop" | "popitem" | "setdefault" | "update" | "clear"
                            ) =>
                            {
                                Ok(self.bound_method(Value::Dict(d.clone()), attr, loc))
                            }
                            None => Err(StarlarkError(format!("{loc}: 'dict' value has no field or method '{attr}'"))),
                        }
                    }
                    recv @ (Value::List(_) | Value::String(_) | Value::Set(_) | Value::Tuple(_)) => {
                        Ok(self.bound_method(recv, attr, loc))
                    }
                    other => Err(StarlarkError(format!(
                        "{loc}: '{}' value has no field or method '{attr}'",
                        other.type_name()
                    ))),
                }
            }
            Expr::Index {
                target,
                index,
                location: loc,
            } => {
                let recv = self.eval_expr(target)?;
                let idx_val = self.eval_expr(index)?;
                match (recv, idx_val) {
                    (Value::List(l), Value::Int(i)) => {
                        let guard = l.lock().unwrap();
                        let len = guard.elements.len() as i64;
                        let actual = if let Some(n) = i.to_i64() {
                            if n < 0 {
                                len + n
                            } else {
                                n
                            }
                        } else {
                            return Err(StarlarkError(format!("{loc}: index out of range")));
                        };
                        if actual >= 0 && (actual as usize) < guard.elements.len() {
                            Ok(guard.elements[actual as usize].clone())
                        } else {
                            Err(StarlarkError(format!("{loc}: index out of range (index is {}, but sequence has {} elements)", i, len)))
                        }
                    }
                    (Value::Tuple(t), Value::Int(i)) => {
                        let len = t.len() as i64;
                        let actual = if let Some(n) = i.to_i64() {
                            if n < 0 {
                                len + n
                            } else {
                                n
                            }
                        } else {
                            return Err(StarlarkError(format!("{loc}: index out of range")));
                        };
                        if actual >= 0 && (actual as usize) < t.len() {
                            Ok(t[actual as usize].clone())
                        } else {
                            Err(StarlarkError(format!("{loc}: index out of range (index is {}, but sequence has {} elements)", i, len)))
                        }
                    }
                    (Value::String(s), Value::Int(i)) => {
                        let chars: Vec<char> = s.chars().collect();
                        let len = chars.len() as i64;
                        let actual = if let Some(n) = i.to_i64() {
                            if n < 0 {
                                len + n
                            } else {
                                n
                            }
                        } else {
                            return Err(StarlarkError(format!("{loc}: index out of range")));
                        };
                        if actual >= 0 && (actual as usize) < chars.len() {
                            Ok(Value::String(chars[actual as usize].to_string()))
                        } else {
                            Err(StarlarkError(format!("{loc}: index out of range (index is {}, but sequence has {} elements)", i, len)))
                        }
                    }
                    (Value::Range(r), Value::Int(i)) => {
                        if let Some(n) = i.to_i64() {
                            if let Some(val) = r.get(n) {
                                Ok(Value::Int(BigInt::from_i64(val)))
                            } else {
                                Err(StarlarkError(format!("{loc}: index out of range")))
                            }
                        } else {
                            Err(StarlarkError(format!("{loc}: index out of range")))
                        }
                    }
                    (Value::Host(kind, id), key) => {
                        let host = self.host.clone().ok_or_else(|| {
                            StarlarkError(format!("{loc}: '{kind}' object is not indexable"))
                        })?;
                        self.call_location = loc.to_string();
                        self.last_call_loc = loc.clone();
                        host(
                            self,
                            &format!("index:{kind}"),
                            vec![Value::Host(kind, id), key],
                            BTreeMap::new(),
                        )
                        .map_err(|e| StarlarkError(format!("{loc}: {}", e.0)))
                    }
                    (Value::Dict(d), key) => d
                        .lock()
                        .unwrap()
                        .get(&key)
                        .cloned()
                        .ok_or_else(|| StarlarkError(format!("{loc}: key {} not found in dictionary", key.repr()))),
                    // `target[OutputGroupInfo]["group"]`: provider instances index by field name.
                    (Value::ProviderInstance { fields, provider_name }, Value::String(n)) => fields
                        .get(&n)
                        .cloned()
                        .ok_or_else(|| StarlarkError(format!("{loc}: key \"{n}\" not found in {provider_name}"))),
                    (other, idx) => Err(StarlarkError(format!(
                        "{loc}: type '{}' is not indexable with '{}'",
                        other.type_name(),
                        idx.type_name()
                    ))),
                }
            }
            Expr::Slice {
                target,
                start,
                end,
                step,
                location: loc,
            } => {
                let recv = self.eval_expr(target)?;
                let s_val = start.as_ref().map(|e| self.eval_expr(e)).transpose()?;
                let e_val = end.as_ref().map(|e| self.eval_expr(e)).transpose()?;
                let st_val = step.as_ref().map(|e| self.eval_expr(e)).transpose()?;

                let step_num = match st_val.as_ref().and_then(|v| v.as_i64()) {
                    Some(0) => return Err(StarlarkError(format!("{loc}: slice step cannot be zero"))),
                    Some(s) => s,
                    None => 1,
                };

                match recv {
                    Value::String(s) => {
                        let chars: Vec<char> = s.chars().collect();
                        let sliced = slice_sequence(&chars, s_val.as_ref(), e_val.as_ref(), step_num);
                        Ok(Value::String(sliced.into_iter().collect()))
                    }
                    Value::List(l) => {
                        let guard = l.lock().unwrap();
                        let sliced = slice_sequence(&guard.elements, s_val.as_ref(), e_val.as_ref(), step_num);
                        Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(sliced)))))
                    }
                    Value::Tuple(t) => {
                        let sliced = slice_sequence(&t, s_val.as_ref(), e_val.as_ref(), step_num);
                        Ok(Value::Tuple(sliced))
                    }
                    Value::Range(r) => {
                        let vec = r.to_vec();
                        let sliced = slice_sequence(&vec, s_val.as_ref(), e_val.as_ref(), step_num);
                        Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(
                            sliced.into_iter().map(BigInt::from_i64).map(Value::Int).collect(),
                        )))))
                    }
                    other => Err(StarlarkError(format!(
                        "{loc}: type '{}' is not sliceable",
                        other.type_name()
                    ))),
                }
            }
            Expr::Lambda {
                params,
                body,
                location: loc,
            } => {
                let user_def = Arc::new(UserFunctionDef {
                    name: "lambda".to_string(),
                    params: params.clone(),
                    body: vec![Stmt::Return(Some((**body).clone()), loc.clone())],
                    location: loc.clone(),
                    module: Some(self.current_module_env()),
                    closure: self.capture_closure(),
                });
                Ok(Value::UserFunction(user_def))
            }
        }
    }

    /// A first-class bound method (`f = d.get`): a function `(*args, **kwargs)` that calls
    /// `recv.<attr>(*args, **kwargs)` with `recv` held in its closure.
    fn bound_method(&self, recv: Value, attr: &str, loc: &Location) -> Value {
        let scope: Scope = Default::default();
        scope.lock().unwrap().insert("$recv".to_string(), recv);
        let call = Expr::Call {
            callee: Box::new(Expr::Attr {
                target: Box::new(Expr::Ident("$recv".to_string(), loc.clone())),
                attr: attr.to_string(),
                location: loc.clone(),
            }),
            pos_args: vec![],
            kw_args: vec![],
            var_args: Some(Box::new(Expr::Ident("$args".to_string(), loc.clone()))),
            kw_var_args: Some(Box::new(Expr::Ident("$kwargs".to_string(), loc.clone()))),
            location: loc.clone(),
        };
        let param = |name: &str, is_args: bool, is_kwargs: bool| ParamDef {
            name: name.to_string(),
            default: None,
            is_args,
            is_kwargs,
            is_kw_only: false,
        };
        Value::UserFunction(Arc::new(UserFunctionDef {
            name: attr.to_string(),
            params: vec![param("$args", true, false), param("$kwargs", false, true)],
            body: vec![Stmt::Return(Some(call), loc.clone())],
            location: loc.clone(),
            module: Some(self.current_module_env()),
            closure: ClosureEnv(vec![scope]),
        }))
    }

    fn eval_binop(
        &self,
        op: BinaryOpKind,
        l: &Value,
        r: &Value,
        loc: &Location,
    ) -> Result<Value, StarlarkError> {
        match op {
            BinaryOpKind::Add => l.add(r).map_err(|e| with_loc(e, loc)),
            BinaryOpKind::BitOr => l.bitor(r).map_err(|e| with_loc(e, loc)),
            BinaryOpKind::Sub => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.sub(b))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a.to_f64() - b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a - b.to_f64())),
                (Value::Set(a), Value::Set(b)) => {
                    let s1 = a.lock().unwrap();
                    let s2 = b.lock().unwrap();
                    Ok(Value::Set(Arc::new(Mutex::new(s1.difference(&s2)))))
                }
                _ => Err(StarlarkError(format!(
                    "{loc}: unsupported binary operation: {} - {}",
                    l.type_name(),
                    r.type_name()
                ))),
            },
            BinaryOpKind::Mul => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.mul(b))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a.to_f64() * b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a * b.to_f64())),
                (Value::String(s), Value::Int(n)) | (Value::Int(n), Value::String(s)) => {
                    let count = n.to_i64().unwrap_or(0).max(0) as usize;
                    Ok(Value::String(s.repeat(count)))
                }
                (Value::List(items), Value::Int(n)) | (Value::Int(n), Value::List(items)) => {
                    let count = n.to_i64().unwrap_or(0).max(0) as usize;
                    let guard = items.lock().unwrap();
                    let mut out = Vec::with_capacity(guard.elements.len() * count);
                    for _ in 0..count {
                        out.extend(guard.elements.clone());
                    }
                    Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(out)))))
                }
                (Value::Tuple(items), Value::Int(n)) | (Value::Int(n), Value::Tuple(items)) => {
                    let count = n.to_i64().unwrap_or(0).max(0) as usize;
                    let mut out = Vec::with_capacity(items.len() * count);
                    for _ in 0..count {
                        out.extend(items.clone());
                    }
                    Ok(Value::Tuple(out))
                }
                _ => Err(StarlarkError(format!(
                    "{loc}: unsupported binary operation: {} * {}",
                    l.type_name(),
                    r.type_name()
                ))),
            },
            BinaryOpKind::Div => match (l, r) {
                (Value::Int(a), Value::Int(b)) => {
                    let bf = b.to_f64();
                    if bf == 0.0 {
                        return Err(StarlarkError(format!("{loc}: division by zero")));
                    }
                    Ok(Value::Float(a.to_f64() / bf))
                }
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a.to_f64() / b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a / b.to_f64())),
                _ => Err(StarlarkError(format!(
                    "{loc}: unsupported binary operation: {} / {}",
                    l.type_name(),
                    r.type_name()
                ))),
            },
            BinaryOpKind::FloorDiv => match (l, r) {
                (Value::Int(a), Value::Int(b)) => {
                    let q = a.floored_div(b).map_err(|e| StarlarkError(format!("{loc}: {e}")))?;
                    Ok(Value::Int(q))
                }
                (Value::Float(a), Value::Float(b)) => {
                    if *b == 0.0 {
                        return Err(StarlarkError(format!("{loc}: integer division by zero")));
                    }
                    Ok(Value::Float((a / b).floor()))
                }
                (Value::Int(a), Value::Float(b)) => {
                    if *b == 0.0 {
                        return Err(StarlarkError(format!("{loc}: integer division by zero")));
                    }
                    Ok(Value::Float((a.to_f64() / b).floor()))
                }
                (Value::Float(a), Value::Int(b)) => {
                    if b.is_zero() {
                        return Err(StarlarkError(format!("{loc}: integer division by zero")));
                    }
                    Ok(Value::Float((a / b.to_f64()).floor()))
                }
                _ => Err(StarlarkError(format!(
                    "{loc}: unsupported binary operation: {} // {}",
                    l.type_name(),
                    r.type_name()
                ))),
            },
            BinaryOpKind::Mod => match (l, r) {
                (Value::Int(a), Value::Int(b)) => {
                    let m = a.floored_mod(b).map_err(|e| StarlarkError(format!("{loc}: {e}")))?;
                    Ok(Value::Int(m))
                }
                (Value::Float(a), Value::Float(b)) => {
                    if *b == 0.0 {
                        return Err(StarlarkError(format!("{loc}: integer modulo by zero")));
                    }
                    let m = a - (a / b).floor() * b;
                    Ok(Value::Float(m))
                }
                (Value::String(fmt_str), Value::Tuple(args)) => {
                    let res = percent_format(fmt_str, args, None)?;
                    Ok(Value::String(res))
                }
                (Value::String(fmt_str), Value::Dict(dict)) => {
                    let guard = dict.lock().unwrap();
                    let map: BTreeMap<String, Value> = guard
                        .entries
                        .iter()
                        .filter_map(|(k, v)| k.as_str().map(|ks| (ks.to_string(), v.clone())))
                        .collect();
                    let res = percent_format(fmt_str, &[], Some(&map))?;
                    Ok(Value::String(res))
                }
                (Value::String(fmt_str), single) => {
                    let res = percent_format(fmt_str, &[single.clone()], None)?;
                    Ok(Value::String(res))
                }
                _ => Err(StarlarkError(format!(
                    "{loc}: unsupported binary operation: {} % {}",
                    l.type_name(),
                    r.type_name()
                ))),
            },
            BinaryOpKind::BitAnd => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.bit_and(b))),
                (Value::Set(a), Value::Set(b)) => {
                    let s1 = a.lock().unwrap();
                    let s2 = b.lock().unwrap();
                    Ok(Value::Set(Arc::new(Mutex::new(s1.intersection(&s2)))))
                }
                _ => Err(StarlarkError(format!(
                    "{loc}: unsupported binary operation: {} & {}",
                    l.type_name(),
                    r.type_name()
                ))),
            },
            BinaryOpKind::BitXor => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.bit_xor(b))),
                (Value::Set(a), Value::Set(b)) => {
                    let s1 = a.lock().unwrap();
                    let s2 = b.lock().unwrap();
                    Ok(Value::Set(Arc::new(Mutex::new(s1.symmetric_difference(&s2)))))
                }
                _ => Err(StarlarkError(format!(
                    "{loc}: unsupported binary operation: {} ^ {}",
                    l.type_name(),
                    r.type_name()
                ))),
            },
            BinaryOpKind::Shl => match (l, r) {
                (Value::Int(a), Value::Int(b)) => {
                    let shift = b
                        .to_i64()
                        .ok_or_else(|| StarlarkError(format!("{loc}: shift count too large")))?;
                    let res = a.shl(shift).map_err(|e| StarlarkError(format!("{loc}: {e}")))?;
                    Ok(Value::Int(res))
                }
                _ => Err(StarlarkError(format!(
                    "{loc}: unsupported binary operation: {} << {}",
                    l.type_name(),
                    r.type_name()
                ))),
            },
            BinaryOpKind::Shr => match (l, r) {
                (Value::Int(a), Value::Int(b)) => {
                    let shift = b
                        .to_i64()
                        .ok_or_else(|| StarlarkError(format!("{loc}: shift count too large")))?;
                    let res = a.shr(shift).map_err(|e| StarlarkError(format!("{loc}: {e}")))?;
                    Ok(Value::Int(res))
                }
                _ => Err(StarlarkError(format!(
                    "{loc}: unsupported binary operation: {} >> {}",
                    l.type_name(),
                    r.type_name()
                ))),
            },
            BinaryOpKind::And | BinaryOpKind::Or => unreachable!(),
        }
    }

    fn check_contains(&mut self, container: &Value, item: &Value) -> Result<bool, StarlarkError> {
        match container {
            Value::List(l) => Ok(l.lock().unwrap().elements.contains(item)),
            Value::Tuple(t) => Ok(t.contains(item)),
            Value::Set(s) => Ok(s.lock().unwrap().contains(item)),
            Value::Dict(d) => Ok(d.lock().unwrap().contains_key(item)),
            // `"group" in target[OutputGroupInfo]`: membership of a field name.
            Value::ProviderInstance { fields, .. } => Ok(item.as_str().is_some_and(|n| fields.contains_key(n))),
            Value::Range(r) => {
                if let Some(i) = item.as_i64() {
                    if r.step > 0 && i >= r.start && i < r.stop && (i - r.start) % r.step == 0 {
                        return Ok(true);
                    }
                    if r.step < 0 && i <= r.start && i > r.stop && (r.start - i) % (-r.step) == 0 {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Value::Host(kind, id) => {
                let host = self.host.clone().ok_or_else(|| {
                    StarlarkError(format!("type '{kind}' is not iterable"))
                })?;
                let r = host(
                    self,
                    &format!("contains:{kind}"),
                    vec![Value::Host(kind.clone(), id.clone()), item.clone()],
                    BTreeMap::new(),
                )?;
                Ok(r.is_truthy())
            }
            Value::String(s) => match item {
                Value::String(sub) => Ok(s.contains(sub)),
                other => Err(StarlarkError(format!(
                    "'in <string>' requires string as left operand, not {}",
                    other.type_name()
                ))),
            },
            other => Err(StarlarkError(format!(
                "type '{}' is not iterable",
                other.type_name()
            ))),
        }
    }

    fn eval_list_comp_clauses(
        &mut self,
        clauses: &[CompClause],
        idx: usize,
        body: &Expr,
        out: &mut Vec<Value>,
    ) -> Result<(), StarlarkError> {
        if idx >= clauses.len() {
            out.push(self.eval_expr(body)?);
            return Ok(());
        }
        match &clauses[idx] {
            CompClause::For { var_names, iter } => {
                let iter_val = self.eval_expr(iter)?;
                let elements = eval::builtins::value_to_sequence(&iter_val)?;
                let saved_globals = self.globals.clone();
                // Comprehension variables are scoped to the comprehension: restore the
                // enclosing function's locals afterwards.
                let saved_locals = self.thread.current_frame().map(|f| f.locals.lock().unwrap().clone());
                let mut res = Ok(());
                for elem in elements {
                    if let Err(e) = self.bind_loop_vars(var_names, elem) {
                        res = Err(e);
                        break;
                    }
                    if let Err(e) = self.eval_list_comp_clauses(clauses, idx + 1, body, out) {
                        res = Err(e);
                        break;
                    }
                }
                self.globals = saved_globals;
                if let (Some(m), Some(f)) = (saved_locals, self.thread.current_frame()) {
                    *f.locals.lock().unwrap() = m;
                }
                res
            }
            CompClause::If(cond) => {
                if self.eval_expr(cond)?.is_truthy() {
                    self.eval_list_comp_clauses(clauses, idx + 1, body, out)?;
                }
                Ok(())
            }
        }
    }

    fn eval_dict_comp_clauses(
        &mut self,
        clauses: &[CompClause],
        idx: usize,
        key: &Expr,
        value: &Expr,
        dict: &mut StarlarkDict,
    ) -> Result<(), StarlarkError> {
        if idx >= clauses.len() {
            let k = self.eval_expr(key)?;
            let v = self.eval_expr(value)?;
            dict.insert(k, v)?;
            return Ok(());
        }
        match &clauses[idx] {
            CompClause::For { var_names, iter } => {
                let iter_val = self.eval_expr(iter)?;
                let elements = eval::builtins::value_to_sequence(&iter_val)?;
                let saved_globals = self.globals.clone();
                let saved_locals = self.thread.current_frame().map(|f| f.locals.lock().unwrap().clone());
                let mut res = Ok(());
                for elem in elements {
                    if let Err(e) = self.bind_loop_vars(var_names, elem) {
                        res = Err(e);
                        break;
                    }
                    if let Err(e) = self.eval_dict_comp_clauses(clauses, idx + 1, key, value, dict) {
                        res = Err(e);
                        break;
                    }
                }
                self.globals = saved_globals;
                if let (Some(m), Some(f)) = (saved_locals, self.thread.current_frame()) {
                    *f.locals.lock().unwrap() = m;
                }
                res
            }
            CompClause::If(cond) => {
                if self.eval_expr(cond)?.is_truthy() {
                    self.eval_dict_comp_clauses(clauses, idx + 1, key, value, dict)?;
                }
                Ok(())
            }
        }
    }

    fn eval_call(
        &mut self,
        callee: &Expr,
        pos_args: &[Expr],
        kw_args: &[(String, Expr)],
        var_args: &Option<Box<Expr>>,
        kw_var_args: &Option<Box<Expr>>,
        loc: &Location,
    ) -> Result<Value, StarlarkError> {
        // Method call on receiver
        if let Expr::Attr { target, attr, location: attr_loc } = callee {
            let recv = self.eval_expr(target)?;
            self.pre_call_loc = self.cur_call_loc.clone();
            self.cur_call_loc = loc.clone();
            let mut eval_pos = Vec::new();
            for a in pos_args {
                eval_pos.push(self.eval_expr(a)?);
            }
            if let Some(va) = var_args {
                let va_val = self.eval_expr(va)?;
                eval_pos.extend(star_args_seq(&va_val).map_err(|e| StarlarkError(format!("{}: {}", star_at(va, loc), e.0)))?);
            }
            let mut eval_kw = BTreeMap::new();
            for (k, v) in kw_args {
                eval_kw.insert(k.clone(), self.eval_expr(v)?);
            }
            if let Some(ka) = kw_var_args {
                let ka_val = self.eval_expr(ka)?;
                self.pending_dup_kw = merge_star_kwargs(&mut eval_kw, &ka_val).map_err(|e| StarlarkError(format!("{}: {}", star_at(ka, loc), e.0)))?;
            }
            self.call_kw_order = kw_call_order(kw_args, kw_var_args.as_deref().map(|_| &eval_kw));
            self.arg_eval_loc = self.cur_call_loc.clone();
            self.cur_call_loc = loc.clone();
            self.attr_loc = attr_loc.clone();
            return self.eval_method_call(recv, attr, &eval_pos, &eval_kw, loc);
        }

        self.pre_call_loc = self.cur_call_loc.clone();
        self.cur_call_loc = loc.clone();
        let mut eval_pos = Vec::new();
        for a in pos_args {
            eval_pos.push(self.eval_expr(a)?);
        }
        if let Some(va) = var_args {
            let va_val = self.eval_expr(va)?;
            eval_pos.extend(star_args_seq(&va_val).map_err(|e| StarlarkError(format!("{}: {}", star_at(va, loc), e.0)))?);
        }
        let mut eval_kw = BTreeMap::new();
        for (k, v) in kw_args {
            eval_kw.insert(k.clone(), self.eval_expr(v)?);
        }
        if let Some(ka) = kw_var_args {
            let ka_val = self.eval_expr(ka)?;
            self.pending_dup_kw = merge_star_kwargs(&mut eval_kw, &ka_val).map_err(|e| StarlarkError(format!("{}: {}", star_at(ka, loc), e.0)))?;
        }

        self.call_kw_order = kw_call_order(kw_args, kw_var_args.as_deref().map(|_| &eval_kw));
        self.arg_eval_loc = self.cur_call_loc.clone();
        self.cur_call_loc = loc.clone();
        if let Expr::Ident(func_name, _) = callee {
            if self.host.is_some() {
                if let Some(Value::BuiltinFunction(bf)) = self.lookup_var(func_name) {
                    if let Some(name) = bf.strip_prefix("host:") {
                        let host = self.host.clone().unwrap();
                        self.call_location = loc.to_string();

                        self.last_call_loc = loc.clone();
                        return host(self, name, eval_pos, eval_kw).map_err(|e| {
                            let prefix = format!("{loc}: ");
                            if e.0.starts_with(&prefix) || e.0.starts_with("ERROR: ") || e.0.contains(": Traceback (most recent call last):\n") {
                                e
                            } else {
                                StarlarkError(format!("{prefix}{}", e.0))
                            }
                        });
                    }
                }
            }
            match func_name.as_str() {
                "select" => {
                    let entries = match eval_pos.first() {
                        Some(Value::Dict(d)) => {
                            let guard = d.lock().unwrap();
                            let mut list = Vec::new();
                            for (k, v) in &guard.entries {
                                list.push((k.to_string(), v.clone()));
                            }
                            list
                        }
                        _ => {
                            return Err(StarlarkError(format!(
                                "{loc}: select() requires a dict argument"
                            )));
                        }
                    };
                    return Ok(Value::SelectorList(SelectorList::from_select(entries)));
                }
                "depset" => {
                    let direct = eval_pos
                        .first()
                        .or_else(|| eval_kw.get("direct"))
                        .and_then(|v| match v {
                            Value::List(l) => Some(l.lock().unwrap().elements.clone()),
                            Value::Tuple(t) => Some(t.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    let order = eval_kw
                        .get("order")
                        .and_then(|v| v.as_str())
                        .map(DepsetOrder::parse)
                        .transpose()?
                        .unwrap_or_default();
                    let transitive = eval_kw
                        .get("transitive")
                        .and_then(|v| match v {
                            Value::List(l) => Some(
                                l.lock()
                                    .unwrap()
                                    .elements
                                    .iter()
                                    .filter_map(|item| match item {
                                        Value::Depset(d) => Some(Arc::clone(d)),
                                        _ => None,
                                    })
                                    .collect(),
                            ),
                            Value::Tuple(t) => Some(
                                t.iter()
                                    .filter_map(|item| match item {
                                        Value::Depset(d) => Some(Arc::clone(d)),
                                        _ => None,
                                    })
                                    .collect(),
                            ),
                            _ => None,
                        })
                        .unwrap_or_default();
                    return Ok(Value::Depset(Arc::new(Depset::new(
                        order, direct, transitive,
                    )?)));
                }
                "glob" => {
                    let include = eval_pos
                        .first()
                        .or_else(|| eval_kw.get("include"))
                        .map(|v| v.to_string_list())
                        .unwrap_or_default();
                    let exclude = eval_pos
                        .get(1)
                        .or_else(|| eval_kw.get("exclude"))
                        .map(|v| v.to_string_list())
                        .unwrap_or_default();
                    let exclude_dirs = eval_kw
                        .get("exclude_directories")
                        .map(|v| match v {
                            Value::Int(i) => !i.is_zero(),
                            Value::Bool(b) => *b,
                            _ => true,
                        })
                        .unwrap_or(true);
                    if let Some(ref fs) = self.fs {
                        if let Ok(matched) = evaluate_glob(
                            fs.as_ref(),
                            &self.package_dir,
                            &include,
                            &exclude,
                            exclude_dirs,
                        ) {
                            return Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(
                                matched.into_iter().map(Value::String).collect(),
                            )))));
                        }
                    }
                    return Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(
                        Vec::new(),
                    )))));
                }
                "struct" => {
                    return Ok(Value::Struct(Arc::new(StructValue::new(eval_kw))));
                }
                "provider" => {
                    return Ok(Value::Provider {
                        name: "Provider".to_string(),
                    });
                }
                "rule" | "repository_rule" | "module_extension" | "aspect" | "tag_class" => {
                    let executable = eval_kw
                        .get("executable")
                        .map(|v| v.is_truthy())
                        .unwrap_or(false);
                    let test = eval_kw.get("test").map(|v| v.is_truthy()).unwrap_or(false);
                    return Ok(Value::RuleCallable {
                        rule_class: "custom_rule".to_string(),
                        executable,
                        test,
                    });
                }
                "Label" => {
                    let s = eval_pos
                        .first()
                        .and_then(|v| v.as_str())
                        .unwrap_or("//:default");
                    let lctx = self.current_module_env().label_ctx.lock().unwrap().clone();
                    let label = match lctx {
                        Some((pkg, mapping)) => {
                            // An apparent repo name the mapping does not know resolves to Bazel's
                            // `[unknown repo 'x' requested from @@owner]` placeholder repository.
                            let mapping = match s.strip_prefix('@').filter(|r| !r.starts_with('@')) {
                                Some(rest) => {
                                    let apparent = rest.split("//").next().unwrap_or(rest);
                                    if !apparent.is_empty() && !mapping.entries().any(|(k, _)| k == apparent) && apparent != "bazel_tools" && apparent != "platforms" {
                                        let owner = mapping.get("");
                                        let marker = format!("[unknown repo '{apparent}' requested from @@{}]", owner.name());
                                        let mut entries: std::collections::HashMap<String, zel_cmdline::RepositoryName> =
                                            mapping.entries().map(|(k, v)| (k.clone(), v.clone())).collect();
                                        entries.insert(apparent.to_string(), zel_cmdline::RepositoryName::create_unvalidated(marker));
                                        zel_cmdline::RepositoryMapping::new(entries, owner)
                                    } else {
                                        mapping
                                    }
                                }
                                None => mapping,
                            };
                            if s.starts_with("//") || s.starts_with('@') {
                                Label::parse_with_repo_mapping(s, &mapping)
                            } else {
                                Label::create(pkg, s.strip_prefix(':').unwrap_or(s))
                            }
                            .map_err(|e| StarlarkError(e.to_string()))?
                        }
                        None => Label::parse_canonical(s).map_err(|e| StarlarkError(e.to_string()))?,
                    };
                    return Ok(Value::Label(label));
                }
                "exports_files" => {
                    if let Some(first) = eval_pos.first() {
                        self.exports_files.extend(first.to_string_list());
                    }
                    return Ok(Value::None);
                }
                "package" | "licenses" | "register_toolchains" | "register_execution_platforms" => {
                    return Ok(Value::None);
                }
                "fail" => {
                    let sep = eval_kw
                        .get("sep")
                        .and_then(|v| v.as_str())
                        .unwrap_or(" ");
                    let attr_prefix = eval_kw
                        .get("attr")
                        .and_then(|v| v.as_str())
                        .map(|a| format!("attribute {a}: "))
                        .unwrap_or_default();

                    let mut parts = Vec::new();
                    if let Some(msg_val) = eval_kw.get("msg") {
                        if !matches!(msg_val, Value::None) {
                            parts.push(msg_val.to_string());
                        }
                    }
                    for arg in eval_pos {
                        parts.push(arg.to_string());
                    }
                    let msg = parts.join(sep);
                    let trace = self.thread.format_stack_trace_at(&loc);
                    return Err(StarlarkError(format!("{trace}Error in fail: {attr_prefix}{msg}")));
                }
                "print" => {
                    let sep = eval_kw
                        .get("sep")
                        .and_then(|v| v.as_str())
                        .unwrap_or(" ");
                    let parts: Vec<String> = eval_pos
                        .iter()
                        .map(|v| match v {
                            Value::Label(l) => label_shorthand_display(l, self.main_repo_mapping.as_deref()),
                            // `Facts.debugPrint` shows the contents.
                            v @ Value::Host(k, _) if k == "Facts" => match self.host.clone() {
                                Some(host) => host(self, "debugprint:Facts", vec![v.clone()], BTreeMap::new()).map(|r| r.to_string()).unwrap_or_else(|_| v.to_string()),
                                None => v.to_string(),
                            },
                            v => v.to_string(),
                        })
                        .collect();
                    let msg = parts.join(sep);
                    if let Some(handler) = &self.print_handler {
                        handler(&loc, &msg);
                    } else if let Some(buf) = self.captured_prints.as_mut() {
                        buf.push((loc.clone(), msg));
                    } else {
                        eprintln!("DEBUG: {}:{}:{}: {msg}", loc.file, loc.line, loc.column);
                    }
                    return Ok(Value::None);
                }
                _ => {}
            }

            if let Some(r) = self.probe_host_attr(func_name, &eval_pos, &eval_kw) {
                return r;
            }
            match eval::builtins::eval_universal_function(func_name, &eval_pos, &eval_kw) {
                Ok(Some(res)) => return Ok(res),
                Ok(None) => {}
                Err(e) => {
                    if !has_location_prefix(&e.0) {
                        self.builtin_error = Some((func_name.to_string(), e.0.clone()));
                    }
                    return Err(with_loc(e, &loc));
                }
            }
        }

        let callee_val = self.eval_expr(callee)?;
        self.call_callable(callee_val, eval_pos, eval_kw, loc)
    }

    /// Calls any callable value with already-evaluated arguments.
    pub fn call_value(
        &mut self,
        callee: Value,
        pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> Result<Value, StarlarkError> {
        self.call_callable(callee, pos, kw, &Location::default())
    }

    fn call_callable(
        &mut self,
        callee_val: Value,
        eval_pos: Vec<Value>,
        eval_kw: BTreeMap<String, Value>,
        loc: &Location,
    ) -> Result<Value, StarlarkError> {
        let loc = loc;
        let pending_dup = self.pending_dup_kw.take();
        match callee_val {
            Value::Host(kind, id) => {
                let host = self.host.clone().ok_or_else(|| {
                    StarlarkError(format!("{loc}: '{kind}' object is not callable"))
                })?;
                let mut args = vec![Value::Host(kind.clone(), id)];
                args.extend(eval_pos);
                self.call_location = loc.to_string();

                self.last_call_loc = loc.clone();
                host(self, &format!("call:{kind}"), args, eval_kw).map_err(|e| {
                    let prefix = format!("{loc}: ");
                    if e.0.starts_with(&prefix) || e.0.starts_with("ERROR: ") || e.0.contains(": Traceback (most recent call last):\n") {
                        e
                    } else {
                        StarlarkError(format!("{prefix}{}", e.0))
                    }
                })
            }
            Value::UserFunction(func) => {
                // Push call frame and check recursion
                let saved_call_loc = self.cur_call_loc.clone();
                // Bazel locates a function (for binding errors, recursion) at its name, after `def `.
                let mut def_loc = func.location.clone();
                def_loc.column += 4;
                if let Err(e) = self.thread.push_frame_for(&func.name, Arc::as_ptr(&func) as *const u8 as usize, loc.clone()) {
                    // The recursive frame is entered before the error is reported.
                    let _ = self.thread.push_frame_for(&func.name, usize::MAX, loc.clone());
                    return Err(StarlarkError(format!("{def_loc}: {}", e.0)));
                }
                self.cur_call_loc = def_loc.clone();
                let bind_err = |m: String| StarlarkError(format!("{def_loc}: {m}"));
                if let Some(d) = &pending_dup {
                    return Err(bind_err(format!("{}() got multiple values for parameter '{d}'", func.name)));
                }
                let plural = |n: usize| if n == 1 { "" } else { "s" };
                // Check positional / keyword arguments like `Starlark.fastcall` argument binding.
                {
                    let has_args = func.params.iter().any(|p| p.is_args);
                    let has_kwargs = func.params.iter().any(|p| p.is_kwargs);
                    let non_kw_only: Vec<&str> =
                        func.params.iter().filter(|p| !p.is_kw_only && !p.is_args && !p.is_kwargs).map(|p| p.name.as_str()).collect();
                    if eval_pos.len() > non_kw_only.len() && !has_args {
                        if non_kw_only.is_empty() {
                            return Err(bind_err(format!("{}() does not accept positional arguments, but got {}", func.name, eval_pos.len())));
                        }
                        return Err(bind_err(format!(
                            "{}() accepts no more than {} positional argument{} but got {}",
                            func.name,
                            non_kw_only.len(),
                            plural(non_kw_only.len()),
                            eval_pos.len()
                        )));
                    }
                    for (i, n) in non_kw_only.iter().enumerate() {
                        if i < eval_pos.len() && eval_kw.contains_key(*n) {
                            return Err(bind_err(format!("{}() got multiple values for parameter '{n}'", func.name)));
                        }
                    }
                    if !has_kwargs {
                        let mut unexpected: Vec<String> = Vec::new();
                        let order: Vec<String> = if self.call_kw_order.iter().all(|k| eval_kw.contains_key(k)) && self.call_kw_order.len() == eval_kw.len() {
                            self.call_kw_order.clone()
                        } else {
                            eval_kw.keys().cloned().collect()
                        };
                        for k in order {
                            if !func.params.iter().any(|p| !p.is_args && !p.is_kwargs && p.name == k) {
                                unexpected.push(k);
                            }
                        }
                        if !unexpected.is_empty() {
                            return Err(bind_err(format!(
                                "{}() got unexpected keyword argument{}: {}",
                                func.name,
                                plural(unexpected.len()),
                                unexpected.join(", ")
                            )));
                        }
                    }
                    let missing_pos: Vec<&str> = func
                        .params
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| !p.is_kw_only && !p.is_args && !p.is_kwargs && p.default.is_none())
                        .filter(|(_, p)| {
                            let idx = non_kw_only.iter().position(|n| *n == p.name).unwrap_or(usize::MAX);
                            idx >= eval_pos.len() && !eval_kw.contains_key(&p.name)
                        })
                        .map(|(_, p)| p.name.as_str())
                        .collect();
                    if !missing_pos.is_empty() {
                        return Err(bind_err(format!(
                            "{}() missing {} required positional argument{}: {}",
                            func.name,
                            missing_pos.len(),
                            plural(missing_pos.len()),
                            missing_pos.join(", ")
                        )));
                    }
                    let missing_kw: Vec<&str> = func
                        .params
                        .iter()
                        .filter(|p| p.is_kw_only && !p.is_args && !p.is_kwargs && p.default.is_none() && !eval_kw.contains_key(&p.name))
                        .map(|p| p.name.as_str())
                        .collect();
                    if !missing_kw.is_empty() {
                        return Err(bind_err(format!(
                            "{}() missing {} required keyword-only argument{}: {}",
                            func.name,
                            missing_kw.len(),
                            plural(missing_kw.len()),
                            missing_kw.join(", ")
                        )));
                    }
                }
                self.cur_call_loc = func.location.clone();
                self.cur_call_loc.column += 4;
                if let Some(m) = &func.module {
                    if !Arc::ptr_eq(m, &self.module_env) {
                        if let Some(frame) = self.thread.current_frame_mut() {
                            frame.module = Some(Arc::clone(m));
                        }
                    }
                }
                if let Some(frame) = self.thread.current_frame_mut() {
                    frame.parents = func.closure.0.clone();
                }

                // Bind parameters into frame
                let mut pos_idx = 0;
                for param in &func.params {
                    if param.is_args {
                        let rest = if pos_idx < eval_pos.len() {
                            eval_pos[pos_idx..].to_vec()
                        } else {
                            Vec::new()
                        };
                        self.set_var(&param.name, Value::Tuple(rest));
                    } else if param.is_kwargs {
                        let mut d = StarlarkDict::new();
                        for (k, v) in &eval_kw {
                            // Keywords consumed by named parameters are not collected.
                            if func.params.iter().any(|p| !p.is_args && !p.is_kwargs && p.name == *k) {
                                continue;
                            }
                            let _ = d.insert(Value::String(k.clone()), v.clone());
                        }
                        self.set_var(&param.name, Value::Dict(Arc::new(Mutex::new(d))));
                    } else if !param.is_kw_only && pos_idx < eval_pos.len() {
                        self.set_var(&param.name, eval_pos[pos_idx].clone());
                        pos_idx += 1;
                    } else if let Some(val) = eval_kw.get(&param.name) {
                        self.set_var(&param.name, val.clone());
                    } else if let Some(ref def_expr) = param.default {
                        let def_val = self.eval_expr(def_expr)?;
                        self.set_var(&param.name, def_val);
                    } else {
                        return Err(StarlarkError(format!(
                            "{loc}: missing required argument '{}'",
                            param.name
                        )));
                    }
                }

                let mut ret = Value::None;
                for s in &func.body {
                    match self.exec_stmt(s)? {
                        ControlFlow::Return(v) => {
                            ret = v;
                            break;
                        }
                        ControlFlow::Break | ControlFlow::Continue => {
                            return Err(StarlarkError(format!(
                                "{loc}: flow statement outside loop"
                            )));
                        }
                        ControlFlow::None => {}
                    }
                }

                self.thread.pop_frame();
                self.cur_call_loc = saved_call_loc;
                Ok(ret)
            }
            Value::BuiltinFunction(bf) if bf.starts_with("host:") && self.host.is_some() => {
                let host = self.host.clone().unwrap();
                self.call_location = loc.to_string();

                self.last_call_loc = loc.clone();
                let host_result = host(self, &bf["host:".len()..], eval_pos, eval_kw);
                if let (Err(e), Some(native_fn)) = (&host_result, bf.strip_prefix("host:native.")) {
                    // Errors of `native.<fn>()` builtins read "Error in <fn>: ...".
                    if !has_location_prefix(&e.0) {
                        self.builtin_error = Some((native_fn.to_string(), e.0.clone()));
                    }
                }
                host_result.map_err(|e| {
                    // Host functions may already have prefixed their own call location.
                    let prefix = format!("{loc}: ");
                    if e.0.starts_with(&prefix) || e.0.starts_with("ERROR: ") || e.0.contains(": Traceback (most recent call last):\n") {
                        e
                    } else {
                        StarlarkError(format!("{prefix}{}", e.0))
                    }
                })
            }
            Value::BuiltinFunction(bf) => match bf.as_str() {
                "json.encode" => {
                    let arg = eval_pos.first().ok_or_else(|| {
                        StarlarkError(format!("{loc}: json.encode requires 1 argument"))
                    })?;
                    Ok(Value::String(json_encode(arg)?))
                }
                "json.decode" => {
                    let arg = eval_pos
                        .first()
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            StarlarkError(format!("{loc}: json.decode requires string argument"))
                        })?;
                    let default = eval_kw.get("default").cloned();
                    json_decode(arg, default)
                }
                "json.indent" => {
                    let arg = eval_pos
                        .first()
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            StarlarkError(format!("{loc}: json.indent requires string argument"))
                        })?;
                    let prefix = eval_kw
                        .get("prefix")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let indent = eval_kw
                        .get("indent")
                        .and_then(|v| v.as_str())
                        .unwrap_or("\t");
                    Ok(Value::String(json_indent(arg, prefix, indent)?))
                }
                "json.encode_indent" => {
                    let arg = eval_pos.first().ok_or_else(|| {
                        StarlarkError(format!("{loc}: json.encode_indent requires 1 argument"))
                    })?;
                    let prefix = eval_kw
                        .get("prefix")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let indent = eval_kw
                        .get("indent")
                        .and_then(|v| v.as_str())
                        .unwrap_or("\t");
                    let encoded = json_encode(arg)?;
                    Ok(Value::String(json_indent(&encoded, prefix, indent)?))
                }
                "proto.encode_text" => {
                    let arg = eval_pos.first().ok_or_else(|| {
                        StarlarkError(format!("{loc}: proto.encode_text requires struct argument"))
                    })?;
                    Ok(Value::String(proto_encode_text(arg)?))
                }
                // `fail` used as a first-class value (e.g. `printer = fail`).
                "fail" => {
                    let sep = eval_kw.get("sep").and_then(|v| v.as_str()).unwrap_or(" ").to_string();
                    let mut parts: Vec<String> = Vec::new();
                    if let Some(m) = eval_kw.get("msg") {
                        if !matches!(m, Value::None) {
                            parts.push(m.to_string());
                        }
                    }
                    for a in &eval_pos {
                        parts.push(match a {
                            Value::String(s) => s.clone(),
                            o => o.to_string(),
                        });
                    }
                    Err(StarlarkError(format!("{loc}: fail: {}", parts.join(&sep))))
                }
                _ => match self
                    .probe_host_attr(bf.as_str(), &eval_pos, &eval_kw)
                    .map(|r| r.map(Some))
                    .unwrap_or_else(|| eval::builtins::eval_universal_function(bf.as_str(), &eval_pos, &eval_kw))
                {
                    Ok(Some(v)) => Ok(v),
                    Ok(None) => Err(StarlarkError(format!("{loc}: unknown builtin '{bf}'"))),
                    Err(e) => Err(StarlarkError(format!("{loc}: {}", e.0))),
                },
            },
            Value::Provider { name } => {
                let mut eval_kw = eval_kw;
                // Native providers with positional constructor parameters
                // (`testing.ExecutionInfo(requirements, exec_group)`).
                if name == "ExecutionInfo" {
                    for (p, v) in ["requirements", "exec_group"].iter().zip(eval_pos.iter()) {
                        eval_kw.entry((*p).to_string()).or_insert_with(|| v.clone());
                    }
                }
                Ok(Value::ProviderInstance { provider_name: name, fields: eval_kw })
            }
            Value::RuleCallable { rule_class, .. } => {
                self.rule_calls.push(RecordedRuleCall {
                    rule_class,
                    kwargs: eval_kw,
                });
                Ok(Value::None)
            }
            other => {
                // Bazel reports this at the frame's location before this call started.
                let at = if self.pre_call_loc.file.is_empty() { loc.clone() } else { self.pre_call_loc.clone() };
                Err(StarlarkError(format!("{at}: '{}' object is not callable", other.type_name())))
            }
        }
    }

    fn eval_method_call(
        &mut self,
        recv: Value,
        method: &str,
        pos_args: &[Value],
        kw_args: &BTreeMap<String, Value>,
        loc: &Location,
    ) -> Result<Value, StarlarkError> {
        match recv {
            Value::String(s) => eval::builtins::eval_string_method(&s, method, pos_args, kw_args),
            Value::Label(l) => match method {
                "same_package_label" => {
                    let n = pos_args.first().and_then(|v| v.as_str()).ok_or_else(|| StarlarkError(format!("{loc}: same_package_label() takes a string")))?;
                    zel_cmdline::Label::create(l.package_identifier().clone(), n)
                        .map(Value::Label)
                        .map_err(|e| StarlarkError(format!("{loc}: {e}")))
                }
                "relative" | "package_relative" => {
                    let raw = pos_args.first().and_then(|v| v.as_str()).ok_or_else(|| StarlarkError(format!("{loc}: relative() takes a string")))?;
                    let parsed = if let Some(n) = raw.strip_prefix(':') {
                        zel_cmdline::Label::create(l.package_identifier().clone(), n)
                    } else if raw.starts_with("@@") {
                        zel_cmdline::Label::parse_canonical(raw)
                    } else if let Some(rest) = raw.strip_prefix("//") {
                        zel_cmdline::Label::parse_canonical(&format!("{}//{rest}", if l.repository().is_main() { "@@".to_string() } else { format!("@@{}", l.repository().name()) }))
                    } else if raw.starts_with('@') {
                        zel_cmdline::Label::parse_canonical(&format!("@{raw}"))
                    } else {
                        zel_cmdline::Label::create(l.package_identifier().clone(), raw)
                    };
                    parsed.map(Value::Label).map_err(|e| StarlarkError(format!("{loc}: {e}")))
                }
                other => Err(StarlarkError(format!("{loc}: type 'Label' has no method '{other}'"))),
            },
            Value::List(l) => {
                let mut guard = l.lock().unwrap();
                match method {
                    "append" => {
                        let item = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: append() takes 1 argument")))?;
                        guard.append(item.clone())?;
                        Ok(Value::None)
                    }
                    "clear" => {
                        guard.clear()?;
                        Ok(Value::None)
                    }
                    "extend" => {
                        let items = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: extend() takes 1 argument")))?;
                        let seq = eval::builtins::value_to_sequence(items)?;
                        guard.extend(seq)?;
                        Ok(Value::None)
                    }
                    "index" => {
                        let item = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: index() takes 1 argument")))?;
                        let s = pos_args.get(1).and_then(|v| v.as_i64());
                        let e = pos_args.get(2).and_then(|v| v.as_i64());
                        let idx = guard.index(item, s, e)?;
                        Ok(Value::Int(BigInt::from_i64(idx)))
                    }
                    "insert" => {
                        let idx = pos_args
                            .first()
                            .and_then(|v| v.as_i64())
                            .ok_or_else(|| StarlarkError(format!("{loc}: insert() takes index argument")))?;
                        let item = pos_args
                            .get(1)
                            .ok_or_else(|| StarlarkError(format!("{loc}: insert() takes item argument")))?;
                        guard.insert(idx, item.clone())?;
                        Ok(Value::None)
                    }
                    "pop" => {
                        let idx = pos_args.first().and_then(|v| v.as_i64());
                        guard.pop(idx)
                    }
                    "remove" => {
                        let item = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: remove() takes 1 argument")))?;
                        guard.remove(item)?;
                        Ok(Value::None)
                    }
                    _ => Err(StarlarkError(format!(
                        "{loc}: 'list' value has no field or method '{method}'"
                    ))),
                }
            }
            Value::Dict(d) => {
                let mut guard = d.lock().unwrap();
                match method {
                    "clear" => {
                        guard.clear()?;
                        Ok(Value::None)
                    }
                    "get" => {
                        let key = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: get() takes at least 1 argument")))?;
                        let default = pos_args.get(1).or_else(|| kw_args.get("default")).cloned().unwrap_or(Value::None);
                        Ok(guard.get(key).cloned().unwrap_or(default))
                    }
                    "items" => {
                        let pairs: Vec<Value> = guard
                            .items()
                            .into_iter()
                            .map(|(k, v)| Value::Tuple(vec![k, v]))
                            .collect();
                        Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(pairs)))))
                    }
                    "keys" => Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(
                        guard.keys(),
                    ))))),
                    "values" => Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(
                        guard.values(),
                    ))))),
                    "pop" => {
                        let key = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: pop() takes at least 1 argument")))?;
                        let default = pos_args.get(1).cloned();
                        guard.pop(key, default)
                    }
                    "popitem" => {
                        let (k, v) = guard.popitem()?;
                        Ok(Value::Tuple(vec![k, v]))
                    }
                    "setdefault" => {
                        let key = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: setdefault() requires key")))?;
                        let default = pos_args.get(1).cloned().unwrap_or(Value::None);
                        guard.setdefault(key.clone(), default)
                    }
                    "update" => {
                        let mut pairs = Vec::new();
                        if let Some(first) = pos_args.first() {
                            match first {
                                Value::Dict(other_d) => {
                                    pairs.extend(other_d.lock().unwrap().items());
                                }
                                other => {
                                    let seq = eval::builtins::value_to_sequence(other)?;
                                    for item in seq {
                                        pairs.push(eval::builtins::universal::unpack_pair(&item)?);
                                    }
                                }
                            }
                        }
                        for (k, v) in kw_args {
                            pairs.push((Value::String(k.clone()), v.clone()));
                        }
                        guard.update(pairs)?;
                        Ok(Value::None)
                    }
                    _ => Err(StarlarkError(format!(
                        "{loc}: 'dict' value has no field or method '{method}'"
                    ))),
                }
            }
            Value::Set(s) => {
                let mut guard = s.lock().unwrap();
                match method {
                    "add" => {
                        let item = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: add() takes 1 argument")))?;
                        guard.add(item.clone())?;
                        Ok(Value::None)
                    }
                    "clear" => {
                        guard.clear()?;
                        Ok(Value::None)
                    }
                    "discard" => {
                        let item = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: discard() takes 1 argument")))?;
                        guard.discard(item)?;
                        Ok(Value::None)
                    }
                    "remove" => {
                        let item = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: remove() takes 1 argument")))?;
                        guard.remove(item)?;
                        Ok(Value::None)
                    }
                    "pop" => guard.pop(),
                    "issubset" => {
                        let other = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: issubset() takes 1 argument")))?;
                        let other_set = match other {
                            Value::Set(os) => os.lock().unwrap().clone(),
                            _ => StarlarkSet::from_elements(eval::builtins::value_to_sequence(other)?)?,
                        };
                        Ok(Value::Bool(guard.issubset(&other_set)))
                    }
                    "issuperset" => {
                        let other = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: issuperset() takes 1 argument")))?;
                        let other_set = match other {
                            Value::Set(os) => os.lock().unwrap().clone(),
                            _ => StarlarkSet::from_elements(eval::builtins::value_to_sequence(other)?)?,
                        };
                        Ok(Value::Bool(guard.issuperset(&other_set)))
                    }
                    "isdisjoint" => {
                        let other = pos_args
                            .first()
                            .ok_or_else(|| StarlarkError(format!("{loc}: isdisjoint() takes 1 argument")))?;
                        let other_set = match other {
                            Value::Set(os) => os.lock().unwrap().clone(),
                            _ => StarlarkSet::from_elements(eval::builtins::value_to_sequence(other)?)?,
                        };
                        Ok(Value::Bool(guard.isdisjoint(&other_set)))
                    }
                    "union" => {
                        let mut res = guard.clone();
                        for arg in pos_args {
                            let other_set = match arg {
                                Value::Set(os) => os.lock().unwrap().clone(),
                                _ => StarlarkSet::from_elements(eval::builtins::value_to_sequence(arg)?)?,
                            };
                            res = res.union(&other_set);
                        }
                        Ok(Value::Set(Arc::new(Mutex::new(res))))
                    }
                    "intersection" => {
                        let mut res = guard.clone();
                        for arg in pos_args {
                            let other_set = match arg {
                                Value::Set(os) => os.lock().unwrap().clone(),
                                _ => StarlarkSet::from_elements(eval::builtins::value_to_sequence(arg)?)?,
                            };
                            res = res.intersection(&other_set);
                        }
                        Ok(Value::Set(Arc::new(Mutex::new(res))))
                    }
                    "difference" => {
                        let mut res = guard.clone();
                        for arg in pos_args {
                            let other_set = match arg {
                                Value::Set(os) => os.lock().unwrap().clone(),
                                _ => StarlarkSet::from_elements(eval::builtins::value_to_sequence(arg)?)?,
                            };
                            res = res.difference(&other_set);
                        }
                        Ok(Value::Set(Arc::new(Mutex::new(res))))
                    }
                    "symmetric_difference" => {
                        let other = pos_args.first().ok_or_else(|| {
                            StarlarkError(format!("{loc}: symmetric_difference() takes 1 argument"))
                        })?;
                        let other_set = match other {
                            Value::Set(os) => os.lock().unwrap().clone(),
                            _ => StarlarkSet::from_elements(eval::builtins::value_to_sequence(other)?)?,
                        };
                        Ok(Value::Set(Arc::new(Mutex::new(
                            guard.symmetric_difference(&other_set),
                        ))))
                    }
                    "update" => {
                        for arg in pos_args {
                            let other_set = match arg {
                                Value::Set(os) => os.lock().unwrap().clone(),
                                _ => StarlarkSet::from_elements(eval::builtins::value_to_sequence(arg)?)?,
                            };
                            guard.update(&other_set)?;
                        }
                        Ok(Value::None)
                    }
                    _ => Err(StarlarkError(format!(
                        "{loc}: 'set' value has no field or method '{method}'"
                    ))),
                }
            }
            Value::Depset(d) => match method {
                "to_list" => Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(
                    d.to_list(),
                ))))),
                _ => Err(StarlarkError(format!(
                    "{loc}: 'depset' value has no field or method '{method}'"
                ))),
            },
            Value::Host(kind, id) => {
                let host = self.host.clone().ok_or_else(|| {
                    StarlarkError(format!("{loc}: no host for method '{method}'"))
                })?;
                let mut args = vec![Value::Host(kind.clone(), id)];
                args.extend(pos_args.iter().cloned());
                self.call_location = loc.to_string();

                self.last_call_loc = loc.clone();
                host(self, &format!("method:{kind}.{method}"), args, kw_args.clone())
                    .map_err(|e| {
                        let prefix = format!("{loc}: ");
                        if e.0.starts_with(&prefix) || e.0.starts_with("ERROR: ") || e.0.contains(": Traceback (most recent call last):\n") {
                            e
                        } else {
                            StarlarkError(format!("{prefix}{}", e.0))
                        }
                    })
            }
            Value::Struct(s) => {
                if let Some(val) = s.fields.get(method) {
                    if let Value::BuiltinFunction(bf) = val {
                        if let (Some(name), Some(host)) = (bf.strip_prefix("host:"), self.host.clone()) {
                            self.call_location = loc.to_string();

                            self.last_call_loc = loc.clone();
                            return host(self, name, pos_args.to_vec(), kw_args.clone())
                                .map_err(|e| {
                                    let prefix = format!("{loc}: ");
                                    if e.0.starts_with(&prefix) || e.0.starts_with("ERROR: ") || e.0.contains(": Traceback (most recent call last):\n") {
                                        e
                                    } else {
                                        StarlarkError(format!("{prefix}{}", e.0))
                                    }
                                });
                        }
                        match bf.as_str() {
                            "json.encode" => {
                                let arg = pos_args.first().ok_or_else(|| {
                                    StarlarkError(format!("{loc}: json.encode requires 1 argument"))
                                })?;
                                return Ok(Value::String(json_encode(arg)?));
                            }
                            "json.decode" => {
                                let arg = pos_args
                                    .first()
                                    .and_then(|v| v.as_str())
                                    .ok_or_else(|| {
                                        StarlarkError(format!("{loc}: json.decode requires string argument"))
                                    })?;
                                let default = kw_args.get("default").cloned();
                                return json_decode(arg, default);
                            }
                            "json.indent" => {
                                let arg = pos_args
                                    .first()
                                    .and_then(|v| v.as_str())
                                    .ok_or_else(|| {
                                        StarlarkError(format!("{loc}: json.indent requires string argument"))
                                    })?;
                                let prefix = kw_args
                                    .get("prefix")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let indent = kw_args
                                    .get("indent")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("\t");
                                return Ok(Value::String(json_indent(arg, prefix, indent)?));
                            }
                            "json.encode_indent" => {
                                let arg = pos_args.first().ok_or_else(|| {
                                    StarlarkError(format!("{loc}: json.encode_indent requires 1 argument"))
                                })?;
                                let prefix = kw_args
                                    .get("prefix")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let indent = kw_args
                                    .get("indent")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("\t");
                                let encoded = json_encode(arg)?;
                                return Ok(Value::String(json_indent(&encoded, prefix, indent)?));
                            }
                            "proto.encode_text" => {
                                let arg = pos_args.first().ok_or_else(|| {
                                    StarlarkError(format!("{loc}: proto.encode_text requires struct argument"))
                                })?;
                                return Ok(Value::String(proto_encode_text(arg)?));
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(val) = s.fields.get(method) {
                    if matches!(
                        val,
                        Value::Provider { .. } | Value::UserFunction(_) | Value::Host(..) | Value::RuleCallable { .. }
                    ) {
                        return self.call_callable(val.clone(), pos_args.to_vec(), kw_args.clone(), loc);
                    }
                }
                Err(StarlarkError(format!(
                    "{loc}: type 'struct' has no method '{method}'"
                )))
            }
            Value::ProviderInstance { fields, provider_name } => {
                // Callable field of a provider instance (`info.execution_info()`).
                match fields.get(method) {
                    Some(val @ (Value::UserFunction(_) | Value::BuiltinFunction(_) | Value::Provider { .. } | Value::Host(..))) => {
                        self.call_callable(val.clone(), pos_args.to_vec(), kw_args.clone(), loc)
                    }
                    _ => Err(StarlarkError(format!(
                        "{loc}: type '{provider_name}' has no method '{method}'"
                    ))),
                }
            }
            other => Err(StarlarkError(format!(
                "{loc}: type '{}' has no method '{method}'",
                other.type_name()
            ))),
        }
    }
}

fn slice_sequence<T: Clone>(
    seq: &[T],
    start_val: Option<&Value>,
    end_val: Option<&Value>,
    step: i64,
) -> Vec<T> {
    let len = seq.len() as i64;
    if step > 0 {
        let start = match start_val.and_then(|v| v.as_i64()) {
            Some(i) if i < 0 => (len + i).max(0),
            Some(i) => i.min(len),
            None => 0,
        };
        let end = match end_val.and_then(|v| v.as_i64()) {
            Some(i) if i < 0 => (len + i).max(0),
            Some(i) => i.min(len),
            None => len,
        };
        let mut out = Vec::new();
        let mut idx = start;
        while idx < end {
            out.push(seq[idx as usize].clone());
            idx += step;
        }
        out
    } else {
        let start = match start_val.and_then(|v| v.as_i64()) {
            Some(i) if i < 0 => len + i,
            Some(i) => i.min(len - 1),
            None => len - 1,
        };
        let end = match end_val.and_then(|v| v.as_i64()) {
            Some(i) if i < 0 => len + i,
            Some(i) => i,
            None => -1,
        };
        let mut out = Vec::new();
        let mut idx = start;
        while idx > end && idx >= 0 && idx < len {
            out.push(seq[idx as usize].clone());
            idx += step;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_vfs::InMemoryFileSystem;

    #[test]
    fn test_starlark_depset_and_comprehensions() {
        let mut ctx = EvalContext::default();
        ctx.eval_str(
            r#"
d1 = depset(["a", "b"])
d2 = depset(["b", "c"], transitive = [d1])
items = [x.upper() for x in d2.to_list() if x != "b"]
"#,
        )
        .unwrap();
        assert_eq!(
            ctx.globals.get("items"),
            Some(&Value::List(Arc::new(Mutex::new(StarlarkList::new(vec![
                Value::String("A".to_string()),
                Value::String("C".to_string()),
            ])))))
        );
    }

    #[test]
    fn test_starlark_macros_glob_and_select_in_build() {
        let fs = Arc::new(InMemoryFileSystem::default());
        let pkg_dir = PathFragment::create("/workspace/pkg");
        fs.create_directory_and_parents(&pkg_dir).unwrap();
        fs.write_file(&pkg_dir.get_relative_str("a.cc"), b"")
            .unwrap();
        fs.write_file(&pkg_dir.get_relative_str("b.cc"), b"")
            .unwrap();
        fs.write_file(&pkg_dir.get_relative_str("b_test.cc"), b"")
            .unwrap();

        let mut ctx = EvalContext::new(pkg_dir, Some(fs));
        ctx.eval_str(
            r#"
load("@rules_cc//cc:defs.bzl", "cc_binary", "cc_library")

def my_cc_macro(name, extra_deps = []):
    cc_library(
        name = name + "_lib",
        srcs = glob(["*.cc"], exclude = ["*_test.cc"]),
        deps = extra_deps + select({
            "//src/conditions:darwin": ["//platform:mac"],
            "//conditions:default": ["//platform:generic"],
        }),
    )

my_cc_macro("engine", extra_deps = ["//base"])
"#,
        )
        .unwrap();

        assert_eq!(ctx.loads.len(), 1);
        assert_eq!(ctx.rule_calls.len(), 1);
        let call = &ctx.rule_calls[0];
        assert_eq!(call.rule_class, "cc_library");
        assert_eq!(
            call.kwargs.get("name"),
            Some(&Value::String("engine_lib".to_string()))
        );
        assert_eq!(
            call.kwargs.get("srcs").unwrap().to_string_list(),
            vec!["a.cc", "b.cc"]
        );
        assert_eq!(
            call.kwargs.get("deps").unwrap().to_string_list(),
            vec!["//base", "//platform:generic"]
        );
    }

    #[test]
    fn test_spot_checks() {
        let mut ctx = EvalContext::default();

        // Spot Check 1: %d %s format order
        ctx.eval_str(r#"sc1 = "%d %s" % (1, "two")"#).unwrap();
        assert_eq!(ctx.globals.get("sc1"), Some(&Value::String("1 two".to_string())));

        // Spot Check 2: Bitwise OR
        ctx.eval_str(r#"sc2 = 5 | 3"#).unwrap();
        assert_eq!(ctx.globals.get("sc2"), Some(&Value::Int(BigInt::from_i64(7))));

        // Spot Check 3: Chained comparisons
        ctx.eval_str(r#"sc3 = 1 < 5 < 10"#).unwrap();
        assert_eq!(ctx.globals.get("sc3"), Some(&Value::Bool(true)));

        // Spot Check 4: Negative slicing
        ctx.eval_str(r#"sc4 = "hello"[-2:]"#).unwrap();
        assert_eq!(ctx.globals.get("sc4"), Some(&Value::String("lo".to_string())));

        // Spot Check 5: Comprehension scoping
        ctx.eval_str(
            r#"
x = "original"
dummy = [x for x in ["leaked"]]
"#,
        )
        .unwrap();
        assert_eq!(ctx.globals.get("x"), Some(&Value::String("original".to_string())));

        // Spot Check 6: Keyword-only parameters
        assert!(ctx
            .eval_str(
                r#"
def f(a, *, b):
    return a + b

res = f(1, 2)
"#
            )
            .is_err());
    }
}

/// Whether `msg` already starts with a `path:line:col: ` position.
/// Prefixes `e` with `loc` unless it already starts with a location.
fn with_loc(e: StarlarkError, loc: &Location) -> StarlarkError {
    if has_location_prefix(&e.0) {
        e
    } else {
        StarlarkError(format!("{loc}: {}", e.0))
    }
}

fn has_location_prefix(msg: &str) -> bool {
    let mut parts = msg.splitn(4, ':');
    let (_, l, c, rest) = (parts.next(), parts.next(), parts.next(), parts.next());
    matches!((l, c, rest), (Some(l), Some(c), Some(_)) if !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()) && !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit()))
}

/// Keyword names of a call in source order; entries contributed by `**kwargs` follow, in sorted
/// order of the merged map (the evaluator does not retain their original order).
fn kw_call_order(kw_args: &[(String, Expr)], merged: Option<&BTreeMap<String, Value>>) -> Vec<String> {
    let mut out: Vec<String> = kw_args.iter().map(|(k, _)| k.clone()).collect();
    if let Some(m) = merged {
        for k in m.keys() {
            if !out.contains(k) {
                out.push(k.clone());
            }
        }
    }
    out
}

/// `f(*x)`: `x` must be iterable.
fn star_args_seq(v: &Value) -> Result<Vec<Value>, StarlarkError> {
    if matches!(v, Value::Int(_) | Value::Bool(_) | Value::None | Value::UserFunction(_) | Value::BuiltinFunction(_)) {
        return Err(StarlarkError(format!("argument after * must be an iterable, not {}", v.type_name())));
    }
    eval::builtins::value_to_sequence(v)
}

/// `f(**x)`: `x` must be a dict with string keys that do not repeat explicit keyword arguments.
fn merge_star_kwargs(eval_kw: &mut BTreeMap<String, Value>, v: &Value) -> Result<Option<String>, StarlarkError> {
    let Value::Dict(d) = v else {
        return Err(StarlarkError(format!("argument after ** must be a dict, not {}", v.type_name())));
    };
    let guard = d.lock().unwrap();
    let mut dup = None;
    for (k, val) in &guard.entries {
        let Some(s) = k.as_str() else {
            return Err(StarlarkError(format!("keywords must be strings, not {}", k.type_name())));
        };
        if eval_kw.contains_key(s) && dup.is_none() {
            dup = Some(s.to_string());
        }
        eval_kw.insert(s.to_string(), val.clone());
    }
    Ok(dup)
}

/// Position of the `*` / `**` token of a star argument whose expression is `e` (inside a call at `call`).
fn star_at(e: &Expr, call: &Location) -> Location {
    let l = e.location();
    if l.file.is_empty() {
        let mut x = call.clone();
        x.column += 1;
        x
    } else {
        let mut x = l;
        x.column = x.column.saturating_sub(1);
        x
    }
}

/// `Label.getShorthandDisplayForm(mainRepositoryMapping)`: the form `print` shows for a label.
pub fn label_shorthand_display(l: &zel_cmdline::Label, mapping: Option<&BTreeMap<String, String>>) -> String {
    let repo = l.repository();
    let (pkg, name) = (l.package_name(), l.name());
    let base = pkg.rsplit('/').next().unwrap_or(pkg);
    let repo_display = if repo.is_main() {
        String::new()
    } else {
        let canonical = repo.name();
        match mapping.and_then(|m| m.iter().find(|(_, c)| c.as_str() == canonical)) {
            Some((apparent, _)) => format!("@{apparent}"),
            None => repo.name_with_at(),
        }
    };
    if !pkg.is_empty() && base == name {
        return format!("{repo_display}//{pkg}");
    }
    if pkg.is_empty() && (repo_display == format!("@{name}") || repo_display == format!("@@{name}")) {
        return repo_display;
    }
    format!("{repo_display}//{pkg}:{name}")
}
