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
use std::collections::HashSet;
use std::sync::Arc;

/// A single `# ...` source comment token (`Comment.java`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub text: String,
    pub location: Location,
}

impl Comment {
    pub fn has_doc_comment_prefix(&self) -> bool {
        self.text.starts_with("#:")
    }

    pub fn get_doc_comment_text(&self) -> Option<&str> {
        self.text
            .strip_prefix("#: ")
            .or_else(|| self.text.strip_prefix("#:"))
    }
}

/// Contiguous block of standalone `#:` doc-comment lines (`DocComments.java`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocComments {
    pub lines: Vec<Comment>,
    text_joined: String,
}

impl DocComments {
    pub fn new(lines: Vec<Comment>) -> Option<Self> {
        if lines.is_empty() || !lines.iter().all(Comment::has_doc_comment_prefix) {
            return None;
        }
        let text_joined = lines
            .iter()
            .filter_map(Comment::get_doc_comment_text)
            .collect::<Vec<_>>()
            .join("\n");
        Some(Self { lines, text_joined })
    }

    pub fn start_location(&self) -> &Location {
        &self.lines[0].location
    }

    pub fn end_location(&self) -> &Location {
        self.lines.last().map(|c| &c.location).unwrap()
    }

    pub fn text(&self) -> String {
        self.text_joined.clone()
    }
}

impl std::fmt::Display for DocComments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let raw = self
            .lines
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        f.write_str(&raw)
    }
}

/// Associates contiguous standalone `#:` doc-comment blocks immediately preceding a single-LHS
/// global assignment (`NAME = ...`) with that global, returning `(attached_in_decl_order, unused)`.
pub fn extract_doc_comments(
    stmts: &[Stmt],
    comments: &[Comment],
    standalone_lines: &HashSet<usize>,
) -> (Vec<(String, DocComments)>, Vec<DocComments>) {
    let mut assign_by_line = std::collections::BTreeMap::new();
    for s in stmts {
        if let Stmt::Assign {
            target: AssignTarget::Ident(name),
            op: AssignOp::Assign,
            location,
            ..
        } = s
        {
            assign_by_line
                .entry(location.line)
                .or_insert_with(|| name.clone());
        }
    }

    let mut attached = Vec::new();
    let mut attached_names = HashSet::new();
    let mut unused = Vec::new();
    let mut block: Vec<Comment> = Vec::new();

    let flush_block = |block: &mut Vec<Comment>,
                       next_non_doc_comment_line: Option<usize>,
                       attached: &mut Vec<(String, DocComments)>,
                       attached_names: &mut HashSet<String>,
                       unused: &mut Vec<DocComments>| {
        if block.is_empty() {
            return;
        }
        let last_line = block.last().unwrap().location.line;
        let lines_taken = std::mem::take(block);
        let Some(doc) = DocComments::new(lines_taken) else {
            return;
        };
        let blocked_by_plain_comment = next_non_doc_comment_line == Some(last_line + 1);
        if !blocked_by_plain_comment {
            if let Some(var_name) = assign_by_line.get(&(last_line + 1)) {
                if attached_names.insert(var_name.clone()) {
                    attached.push((var_name.clone(), doc));
                    return;
                }
            }
        }
        unused.push(doc);
    };

    for (idx, c) in comments.iter().enumerate() {
        let is_standalone = standalone_lines.contains(&c.location.line);
        if is_standalone && c.has_doc_comment_prefix() {
            if let Some(prev) = block.last() {
                if c.location.line != prev.location.line + 1 {
                    flush_block(
                        &mut block,
                        None,
                        &mut attached,
                        &mut attached_names,
                        &mut unused,
                    );
                }
            }
            block.push(c.clone());
        } else {
            let plain_line = if is_standalone {
                Some(c.location.line)
            } else {
                None
            };
            flush_block(
                &mut block,
                plain_line,
                &mut attached,
                &mut attached_names,
                &mut unused,
            );
            let _ = idx;
        }
    }
    flush_block(
        &mut block,
        None,
        &mut attached,
        &mut attached_names,
        &mut unused,
    );
    (attached, unused)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadStatement {
    pub module_label: String,
    pub symbols: Vec<(String, String)>, // (local_name, exported_name)
    pub symbol_locations: Vec<(Location, Location)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOpKind {
    Not,
    Neg,
    Pos,
    Invert, // ~
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOpKind {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
}

impl CmpOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::In => "in",
            Self::NotIn => "not in",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CompClause {
    For { var_names: Vec<Pattern>, iter: Expr },
    If(Expr),
}

/// A (possibly nested) unpacking pattern, e.g. the `a` / `(b, c)` / `x[0]` of `for a, (b, c) in xs`.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Name(String),
    Tuple(Vec<Pattern>),
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
        location: Location,
    },
    Attr {
        target: Box<Expr>,
        attr: String,
        location: Location,
    },
}

impl Pattern {
    /// All variable names bound by this pattern (in source order, including duplicates).
    pub fn names(&self) -> Vec<String> {
        match self {
            Pattern::Name(n) => vec![n.clone()],
            Pattern::Tuple(ps) => ps.iter().flat_map(|p| p.names()).collect(),
            Pattern::Index { .. } | Pattern::Attr { .. } => Vec::new(),
        }
    }

    /// Insertion-ordered distinct identifiers bound by this pattern (`Identifier.boundIdentifiers`).
    pub fn bound_identifiers(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for n in self.names() {
            if seen.insert(n.clone()) {
                out.push(n);
            }
        }
        out
    }

    pub fn walk_exprs<'a>(&'a self, f: &mut impl FnMut(&'a Expr)) {
        match self {
            Pattern::Name(_) => {}
            Pattern::Tuple(ps) => {
                for p in ps {
                    p.walk_exprs(f);
                }
            }
            Pattern::Index { target, index, .. } => {
                f(target);
                f(index);
            }
            Pattern::Attr { target, .. } => f(target),
        }
    }
}

/// All names bound by a list of patterns.
pub fn pattern_names(ps: &[Pattern]) -> Vec<String> {
    ps.iter().flat_map(|p| p.names()).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssignTarget {
    Ident(String),
    Tuple(Vec<Pattern>),
    Index { target: Box<Expr>, index: Box<Expr> },
    Attr { target: Box<Expr>, attr: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamDef {
    pub name: String,
    pub default: Option<Expr>,
    pub is_args: bool,
    pub is_kwargs: bool,
    pub is_kw_only: bool,
}

/// The global (module-level) bindings of a Starlark module (`BUILD`, `.bzl`, ...).
///
/// A function value captures the module in which it was defined so that, when it is called from a
/// different module (e.g. a macro from a loaded `.bzl` file invoked by a `BUILD` file), free
/// variables resolve against the defining module's globals rather than the caller's.
#[derive(Default)]
pub struct ModuleEnv {
    pub globals: std::sync::Mutex<std::collections::BTreeMap<String, crate::types::value::Value>>,
    /// Names bound at module top level (`def`, `=`, `load` when global), used to report
    /// `global variable '<name>' is referenced before assignment.` if read before assignment.
    pub declared_globals: std::sync::Mutex<std::collections::HashSet<String>>,
    /// For a `.bzl` module: the package it lives in and the repository mapping of its
    /// repository, so that `Label("//x:y")` in a function body resolves against the
    /// defining repository even when the function is called later (e.g. at analysis time).
    pub label_ctx: std::sync::Mutex<
        Option<(
            zel_cmdline::PackageIdentifier,
            zel_cmdline::RepositoryMapping,
        )>,
    >,
    /// Label of the `.bzl` file as Bazel prints it in `<function f from LABEL>` (`//:e.bzl`, `@@repo+//p:f.bzl`).
    pub label_string: std::sync::Mutex<Option<String>>,
    /// Global variable `#:` doc comments in declaration order.
    pub doc_comments: std::sync::Mutex<Vec<(String, DocComments)>>,
    /// Unattached `#:` doc comment blocks in file order.
    pub unused_doc_comments: std::sync::Mutex<Vec<DocComments>>,
}

impl std::fmt::Debug for ModuleEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ModuleEnv(..)")
    }
}

impl PartialEq for ModuleEnv {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

/// A shared, mutable scope of local variables. Frames own one; closures capture them so that inner
/// functions see (and observe later assignments to) variables of their enclosing functions.
pub type Scope =
    Arc<std::sync::Mutex<std::collections::BTreeMap<String, crate::types::value::Value>>>;

/// The enclosing function scopes captured by a nested `def`/`lambda`, innermost first.
#[derive(Clone, Default)]
pub struct ClosureEnv {
    pub scopes: Vec<Scope>,
    pub local_names: Vec<Arc<std::collections::HashSet<String>>>,
}

impl std::fmt::Debug for ClosureEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClosureEnv({})", self.scopes.len())
    }
}

impl PartialEq for ClosureEnv {
    fn eq(&self, other: &Self) -> bool {
        self.scopes.len() == other.scopes.len()
            && self
                .scopes
                .iter()
                .zip(&other.scopes)
                .all(|(a, b)| Arc::ptr_eq(a, b))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserFunctionDef {
    pub name: String,
    pub params: Vec<ParamDef>,
    pub body: Vec<Stmt>,
    pub location: Location,
    /// Module in which this function was defined (`None` for synthetic functions).
    pub module: Option<Arc<ModuleEnv>>,
    /// Custom representation for a synthetic callable such as a first-class bound method.
    pub repr_override: Option<String>,
    /// Enclosing function scopes (empty for module-level functions).
    pub closure: ClosureEnv,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(crate::types::value::Value),
    Ident(String, Location),
    List(Vec<Expr>, Location),
    Tuple(Vec<Expr>, Location),
    /// Entries, the `{` location, and the location of each entry's `:` (where Bazel reports insertion errors).
    Dict(Vec<(Expr, Expr)>, Location, Vec<Location>),
    ListComp {
        body: Box<Expr>,
        clauses: Vec<CompClause>,
        location: Location,
    },
    DictComp {
        key: Box<Expr>,
        value: Box<Expr>,
        clauses: Vec<CompClause>,
        location: Location,
    },
    UnaryOp {
        op: UnaryOpKind,
        expr: Box<Expr>,
        location: Location,
    },
    BinaryOp {
        op: BinaryOpKind,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        location: Location,
    },
    Compare {
        lhs: Box<Expr>,
        comparisons: Vec<(CmpOp, Expr)>,
        location: Location,
    },
    IfExpr {
        test: Box<Expr>,
        body: Box<Expr>,
        orelse: Box<Expr>,
        location: Location,
    },
    Call {
        callee: Box<Expr>,
        pos_args: Vec<Expr>,
        kw_args: Vec<(String, Expr)>,
        var_args: Option<Box<Expr>>,
        kw_var_args: Option<Box<Expr>>,
        location: Location,
    },
    Attr {
        target: Box<Expr>,
        attr: String,
        location: Location,
    },
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
        location: Location,
    },
    Slice {
        target: Box<Expr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        step: Option<Box<Expr>>,
        location: Location,
    },
    Lambda {
        params: Vec<ParamDef>,
        body: Box<Expr>,
        location: Location,
    },
}

impl Expr {
    pub fn location(&self) -> Location {
        match self {
            Self::Literal(_) => Location::default(),
            Self::Ident(_, loc)
            | Self::List(_, loc)
            | Self::Tuple(_, loc)
            | Self::Dict(_, loc, _)
            | Self::ListComp { location: loc, .. }
            | Self::DictComp { location: loc, .. }
            | Self::UnaryOp { location: loc, .. }
            | Self::BinaryOp { location: loc, .. }
            | Self::Compare { location: loc, .. }
            | Self::IfExpr { location: loc, .. }
            | Self::Call { location: loc, .. }
            | Self::Attr { location: loc, .. }
            | Self::Index { location: loc, .. }
            | Self::Slice { location: loc, .. }
            | Self::Lambda { location: loc, .. } => loc.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Assign {
        target: AssignTarget,
        op: AssignOp,
        value: Expr,
        location: Location,
        /// Position of the `=` (or augmented operator) token: where Bazel reports errors of the statement.
        op_location: Location,
    },
    Def(Arc<UserFunctionDef>),
    If {
        branches: Vec<(Expr, Vec<Stmt>)>,
        else_body: Vec<Stmt>,
        location: Location,
    },
    For {
        var_names: Vec<Pattern>,
        iter: Expr,
        body: Vec<Stmt>,
        location: Location,
    },
    Return(Option<Expr>, Location),
    Expr(Expr),
    Pass(Location),
    Break(Location),
    Continue(Location),
    Load(LoadStatement, Location),
}

impl Stmt {
    pub fn location(&self) -> Location {
        match self {
            Self::Assign { location, .. }
            | Self::If { location, .. }
            | Self::For { location, .. }
            | Self::Return(_, location)
            | Self::Pass(location)
            | Self::Break(location)
            | Self::Continue(location)
            | Self::Load(_, location) => location.clone(),
            Self::Def(def) => def.location.clone(),
            Self::Expr(e) => e.location(),
        }
    }
}
