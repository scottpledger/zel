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
use crate::syntax::location::Location;
use crate::syntax::token::{Token, TokenKind};
use crate::types::list::StarlarkError;
use crate::types::value::Value;
use std::sync::Arc;

fn token_desc(k: &TokenKind) -> String {
    match k {
        TokenKind::Newline => "'newline'".to_string(),
        TokenKind::Eof => "'end of file'".to_string(),
        TokenKind::Comma => "','".to_string(),
        TokenKind::Colon => "':'".to_string(),
        TokenKind::Semicolon => "';'".to_string(),
        TokenKind::LParen => "'('".to_string(),
        TokenKind::RParen => "')'".to_string(),
        TokenKind::LBracket => "'['".to_string(),
        TokenKind::RBracket => "']'".to_string(),
        TokenKind::LBrace => "'{'".to_string(),
        TokenKind::RBrace => "'}'".to_string(),
        TokenKind::Assign => "'='".to_string(),
        TokenKind::Ident(s) => format!("'{s}'"),
        TokenKind::Str(s) => format!("'{s}'"),
        TokenKind::Int(i) => format!("'{i}'"),
        other => format!("'{other:?}'").to_lowercase(),
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &TokenKind {
        self.tokens
            .get(self.pos)
            .map(|t| &t.kind)
            .unwrap_or(&TokenKind::Eof)
    }

    fn loc(&self) -> Location {
        self.tokens
            .get(self.pos)
            .map(|t| t.location.clone())
            .unwrap_or_default()
    }

    #[allow(dead_code)]
    fn prev_loc(&self) -> Location {
        if self.pos > 0 {
            self.tokens
                .get(self.pos - 1)
                .map(|t| t.location.clone())
                .unwrap_or_default()
        } else {
            self.loc()
        }
    }

    fn next(&mut self) -> Token {
        let tok = self
            .tokens
            .get(self.pos)
            .cloned()
            .unwrap_or_else(|| Token {
                kind: TokenKind::Eof,
                location: Location::default(),
            });
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn eat(&mut self, expected: &TokenKind) -> Result<(), StarlarkError> {
        let loc = self.loc();
        let got = self.next();
        if &got.kind == expected {
            Ok(())
        } else {
            let exp_str = match expected {
                TokenKind::Comma => ",",
                TokenKind::Colon => ":",
                TokenKind::LParen => "(",
                TokenKind::RParen => ")",
                TokenKind::LBracket => "[",
                TokenKind::RBracket => "]",
                TokenKind::LBrace => "{",
                TokenKind::RBrace => "}",
                TokenKind::Assign => "=",
                _ => &token_desc(expected),
            };
            Err(StarlarkError(format!(
                "{loc}: syntax error at {}: expected {exp_str}",
                token_desc(&got.kind)
            )))
        }
    }

    fn skip_newlines(&mut self) {
        while matches!(self.peek(), TokenKind::Newline) {
            self.next();
        }
    }

    pub fn parse_module(&mut self) -> Result<Vec<Stmt>, StarlarkError> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !matches!(self.peek(), TokenKind::Eof | TokenKind::Dedent) {
            self.parse_statement(&mut stmts)?;
            self.skip_newlines();
        }
        Ok(stmts)
    }

    fn parse_suite(&mut self) -> Result<Vec<Stmt>, StarlarkError> {
        self.eat(&TokenKind::Colon)?;
        if matches!(self.peek(), TokenKind::Newline) {
            self.next();
            self.eat(&TokenKind::Indent)?;
            let mut stmts = Vec::new();
            self.skip_newlines();
            while !matches!(self.peek(), TokenKind::Dedent | TokenKind::Eof) {
                self.parse_statement(&mut stmts)?;
                self.skip_newlines();
            }
            if matches!(self.peek(), TokenKind::Dedent) {
                self.next();
            }
            Ok(stmts)
        } else {
            let mut stmts = Vec::new();
            self.parse_simple_statement(&mut stmts)?;
            Ok(stmts)
        }
    }

    fn parse_statement(&mut self, list: &mut Vec<Stmt>) -> Result<(), StarlarkError> {
        match self.peek() {
            TokenKind::Def => list.push(self.parse_def_stmt()?),
            TokenKind::If => list.push(self.parse_if_stmt()?),
            TokenKind::For => list.push(self.parse_for_stmt()?),
            _ => self.parse_simple_statement(list)?,
        }
        Ok(())
    }

    fn parse_simple_statement(&mut self, list: &mut Vec<Stmt>) -> Result<(), StarlarkError> {
        while matches!(self.peek(), TokenKind::Semicolon) {
            self.next();
        }
        if matches!(self.peek(), TokenKind::Newline | TokenKind::Dedent | TokenKind::Eof) {
            if matches!(self.peek(), TokenKind::Newline) {
                self.next();
            }
            return Ok(());
        }
        list.push(self.parse_small_stmt()?);
        while matches!(self.peek(), TokenKind::Semicolon) {
            self.next();
            while matches!(self.peek(), TokenKind::Semicolon) {
                self.next();
            }
            if matches!(self.peek(), TokenKind::Newline | TokenKind::Dedent | TokenKind::Eof) {
                break;
            }
            list.push(self.parse_small_stmt()?);
        }
        if matches!(self.peek(), TokenKind::Newline) {
            self.next();
        }
        Ok(())
    }

    fn parse_def_stmt(&mut self) -> Result<Stmt, StarlarkError> {
        let loc = self.loc();
        self.eat(&TokenKind::Def)?;
        let fn_loc = self.loc();
        let TokenKind::Ident(name) = self.next().kind else {
            return Err(StarlarkError(format!(
                "{fn_loc}: Expected function name after 'def'"
            )));
        };
        self.eat(&TokenKind::LParen)?;
        let mut params = Vec::new();
        let mut saw_star = false;
        while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
            let mut is_args = false;
            let mut is_kwargs = false;
            let mut saw_star_after = false;
            if matches!(self.peek(), TokenKind::Star) {
                self.next();
                if matches!(self.peek(), TokenKind::Comma) {
                    // Bare `*` -> marks subsequent parameters as keyword-only
                    self.next();
                    saw_star = true;
                    continue;
                }
                is_args = true;
                // Parameters after `*args` are keyword-only.
                saw_star_after = true;
            } else if matches!(self.peek(), TokenKind::StarStar) {
                self.next();
                is_kwargs = true;
            }

            let TokenKind::Ident(pname) = self.next().kind else {
                return Err(StarlarkError(format!(
                    "{}: Expected parameter name",
                    self.loc()
                )));
            };

            // Optional type annotation `: Type`
            if matches!(self.peek(), TokenKind::Colon) {
                self.next();
                let _ = self.parse_expr()?;
            }

            let default = if matches!(self.peek(), TokenKind::Assign) {
                self.next();
                Some(self.parse_expr()?)
            } else {
                None
            };

            params.push(ParamDef {
                name: pname,
                default,
                is_args,
                is_kwargs,
                is_kw_only: saw_star && !is_args && !is_kwargs,
            });
            if saw_star_after {
                saw_star = true;
            }

            if matches!(self.peek(), TokenKind::Comma) {
                self.next();
            }
        }
        self.eat(&TokenKind::RParen)?;

        // Optional return type `-> Type`
        // (e.g. `-> None`)
        // In Starlark lexer, `->` is minus followed by gt
        if matches!(self.peek(), TokenKind::Minus)
            && matches!(self.tokens.get(self.pos + 1).map(|t| &t.kind), Some(TokenKind::Gt))
        {
            self.next();
            self.next();
            let _ = self.parse_expr()?;
        }

        let body = self.parse_suite()?;
        Ok(Stmt::Def(Arc::new(UserFunctionDef {
            name,
            params,
            body,
            location: loc,
            module: None,
            repr_override: None,
            closure: ClosureEnv::default(),
        })))
    }

    fn parse_if_stmt(&mut self) -> Result<Stmt, StarlarkError> {
        let loc = self.loc();
        self.eat(&TokenKind::If)?;
        let cond = self.parse_expr()?;
        let body = self.parse_suite()?;
        let mut branches = vec![(cond, body)];
        let mut else_body = Vec::new();
        self.skip_newlines();
        while matches!(self.peek(), TokenKind::Elif) {
            self.next();
            let econd = self.parse_expr()?;
            let ebody = self.parse_suite()?;
            branches.push((econd, ebody));
            self.skip_newlines();
        }
        if matches!(self.peek(), TokenKind::Else) {
            self.next();
            else_body = self.parse_suite()?;
        }
        Ok(Stmt::If {
            branches,
            else_body,
            location: loc,
        })
    }

    fn parse_for_stmt(&mut self) -> Result<Stmt, StarlarkError> {
        let loc = self.loc();
        self.eat(&TokenKind::For)?;
        let var_names = self.parse_var_list()?;
        self.eat(&TokenKind::In)?;
        let iter = self.parse_expr()?;
        let body = self.parse_suite()?;
        Ok(Stmt::For {
            var_names,
            iter,
            body,
            location: loc,
        })
    }

    fn parse_small_stmt(&mut self) -> Result<Stmt, StarlarkError> {
        let loc = self.loc();
        match self.peek().clone() {
            TokenKind::Return => {
                self.next();
                if matches!(
                    self.peek(),
                    TokenKind::Newline | TokenKind::Dedent | TokenKind::Eof | TokenKind::Semicolon
                ) {
                    Ok(Stmt::Return(None, loc))
                } else {
                    Ok(Stmt::Return(Some(self.parse_expr_or_tuple()?), loc))
                }
            }
            TokenKind::Pass => {
                self.next();
                Ok(Stmt::Pass(loc))
            }
            TokenKind::Break => {
                self.next();
                Ok(Stmt::Break(loc))
            }
            TokenKind::Continue => {
                self.next();
                Ok(Stmt::Continue(loc))
            }
            TokenKind::Load => {
                self.next();
                self.eat(&TokenKind::LParen)?;
                let TokenKind::Str(module_label) = self.next().kind else {
                    return Err(StarlarkError(format!(
                        "{}: First argument to load() must be a string literal",
                        self.loc()
                    )));
                };
                if matches!(self.peek(), TokenKind::Comma) {
                    self.next();
                }
                let mut symbols = Vec::new();
                while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                    match self.next().kind {
                        TokenKind::Str(sym) => {
                            symbols.push((sym.clone(), sym));
                        }
                        TokenKind::Ident(alias) => {
                            self.eat(&TokenKind::Assign)?;
                            let TokenKind::Str(orig) = self.next().kind else {
                                return Err(StarlarkError(format!(
                                    "{}: Expected string literal in load alias",
                                    self.loc()
                                )));
                            };
                            symbols.push((alias, orig));
                        }
                        other => {
                            return Err(StarlarkError(format!(
                                "{}: Unexpected token in load(): {other:?}",
                                self.loc()
                            )))
                        }
                    }
                    if matches!(self.peek(), TokenKind::Comma) {
                        self.next();
                    }
                }
                self.eat(&TokenKind::RParen)?;
                Ok(Stmt::Load(
                    LoadStatement {
                        module_label,
                        symbols,
                    },
                    loc,
                ))
            }
            TokenKind::Ident(ref kw)
                if kw == "type"
                    && matches!(self.tokens.get(self.pos + 1).map(|t| &t.kind), Some(TokenKind::Ident(_)))
                    && matches!(self.tokens.get(self.pos + 2).map(|t| &t.kind), Some(TokenKind::Assign)) =>
            {
                // Type alias statement (`type T = struct[...]`): types are not checked; skip.
                self.next();
                self.next();
                self.next();
                let _ = self.parse_expr()?;
                Ok(Stmt::Pass(loc))
            }
            _ => {
                let expr = self.parse_expr_or_tuple()?;

                // Variable annotation: `name: Type` or `name: Type = value`.
                if matches!(self.peek(), TokenKind::Colon) && matches!(expr, Expr::Ident(..)) {
                    self.next();
                    let _ty = self.parse_expr()?;
                    if !matches!(self.peek(), TokenKind::Assign) {
                        return Ok(Stmt::Pass(loc));
                    }
                }

                let assign_op = match self.peek() {
                    TokenKind::Assign => Some(AssignOp::Assign),
                    TokenKind::PlusAssign => Some(AssignOp::Add),
                    TokenKind::MinusAssign => Some(AssignOp::Sub),
                    TokenKind::StarAssign => Some(AssignOp::Mul),
                    TokenKind::SlashAssign => Some(AssignOp::Div),
                    TokenKind::SlashSlashAssign => Some(AssignOp::FloorDiv),
                    TokenKind::PercentAssign => Some(AssignOp::Mod),
                    TokenKind::AmpAssign => Some(AssignOp::BitAnd),
                    TokenKind::PipeAssign => Some(AssignOp::BitOr),
                    TokenKind::CaretAssign => Some(AssignOp::BitXor),
                    TokenKind::ShlAssign => Some(AssignOp::Shl),
                    TokenKind::ShrAssign => Some(AssignOp::Shr),
                    _ => None,
                };

                if let Some(op) = assign_op {
                    let op_location = self.loc();
                    self.next();
                    let target = match expr {
                        Expr::Ident(name, _) => AssignTarget::Ident(name),
                        Expr::Tuple(items, _) | Expr::List(items, _) if op == AssignOp::Assign => {
                            AssignTarget::Tuple(
                                items
                                    .into_iter()
                                    .map(|i| Self::expr_to_pattern(i, &loc))
                                    .collect::<Result<_, _>>()?,
                            )
                        }
                        Expr::Index { target, index, .. } => AssignTarget::Index { target, index },
                        Expr::Attr { target, attr, .. } => AssignTarget::Attr { target, attr },
                        _ => {
                            return Err(StarlarkError(format!(
                                "{loc}: cannot assign to non-lvalue expression"
                            )))
                        }
                    };
                    let rhs = self.parse_expr_or_tuple()?;
                    return Ok(Stmt::Assign {
                        target,
                        op,
                        value: rhs,
                        location: loc,
                        op_location,
                    });
                }

                Ok(Stmt::Expr(expr))
            }
        }
    }

    /// Parses a `for` loop target list: `a`, `a, b`, `(a, b)`, `a, (b, c)`, `[a, b]`.
    fn parse_var_list(&mut self) -> Result<Vec<Pattern>, StarlarkError> {
        let mut vars = vec![self.parse_pattern()?];
        while matches!(self.peek(), TokenKind::Comma) {
            self.next();
            if matches!(self.peek(), TokenKind::In | TokenKind::RParen | TokenKind::RBracket) {
                break;
            }
            vars.push(self.parse_pattern()?);
        }
        // A lone parenthesised/bracketed tuple is the whole target list.
        if vars.len() == 1 {
            if let Pattern::Tuple(inner) = &vars[0] {
                return Ok(inner.clone());
            }
        }
        Ok(vars)
    }

    fn parse_pattern(&mut self) -> Result<Pattern, StarlarkError> {
        let (close, tuple) = match self.peek() {
            TokenKind::LParen => (TokenKind::RParen, true),
            TokenKind::LBracket => (TokenKind::RBracket, true),
            _ => (TokenKind::RParen, false),
        };
        if tuple {
            self.next();
            let mut items = Vec::new();
            while self.peek() != &close {
                items.push(self.parse_pattern()?);
                if matches!(self.peek(), TokenKind::Comma) {
                    self.next();
                } else {
                    break;
                }
            }
            self.eat(&close)?;
            return Ok(Pattern::Tuple(items));
        }
        match self.next().kind {
            TokenKind::Ident(n) => Ok(Pattern::Name(n)),
            _ => Err(StarlarkError(format!("{}: Expected variable name", self.loc()))),
        }
    }

    fn expr_to_pattern(e: Expr, loc: &Location) -> Result<Pattern, StarlarkError> {
        match e {
            Expr::Ident(n, _) => Ok(Pattern::Name(n)),
            Expr::Tuple(items, _) | Expr::List(items, _) => Ok(Pattern::Tuple(
                items.into_iter().map(|i| Self::expr_to_pattern(i, loc)).collect::<Result<_, _>>()?,
            )),
            _ => Err(StarlarkError(format!("{loc}: cannot assign to non-lvalue expression"))),
        }
    }

    /// Parses `expr` or an unparenthesised tuple `expr, expr, ...` (with optional trailing comma).
    fn parse_expr_or_tuple(&mut self) -> Result<Expr, StarlarkError> {
        let loc = self.loc();
        let first = self.parse_expr()?;
        if !matches!(self.peek(), TokenKind::Comma) {
            return Ok(first);
        }
        let mut items = vec![first];
        while matches!(self.peek(), TokenKind::Comma) {
            self.next();
            if matches!(
                self.peek(),
                TokenKind::Newline
                    | TokenKind::Eof
                    | TokenKind::Assign
                    | TokenKind::RBracket
                    | TokenKind::RParen
                    | TokenKind::Semicolon
                    | TokenKind::Dedent
            ) {
                break;
            }
            items.push(self.parse_expr()?);
        }
        Ok(Expr::Tuple(items, loc))
    }

    pub fn parse_expr(&mut self) -> Result<Expr, StarlarkError> {
        if matches!(self.peek(), TokenKind::Lambda) {
            return self.parse_lambda();
        }

        let expr = self.parse_or()?;
        if matches!(self.peek(), TokenKind::If) {
            let loc = self.loc();
            self.next();
            let cond = self.parse_or()?;
            self.eat(&TokenKind::Else)?;
            let else_body = self.parse_expr()?;
            return Ok(Expr::IfExpr {
                test: Box::new(cond),
                body: Box::new(expr),
                orelse: Box::new(else_body),
                location: loc,
            });
        }
        Ok(expr)
    }

    fn parse_lambda(&mut self) -> Result<Expr, StarlarkError> {
        let loc = self.loc();
        self.next(); // lambda
        let mut params = Vec::new();
        let mut saw_star = false;
        while !matches!(self.peek(), TokenKind::Colon | TokenKind::Eof) {
            let is_args = if matches!(self.peek(), TokenKind::Star) {
                self.next();
                if matches!(self.peek(), TokenKind::Comma) {
                    // Bare `*`: subsequent parameters are keyword-only.
                    self.next();
                    saw_star = true;
                    continue;
                }
                true
            } else {
                false
            };
            let is_kwargs = if matches!(self.peek(), TokenKind::StarStar) {
                self.next();
                true
            } else {
                false
            };
            let TokenKind::Ident(name) = self.next().kind else {
                return Err(StarlarkError(format!(
                    "{}: Expected parameter name in lambda",
                    self.loc()
                )));
            };
            let default = if matches!(self.peek(), TokenKind::Assign) {
                self.next();
                Some(self.parse_expr()?)
            } else {
                None
            };
            params.push(ParamDef {
                name,
                default,
                is_args,
                is_kwargs,
                is_kw_only: saw_star && !is_args && !is_kwargs,
            });
            if is_args {
                saw_star = true;
            }
            if matches!(self.peek(), TokenKind::Comma) {
                self.next();
            }
        }
        self.eat(&TokenKind::Colon)?;
        let body = self.parse_expr()?;
        Ok(Expr::Lambda {
            params,
            body: Box::new(body),
            location: loc,
        })
    }

    fn parse_or(&mut self) -> Result<Expr, StarlarkError> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), TokenKind::Or) {
            let loc = self.loc();
            self.next();
            let rhs = self.parse_and()?;
            lhs = Expr::BinaryOp {
                op: BinaryOpKind::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                location: loc,
            };
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, StarlarkError> {
        let mut lhs = self.parse_not()?;
        while matches!(self.peek(), TokenKind::And) {
            let loc = self.loc();
            self.next();
            let rhs = self.parse_not()?;
            lhs = Expr::BinaryOp {
                op: BinaryOpKind::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                location: loc,
            };
        }
        Ok(lhs)
    }

    fn parse_not(&mut self) -> Result<Expr, StarlarkError> {
        if matches!(self.peek(), TokenKind::Not) {
            let loc = self.loc();
            self.next();
            let inner = self.parse_not()?;
            return Ok(Expr::UnaryOp {
                op: UnaryOpKind::Not,
                expr: Box::new(inner),
                location: loc,
            });
        }
        self.parse_cmp()
    }

    fn parse_cmp(&mut self) -> Result<Expr, StarlarkError> {
        let lhs = self.parse_bitor()?;
        let mut comparisons = Vec::new();
        let loc = self.loc();

        loop {
            let op = match self.peek().clone() {
                TokenKind::EqEq => {
                    self.next();
                    Some(CmpOp::Eq)
                }
                TokenKind::NotEq => {
                    self.next();
                    Some(CmpOp::Ne)
                }
                TokenKind::Lt => {
                    self.next();
                    Some(CmpOp::Lt)
                }
                TokenKind::Le => {
                    self.next();
                    Some(CmpOp::Le)
                }
                TokenKind::Gt => {
                    self.next();
                    Some(CmpOp::Gt)
                }
                TokenKind::Ge => {
                    self.next();
                    Some(CmpOp::Ge)
                }
                TokenKind::In => {
                    self.next();
                    Some(CmpOp::In)
                }
                TokenKind::Not => {
                    if matches!(self.tokens.get(self.pos + 1).map(|t| &t.kind), Some(TokenKind::In)) {
                        self.next(); // not
                        self.next(); // in
                        Some(CmpOp::NotIn)
                    } else {
                        None
                    }
                }
                _ => None,
            };

            if let Some(op) = op {
                let rhs = self.parse_bitor()?;
                comparisons.push((op, rhs));
            } else {
                break;
            }
        }

        if comparisons.is_empty() {
            Ok(lhs)
        } else {
            Ok(Expr::Compare {
                lhs: Box::new(lhs),
                comparisons,
                location: loc,
            })
        }
    }

    fn parse_bitor(&mut self) -> Result<Expr, StarlarkError> {
        let mut lhs = self.parse_bitxor()?;
        while matches!(self.peek(), TokenKind::Pipe) {
            let loc = self.loc();
            self.next();
            let rhs = self.parse_bitxor()?;
            lhs = Expr::BinaryOp {
                op: BinaryOpKind::BitOr,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                location: loc,
            };
        }
        Ok(lhs)
    }

    fn parse_bitxor(&mut self) -> Result<Expr, StarlarkError> {
        let mut lhs = self.parse_bitand()?;
        while matches!(self.peek(), TokenKind::Caret) {
            let loc = self.loc();
            self.next();
            let rhs = self.parse_bitand()?;
            lhs = Expr::BinaryOp {
                op: BinaryOpKind::BitXor,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                location: loc,
            };
        }
        Ok(lhs)
    }

    fn parse_bitand(&mut self) -> Result<Expr, StarlarkError> {
        let mut lhs = self.parse_shift()?;
        while matches!(self.peek(), TokenKind::Amp) {
            let loc = self.loc();
            self.next();
            let rhs = self.parse_shift()?;
            lhs = Expr::BinaryOp {
                op: BinaryOpKind::BitAnd,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                location: loc,
            };
        }
        Ok(lhs)
    }

    fn parse_shift(&mut self) -> Result<Expr, StarlarkError> {
        let mut lhs = self.parse_add()?;
        while matches!(self.peek(), TokenKind::Shl | TokenKind::Shr) {
            let loc = self.loc();
            let op = if matches!(self.next().kind, TokenKind::Shl) {
                BinaryOpKind::Shl
            } else {
                BinaryOpKind::Shr
            };
            let rhs = self.parse_add()?;
            lhs = Expr::BinaryOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                location: loc,
            };
        }
        Ok(lhs)
    }

    fn parse_add(&mut self) -> Result<Expr, StarlarkError> {
        let mut lhs = self.parse_mul()?;
        while matches!(self.peek(), TokenKind::Plus | TokenKind::Minus) {
            let loc = self.loc();
            let op = if matches!(self.next().kind, TokenKind::Plus) {
                BinaryOpKind::Add
            } else {
                BinaryOpKind::Sub
            };
            let rhs = self.parse_mul()?;
            lhs = Expr::BinaryOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                location: loc,
            };
        }
        Ok(lhs)
    }

    fn parse_mul(&mut self) -> Result<Expr, StarlarkError> {
        let mut lhs = self.parse_unary()?;
        while matches!(
            self.peek(),
            TokenKind::Star | TokenKind::Slash | TokenKind::SlashSlash | TokenKind::Percent
        ) {
            let loc = self.loc();
            let op = match self.next().kind {
                TokenKind::Star => BinaryOpKind::Mul,
                TokenKind::Slash => BinaryOpKind::Div,
                TokenKind::SlashSlash => BinaryOpKind::FloorDiv,
                _ => BinaryOpKind::Mod,
            };
            let rhs = self.parse_unary()?;
            lhs = Expr::BinaryOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                location: loc,
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, StarlarkError> {
        let loc = self.loc();
        if matches!(self.peek(), TokenKind::Minus) {
            self.next();
            let inner = self.parse_unary()?;
            return Ok(Expr::UnaryOp {
                op: UnaryOpKind::Neg,
                expr: Box::new(inner),
                location: loc,
            });
        }
        if matches!(self.peek(), TokenKind::Plus) {
            self.next();
            let inner = self.parse_unary()?;
            return Ok(Expr::UnaryOp {
                op: UnaryOpKind::Pos,
                expr: Box::new(inner),
                location: loc,
            });
        }
        if matches!(self.peek(), TokenKind::Tilde) {
            self.next();
            let inner = self.parse_unary()?;
            return Ok(Expr::UnaryOp {
                op: UnaryOpKind::Invert,
                expr: Box::new(inner),
                location: loc,
            });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, StarlarkError> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek().clone() {
                TokenKind::LParen => {
                    let loc = self.loc();
                    self.next();
                    let mut pos_args = Vec::new();
                    let mut kw_args = Vec::new();
                    let mut var_args = None;
                    let mut kw_var_args = None;

                    while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                        if matches!(self.peek(), TokenKind::StarStar) {
                            self.next();
                            kw_var_args = Some(Box::new(self.parse_expr()?));
                        } else if matches!(self.peek(), TokenKind::Star) {
                            self.next();
                            var_args = Some(Box::new(self.parse_expr()?));
                        } else if let TokenKind::Ident(ref name) = self.peek().clone() {
                            if matches!(
                                self.tokens.get(self.pos + 1).map(|t| &t.kind),
                                Some(TokenKind::Assign)
                            ) {
                                let arg_name = name.clone();
                                self.next(); // ident
                                self.next(); // =
                                let val = self.parse_expr()?;
                                kw_args.push((arg_name, val));
                            } else {
                                pos_args.push(self.parse_expr()?);
                            }
                        } else {
                            pos_args.push(self.parse_expr()?);
                        }

                        if !matches!(self.peek(), TokenKind::RParen) {
                            self.eat(&TokenKind::Comma)?;
                        }
                    }
                    self.eat(&TokenKind::RParen)?;
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        pos_args,
                        kw_args,
                        var_args,
                        kw_var_args,
                        location: loc,
                    };
                }
                TokenKind::Dot => {
                    let loc = self.loc();
                    self.next();
                    let TokenKind::Ident(attr) = self.next().kind else {
                        return Err(StarlarkError(format!(
                            "{loc}: Expected attribute name after '.'"
                        )));
                    };
                    expr = Expr::Attr {
                        target: Box::new(expr),
                        attr,
                        location: loc,
                    };
                }
                TokenKind::LBracket => {
                    let loc = self.loc();
                    self.next();
                    if matches!(self.peek(), TokenKind::Colon) {
                        // [:end:step]
                        self.next();
                        let end = if matches!(self.peek(), TokenKind::Colon | TokenKind::RBracket) {
                            None
                        } else {
                            Some(Box::new(self.parse_expr()?))
                        };
                        let step = if matches!(self.peek(), TokenKind::Colon) {
                            self.next();
                            if matches!(self.peek(), TokenKind::RBracket) {
                                None
                            } else {
                                Some(Box::new(self.parse_expr()?))
                            }
                        } else {
                            None
                        };
                        self.eat(&TokenKind::RBracket)?;
                        expr = Expr::Slice {
                            target: Box::new(expr),
                            start: None,
                            end,
                            step,
                            location: loc,
                        };
                    } else {
                        let first = self.parse_expr_or_tuple()?;
                        if matches!(self.peek(), TokenKind::Colon) {
                            // [start:end:step]
                            self.next();
                            let end = if matches!(self.peek(), TokenKind::Colon | TokenKind::RBracket) {
                                None
                            } else {
                                Some(Box::new(self.parse_expr()?))
                            };
                            let step = if matches!(self.peek(), TokenKind::Colon) {
                                self.next();
                                if matches!(self.peek(), TokenKind::RBracket) {
                                None
                            } else {
                                Some(Box::new(self.parse_expr()?))
                            }
                            } else {
                                None
                            };
                            self.eat(&TokenKind::RBracket)?;
                            expr = Expr::Slice {
                                target: Box::new(expr),
                                start: Some(Box::new(first)),
                                end,
                                step,
                                location: loc,
                            };
                        } else {
                            // [index]
                            self.eat(&TokenKind::RBracket)?;
                            expr = Expr::Index {
                                target: Box::new(expr),
                                index: Box::new(first),
                                location: loc,
                            };
                        }
                    }
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, StarlarkError> {
        let loc = self.loc();
        match self.next().kind {
            TokenKind::Int(i) => Ok(Expr::Literal(Value::Int(i))),
            TokenKind::Float(f) => Ok(Expr::Literal(Value::Float(f))),
            TokenKind::Str(s) => Ok(Expr::Literal(Value::String(s))),
            TokenKind::True => Ok(Expr::Literal(Value::Bool(true))),
            TokenKind::False => Ok(Expr::Literal(Value::Bool(false))),
            TokenKind::None => Ok(Expr::Literal(Value::None)),
            TokenKind::Ident(name) => Ok(Expr::Ident(name, loc)),
            TokenKind::LParen => {
                if matches!(self.peek(), TokenKind::RParen) {
                    self.next();
                    return Ok(Expr::Tuple(Vec::new(), loc));
                }
                let first = self.parse_expr()?;
                if matches!(self.peek(), TokenKind::Comma) {
                    let mut items = vec![first];
                    while matches!(self.peek(), TokenKind::Comma) {
                        self.next();
                        if matches!(self.peek(), TokenKind::RParen) {
                            break;
                        }
                        items.push(self.parse_expr()?);
                    }
                    self.eat(&TokenKind::RParen)?;
                    Ok(Expr::Tuple(items, loc))
                } else {
                    self.eat(&TokenKind::RParen)?;
                    Ok(first)
                }
            }
            TokenKind::LBracket => {
                if matches!(self.peek(), TokenKind::RBracket) {
                    self.next();
                    return Ok(Expr::List(Vec::new(), loc));
                }
                let first = self.parse_expr()?;
                if matches!(self.peek(), TokenKind::For) {
                    // List comprehension: [body for ... if ... for ... if ...]
                    let clauses = self.parse_comp_clauses()?;
                    self.eat(&TokenKind::RBracket)?;
                    return Ok(Expr::ListComp {
                        body: Box::new(first),
                        clauses,
                        location: loc,
                    });
                }
                let mut items = vec![first];
                while matches!(self.peek(), TokenKind::Comma) {
                    self.next();
                    if matches!(self.peek(), TokenKind::RBracket) {
                        break;
                    }
                    items.push(self.parse_expr()?);
                }
                self.eat(&TokenKind::RBracket)?;
                Ok(Expr::List(items, loc))
            }
            TokenKind::LBrace => {
                if matches!(self.peek(), TokenKind::RBrace) {
                    self.next();
                    return Ok(Expr::Dict(Vec::new(), loc, Vec::new()));
                }
                let k = self.parse_expr()?;
                let colon_loc = self.loc();
                self.eat(&TokenKind::Colon)?;
                let v = self.parse_expr()?;
                if matches!(self.peek(), TokenKind::For) {
                    // Dict comprehension
                    let clauses = self.parse_comp_clauses()?;
                    self.eat(&TokenKind::RBrace)?;
                    return Ok(Expr::DictComp {
                        key: Box::new(k),
                        value: Box::new(v),
                        clauses,
                        location: loc,
                    });
                }
                let mut pairs = vec![(k, v)];
                let mut colons = vec![colon_loc];
                while matches!(self.peek(), TokenKind::Comma) {
                    self.next();
                    if matches!(self.peek(), TokenKind::RBrace) {
                        break;
                    }
                    let nk = self.parse_expr()?;
                    colons.push(self.loc());
                    self.eat(&TokenKind::Colon)?;
                    let nv = self.parse_expr()?;
                    pairs.push((nk, nv));
                }
                self.eat(&TokenKind::RBrace)?;
                Ok(Expr::Dict(pairs, loc, colons))
            }
            other => Err(StarlarkError(format!(
                "{loc}: syntax error at {}: expected expression",
                token_desc(&other)
            ))),
        }
    }

    fn parse_comp_clauses(&mut self) -> Result<Vec<CompClause>, StarlarkError> {
        let mut clauses = Vec::new();
        while matches!(self.peek(), TokenKind::For | TokenKind::If) {
            if matches!(self.peek(), TokenKind::For) {
                self.next();
                let var_names = self.parse_var_list()?;
                self.eat(&TokenKind::In)?;
                let iter = self.parse_or()?;
                clauses.push(CompClause::For { var_names, iter });
            } else if matches!(self.peek(), TokenKind::If) {
                self.next();
                let cond = self.parse_or()?;
                clauses.push(CompClause::If(cond));
            }
        }
        Ok(clauses)
    }
}
