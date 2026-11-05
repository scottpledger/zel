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

//! Query language lexer, parser and AST (`Lexer.java`, `QueryParser.java`, `QueryExpression`
//! and its subclasses).

use std::collections::BTreeSet;

/// `QuerySyntaxException`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySyntaxError(pub String);

impl std::fmt::Display for QuerySyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Word,
    Eof,
    Comma,
    Equals,
    LParen,
    Minus,
    Plus,
    RParen,
    Caret,
    In,
    Let,
    Set,
    Intersect,
    Except,
    Union,
}

impl TokenKind {
    pub fn pretty_name(self) -> &'static str {
        match self {
            Self::Word => "word",
            Self::Eof => "EOF",
            Self::Comma => ",",
            Self::Equals => "=",
            Self::LParen => "(",
            Self::Minus => "-",
            Self::Plus => "+",
            Self::RParen => ")",
            Self::Caret => "^",
            Self::In => "in",
            Self::Let => "let",
            Self::Set => "set",
            Self::Intersect => "intersect",
            Self::Except => "except",
            Self::Union => "union",
        }
    }

    pub fn is_binary_operator(self) -> bool {
        matches!(
            self,
            Self::Intersect | Self::Caret | Self::Union | Self::Plus | Self::Except | Self::Minus
        )
    }
}

fn keyword(word: &str) -> Option<TokenKind> {
    Some(match word {
        "in" => TokenKind::In,
        "let" => TokenKind::Let,
        "set" => TokenKind::Set,
        "intersect" => TokenKind::Intersect,
        "except" => TokenKind::Except,
        "union" => TokenKind::Union,
        _ => return None,
    })
}

/// `Lexer.isReservedWord`.
pub fn is_reserved_word(word: &str) -> bool {
    keyword(word).is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub word: Option<String>,
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.kind, &self.word) {
            (TokenKind::Word, Some(w)) => f.write_str(w),
            (k, _) => f.write_str(k.pretty_name()),
        }
    }
}

fn tok(kind: TokenKind) -> Token {
    Token { kind, word: None }
}

/// `Lexer.scan`.
pub fn scan(input: &str) -> Result<Vec<Token>, QuerySyntaxError> {
    let chars: Vec<char> = input.chars().collect();
    let mut pos = 0;
    let mut tokens = Vec::new();
    while pos < chars.len() {
        let c = chars[pos];
        pos += 1;
        match c {
            '(' => tokens.push(tok(TokenKind::LParen)),
            ')' => tokens.push(tok(TokenKind::RParen)),
            ',' => tokens.push(tok(TokenKind::Comma)),
            '+' => tokens.push(tok(TokenKind::Plus)),
            '-' => tokens.push(tok(TokenKind::Minus)),
            '=' => tokens.push(tok(TokenKind::Equals)),
            '^' => tokens.push(tok(TokenKind::Caret)),
            '\n' | ' ' | '\t' | '\r' => {}
            '\'' | '"' => {
                let old = pos - 1;
                let mut closed = None;
                while pos < chars.len() {
                    let ch = chars[pos];
                    pos += 1;
                    if ch == c {
                        closed = Some(pos - 1);
                        break;
                    }
                }
                match closed {
                    Some(end) => tokens.push(Token {
                        kind: TokenKind::Word,
                        word: Some(chars[old + 1..end].iter().collect()),
                    }),
                    None => return Err(QuerySyntaxError("unclosed quotation".into())),
                }
            }
            _ => {
                let old = pos - 1;
                let starts_with_double_at = c == '@' && pos < chars.len() && chars[pos] == '@';
                while pos < chars.len() {
                    let ch = chars[pos];
                    match ch {
                        'a'..='z' | 'A'..='Z' | '0'..='9' | '*' | '/' | '@' | '.' | '-' | '_' | ':' | '$' | '~' | '['
                        | ']' => pos += 1,
                        '+' => {
                            if starts_with_double_at {
                                pos += 1;
                            } else {
                                break;
                            }
                        }
                        _ => {
                            if (ch as u32) <= 0x7F {
                                break;
                            }
                            pos += 1;
                        }
                    }
                }
                let word: String = chars[old..pos].iter().collect();
                match keyword(&word) {
                    Some(k) => tokens.push(tok(k)),
                    None => tokens.push(Token { kind: TokenKind::Word, word: Some(word) }),
                }
            }
        }
    }
    tokens.push(tok(TokenKind::Eof));
    Ok(tokens)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    Expression,
    Word,
    Integer,
}

/// Static description of a query function (`QueryFunction`).
#[derive(Debug, Clone, Copy)]
pub struct FuncSpec {
    pub name: &'static str,
    pub mandatory: usize,
    /// Argument types; if `variadic_word` the last entry repeats forever.
    pub args: &'static [ArgType],
    pub variadic_word: bool,
}

use ArgType::{Expression as E, Integer as I, Word as W};

macro_rules! f {
    ($n:expr, $m:expr, [$($a:expr),*]) => {
        FuncSpec { name: $n, mandatory: $m, args: &[$($a),*], variadic_word: false }
    };
}

pub const ALLPATHS: FuncSpec = f!("allpaths", 2, [E, E]);
pub const ATTR: FuncSpec = f!("attr", 3, [W, W, E]);
pub const BUILDFILES: FuncSpec = f!("buildfiles", 1, [E]);
pub const DEPS: FuncSpec = f!("deps", 1, [E, I]);
pub const EXECUTABLES: FuncSpec = f!("executables", 1, [E]);
pub const FILTER: FuncSpec = f!("filter", 2, [W, E]);
pub const KIND: FuncSpec = f!("kind", 2, [W, E]);
pub const LABELS: FuncSpec = f!("labels", 2, [W, E]);
pub const LOADFILES: FuncSpec = f!("loadfiles", 1, [E]);
pub const RDEPS: FuncSpec = f!("rdeps", 2, [E, E, I]);
pub const SAME_PKG_DIRECT_RDEPS: FuncSpec = f!("same_pkg_direct_rdeps", 1, [E]);
pub const SIBLINGS: FuncSpec = f!("siblings", 1, [E]);
pub const SOME: FuncSpec = f!("some", 1, [E, I]);
pub const SOMEPATH: FuncSpec = f!("somepath", 2, [E, E]);
pub const TESTS: FuncSpec = f!("tests", 1, [E]);
pub const VISIBLE: FuncSpec = f!("visible", 2, [E, E]);
pub const ALLRDEPS: FuncSpec = f!("allrdeps", 1, [E, I]);
pub const RBUILDFILES: FuncSpec =
    FuncSpec { name: "rbuildfiles", mandatory: 1, args: &[W], variadic_word: true };
pub const CONFIG: FuncSpec = f!("config", 2, [E, W]);
pub const INPUTS: FuncSpec = f!("inputs", 1, [W, E]);
pub const OUTPUTS: FuncSpec = f!("outputs", 1, [W, E]);
pub const MNEMONIC: FuncSpec = f!("mnemonic", 1, [W, E]);

/// `QueryEnvironment.DEFAULT_QUERY_FUNCTIONS`.
pub const DEFAULT_FUNCTIONS: &[FuncSpec] = &[
    ALLPATHS,
    ATTR,
    BUILDFILES,
    DEPS,
    EXECUTABLES,
    FILTER,
    KIND,
    LABELS,
    LOADFILES,
    RDEPS,
    SAME_PKG_DIRECT_RDEPS,
    SIBLINGS,
    SOME,
    SOMEPATH,
    TESTS,
    VISIBLE,
];

/// Every function any query flavour knows (used for syntactic checks outside an environment).
pub fn all_functions() -> Vec<FuncSpec> {
    let mut f = DEFAULT_FUNCTIONS.to_vec();
    f.extend([ALLRDEPS, RBUILDFILES, CONFIG, INPUTS, OUTPUTS, MNEMONIC]);
    f
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Intersect,
    Union,
    Except,
}

impl BinOp {
    fn from_token(k: TokenKind) -> Self {
        match k {
            TokenKind::Intersect | TokenKind::Caret => Self::Intersect,
            TokenKind::Union | TokenKind::Plus => Self::Union,
            _ => Self::Except,
        }
    }

    /// `TokenKind.getPrettyName` of the token that was used (the keyword spelling is preserved
    /// by Bazel, so we keep the token kind too).
    fn pretty(k: TokenKind) -> &'static str {
        k.pretty_name()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Expr(Expr),
    Word(String),
    Int(i32),
}

impl std::fmt::Display for Arg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Arg::Word(w) => write!(f, "'{w}'"),
            Arg::Expr(e) => write!(f, "{e}"),
            Arg::Int(i) => write!(f, "{i}"),
        }
    }
}

/// Parsed query expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// A target pattern or `$variable` reference.
    Target(String),
    Set(Vec<String>),
    Let { name: String, var: Box<Expr>, body: Box<Expr> },
    Binary { op: BinOp, kind: TokenKind, operands: Vec<Expr> },
    Func { spec: FuncSpec_, args: Vec<Arg> },
}

/// Wrapper to give [`FuncSpec`] value equality by name.
#[derive(Debug, Clone, Copy)]
pub struct FuncSpec_(pub FuncSpec);

impl PartialEq for FuncSpec_ {
    fn eq(&self, other: &Self) -> bool {
        self.0.name == other.0.name
    }
}
impl Eq for FuncSpec_ {}

impl Expr {
    /// `QueryExpression.collectTargetPatterns`.
    pub fn collect_target_patterns(&self, out: &mut Vec<String>) {
        match self {
            Expr::Target(w) => {
                if !is_valid_var_reference(w) && !out.contains(w) {
                    out.push(w.clone());
                }
            }
            Expr::Set(ws) => {
                for w in ws {
                    if !out.contains(w) {
                        out.push(w.clone());
                    }
                }
            }
            Expr::Let { var, body, .. } => {
                var.collect_target_patterns(out);
                body.collect_target_patterns(out);
            }
            Expr::Binary { operands, .. } => operands.iter().for_each(|o| o.collect_target_patterns(out)),
            Expr::Func { args, .. } => {
                for a in args {
                    if let Arg::Expr(e) = a {
                        e.collect_target_patterns(out);
                    }
                }
            }
        }
    }

    /// Names of all functions used by this expression.
    pub fn function_names(&self, out: &mut BTreeSet<&'static str>) {
        match self {
            Expr::Target(_) | Expr::Set(_) => {}
            Expr::Let { var, body, .. } => {
                var.function_names(out);
                body.function_names(out);
            }
            Expr::Binary { operands, .. } => operands.iter().for_each(|o| o.function_names(out)),
            Expr::Func { spec, args } => {
                out.insert(spec.0.name);
                for a in args {
                    if let Arg::Expr(e) = a {
                        e.function_names(out);
                    }
                }
            }
        }
    }

    /// `QueryExpression.isSomePathFunction`.
    pub fn is_somepath(&self) -> bool {
        matches!(self, Expr::Func { spec, .. } if spec.0.name == "somepath")
    }

    pub fn to_truncated_string(&self) -> String {
        truncate(&self.to_string())
    }
}

/// `QueryExpression.truncate` (`Ascii.truncate(expr, 1000, "[truncated]")`).
pub fn truncate(s: &str) -> String {
    const MAX: usize = 1000;
    const ELLIPSIS: &str = "[truncated]";
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= MAX {
        return s.to_string();
    }
    let keep = MAX - ELLIPSIS.len();
    let mut out: String = chars[..keep].iter().collect();
    out.push_str(ELLIPSIS);
    out
}

pub fn is_valid_var_reference(word: &str) -> bool {
    word.starts_with('$') && word.len() > 1
}

pub fn var_name_from_reference(word: &str) -> &str {
    &word[1..]
}

impl std::fmt::Display for Expr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expr::Target(p) => f.write_str(&quote_word(p)),
            Expr::Set(ws) => {
                let v: Vec<String> = ws.iter().map(|w| quote_word(w)).collect();
                write!(f, "set({})", v.join(" "))
            }
            Expr::Let { name, var, body } => write!(f, "let {name} = {var} in {body}"),
            Expr::Binary { kind, operands, .. } => {
                write!(f, "({}", operands[0])?;
                for o in &operands[1..] {
                    write!(f, " {} {}", BinOp::pretty(*kind), o)?;
                }
                f.write_str(")")
            }
            Expr::Func { spec, args } => {
                let v: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                write!(f, "{}({})", spec.0.name, v.join(", "))
            }
        }
    }
}

/// `TargetLiteral.toString`.
fn quote_word(pattern: &str) -> String {
    let needs_quoting = is_reserved_word(pattern)
        || pattern.is_empty()
        || pattern.starts_with('-')
        || pattern.starts_with('*')
        || pattern.chars().any(|c| " \"#&'()+,;<=>?[]{|}".contains(c));
    if !needs_quoting {
        return pattern.to_string();
    }
    let quote = if pattern.contains('"') { '\'' } else { '"' };
    format!("{quote}{pattern}{quote}")
}

struct Parser<'a> {
    tokens: &'a [Token],
    idx: usize,
    functions: &'a [FuncSpec],
    bound: std::collections::HashMap<String, usize>,
}

/// `QueryParser.parse`.
pub fn parse(query: &str, functions: &[FuncSpec]) -> Result<Expr, QuerySyntaxError> {
    let tokens = scan(query)?;
    let mut p = Parser { tokens: &tokens, idx: 0, functions, bound: Default::default() };
    let expr = p.parse_expression()?;
    if p.cur().kind != TokenKind::Eof {
        return Err(QuerySyntaxError(format!(
            "unexpected token '{}' after query expression '{}'",
            p.cur(),
            expr.to_truncated_string()
        )));
    }
    Ok(expr)
}

impl<'a> Parser<'a> {
    fn cur(&self) -> &Token {
        &self.tokens[self.idx]
    }

    fn next(&mut self) {
        if self.cur().kind != TokenKind::Eof {
            self.idx += 1;
        }
    }

    fn context(&self, from: usize) -> String {
        let max = std::cmp::min(self.tokens.len() - 1, from + 3);
        let mut parts = Vec::new();
        for i in from..max {
            parts.push(self.tokens[i].to_string());
        }
        parts.join(" ")
    }

    fn syntax_error(&self) -> QuerySyntaxError {
        if self.cur().kind == TokenKind::Eof {
            return QuerySyntaxError("premature end of input".into());
        }
        QuerySyntaxError(format!("syntax error at '{}'", self.context(self.idx)))
    }

    fn consume(&mut self, kind: TokenKind) -> Result<Option<String>, QuerySyntaxError> {
        if self.cur().kind != kind {
            return Err(self.syntax_error());
        }
        let w = self.cur().word.clone();
        self.next();
        Ok(w)
    }

    fn parse_expression(&mut self) -> Result<Expr, QuerySyntaxError> {
        let lhs = self.parse_primary()?;
        self.parse_binary_tail(lhs)
    }

    fn parse_binary_tail(&mut self, lhs: Expr) -> Result<Expr, QuerySyntaxError> {
        if !self.cur().kind.is_binary_operator() {
            return Ok(lhs);
        }
        let mut operands = vec![lhs];
        let mut last = self.cur().kind;
        while self.cur().kind.is_binary_operator() {
            let operator = self.cur().kind;
            self.consume(operator)?;
            if operator != last {
                let folded = Expr::Binary {
                    op: BinOp::from_token(last),
                    kind: last,
                    operands: std::mem::take(&mut operands),
                };
                operands.push(folded);
                last = operator;
            }
            operands.push(self.parse_primary()?);
        }
        Ok(Expr::Binary { op: BinOp::from_token(last), kind: last, operands })
    }

    fn validate_target_literal(word: String) -> Result<String, QuerySyntaxError> {
        if word.starts_with('-') || word.starts_with('*') {
            return Err(QuerySyntaxError(format!(
                "target literal must not begin with ({}): {}",
                word.chars().next().unwrap(),
                word
            )));
        }
        Ok(word)
    }

    fn parse_primary(&mut self) -> Result<Expr, QuerySyntaxError> {
        match self.cur().kind {
            TokenKind::Word => {
                let word_idx = self.idx;
                let word = self.consume(TokenKind::Word)?.unwrap_or_default();
                if self.cur().kind == TokenKind::LParen {
                    let Some(spec) = self.functions.iter().find(|f| f.name == word).copied() else {
                        let mut names: Vec<&str> = self.functions.iter().map(|f| f.name).collect();
                        names.sort_unstable();
                        return Err(QuerySyntaxError(format!(
                            "unknown function '{}' at '{}'; expected one of ['{}']",
                            word,
                            self.context(word_idx),
                            names.join("', '")
                        )));
                    };
                    self.parse_function_call(spec)
                } else {
                    Ok(Expr::Target(Self::validate_target_literal(word)?))
                }
            }
            TokenKind::Let => {
                self.consume(TokenKind::Let)?;
                let name = self.consume(TokenKind::Word)?.unwrap_or_default();
                self.consume(TokenKind::Equals)?;
                let var = self.parse_expression()?;
                self.consume(TokenKind::In)?;
                *self.bound.entry(name.clone()).or_insert(0) += 1;
                let body = self.parse_expression();
                match self.bound.get_mut(&name) {
                    Some(v) if *v > 1 => *v -= 1,
                    _ => {
                        self.bound.remove(&name);
                    }
                }
                Ok(Expr::Let { name, var: Box::new(var), body: Box::new(body?) })
            }
            TokenKind::LParen => {
                self.consume(TokenKind::LParen)?;
                let e = self.parse_expression()?;
                self.consume(TokenKind::RParen)?;
                Ok(e)
            }
            TokenKind::Set => {
                self.next();
                self.consume(TokenKind::LParen)?;
                let mut words = Vec::new();
                while self.cur().kind == TokenKind::Word {
                    let w = self.consume(TokenKind::Word)?.unwrap_or_default();
                    words.push(Self::validate_target_literal(w)?);
                }
                self.consume(TokenKind::RParen)?;
                Ok(Expr::Set(words))
            }
            _ => Err(self.syntax_error()),
        }
    }

    fn arg_count_error(&self, spec: &FuncSpec, description: &str) -> QuerySyntaxError {
        QuerySyntaxError(format!(
            "{} arguments to function '{}' at '{}'",
            description,
            spec.name,
            self.context(self.idx)
        ))
    }

    fn parse_function_call(&mut self, spec: FuncSpec) -> Result<Expr, QuerySyntaxError> {
        let mut args = Vec::new();
        let mut token_kind = TokenKind::LParen;
        let mut seen = 0usize;
        let mut i = 0usize;
        loop {
            // Argument type i (cycling for variadic functions).
            let ty = if i < spec.args.len() {
                spec.args[i]
            } else if spec.variadic_word {
                *spec.args.last().unwrap()
            } else {
                break;
            };
            if self.cur().kind == TokenKind::RParen {
                if seen >= spec.mandatory {
                    break;
                }
                return Err(self.arg_count_error(&spec, "too few"));
            }
            self.consume(token_kind)?;
            token_kind = TokenKind::Comma;
            if seen == 0 && self.cur().kind == TokenKind::RParen {
                return Err(self.arg_count_error(&spec, "too few"));
            }
            match ty {
                ArgType::Expression => args.push(Arg::Expr(self.parse_expression()?)),
                ArgType::Word => {
                    let w = self.consume(TokenKind::Word)?.unwrap_or_default();
                    if is_valid_var_reference(&w) {
                        let var = var_name_from_reference(&w);
                        if self.bound.get(var).copied().unwrap_or(0) > 0 {
                            return Err(QuerySyntaxError(format!(
                                "variable '${}' cannot be used as an argument to function '{}'; variable references are only allowed in expression arguments",
                                var, spec.name
                            )));
                        }
                    }
                    args.push(Arg::Word(w));
                }
                ArgType::Integer => {
                    let s = self.consume(TokenKind::Word)?.unwrap_or_default();
                    match parse_java_int(&s) {
                        Some(n) => args.push(Arg::Int(n)),
                        None => {
                            return Err(QuerySyntaxError(format!("expected an integer literal: '{s}'")))
                        }
                    }
                }
            }
            seen += 1;
            i += 1;
        }
        if self.cur().kind == TokenKind::Comma && seen > 0 {
            return Err(self.arg_count_error(&spec, "too many"));
        }
        self.consume(TokenKind::RParen)?;
        Ok(Expr::Func { spec: FuncSpec_(spec), args })
    }
}

/// `Integer.parseInt`.
fn parse_java_int(s: &str) -> Option<i32> {
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<i32>().ok().or_else(|| s.strip_prefix('+').and_then(|r| r.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(q: &str) -> Result<Expr, String> {
        parse(q, DEFAULT_FUNCTIONS).map_err(|e| e.0)
    }

    #[test]
    fn lexes_words_like_bazel() {
        let t = scan("deps(//a:b-c) + @@foo+//x:y").unwrap();
        let kinds: Vec<String> = t.iter().map(|t| t.to_string()).collect();
        assert_eq!(kinds, vec!["deps", "(", "//a:b-c", ")", "+", "@@foo+//x:y", "EOF"]);
        assert_eq!(scan("a-b").unwrap()[0].word.as_deref(), Some("a-b"));
        assert_eq!(scan("'a b'").unwrap()[0].word.as_deref(), Some("a b"));
        assert_eq!(scan("'abc").unwrap_err().0, "unclosed quotation");
        assert_eq!(scan("//a union //b").unwrap()[1].kind, TokenKind::Union);
    }

    #[test]
    fn parses_operators_left_associatively() {
        assert_eq!(p("//a + //b - //c").unwrap().to_string(), "((//a + //b) - //c)");
        assert_eq!(p("//a + //b + //c").unwrap().to_string(), "(//a + //b + //c)");
        assert_eq!(p("//a ^ //b").unwrap().to_string(), "(//a ^ //b)");
        assert_eq!(p("//a union //b except //c").unwrap().to_string(), "((//a union //b) except //c)");
        assert_eq!(p("deps(//a)-//b").unwrap().to_string(), "(deps(//a) - //b)");
    }

    #[test]
    fn parses_functions_let_and_set() {
        assert_eq!(p("deps(//a, 2)").unwrap().to_string(), "deps(//a, 2)");
        assert_eq!(p("kind('.*_test', //a/...)").unwrap().to_string(), "kind('.*_test', //a/...)");
        assert_eq!(p("let x = //a in deps($x)").unwrap().to_string(), "let x = //a in deps($x)");
        assert_eq!(p("set(//a //b)").unwrap().to_string(), "set(//a //b)");
        assert_eq!(p("set()").unwrap().to_string(), "set()");
        assert_eq!(p("some(//a)").unwrap().to_string(), "some(//a)");
        assert_eq!(p("rdeps(//a, //b)").unwrap().to_string(), "rdeps(//a, //b)");
    }

    #[test]
    fn reports_errors_like_bazel() {
        assert_eq!(p("deps(").unwrap_err(), "premature end of input");
        assert_eq!(p("deps()").unwrap_err(), "too few arguments to function 'deps' at ')'");
        assert_eq!(
            p("deps(//a, 1, 2)").unwrap_err(),
            "too many arguments to function 'deps' at ', 2 )'"
        );
        assert_eq!(p("'-//a'").unwrap_err(), "target literal must not begin with (-): -//a");
        assert_eq!(p("'*foo'").unwrap_err(), "target literal must not begin with (*): *foo");
        assert_eq!(p("-//a").unwrap_err(), "syntax error at '- //a'");
        assert!(p("foo(//a)").unwrap_err().starts_with("unknown function 'foo' at 'foo ( //a'; expected one of ['allpaths', 'attr',"));
        assert_eq!(p("//a //b").unwrap_err(), "unexpected token '//b' after query expression '//a'");
        assert_eq!(p("deps(//a, x)").unwrap_err(), "expected an integer literal: 'x'");
        assert_eq!(p("let x = //a in filter($x, //b)").unwrap_err(), "variable '$x' cannot be used as an argument to function 'filter'; variable references are only allowed in expression arguments");
        assert_eq!(p("+ //a").unwrap_err(), "syntax error at '+ //a'");
    }

    #[test]
    fn collects_patterns() {
        let e = p("let x = //a in deps($x) + set(//b //c) - rdeps(//d, //e)").unwrap();
        let mut v = Vec::new();
        e.collect_target_patterns(&mut v);
        assert_eq!(v, vec!["//a", "//b", "//c", "//d", "//e"]);
    }
}
