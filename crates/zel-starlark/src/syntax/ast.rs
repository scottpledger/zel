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
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadStatement {
    pub module_label: String,
    pub symbols: Vec<(String, String)>, // (local_name, exported_name)
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
    For {
        var_names: Vec<Pattern>,
        iter: Expr,
    },
    If(Expr),
}

/// A (possibly nested) unpacking pattern, e.g. the `a` / `(b, c)` of `for a, (b, c) in xs`.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Name(String),
    Tuple(Vec<Pattern>),
}

impl Pattern {
    /// All variable names bound by this pattern.
    pub fn names(&self) -> Vec<String> {
        match self {
            Pattern::Name(n) => vec![n.clone()],
            Pattern::Tuple(ps) => ps.iter().flat_map(|p| p.names()).collect(),
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
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
    },
    Attr {
        target: Box<Expr>,
        attr: String,
    },
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
    /// For a `.bzl` module: the package it lives in and the repository mapping of its
    /// repository, so that `Label("//x:y")` in a function body resolves against the
    /// defining repository even when the function is called later (e.g. at analysis time).
    pub label_ctx: std::sync::Mutex<Option<(zel_cmdline::PackageIdentifier, zel_cmdline::RepositoryMapping)>>,
    /// Label of the `.bzl` file as Bazel prints it in `<function f from LABEL>` (`//:e.bzl`, `@@repo+//p:f.bzl`).
    pub label_string: std::sync::Mutex<Option<String>>,
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
pub type Scope = Arc<std::sync::Mutex<std::collections::BTreeMap<String, crate::types::value::Value>>>;

/// The enclosing function scopes captured by a nested `def`/`lambda`, innermost first.
#[derive(Clone, Default)]
pub struct ClosureEnv(pub Vec<Scope>);

impl std::fmt::Debug for ClosureEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClosureEnv({})", self.0.len())
    }
}

impl PartialEq for ClosureEnv {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len() && self.0.iter().zip(&other.0).all(|(a, b)| Arc::ptr_eq(a, b))
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
