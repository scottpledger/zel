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

//! Faithful port of Bazel's `net.starlark.java.syntax` scanner and parser *diagnostics*
//! (`Lexer.java`, `Parser.java`, `FileLocations.java`, `SyntaxError.java`).
//!
//! The parser in `parser.rs` builds the AST that the evaluator runs, but it stops at the first
//! problem and words its messages differently. This module re-implements Bazel's lexer and
//! recursive-descent parser *only* to reproduce its accumulated, recovering error list
//! byte-for-byte (including error recovery, the five-error cap and the Latin-1 column
//! counting Bazel applies to source files). It builds no syntax tree beyond what the
//! parser needs to make decisions (whether an expression is a bare identifier).
//!
//! Not ported: doc comments (`#:`), which never affect diagnostics, and the experimental type
//! syntax (it is disabled in Bazel by default, which makes annotations a syntax error).

use std::fmt;

/// One scanner or parser error: a 1-based line/column and Bazel's message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for SyntaxError {
    /// `file:line:col: message` (`SyntaxError.toString`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}: {}", self.file, self.line, self.column, self.message)
    }
}

/// Parses `src` like `StarlarkFile.parse` and returns every scanner/parser error, in order.
///
/// `src` is treated as Latin-1 (one char per byte), exactly like Bazel's `ParserInput` for
/// files, so columns count bytes.
pub fn parse_errors(file: &str, src: &str) -> Vec<SyntaxError> {
    parse(file, src).errors
}

/// A `Resolver` error that does not depend on name resolution, with the source position at
/// which Bazel's resolver would be traversing when it reports it (`key`, `(line, column)`),
/// so callers can interleave it with name-resolution errors in traversal order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverError {
    pub error: SyntaxError,
    pub key: (usize, usize),
}

/// Result of [`parse`].
#[derive(Debug)]
pub struct ParseOutput {
    /// Scanner and parser errors (what `StarlarkFile.parse` collects).
    pub errors: Vec<SyntaxError>,
    /// Structural `Resolver` errors, in traversal order; meaningful only if `errors` is empty.
    pub resolver_errors: Vec<ResolverError>,
    /// What `DotBazelFileSyntaxChecker` would see (offsets resolved to positions).
    dot_events: Vec<((usize, usize), DotEventInfo)>,
    /// Top-level `load()` statements, in source order.
    pub loads: Vec<LoadInfo>,
}

/// A top-level `load("label", ...)`; the position is that of the label string literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadInfo {
    pub label: String,
    pub line: usize,
    pub column: usize,
    /// Symbols loaded from the module: `(name in the loaded module, line, column)`; the
    /// position is that of the name inside its string literal (where Bazel reports a missing
    /// symbol).
    pub symbols: Vec<(String, usize, usize)>,
}

/// Position-resolved checker events; see [`ParseOutput::dot_bazel_errors`].
#[derive(Debug, Clone)]
enum DotEventInfo {
    Load,
    Def,
    Lambda,
    For,
    If,
    Call(Vec<(bool, (usize, usize), bool)>),
}

impl ParseOutput {
    /// No errors, no loads (used when the port is bypassed).
    pub fn empty() -> Self {
        ParseOutput { errors: Vec::new(), resolver_errors: Vec::new(), dot_events: Vec::new(), loads: Vec::new() }
    }

    /// Port of `DotBazelFileSyntaxChecker` (`allowLiteralStarStarArgs = false`): the errors for
    /// a file of kind `where_` (`"BUILD files"`, ...), in the checker's traversal order.
    pub fn dot_bazel_errors(&self, file: &str, where_: &str, can_load_bzl: bool) -> Vec<SyntaxError> {
        let mut out = Vec::new();
        let mk = |pos: (usize, usize), message: String| SyntaxError {
            file: file.to_string(),
            line: pos.0,
            column: pos.1,
            message,
        };
        for (pos, ev) in &self.dot_events {
            match ev {
                DotEventInfo::Load => {
                    if !can_load_bzl {
                        out.push(mk(*pos, format!("`load` statements may not be used in {where_}")));
                    }
                }
                DotEventInfo::Def | DotEventInfo::Lambda => out.push(mk(
                    *pos,
                    format!(
                        "functions may not be defined in {where_}{}",
                        if can_load_bzl { ". You may move the function to a .bzl file and load it." } else { "." }
                    ),
                )),
                DotEventInfo::For => out.push(mk(
                    *pos,
                    format!(
                        "`for` statements are not allowed in {where_}. You may inline the loop{} or as a last resort use a list comprehension.",
                        if can_load_bzl { ", move it to a function definition (in a .bzl file)," } else { "" }
                    ),
                )),
                DotEventInfo::If => out.push(mk(
                    *pos,
                    format!(
                        "`if` statements are not allowed in {where_}. You may{} use an `if` expression for simple cases.",
                        if can_load_bzl { " move conditional logic to a function definition (in a .bzl file), or" } else { "" }
                    ),
                )),
                DotEventInfo::Call(args) => {
                    for (star_star, at, dict) in args {
                        if *star_star {
                            out.push(mk(*at, format!("**kwargs arguments are not allowed in {where_}. Pass the arguments in explicitly.")));
                            if !*dict {
                                out.push(mk(*at, format!("**kwargs arguments must be a literal dict in {where_}.")));
                            }
                        } else {
                            out.push(mk(*at, format!("*args arguments are not allowed in {where_}. Pass the arguments in explicitly.")));
                        }
                    }
                }
            }
        }
        out
    }
}

/// Parses `src` and returns scanner/parser errors plus the structural resolver errors.
pub fn parse(file: &str, src: &str) -> ParseOutput {
    let buf: Vec<char> = src.as_bytes().iter().map(|&b| b as char).collect();
    let mut p = Parser::new(Lexer::new(file, buf));
    p.parse_file_input();
    let mut rerrs = std::mem::take(&mut p.rerrors);
    // The first pass (load bindings) runs before the traversal; the rest is in source order
    // of the node that triggers the error. A stable sort by key keeps emission order for ties.
    rerrs.sort_by_key(|r| r.0);
    let resolver_errors = rerrs
        .into_iter()
        .map(|(key, off, message)| {
            let (line, column) = p.lexer.location(off);
            let key = if key < 0 { (0, 0) } else { p.lexer.location(key) };
            ResolverError {
                error: SyntaxError { file: file.to_string(), line, column, message },
                key,
            }
        })
        .collect();
    // Pre-order traversal = ascending start offset; for equal offsets the outer node (emitted
    // last by the recursive descent) comes first.
    let mut events: Vec<(isize, usize, DotEvent)> = std::mem::take(&mut p.dot_events)
        .into_iter()
        .enumerate()
        .map(|(i, (off, ev))| (off, i, ev))
        .collect();
    events.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let dot_events = events
        .into_iter()
        .map(|(off, _, ev)| {
            let pos = p.lexer.location(off);
            let info = match ev {
                DotEvent::Load => DotEventInfo::Load,
                DotEvent::Def => DotEventInfo::Def,
                DotEvent::Lambda => DotEventInfo::Lambda,
                DotEvent::For => DotEventInfo::For,
                DotEvent::If => DotEventInfo::If,
                DotEvent::Call(args) => DotEventInfo::Call(
                    args.into_iter().map(|(ss, at, d)| (ss, p.lexer.location(at), d)).collect(),
                ),
            };
            (pos, info)
        })
        .collect();
    let loads = std::mem::take(&mut p.loads)
        .into_iter()
        .map(|(label, at, syms)| {
            let (line, column) = p.lexer.location(at);
            let symbols = syms
                .into_iter()
                .map(|(n, a)| {
                    let (l, c) = p.lexer.location(a);
                    (n, l, c)
                })
                .collect();
            LoadInfo { label, line, column, symbols }
        })
        .collect();
    ParseOutput { errors: p.lexer.errors, resolver_errors, dot_events, loads }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TK {
    /// Java `null` (no token scanned yet).
    Null,
    Ampersand,
    AmpersandEquals,
    And,
    As,
    Assert,
    Break,
    Caret,
    CaretEquals,
    Class,
    Colon,
    Comma,
    Continue,
    Def,
    Del,
    Dot,
    Elif,
    Ellipsis,
    Else,
    Eof,
    Equals,
    EqualsEquals,
    Except,
    Finally,
    Float,
    For,
    From,
    Global,
    Greater,
    GreaterEquals,
    GreaterGreater,
    GreaterGreaterEquals,
    Identifier,
    If,
    Illegal,
    Import,
    In,
    Indent,
    Int,
    Is,
    Lambda,
    LBrace,
    LBracket,
    Less,
    LessEquals,
    LessLess,
    LessLessEquals,
    Load,
    LParen,
    Minus,
    MinusEquals,
    Newline,
    Nonlocal,
    Not,
    NotEquals,
    NotIn,
    Or,
    Outdent,
    Pass,
    Percent,
    PercentEquals,
    Pipe,
    PipeEquals,
    Plus,
    PlusEquals,
    Raise,
    RArrow,
    RBrace,
    RBracket,
    Return,
    RParen,
    Semi,
    Slash,
    SlashEquals,
    SlashSlash,
    SlashSlashEquals,
    Star,
    StarEquals,
    StarStar,
    String,
    Tilde,
    Try,
    While,
    With,
    Yield,
}

impl TK {
    /// `TokenKind.toString()`.
    fn text(self) -> &'static str {
        match self {
            TK::Null => "null",
            TK::Ampersand => "&",
            TK::AmpersandEquals => "&=",
            TK::And => "and",
            TK::As => "as",
            TK::Assert => "assert",
            TK::Break => "break",
            TK::Caret => "^",
            TK::CaretEquals => "^=",
            TK::Class => "class",
            TK::Colon => ":",
            TK::Comma => ",",
            TK::Continue => "continue",
            TK::Def => "def",
            TK::Del => "del",
            TK::Dot => ".",
            TK::Elif => "elif",
            TK::Ellipsis => "...",
            TK::Else => "else",
            TK::Eof => "EOF",
            TK::Equals => "=",
            TK::EqualsEquals => "==",
            TK::Except => "except",
            TK::Finally => "finally",
            TK::Float => "float literal",
            TK::For => "for",
            TK::From => "from",
            TK::Global => "global",
            TK::Greater => ">",
            TK::GreaterEquals => ">=",
            TK::GreaterGreater => ">>",
            TK::GreaterGreaterEquals => ">>=",
            TK::Identifier => "identifier",
            TK::If => "if",
            TK::Illegal => "illegal character",
            TK::Import => "import",
            TK::In => "in",
            TK::Indent => "indent",
            TK::Int => "integer literal",
            TK::Is => "is",
            TK::Lambda => "lambda",
            TK::LBrace => "{",
            TK::LBracket => "[",
            TK::Less => "<",
            TK::LessEquals => "<=",
            TK::LessLess => "<<",
            TK::LessLessEquals => "<<=",
            TK::Load => "load",
            TK::LParen => "(",
            TK::Minus => "-",
            TK::MinusEquals => "-=",
            TK::Newline => "newline",
            TK::Nonlocal => "nonlocal",
            TK::Not => "not",
            TK::NotEquals => "!=",
            TK::NotIn => "not in",
            TK::Or => "or",
            TK::Outdent => "outdent",
            TK::Pass => "pass",
            TK::Percent => "%",
            TK::PercentEquals => "%=",
            TK::Pipe => "|",
            TK::PipeEquals => "|=",
            TK::Plus => "+",
            TK::PlusEquals => "+=",
            TK::Raise => "raise",
            TK::RArrow => "->",
            TK::RBrace => "}",
            TK::RBracket => "]",
            TK::Return => "return",
            TK::RParen => ")",
            TK::Semi => ";",
            TK::Slash => "/",
            TK::SlashEquals => "/=",
            TK::SlashSlash => "//",
            TK::SlashSlashEquals => "//=",
            TK::Star => "*",
            TK::StarEquals => "*=",
            TK::StarStar => "**",
            TK::String => "string literal",
            TK::Tilde => "~",
            TK::Try => "try",
            TK::While => "while",
            TK::With => "with",
            TK::Yield => "yield",
        }
    }
}

fn keyword(id: &str) -> Option<TK> {
    Some(match id {
        "and" => TK::And,
        "as" => TK::As,
        "assert" => TK::Assert,
        "break" => TK::Break,
        "class" => TK::Class,
        "continue" => TK::Continue,
        "def" => TK::Def,
        "del" => TK::Del,
        "elif" => TK::Elif,
        "else" => TK::Else,
        "except" => TK::Except,
        "finally" => TK::Finally,
        "for" => TK::For,
        "from" => TK::From,
        "global" => TK::Global,
        "if" => TK::If,
        "import" => TK::Import,
        "in" => TK::In,
        "is" => TK::Is,
        "lambda" => TK::Lambda,
        "load" => TK::Load,
        "nonlocal" => TK::Nonlocal,
        "not" => TK::Not,
        "or" => TK::Or,
        "pass" => TK::Pass,
        "raise" => TK::Raise,
        "return" => TK::Return,
        "try" => TK::Try,
        "while" => TK::While,
        "with" => TK::With,
        "yield" => TK::Yield,
        _ => return None,
    })
}

/// The scanner state's token value (`Lexer.value`), as far as error messages need it.
#[derive(Clone, Debug)]
enum Val {
    None,
    /// STRING / IDENTIFIER / ILLEGAL text.
    Text(String),
    /// Display form of an INT or FLOAT (`Number.toString()`).
    Num(String),
}

struct Lexer {
    file: String,
    // `FileLocations`
    linestart: Vec<usize>,
    size: usize,
    buffer: Vec<char>,
    pos: usize,
    kind: TK,
    start: isize,
    end: isize,
    value: Val,
    errors: Vec<SyntaxError>,
    indent_stack: Vec<usize>,
    open_paren_depth: usize,
    check_indentation: bool,
    dents: i64,
}

fn isdigit(c: i32) -> bool {
    (b'0' as i32..=b'9' as i32).contains(&c)
}
fn isxdigit(c: i32) -> bool {
    isdigit(c) || (b'A' as i32..=b'F' as i32).contains(&c) || (b'a' as i32..=b'f' as i32).contains(&c)
}
fn isbdigit(c: i32) -> bool {
    c == b'0' as i32 || c == b'1' as i32
}

/// `Double.toString` for finite values.
fn java_double_to_string(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let a = v.abs();
    if (1e-3..1e7).contains(&a) {
        let s = format!("{v}");
        if s.contains('.') { s } else { format!("{s}.0") }
    } else {
        // Scientific: d.dddE±n using the shortest round-trip digits.
        let s = format!("{v:e}"); // e.g. 1.5e10, 1e-7
        let (mant, exp) = s.split_once('e').unwrap();
        let mant = if mant.contains('.') { mant.to_string() } else { format!("{mant}.0") };
        format!("{mant}E{exp}")
    }
}

impl Lexer {
    fn new(file: &str, buffer: Vec<char>) -> Self {
        let mut linestart = vec![0, 0];
        for (i, &c) in buffer.iter().enumerate() {
            if c == '\n' {
                linestart.push(i + 1);
            }
        }
        Lexer {
            file: file.to_string(),
            linestart,
            size: buffer.len(),
            buffer,
            pos: 0,
            kind: TK::Null,
            start: 0,
            end: 0,
            value: Val::None,
            errors: Vec::new(),
            indent_stack: vec![0],
            open_paren_depth: 0,
            check_indentation: true,
            dents: 0,
        }
    }

    /// `FileLocations.getLocation`.
    fn location(&self, offset: isize) -> (usize, usize) {
        let off = offset.clamp(0, self.size as isize) as usize;
        // Greatest line >= 1 whose start <= off.
        let mut line = 1;
        for l in 1..self.linestart.len() {
            if self.linestart[l] <= off {
                line = l;
            } else {
                break;
            }
        }
        (line, off - self.linestart[line] + 1)
    }

    fn error(&mut self, message: impl Into<String>, pos: isize) {
        let (line, column) = self.location(pos);
        self.errors.push(SyntaxError {
            file: self.file.clone(),
            line,
            column,
            message: message.into(),
        });
    }

    fn set_token(&mut self, kind: TK, start: isize, end: isize) {
        self.kind = kind;
        self.start = start;
        self.end = end;
        self.value = Val::None;
    }

    fn peek(&self, i: usize) -> i32 {
        if self.pos + i < self.buffer.len() {
            self.buffer[self.pos + i] as i32
        } else {
            -1
        }
    }

    fn next_c(&mut self) -> i32 {
        self.pos += 1;
        self.peek(0)
    }

    fn slice(&self, start: usize, end: usize) -> String {
        self.buffer[start..end].iter().collect()
    }

    fn next_token(&mut self) {
        let after_newline = self.kind == TK::Newline;
        self.tokenize();
        if self.kind == TK::Eof && !after_newline {
            self.kind = TK::Newline;
        }
    }

    fn pop_paren(&mut self) {
        if self.open_paren_depth == 0 {
            let p = self.pos as isize - 1;
            self.error("indentation error", p);
        } else {
            self.open_paren_depth -= 1;
        }
    }

    fn newline(&mut self) {
        if self.open_paren_depth > 0 {
            while self.pos < self.buffer.len() {
                match self.buffer[self.pos] {
                    ' ' | '\t' | '\r' => self.pos += 1,
                    _ => return,
                }
            }
        } else {
            self.check_indentation = true;
            let p = self.pos as isize;
            self.set_token(TK::Newline, p - 1, p);
        }
    }

    fn scan_to_newline(&mut self) {
        while self.pos < self.buffer.len() && self.buffer[self.pos] != '\n' {
            self.pos += 1;
        }
    }

    fn compute_indentation(&mut self) {
        let mut indent_len: usize = 0;
        while self.pos < self.buffer.len() {
            let c = self.buffer[self.pos];
            if c == ' ' {
                indent_len += 1;
                self.pos += 1;
            } else if c == '\r' {
                self.pos += 1;
            } else if c == '\t' {
                indent_len += 1;
                self.pos += 1;
                let p = self.pos as isize;
                self.error("Tab characters are not allowed for indentation. Use spaces instead.", p);
            } else if c == '\n' {
                indent_len = 0;
                self.pos += 1;
            } else if c == '#' {
                self.scan_to_newline();
                indent_len = 0;
            } else {
                break;
            }
        }
        if self.pos == self.buffer.len() {
            indent_len = 0;
        }
        let mut peeked = *self.indent_stack.last().unwrap();
        if peeked < indent_len {
            self.indent_stack.push(indent_len);
            self.dents += 1;
        } else if peeked > indent_len {
            while peeked > indent_len {
                self.indent_stack.pop();
                self.dents -= 1;
                peeked = *self.indent_stack.last().unwrap();
            }
            if peeked < indent_len {
                let p = self.pos as isize - 1;
                self.error("indentation error", p);
            }
        }
    }

    fn skip_triple_quote(&mut self, quot: char) -> bool {
        if self.peek(0) == quot as i32 && self.peek(1) == quot as i32 {
            self.pos += 2;
            true
        } else {
            false
        }
    }

    fn escaped_string_literal(&mut self, quot: char, is_raw: bool) {
        let literal_start = if is_raw { self.pos as isize - 2 } else { self.pos as isize - 1 };
        let in_triple = self.skip_triple_quote(quot);
        let mut literal = String::new();
        while self.pos < self.buffer.len() {
            let mut c = self.buffer[self.pos];
            self.pos += 1;
            match c {
                '\n' => {
                    if in_triple {
                        literal.push(c);
                    } else {
                        self.error("unclosed string literal", literal_start);
                        self.set_token(TK::String, literal_start, self.pos as isize);
                        self.value = Val::Text(literal);
                        return;
                    }
                }
                '\\' => {
                    if self.pos == self.buffer.len() {
                        self.error("unclosed string literal", literal_start);
                        self.set_token(TK::String, literal_start, self.pos as isize);
                        self.value = Val::Text(literal);
                        return;
                    }
                    if is_raw {
                        literal.push('\\');
                        if self.peek(0) == '\r' as i32 && self.peek(1) == '\n' as i32 {
                            literal.push('\n');
                            self.pos += 2;
                        } else if self.buffer[self.pos] == '\r' || self.buffer[self.pos] == '\n' {
                            literal.push('\n');
                            self.pos += 1;
                        } else {
                            literal.push(self.buffer[self.pos]);
                            self.pos += 1;
                        }
                        continue;
                    }
                    c = self.buffer[self.pos];
                    self.pos += 1;
                    match c {
                        '\r' => {
                            if self.peek(0) == '\n' as i32 {
                                self.pos += 1;
                            }
                        }
                        '\n' => {}
                        'a' => literal.push('\u{7}'),
                        'b' => literal.push('\u{8}'),
                        'f' => literal.push('\u{c}'),
                        'n' => literal.push('\n'),
                        'r' => literal.push('\r'),
                        't' => literal.push('\t'),
                        'v' => literal.push('\u{b}'),
                        '\\' => literal.push('\\'),
                        '\'' => literal.push('\''),
                        '"' => literal.push('"'),
                        '0'..='7' => {
                            let mut octal = c as u32 - '0' as u32;
                            if self.pos < self.buffer.len() {
                                let c1 = self.buffer[self.pos];
                                if ('0'..='7').contains(&c1) {
                                    self.pos += 1;
                                    octal = (octal << 3) | (c1 as u32 - '0' as u32);
                                    if self.pos < self.buffer.len() {
                                        let c2 = self.buffer[self.pos];
                                        if ('0'..='7').contains(&c2) {
                                            self.pos += 1;
                                            octal = (octal << 3) | (c2 as u32 - '0' as u32);
                                        }
                                    }
                                }
                            }
                            if octal > 0xff {
                                let p = self.pos as isize - 1;
                                self.error("octal escape sequence out of range (maximum is \\377)", p);
                            }
                            literal.push(char::from_u32(octal & 0xff).unwrap());
                        }
                        _ => {
                            let p = self.pos as isize - 1;
                            self.error(
                                format!("invalid escape sequence: \\{c}. Use '\\\\' to insert '\\'."),
                                p,
                            );
                            literal.push('\\');
                            literal.push(c);
                        }
                    }
                }
                '\'' | '"' => {
                    if c != quot || (in_triple && !self.skip_triple_quote(quot)) {
                        literal.push(c);
                    } else {
                        self.set_token(TK::String, literal_start, self.pos as isize);
                        self.value = Val::Text(literal);
                        return;
                    }
                }
                _ => literal.push(c),
            }
        }
        self.error("unclosed string literal", literal_start);
        self.set_token(TK::String, literal_start, self.pos as isize);
        self.value = Val::Text(literal);
    }

    fn string_literal(&mut self, quot: char, is_raw: bool) {
        let literal_start = if is_raw { self.pos as isize - 2 } else { self.pos as isize - 1 };
        let content_start = self.pos;
        if self.skip_triple_quote(quot) {
            self.pos -= 2;
            self.escaped_string_literal(quot, is_raw);
            return;
        }
        while self.pos < self.buffer.len() {
            let c = self.buffer[self.pos];
            self.pos += 1;
            match c {
                '\n' => {
                    self.error("unclosed string literal", literal_start);
                    self.set_token(TK::String, literal_start, self.pos as isize);
                    self.value = Val::Text(self.slice(content_start, self.pos - 1));
                    return;
                }
                '\\' => {
                    if is_raw {
                        if self.peek(0) == '\r' as i32 && self.peek(1) == '\n' as i32 {
                            self.pos = content_start;
                            self.escaped_string_literal(quot, true);
                            return;
                        } else {
                            self.pos += 1;
                        }
                    } else {
                        self.pos = content_start;
                        self.escaped_string_literal(quot, false);
                        return;
                    }
                }
                '\'' | '"' => {
                    if c == quot {
                        self.set_token(TK::String, literal_start, self.pos as isize);
                        self.value = Val::Text(self.slice(content_start, self.pos - 1));
                        return;
                    }
                }
                _ => {}
            }
        }
        if self.pos > self.buffer.len() {
            self.pos = self.buffer.len();
        }
        self.error("unclosed string literal", literal_start);
        self.set_token(TK::String, literal_start, self.pos as isize);
        self.value = Val::Text(self.slice(content_start, self.pos));
    }

    fn identifier_or_keyword(&mut self) {
        let old = self.pos - 1;
        while self.pos < self.buffer.len()
            && (self.buffer[self.pos] == '_' || self.buffer[self.pos].is_ascii_alphanumeric())
        {
            self.pos += 1;
        }
        let id = self.slice(old, self.pos);
        match keyword(&id) {
            None => {
                self.set_token(TK::Identifier, old as isize, self.pos as isize);
                self.value = Val::Text(id);
            }
            Some(k) => self.set_token(k, old as isize, self.pos as isize),
        }
    }

    fn tokenize_two_chars(&mut self) -> bool {
        if self.pos + 2 >= self.buffer.len() {
            return false;
        }
        let c1 = self.buffer[self.pos];
        let c2 = self.buffer[self.pos + 1];
        let tok = if c2 == '=' {
            match c1 {
                '=' => Some(TK::EqualsEquals),
                '!' => Some(TK::NotEquals),
                '>' => Some(TK::GreaterEquals),
                '<' => Some(TK::LessEquals),
                '+' => Some(TK::PlusEquals),
                '-' => Some(TK::MinusEquals),
                '*' => Some(TK::StarEquals),
                '/' => Some(TK::SlashEquals),
                '%' => Some(TK::PercentEquals),
                '^' => Some(TK::CaretEquals),
                '&' => Some(TK::AmpersandEquals),
                '|' => Some(TK::PipeEquals),
                _ => None,
            }
        } else if c2 == '*' && c1 == '*' {
            Some(TK::StarStar)
        } else {
            None
        };
        match tok {
            None => false,
            Some(t) => {
                let p = self.pos as isize;
                self.set_token(t, p, p + 2);
                true
            }
        }
    }

    fn tokenize(&mut self) {
        if self.check_indentation {
            self.check_indentation = false;
            self.compute_indentation();
        }
        if self.dents != 0 {
            let p = self.pos as isize;
            if self.dents < 0 {
                self.dents += 1;
                self.set_token(TK::Outdent, p - 1, p);
            } else {
                self.dents -= 1;
                self.set_token(TK::Indent, p - 1, p);
            }
            return;
        }

        self.kind = TK::Null;
        while self.pos < self.buffer.len() {
            if self.tokenize_two_chars() {
                self.pos += 2;
                return;
            }
            let c = self.buffer[self.pos];
            self.pos += 1;
            let p = self.pos as isize;
            match c {
                '{' => {
                    self.set_token(TK::LBrace, p - 1, p);
                    self.open_paren_depth += 1;
                }
                '}' => {
                    self.set_token(TK::RBrace, p - 1, p);
                    self.pop_paren();
                }
                '(' => {
                    self.set_token(TK::LParen, p - 1, p);
                    self.open_paren_depth += 1;
                }
                ')' => {
                    self.set_token(TK::RParen, p - 1, p);
                    self.pop_paren();
                }
                '[' => {
                    self.set_token(TK::LBracket, p - 1, p);
                    self.open_paren_depth += 1;
                }
                ']' => {
                    self.set_token(TK::RBracket, p - 1, p);
                    self.pop_paren();
                }
                '>' => {
                    if self.peek(0) == '>' as i32 && self.peek(1) == '=' as i32 {
                        self.set_token(TK::GreaterGreaterEquals, p - 1, p + 2);
                        self.pos += 2;
                    } else if self.peek(0) == '>' as i32 {
                        self.set_token(TK::GreaterGreater, p - 1, p + 1);
                        self.pos += 1;
                    } else {
                        self.set_token(TK::Greater, p - 1, p);
                    }
                }
                '<' => {
                    if self.peek(0) == '<' as i32 && self.peek(1) == '=' as i32 {
                        self.set_token(TK::LessLessEquals, p - 1, p + 2);
                        self.pos += 2;
                    } else if self.peek(0) == '<' as i32 {
                        self.set_token(TK::LessLess, p - 1, p + 1);
                        self.pos += 1;
                    } else {
                        self.set_token(TK::Less, p - 1, p);
                    }
                }
                ':' => self.set_token(TK::Colon, p - 1, p),
                ',' => self.set_token(TK::Comma, p - 1, p),
                '+' => self.set_token(TK::Plus, p - 1, p),
                '-' => {
                    if self.peek(0) == '>' as i32 {
                        self.set_token(TK::RArrow, p - 1, p + 1);
                        self.pos += 1;
                    } else {
                        self.set_token(TK::Minus, p - 1, p);
                    }
                }
                '|' => self.set_token(TK::Pipe, p - 1, p),
                '=' => self.set_token(TK::Equals, p - 1, p),
                '%' => self.set_token(TK::Percent, p - 1, p),
                '~' => self.set_token(TK::Tilde, p - 1, p),
                '&' => self.set_token(TK::Ampersand, p - 1, p),
                '^' => self.set_token(TK::Caret, p - 1, p),
                '/' => {
                    if self.peek(0) == '/' as i32 && self.peek(1) == '=' as i32 {
                        self.set_token(TK::SlashSlashEquals, p - 1, p + 2);
                        self.pos += 2;
                    } else if self.peek(0) == '/' as i32 {
                        self.set_token(TK::SlashSlash, p - 1, p + 1);
                        self.pos += 1;
                    } else {
                        self.set_token(TK::Slash, p - 1, p);
                    }
                }
                ';' => self.set_token(TK::Semi, p - 1, p),
                '*' => self.set_token(TK::Star, p - 1, p),
                ' ' | '\t' | '\r' => {}
                '\\' => {
                    if self.peek(0) == '\n' as i32 {
                        self.pos += 1;
                    } else if self.peek(0) == '\r' as i32 && self.peek(1) == '\n' as i32 {
                        self.pos += 2;
                    } else {
                        self.set_token(TK::Illegal, p - 1, p);
                        self.value = Val::Text(c.to_string());
                    }
                }
                '\n' => self.newline(),
                '#' => self.scan_to_newline(),
                '\'' | '"' => self.string_literal(c, false),
                _ => {
                    if c == 'r' {
                        let c0 = self.peek(0);
                        if c0 == '\'' as i32 || c0 == '"' as i32 {
                            self.pos += 1;
                            self.string_literal(char::from_u32(c0 as u32).unwrap(), true);
                            if self.kind != TK::Null {
                                return;
                            }
                            continue;
                        }
                    }
                    if c == '.' || c.is_ascii_digit() {
                        self.pos -= 1;
                        self.scan_number_or_dot_or_ellipsis(c as i32);
                    } else if c.is_ascii_alphabetic() || c == '_' {
                        self.identifier_or_keyword();
                    } else {
                        self.error(format!("invalid character: '{c}'"), p - 1);
                    }
                }
            }
            if self.kind != TK::Null {
                return;
            }
        }

        let p = self.pos as isize;
        if self.indent_stack.len() > 1 {
            self.set_token(TK::Newline, p - 1, p);
            while self.indent_stack.len() > 1 {
                self.indent_stack.pop();
                self.dents -= 1;
            }
            return;
        }
        self.set_token(TK::Eof, p, p);
    }

    fn scan_number_or_dot_or_ellipsis(&mut self, c0: i32) {
        let mut c = c0;
        let start = self.pos;
        let mut fraction = false;
        let mut exponent = false;

        if c == '.' as i32 {
            if !isdigit(self.peek(1)) {
                if self.peek(1) == '.' as i32 && self.peek(2) == '.' as i32 {
                    self.pos += 3;
                    self.set_token(TK::Ellipsis, start as isize, self.pos as isize);
                    return;
                } else {
                    self.pos += 1;
                    self.set_token(TK::Dot, start as isize, self.pos as isize);
                    return;
                }
            }
            fraction = true;
        } else if c == '0' as i32 {
            c = self.next_c();
            if c == '.' as i32 {
                fraction = true;
            } else if c == 'x' as i32 || c == 'X' as i32 {
                c = self.next_c();
                if !isxdigit(c) {
                    self.error("invalid hex literal", start as isize);
                }
                while isxdigit(c) {
                    c = self.next_c();
                }
            } else if c == 'o' as i32 || c == 'O' as i32 {
                c = self.next_c();
                while isdigit(c) {
                    c = self.next_c();
                }
            } else if c == 'b' as i32 || c == 'B' as i32 {
                c = self.next_c();
                if !isbdigit(c) {
                    self.error("invalid binary literal", start as isize);
                }
                while isbdigit(c) {
                    c = self.next_c();
                }
            } else {
                while isdigit(c) {
                    c = self.next_c();
                }
                if c == '.' as i32 {
                    fraction = true;
                } else if c == 'e' as i32 || c == 'E' as i32 {
                    exponent = true;
                }
            }
        } else {
            while isdigit(c) {
                c = self.next_c();
            }
            if c == '.' as i32 {
                fraction = true;
            } else if c == 'e' as i32 || c == 'E' as i32 {
                exponent = true;
            }
        }

        if fraction {
            c = self.next_c();
            while isdigit(c) {
                c = self.next_c();
            }
            if c == 'e' as i32 || c == 'E' as i32 {
                exponent = true;
            }
        }
        if exponent {
            c = self.next_c();
            if c == '+' as i32 || c == '-' as i32 {
                c = self.next_c();
            }
            while isdigit(c) {
                c = self.next_c();
            }
        }

        if fraction || exponent {
            self.set_token(TK::Float, start as isize, self.pos as isize);
            let raw = self.slice(start, self.pos);
            let mut value = 0.0f64;
            match java_parse_double(&raw) {
                Some(v) => {
                    value = v;
                    if !v.is_finite() {
                        self.error("floating-point literal too large", start as isize);
                    }
                }
                None => self.error("invalid float literal", start as isize),
            }
            self.value = Val::Num(java_double_to_string(value));
            return;
        }

        self.set_token(TK::Int, start as isize, self.pos as isize);
        let literal = self.slice(start, self.pos);
        let mut shown = "0".to_string();
        match int_literal_scan(&literal) {
            Ok(v) => shown = v,
            Err(msg) => self.error(msg, start as isize),
        }
        self.value = Val::Num(shown);
    }
}

/// Validates the float forms the scanner can produce like `Double.parseDouble` and parses them.
fn java_parse_double(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut int_digits = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        int_digits += 1;
    }
    let mut frac_digits = 0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            frac_digits += 1;
        }
    }
    if int_digits + frac_digits == 0 {
        return None;
    }
    let mant_end = i;
    let mut exp = String::new();
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        let mut e = String::from("e");
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            e.push(b[i] as char);
            i += 1;
        }
        let ds = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if ds == i {
            return None;
        }
        e.push_str(&s[ds..i]);
        exp = e;
    }
    if i != b.len() {
        return None;
    }
    let mant = &s[..mant_end];
    let (ip, fp) = match mant.split_once('.') {
        Some((a, b)) => (a, b),
        None => (mant, ""),
    };
    let norm = format!(
        "{}.{}{}",
        if ip.is_empty() { "0" } else { ip },
        if fp.is_empty() { "0" } else { fp },
        exp
    );
    norm.parse::<f64>().ok()
}

/// `IntLiteral.scan`: returns the decimal display of the value, or Java's exception message.
fn int_literal_scan(lit: &str) -> Result<String, String> {
    let mut s = lit;
    let mut radix = 10u32;
    if s.len() > 1 && s.as_bytes()[0] == b'0' {
        match s.as_bytes()[1] {
            b'x' | b'X' => {
                radix = 16;
                s = &s[2..];
            }
            b'o' | b'O' => {
                radix = 8;
                s = &s[2..];
            }
            _ => {
                return Err(format!(
                    "invalid octal literal: {} (use '0o{}')",
                    s,
                    &s[1..]
                ))
            }
        }
    }
    if s.is_empty() || !s.chars().all(|c| c.is_digit(radix)) {
        return Err(format!("invalid base-{radix} integer literal: {lit}"));
    }
    match u128::from_str_radix(s, radix) {
        Ok(v) => Ok(v.to_string()),
        // Larger than 128 bits: only reachable by absurd literals; show the raw digits.
        Err(_) => Ok(s.to_string()),
    }
}

/// Minimal expression result: the parser only needs to know about bare identifiers.
enum Ex {
    Ident(String),
    /// A list or tuple expression (`ListExpression`).
    List,
    /// A dict display (`DictExpression`; comprehensions are `Other`).
    Dict,
    Other,
}

/// One call argument, for `Resolver.visit(CallExpression)`.
enum ArgKind {
    Positional,
    Keyword(String),
    Star,
    /// `**value`; the flag is whether the value is a dict display.
    StarStar(bool),
}

/// A node the `DotBazelFileSyntaxChecker` looks at, recorded in parse order together with the
/// start offset of the node.
enum DotEvent {
    Load,
    Def,
    Lambda,
    For,
    If,
    /// Call with its `*args` / `**kwargs` arguments `(is_star_star, start offset, dict literal)`.
    Call(Vec<(bool, isize, bool)>),
}

/// One function parameter, for `Resolver.resolveFunction`.
enum ParamKind {
    Mandatory(String),
    Optional(String),
    /// `*` (None) or `*args`.
    Star(Option<String>),
    StarStar(String),
}

const ERROR_LIMIT: usize = 5;

struct Parser {
    lexer: Lexer,
    errors_count: usize,
    recovery_mode: bool,
    /// Structural resolver errors: `(traversal key offset (< 0: first pass), error offset, message)`.
    rerrors: Vec<(isize, isize, String)>,
    /// Nesting of `def` bodies (`locals.syntax instanceof StarlarkFile` is `func_depth == 0`).
    func_depth: usize,
    /// `Resolver.loopCount`.
    loop_depth: usize,
    /// Depth inside nodes the dot-bazel checker does not descend into (def, lambda, for, if).
    pruned: usize,
    /// `(start offset, event)` for the checker, in emission order.
    dot_events: Vec<(isize, DotEvent)>,
    /// Nesting of suites (`def`/`for`/`if` bodies).
    suite_depth: usize,
    /// Top-level loads: `(label, offset of the label literal)`.
    loads: Vec<(String, isize, Vec<(String, isize)>)>,
}

fn in_set(set: &[TK], k: TK) -> bool {
    set.contains(&k)
}

const STATEMENT_TERMINATOR_SET: &[TK] = &[TK::Eof, TK::Newline, TK::Semi];
const LIST_TERMINATOR_SET: &[TK] = &[TK::Eof, TK::RBracket, TK::Semi];
const DICT_TERMINATOR_SET: &[TK] = &[TK::Eof, TK::RBrace, TK::Semi];
const EXPR_LIST_TERMINATOR_SET: &[TK] = &[
    TK::Eof,
    TK::Newline,
    TK::Equals,
    TK::RBrace,
    TK::RBracket,
    TK::RParen,
    TK::Semi,
];
const EXPR_TERMINATOR_SET: &[TK] = &[
    TK::Colon,
    TK::Comma,
    TK::Eof,
    TK::For,
    TK::Minus,
    TK::Percent,
    TK::Plus,
    TK::RBracket,
    TK::RParen,
    TK::Slash,
];

fn operator_precedence(prec: usize) -> &'static [TK] {
    match prec {
        0 => &[TK::Or],
        1 => &[TK::And],
        2 => &[TK::Not],
        3 => &[
            TK::EqualsEquals,
            TK::NotEquals,
            TK::Less,
            TK::LessEquals,
            TK::Greater,
            TK::GreaterEquals,
            TK::In,
            TK::NotIn,
        ],
        4 => &[TK::Pipe],
        5 => &[TK::Caret],
        6 => &[TK::Ampersand],
        7 => &[TK::GreaterGreater, TK::LessLess],
        8 => &[TK::Minus, TK::Plus],
        9 => &[TK::Slash, TK::SlashSlash, TK::Star, TK::Percent],
        _ => &[],
    }
}
const PREC_COUNT: usize = 10;

fn is_augmented_assignment(k: TK) -> bool {
    matches!(
        k,
        TK::PlusEquals
            | TK::MinusEquals
            | TK::StarEquals
            | TK::SlashEquals
            | TK::SlashSlashEquals
            | TK::PercentEquals
            | TK::AmpersandEquals
            | TK::CaretEquals
            | TK::PipeEquals
            | TK::GreaterGreaterEquals
            | TK::LessLessEquals
    )
}

fn is_forbidden_keyword(k: TK) -> bool {
    matches!(
        k,
        TK::As
            | TK::Assert
            | TK::Class
            | TK::Del
            | TK::Except
            | TK::Finally
            | TK::From
            | TK::Global
            | TK::Import
            | TK::Is
            | TK::Nonlocal
            | TK::Raise
            | TK::Try
            | TK::With
            | TK::While
            | TK::Yield
    )
}

impl Parser {
    fn new(lexer: Lexer) -> Self {
        let mut p = Parser {
            lexer,
            errors_count: 0,
            recovery_mode: false,
            rerrors: Vec::new(),
            func_depth: 0,
            loop_depth: 0,
            pruned: 0,
            dot_events: Vec::new(),
            suite_depth: 0,
            loads: Vec::new(),
        };
        p.next_token();
        p
    }

    fn kind(&self) -> TK {
        self.lexer.kind
    }

    /// `Parser.tokenString`.
    fn token_string(kind: TK, value: &Val) -> String {
        match (kind, value) {
            (TK::String, Val::Text(t)) => format!("\"{t}\""),
            (_, Val::None) => kind.text().to_string(),
            (_, Val::Text(t)) | (_, Val::Num(t)) => t.clone(),
        }
    }

    fn report_error(&mut self, offset: isize, message: String) {
        self.errors_count += 1;
        if self.errors_count <= ERROR_LIMIT {
            let lx = &mut self.lexer;
            lx.error(message, offset);
        }
    }

    fn syntax_error(&mut self, message: &str) {
        let (start, kind, value) = (self.lexer.start, self.lexer.kind, self.lexer.value.clone());
        self.syntax_error_at(start, kind, &value, message);
    }

    fn syntax_error_at(&mut self, offset: isize, kind: TK, value: &Val, message: &str) {
        if !self.recovery_mode {
            if kind == TK::Indent {
                self.report_error(offset, "indentation error".to_string());
            } else {
                self.report_error(
                    offset,
                    format!("syntax error at '{}': {}", Self::token_string(kind, value), message),
                );
            }
            self.recovery_mode = true;
        }
    }

    fn expect(&mut self, kind: TK) -> isize {
        if self.kind() != kind {
            self.syntax_error(&format!("expected {}", kind.text()));
        }
        self.next_token()
    }

    fn expect_and_recover(&mut self, kind: TK) -> isize {
        if self.kind() != kind {
            self.syntax_error(&format!("expected {}", kind.text()));
        } else {
            self.recovery_mode = false;
        }
        self.next_token()
    }

    fn sync_past(&mut self, terminating: &[TK]) -> isize {
        while !in_set(terminating, self.kind()) {
            self.next_token();
        }
        let end = self.lexer.end;
        self.next_token();
        end
    }

    fn sync_to(&mut self, terminating: &[TK]) -> isize {
        let mut previous = self.lexer.end;
        self.next_token();
        let mut current = previous;
        while !in_set(terminating, self.kind()) {
            self.next_token();
            previous = current;
            current = self.lexer.end;
        }
        previous
    }

    fn check_forbidden_keywords(&mut self) {
        let k = self.kind();
        if !is_forbidden_keyword(k) {
            return;
        }
        let msg = match k {
            TK::Assert => "'assert' not supported, use 'fail' instead".to_string(),
            TK::Del => "'del' not supported, use '.pop()' to delete an item from a dictionary or a list".to_string(),
            TK::Import => "'import' not supported, use 'load' instead".to_string(),
            TK::Is => "'is' not supported, use '==' instead".to_string(),
            TK::Raise => "'raise' not supported, use 'fail' instead".to_string(),
            TK::Try => "'try' not supported, all exceptions are fatal".to_string(),
            TK::While => "'while' not supported, use 'for' instead".to_string(),
            _ => format!("keyword '{}' not supported", k.text()),
        };
        let start = self.lexer.start;
        self.report_error(start, msg);
    }

    fn next_token(&mut self) -> isize {
        let prev = self.lexer.start;
        if self.kind() != TK::Eof {
            self.lexer.next_token();
        }
        self.check_forbidden_keywords();
        prev
    }

    fn error_expression(&mut self, start: isize, end: isize) -> Ex {
        // `Resolver.use`: an identifier created by `Parser.makeErrorExpression` reports "contains syntax errors".
        self.rerror(start, start, "contains syntax errors");
        let s = start.max(0) as usize;
        let e = (end.max(0) as usize).min(self.lexer.buffer.len()).max(s.min(self.lexer.buffer.len()));
        let s = s.min(self.lexer.buffer.len());
        Ex::Ident(self.lexer.slice(s, e))
    }

    fn rerror(&mut self, key: isize, off: isize, message: impl Into<String>) {
        self.rerrors.push((key, off, message.into()));
    }

    fn parse_argument(&mut self) -> (ArgKind, isize) {
        let start = self.lexer.start;
        if self.kind() == TK::StarStar {
            self.next_token();
            let v = self.parse_test();
            return (ArgKind::StarStar(matches!(v, Ex::Dict)), start);
        }
        if self.kind() == TK::Star {
            self.next_token();
            self.parse_test();
            return (ArgKind::Star, start);
        }
        let bare_identifier = self.kind() == TK::Identifier;
        let expr = self.parse_test();
        if let Ex::Ident(name) = expr {
            if self.kind() == TK::Equals {
                if !bare_identifier {
                    self.syntax_error("keyword argument must have form name=expr");
                }
                self.next_token();
                self.parse_test();
                return (ArgKind::Keyword(name), start);
            }
        }
        (ArgKind::Positional, start)
    }

    fn parse_parameter(&mut self, def_statement: bool) -> (ParamKind, isize) {
        let start = self.lexer.start;
        // **kwargs
        if self.kind() == TK::StarStar {
            self.next_token();
            let name = self.parse_ident_name();
            if def_statement {
                self.maybe_parse_type_annotation_after(TK::Colon);
            }
            return (ParamKind::StarStar(name), start);
        }
        // * or *args
        if self.kind() == TK::Star {
            self.next_token();
            if self.kind() == TK::Identifier {
                let name = self.parse_ident_name();
                if def_statement {
                    self.maybe_parse_type_annotation_after(TK::Colon);
                }
                return (ParamKind::Star(Some(name)), start);
            }
            return (ParamKind::Star(None), start);
        }
        // name, name: type, name=default
        let name = self.parse_ident_name();
        if def_statement {
            self.maybe_parse_type_annotation_after(TK::Colon);
        }
        if self.kind() == TK::Equals {
            self.next_token();
            self.parse_test();
            return (ParamKind::Optional(name), start);
        }
        (ParamKind::Mandatory(name), start)
    }

    /// `parseIdent`, returning the (possibly error-expression) identifier text.
    fn parse_ident_name(&mut self) -> String {
        match self.parse_ident() {
            Ex::Ident(n) => n,
            _ => String::new(),
        }
    }

    /// `Resolver.resolveFunction`: parameter ordering and duplicate names.
    fn validate_parameters(&mut self, key: isize, params: &[(ParamKind, isize)]) {
        let mut star: Option<(&Option<String>, isize)> = None;
        let mut star_star: Option<&str> = None;
        let mut seen_optional = false;
        let mut kw_only = 0;
        let mut bound: Vec<&str> = Vec::new();
        for (p, at) in params {
            let at = *at;
            match p {
                ParamKind::Mandatory(name) => {
                    if let Some(ss) = star_star {
                        self.rerror(key, at, format!("required parameter {name} may not follow **{ss}"));
                    } else if star.is_some() {
                        kw_only += 1;
                    } else if seen_optional {
                        self.rerror(key, at, format!("required positional parameter {name} may not follow an optional parameter"));
                    }
                    self.bind_param(key, at, name, &mut bound);
                }
                ParamKind::Optional(name) => {
                    seen_optional = true;
                    if let Some(ss) = star_star {
                        self.rerror(key, at, format!("optional parameter may not follow **{ss}"));
                    } else if star.is_some() {
                        kw_only += 1;
                    }
                    self.bind_param(key, at, name, &mut bound);
                }
                ParamKind::Star(n) => {
                    if let Some(ss) = star_star {
                        self.rerror(key, at, format!("* parameter may not follow **{ss}"));
                    } else if star.is_some() {
                        self.rerror(key, at, "multiple * parameters not allowed");
                    } else {
                        star = Some((n, at));
                    }
                }
                ParamKind::StarStar(n) => {
                    if star_star.is_some() {
                        self.rerror(key, at, "multiple ** parameters not allowed");
                    }
                    star_star = Some(n);
                }
            }
        }
        if let Some((name, at)) = star {
            match name {
                Some(n) => self.bind_param(key, at, n, &mut bound),
                None if kw_only == 0 => self.rerror(key, at, "bare * must be followed by keyword-only parameters"),
                None => {}
            }
        }
        if let Some(n) = star_star {
            // The `**kwargs` parameter's own start is the last `**` seen.
            let at = params
                .iter()
                .rev()
                .find(|(p, _)| matches!(p, ParamKind::StarStar(_)))
                .map(|(_, a)| *a)
                .unwrap_or(0);
            self.bind_param(key, at, n, &mut bound);
        }
    }

    fn bind_param<'a>(&mut self, key: isize, at: isize, name: &'a str, bound: &mut Vec<&'a str>) {
        if bound.contains(&name) {
            self.rerror(key, at, format!("duplicate parameter: {name}"));
        } else {
            bound.push(name);
        }
    }

    fn parse_call_suffix(&mut self, call_start: isize) -> Ex {
        self.expect(TK::LParen);
        if self.kind() != TK::RParen {
            let args = self.parse_arguments();
            self.validate_call_arguments(call_start, &args);
            if self.pruned == 0 {
                let stars: Vec<(bool, isize, bool)> = args
                    .iter()
                    .filter_map(|(k, at)| match k {
                        ArgKind::Star => Some((false, *at, false)),
                        ArgKind::StarStar(d) => Some((true, *at, *d)),
                        _ => None,
                    })
                    .collect();
                if !stars.is_empty() {
                    self.dot_events.push((call_start, DotEvent::Call(stars)));
                }
            }
        }
        self.expect(TK::RParen);
        Ex::Other
    }

    /// `Resolver.visit(CallExpression)`: argument ordering and duplicate keywords.
    fn validate_call_arguments(&mut self, key: isize, args: &[(ArgKind, isize)]) {
        let mut seen_varargs = false;
        let mut seen_kwargs = false;
        let mut keywords: Option<Vec<&str>> = None;
        for (arg, at) in args {
            let at = *at;
            match arg {
                ArgKind::Positional => {
                    if seen_varargs {
                        self.rerror(key, at, "positional argument may not follow *args");
                    } else if seen_kwargs {
                        self.rerror(key, at, "positional argument may not follow **kwargs");
                    } else if keywords.is_some() {
                        self.rerror(key, at, "positional argument may not follow keyword argument");
                    }
                }
                ArgKind::Keyword(name) => {
                    if seen_varargs {
                        self.rerror(key, at, format!("keyword argument {name} may not follow *args"));
                    } else if seen_kwargs {
                        self.rerror(key, at, format!("keyword argument {name} may not follow **kwargs"));
                    }
                    let kws = keywords.get_or_insert_with(Vec::new);
                    if kws.contains(&name.as_str()) {
                        self.rerror(key, at, format!("duplicate keyword argument: {name}"));
                    } else {
                        kws.push(name);
                    }
                }
                ArgKind::Star => {
                    if seen_kwargs {
                        self.rerror(key, at, "*args may not follow **kwargs");
                    } else if seen_varargs {
                        self.rerror(key, at, "multiple *args not allowed");
                    }
                    seen_varargs = true;
                }
                ArgKind::StarStar(_) => {
                    if seen_kwargs {
                        self.rerror(key, at, "multiple **kwargs not allowed");
                    }
                    seen_kwargs = true;
                }
            }
        }
    }

    fn parse_arguments(&mut self) -> Vec<(ArgKind, isize)> {
        let mut out = Vec::new();
        let mut seen_arg = false;
        while self.kind() != TK::RParen && self.kind() != TK::Eof {
            if seen_arg {
                if self.kind() == TK::For {
                    self.syntax_error("Starlark does not support Python-style generator expressions");
                }
                self.expect(TK::Comma);
                if self.kind() == TK::RParen {
                    break;
                }
            }
            out.push(self.parse_argument());
            seen_arg = true;
        }
        out
    }

    fn parse_selector_suffix(&mut self, e: Ex) -> Ex {
        self.expect(TK::Dot);
        if self.kind() == TK::Identifier {
            self.parse_ident();
            return Ex::Other;
        }
        self.syntax_error("expected identifier after dot");
        self.sync_to(EXPR_TERMINATOR_SET);
        e
    }

    fn parse_expr_list(&mut self, trailing_comma_allowed: bool) {
        while self.kind() == TK::Comma {
            self.expect(TK::Comma);
            if in_set(EXPR_LIST_TERMINATOR_SET, self.kind()) {
                if !trailing_comma_allowed {
                    let s = self.lexer.start;
                    self.report_error(s, "Trailing comma is allowed only in parenthesized tuples.".into());
                }
                break;
            }
            self.parse_test();
        }
    }

    fn parse_dict_entry_list(&mut self) {
        while self.kind() != TK::RBrace {
            self.parse_dict_entry();
            if self.kind() == TK::Comma {
                self.next_token();
            } else {
                break;
            }
        }
    }

    fn parse_dict_entry(&mut self) {
        self.parse_test();
        self.expect(TK::Colon);
        self.parse_test();
    }

    fn parse_string_literal(&mut self) -> Ex {
        self.next_token();
        if self.kind() == TK::String {
            let s = self.lexer.start;
            self.report_error(s, "Implicit string concatenation is forbidden, use the + operator".into());
        }
        Ex::Other
    }

    fn parse_primary(&mut self) -> Ex {
        match self.kind() {
            TK::Int | TK::Float => {
                self.next_token();
                Ex::Other
            }
            TK::String => self.parse_string_literal(),
            TK::Identifier => self.parse_ident(),
            TK::LBracket => self.parse_list_maker(),
            TK::LBrace => self.parse_dict_expression(),
            TK::LParen => {
                let lparen = self.next_token();
                if self.kind() == TK::RParen {
                    self.next_token();
                    return Ex::List;
                }
                let e = self.parse_test();
                if self.kind() == TK::RParen {
                    self.next_token();
                    return e;
                }
                if self.kind() == TK::Comma {
                    self.parse_expr_list(true);
                    self.expect(TK::RParen);
                    return Ex::List;
                }
                if self.kind() == TK::For {
                    self.syntax_error("Starlark does not support Python-style generator expressions");
                }
                self.expect(TK::RParen);
                let end = self.sync_to(EXPR_TERMINATOR_SET);
                self.error_expression(lparen, end)
            }
            TK::Minus => {
                self.next_token();
                self.parse_primary_with_suffix();
                Ex::Other
            }
            TK::Plus | TK::Tilde => {
                self.next_token();
                self.parse_primary_with_suffix();
                Ex::Other
            }
            TK::Ellipsis => {
                self.syntax_error("ellipsis ('...') is not allowed outside type expressions");
                self.next_token();
                Ex::Other
            }
            _ => {
                let start = self.lexer.start;
                self.syntax_error("expected expression");
                let end = self.sync_to(EXPR_TERMINATOR_SET);
                self.error_expression(start, end)
            }
        }
    }

    fn parse_primary_with_suffix(&mut self) -> Ex {
        let call_start = self.lexer.start;
        let mut e = self.parse_primary();
        loop {
            match self.kind() {
                TK::Dot => e = self.parse_selector_suffix(e),
                TK::LBracket => e = self.parse_slice_suffix(),
                TK::LParen => e = self.parse_call_suffix(call_start),
                _ => return e,
            }
        }
    }

    fn parse_slice_suffix(&mut self) -> Ex {
        self.expect(TK::LBracket);
        if self.kind() != TK::Colon {
            self.parse_expr();
            if self.kind() == TK::RBracket {
                self.expect(TK::RBracket);
                return Ex::Other;
            }
        }
        self.expect(TK::Colon);
        if self.kind() != TK::Colon && self.kind() != TK::RBracket {
            self.parse_test();
        }
        if self.kind() == TK::Colon {
            self.expect(TK::Colon);
            if self.kind() != TK::RBracket {
                self.parse_test();
            }
        }
        self.expect(TK::RBracket);
        Ex::Other
    }

    fn parse_for_loop_variables(&mut self) {
        self.parse_primary_with_suffix();
        if self.kind() != TK::Comma {
            return;
        }
        while self.kind() == TK::Comma {
            self.expect(TK::Comma);
            if in_set(EXPR_LIST_TERMINATOR_SET, self.kind()) {
                break;
            }
            self.parse_primary_with_suffix();
        }
    }

    fn parse_comprehension_suffix(&mut self, loffset: isize, closing: TK) -> Ex {
        loop {
            if self.kind() == TK::For {
                self.next_token();
                self.parse_for_loop_variables();
                self.expect(TK::In);
                self.parse_test_prec(0);
            } else if self.kind() == TK::If {
                self.next_token();
                self.parse_test_no_cond();
            } else if self.kind() == closing {
                break;
            } else {
                self.syntax_error(&format!("expected '{}', 'for' or 'if'", closing.text()));
                let end = self.sync_past(LIST_TERMINATOR_SET);
                return self.error_expression(loffset, end);
            }
        }
        self.expect(closing);
        Ex::Other
    }

    fn parse_list_maker(&mut self) -> Ex {
        let lbracket = self.expect(TK::LBracket);
        if self.kind() == TK::RBracket {
            self.next_token();
            return Ex::List;
        }
        self.parse_test();
        match self.kind() {
            TK::RBracket => {
                self.next_token();
                Ex::List
            }
            TK::For => self.parse_comprehension_suffix(lbracket, TK::RBracket),
            TK::Comma => {
                self.parse_expr_list(true);
                if self.kind() == TK::RBracket {
                    self.next_token();
                    return Ex::List;
                }
                self.expect(TK::RBracket);
                let end = self.sync_past(LIST_TERMINATOR_SET);
                self.error_expression(lbracket, end)
            }
            _ => {
                self.syntax_error("expected ',', 'for' or ']'");
                let end = self.sync_past(LIST_TERMINATOR_SET);
                self.error_expression(lbracket, end)
            }
        }
    }

    fn parse_dict_expression(&mut self) -> Ex {
        let lbrace = self.expect(TK::LBrace);
        if self.kind() == TK::RBrace {
            self.next_token();
            return Ex::Dict;
        }
        self.parse_dict_entry();
        if self.kind() == TK::For {
            return self.parse_comprehension_suffix(lbrace, TK::RBrace);
        }
        if self.kind() == TK::Comma {
            self.expect(TK::Comma);
            self.parse_dict_entry_list();
        }
        if self.kind() == TK::RBrace {
            self.next_token();
            return Ex::Dict;
        }
        self.expect(TK::RBrace);
        let end = self.sync_past(DICT_TERMINATOR_SET);
        self.error_expression(lbrace, end)
    }

    fn parse_ident(&mut self) -> Ex {
        if self.kind() != TK::Identifier {
            let start = self.lexer.start;
            let end = self.expect(TK::Identifier);
            return self.error_expression(start, end);
        }
        let name = match &self.lexer.value {
            Val::Text(t) => t.clone(),
            _ => String::new(),
        };
        self.next_token();
        Ex::Ident(name)
    }

    fn parse_bin_op_expression(&mut self, prec: usize) -> Ex {
        let mut x = self.parse_test_prec(prec + 1);
        let mut last_op: Option<TK> = None;
        loop {
            if self.kind() == TK::Not {
                self.expect(TK::Not);
                if self.kind() != TK::In {
                    self.syntax_error("expected 'in'");
                }
                self.lexer.kind = TK::NotIn;
            }
            let op = self.kind();
            if !in_set(operator_precedence(prec), op) {
                return x;
            }
            if let Some(lo) = last_op {
                if in_set(operator_precedence(prec), TK::EqualsEquals) {
                    let s = self.lexer.start;
                    self.report_error(
                        s,
                        format!(
                            "Operator '{}' is not associative with operator '{}'. Use parens.",
                            lo.text(),
                            op.text()
                        ),
                    );
                }
            }
            self.next_token();
            self.parse_test_prec(prec + 1);
            x = Ex::Other;
            last_op = Some(op);
        }
    }

    /// Type annotations are disabled in Bazel by default: report and do not consume.
    fn maybe_parse_type_annotation_after(&mut self, expected: TK) {
        if self.kind() == expected {
            let (s, k, v) = (self.lexer.start, self.lexer.kind, self.lexer.value.clone());
            self.syntax_error_at(s, k, &v, "type annotations are disallowed");
        }
    }

    fn parse_test(&mut self) -> Ex {
        let start = self.lexer.start;
        if self.kind() == TK::Lambda {
            return self.parse_lambda(true);
        }
        let expr = self.parse_test_prec(0);
        if self.kind() == TK::If {
            self.next_token();
            self.parse_test_prec(0);
            if self.kind() == TK::Else {
                self.next_token();
                self.parse_test();
                return Ex::Other;
            } else {
                self.report_error(
                    start,
                    "missing else clause in conditional expression or semicolon before if".into(),
                );
                return expr;
            }
        }
        expr
    }

    fn parse_test_prec(&mut self, prec: usize) -> Ex {
        if prec >= PREC_COUNT {
            return self.parse_primary_with_suffix();
        }
        if self.kind() == TK::Not && in_set(operator_precedence(prec), TK::Not) {
            self.expect(TK::Not);
            self.parse_test_prec(prec);
            return Ex::Other;
        }
        self.parse_bin_op_expression(prec)
    }

    fn parse_lambda(&mut self, allow_cond: bool) -> Ex {
        let at = self.expect(TK::Lambda);
        self.record_pruning_node(at, DotEvent::Lambda);
        self.parse_parameters(false);
        self.expect(TK::Colon);
        if allow_cond {
            self.parse_test();
        } else {
            self.parse_test_no_cond();
        }
        self.pruned -= 1;
        Ex::Other
    }

    /// Records a node the dot-bazel checker reports without descending into it, and enters it.
    fn record_pruning_node(&mut self, at: isize, ev: DotEvent) {
        if self.pruned == 0 {
            self.dot_events.push((at, ev));
        }
        self.pruned += 1;
    }

    fn parse_test_no_cond(&mut self) -> Ex {
        if self.kind() == TK::Lambda {
            return self.parse_lambda(false);
        }
        self.parse_test_prec(0)
    }

    fn parse_expr(&mut self) -> Ex {
        let e = self.parse_test();
        if self.kind() != TK::Comma {
            return e;
        }
        self.parse_expr_list(false);
        Ex::List
    }

    fn parse_file_input(&mut self) {
        while self.kind() != TK::Eof {
            if self.kind() == TK::Newline {
                self.expect_and_recover(TK::Newline);
            } else if self.recovery_mode {
                self.sync_to(STATEMENT_TERMINATOR_SET);
                self.recovery_mode = false;
            } else {
                self.parse_statement();
            }
        }
    }

    fn parse_statement(&mut self) {
        match self.kind() {
            TK::Def => self.parse_def_statement(),
            TK::If => self.parse_if_statement(),
            TK::For => self.parse_for_statement(),
            _ => self.parse_simple_statement(),
        }
    }

    fn parse_load_statement(&mut self) {
        let load_at = self.expect(TK::Load);
        if self.pruned == 0 {
            self.dot_events.push((load_at, DotEvent::Load));
        }
        if self.func_depth > 0 {
            self.rerror(load_at, load_at, "load statement not at top level");
        }
        let mut bindings: Vec<(String, isize, String, isize)> = Vec::new();
        let loads_before = self.loads.len();
        self.parse_load_statement_inner(&mut bindings);
        if self.loads.len() > loads_before {
            self.loads[loads_before].2 = bindings.iter().map(|b| (b.2.clone(), b.3)).collect();
        }
        // `Resolver.createBindings` (first pass): private symbols and duplicate local names.
        let mut seen: Vec<String> = Vec::new();
        for (local, local_at, orig, orig_at) in bindings {
            if orig.starts_with('_') {
                self.rerror(-1, orig_at, format!("symbol '{orig}' is private and cannot be imported"));
            }
            if seen.contains(&local) {
                self.rerror(-1, local_at, format!("load statement defines '{local}' more than once"));
            } else {
                seen.push(local);
            }
        }
    }

    fn parse_load_statement_inner(&mut self, bindings: &mut Vec<(String, isize, String, isize)>) {
        self.expect(TK::LParen);
        if self.kind() == TK::String && self.suite_depth == 0 {
            if let Val::Text(t) = &self.lexer.value {
                let item = (t.clone(), self.lexer.start, Vec::new());
                self.loads.push(item);
            }
        }
        if self.kind() != TK::String {
            self.expect(TK::String);
            return;
        }
        self.parse_string_literal();
        if self.kind() == TK::RParen {
            self.syntax_error("expected at least one symbol to load");
            return;
        }
        self.expect(TK::Comma);
        self.parse_load_symbol(bindings);
        while self.kind() != TK::RParen && self.kind() != TK::Eof {
            self.expect(TK::Comma);
            if self.kind() == TK::RParen {
                break;
            }
            self.parse_load_symbol(bindings);
        }
        self.expect(TK::RParen);
    }

    fn parse_load_symbol(&mut self, bindings: &mut Vec<(String, isize, String, isize)>) {
        if self.kind() != TK::String && self.kind() != TK::Identifier {
            self.syntax_error("expected either a literal string or an identifier");
            return;
        }
        let name = match &self.lexer.value {
            Val::Text(t) => t.clone(),
            _ => String::new(),
        };
        let name_at = self.lexer.start + if self.kind() == TK::String { 1 } else { 0 };
        let (orig, orig_at);
        if self.kind() == TK::String {
            orig = name.clone();
            orig_at = name_at;
        } else {
            self.expect(TK::Identifier);
            self.expect(TK::Equals);
            if self.kind() != TK::String {
                self.syntax_error("expected string");
                return;
            }
            orig = match &self.lexer.value {
                Val::Text(t) => t.clone(),
                _ => String::new(),
            };
            orig_at = self.lexer.start + 1;
        }
        self.next_token();
        bindings.push((name, name_at, orig, orig_at));
    }

    fn parse_simple_statement(&mut self) {
        self.parse_small_statement();
        while self.kind() == TK::Semi {
            self.next_token();
            if self.kind() == TK::Newline {
                break;
            }
            self.parse_small_statement();
        }
        self.expect_and_recover(TK::Newline);
    }

    fn parse_small_statement(&mut self) {
        if self.kind() == TK::Return {
            let at = self.expect(TK::Return);
            if self.func_depth == 0 {
                self.rerror(at, at, "return statements must be inside a function");
            }
            if !in_set(STATEMENT_TERMINATOR_SET, self.kind()) {
                self.parse_expr();
            }
            return;
        }
        if matches!(self.kind(), TK::Break | TK::Continue | TK::Pass) {
            let kind = self.kind();
            let at = self.next_token();
            if kind != TK::Pass && self.loop_depth == 0 {
                self.rerror(at, at, format!("{} statement must be inside a for loop", kind.text()));
            }
            return;
        }
        if self.kind() == TK::Load {
            self.parse_load_statement();
            return;
        }
        let lhs_start = self.lexer.start;
        let lhs = self.parse_expr();

        // Type alias: `type X = ...`; type syntax is disabled, so only the diagnostics remain.
        if self.kind() == TK::Identifier {
            if let Ex::Ident(n) = &lhs {
                if n == "type" {
                    // For user-friendliness the error is marked as if detected at 'type'.
                    self.parse_type_alias_tail(lhs_start);
                    return;
                }
            }
        }

        self.maybe_parse_type_annotation_after(TK::Colon);

        let k = self.kind();
        if k == TK::Equals || is_augmented_assignment(k) {
            let op_at = self.next_token();
            self.parse_expr();
            if is_augmented_assignment(k) && matches!(lhs, Ex::List) {
                let key = self.lexer.start;
                self.rerror(key, op_at, "cannot perform augmented assignment on a list or tuple expression");
            }
        }
    }

    /// `parseTypeAliasStatementTail`: only the diagnostics (type syntax is disabled).
    fn parse_type_alias_tail(&mut self, type_start: isize) {
        self.syntax_error_at(type_start, TK::Identifier, &Val::Text("type".into()), "type annotations are disallowed");
        self.parse_ident();
        self.parse_optional_type_parameters();
        self.expect(TK::Equals);
        self.parse_test();
    }

    fn parse_if_statement(&mut self) {
        let at = self.expect(TK::If);
        self.record_pruning_node(at, DotEvent::If);
        self.parse_test();
        self.expect(TK::Colon);
        self.parse_suite();
        while self.kind() == TK::Elif {
            self.expect(TK::Elif);
            self.parse_test();
            self.expect(TK::Colon);
            self.parse_suite();
        }
        if self.kind() == TK::Else {
            self.expect(TK::Else);
            self.expect(TK::Colon);
            self.parse_suite();
        }
        self.pruned -= 1;
    }

    fn parse_for_statement(&mut self) {
        let at = self.expect(TK::For);
        self.record_pruning_node(at, DotEvent::For);
        self.parse_for_loop_variables();
        self.expect(TK::In);
        self.parse_expr();
        self.expect(TK::Colon);
        self.loop_depth += 1;
        self.parse_suite();
        self.loop_depth -= 1;
        self.pruned -= 1;
    }

    fn parse_def_statement(&mut self) {
        let at = self.expect(TK::Def);
        self.record_pruning_node(at, DotEvent::Def);
        self.parse_ident();
        self.parse_optional_type_parameters();
        self.expect(TK::LParen);
        self.parse_parameters(true);
        self.expect(TK::RParen);
        self.maybe_parse_type_annotation_after(TK::RArrow);
        self.expect(TK::Colon);
        self.func_depth += 1;
        self.parse_suite();
        self.func_depth -= 1;
        self.pruned -= 1;
    }

    fn parse_optional_type_parameters(&mut self) {
        if self.kind() == TK::LBracket {
            let (s, k, v) = (self.lexer.start, self.lexer.kind, self.lexer.value.clone());
            self.syntax_error_at(s, k, &v, "type annotations are disallowed");
            self.next_token();
            self.parse_type_parameter();
            while self.kind() != TK::RBracket && self.kind() != TK::Eof {
                self.expect(TK::Comma);
                if self.kind() == TK::RBracket {
                    break;
                }
                self.parse_type_parameter();
            }
            self.expect(TK::RBracket);
        }
    }

    fn parse_type_parameter(&mut self) {
        self.parse_ident();
    }

    fn parse_parameters(&mut self, def_statement: bool) {
        let mut has_param = false;
        let mut params: Vec<(ParamKind, isize)> = Vec::new();
        while self.kind() != TK::RParen && self.kind() != TK::Colon && self.kind() != TK::Eof {
            if has_param {
                self.expect(TK::Comma);
                if self.kind() == TK::RParen {
                    break;
                }
            }
            params.push(self.parse_parameter(def_statement));
            has_param = true;
        }
        let key = self.lexer.start;
        self.validate_parameters(key, &params);
    }

    fn parse_suite(&mut self) {
        if self.kind() == TK::Newline {
            self.expect(TK::Newline);
            if self.kind() != TK::Indent {
                let s = self.lexer.start;
                self.report_error(s, "expected an indented block".into());
                return;
            }
            self.expect(TK::Indent);
            self.suite_depth += 1;
            while self.kind() != TK::Outdent && self.kind() != TK::Eof {
                self.parse_statement();
            }
            self.suite_depth -= 1;
            self.expect_and_recover(TK::Outdent);
        } else {
            self.suite_depth += 1;
            self.parse_simple_statement();
            self.suite_depth -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errs(src: &str) -> Vec<String> {
        parse_errors("BUILD", src)
            .into_iter()
            .map(|e| format!("{}:{}: {}", e.line, e.column, e.message))
            .collect()
    }

    #[test]
    fn valid_files_have_no_errors() {
        assert!(errs("").is_empty());
        assert!(errs("cc_library(name = 'a', srcs = glob(['*.cc']) + [\"b\"],)\n").is_empty());
        assert!(errs("load('//a:b.bzl', 'x', y = 'z')\ndef f(a, b = 1, *c, **d):\n  return [i for i in c if i]\n").is_empty());
        assert!(errs("x = {1: 2, 'a': [1, (2, 3)]}\ny = a[1:2:3] if b else -c\n").is_empty());
        assert!(errs("x = 1 # no trailing newline").is_empty());
        assert!(errs("if a:\n  pass\nelif b:\n  pass\nelse:\n  pass\n").is_empty());
    }

    #[test]
    fn is_keyword_reports_both_errors_like_bazel() {
        assert_eq!(
            errs("this is not valid\n"),
            vec![
                "1:6: 'is' not supported, use '==' instead".to_string(),
                "1:6: syntax error at 'is': expected newline".to_string()
            ]
        );
    }

    #[test]
    fn lambda_parameters_take_no_annotations() {
        assert_eq!(errs("x = lambda a: a\n"), Vec::<String>::new());
        assert_eq!(
            errs("def f(x: int):\n    pass\n"),
            vec![
                "1:8: syntax error at ':': type annotations are disallowed".to_string(),
                "2:4: indentation error".to_string()
            ]
        );
    }

    #[test]
    fn parser_errors_are_capped_at_five_scanner_errors_are_not() {
        let src = "a b\nc d\ne f\ng h\ni j\nk l\nm n\n";
        assert_eq!(errs(src).len(), 5);
        let lex = "x = $ $ $ $ $ $ $\n";
        assert!(errs(lex).iter().filter(|e| e.contains("invalid character")).count() == 7);
    }

    #[test]
    fn columns_count_latin1_bytes() {
        // 'é' is two UTF-8 bytes: Bazel decodes files as Latin-1, so two columns, and each byte
        // is its own invalid character.
        let e = parse_errors("BUILD", "x = é\n");
        assert_eq!((e[0].column, e[1].column), (5, 6));
        assert_eq!(e[0].message, "invalid character: '\u{c3}'");
        assert_eq!(e[1].message, "invalid character: '\u{a9}'");
    }

    #[test]
    fn number_literals() {
        assert_eq!(errs("x = 0x\n"), vec!["1:5: invalid hex literal", "1:5: invalid base-16 integer literal: 0x"]);
        assert_eq!(errs("x = 09\n"), vec!["1:5: invalid octal literal: 09 (use '0o9')"]);
        assert_eq!(errs("x = 0b1\n"), vec!["1:5: invalid octal literal: 0b1 (use '0ob1')"]);
        assert_eq!(errs("x = 1e\n"), vec!["1:5: invalid float literal"]);
        assert_eq!(errs("x = 1e999\n"), vec!["1:5: floating-point literal too large"]);
        // Offending float tokens are shown with Java's Double.toString.
        assert_eq!(errs("x = 1 1.5\n"), vec!["1:7: syntax error at '1.5': expected newline"]);
        assert_eq!(errs("x = 1 1e10\n"), vec!["1:7: syntax error at '1.0E10': expected newline"]);
        assert_eq!(java_double_to_string(0.001), "0.001");
        assert_eq!(java_double_to_string(1e-4), "1.0E-4");
        assert_eq!(java_double_to_string(1234567.0), "1234567.0");
        assert_eq!(java_double_to_string(1e7), "1.0E7");
    }

    #[test]
    fn string_token_is_quoted_in_messages() {
        assert_eq!(errs("x = 1 'abc'\n"), vec!["1:7: syntax error at '\"abc\"': expected newline"]);
    }

    #[test]
    fn unterminated_bracket_reports_at_newline() {
        assert_eq!(errs("x = (1\n"), vec!["2:1: syntax error at 'newline': expected )"]);
    }

    fn rerrs(src: &str) -> Vec<String> {
        parse("BUILD", src)
            .resolver_errors
            .into_iter()
            .map(|e| format!("{}:{}: {}", e.error.line, e.error.column, e.error.message))
            .collect()
    }

    #[test]
    fn resolver_call_argument_errors() {
        // Verified against Bazel 9.3.2 (`bazel query` on a BUILD file).
        assert_eq!(rerrs("f(a=1, 2)\n"), vec!["1:8: positional argument may not follow keyword argument"]);
        assert_eq!(rerrs("f(a=1, a=2)\n"), vec!["1:8: duplicate keyword argument: a"]);
        assert_eq!(rerrs("f(*a, b)\n"), vec!["1:7: positional argument may not follow *args"]);
        assert_eq!(rerrs("f(**a, b)\n"), vec!["1:8: positional argument may not follow **kwargs"]);
        assert_eq!(rerrs("f(**a, *b)\n"), vec!["1:8: *args may not follow **kwargs"]);
        assert_eq!(rerrs("f(*a, *b)\n"), vec!["1:7: multiple *args not allowed"]);
        assert_eq!(rerrs("f(**a, **b)\n"), vec!["1:8: multiple **kwargs not allowed"]);
        assert_eq!(rerrs("f(*a, x=1)\n"), vec!["1:7: keyword argument x may not follow *args"]);
        assert!(rerrs("f(1, x=1, *a, **k)\n").is_empty());
    }

    #[test]
    fn resolver_parameter_errors() {
        assert_eq!(rerrs("def f(a, a): pass\n"), vec!["1:10: duplicate parameter: a"]);
        assert_eq!(
            rerrs("def f(a=1, b): pass\n"),
            vec!["1:12: required positional parameter b may not follow an optional parameter"]
        );
        assert_eq!(rerrs("def f(*): pass\n"), vec!["1:7: bare * must be followed by keyword-only parameters"]);
        assert_eq!(rerrs("def f(*a, *b): pass\n"), vec!["1:11: multiple * parameters not allowed"]);
        assert_eq!(rerrs("def f(**k, a): pass\n"), vec!["1:12: required parameter a may not follow **k"]);
        assert!(rerrs("def f(a, b=1, *, c, d=2, **k): pass\n").is_empty());
        assert_eq!(rerrs("x = lambda a, a: 1\n"), vec!["1:15: duplicate parameter: a"]);
    }

    #[test]
    fn resolver_statement_errors() {
        assert_eq!(rerrs("return 1\n"), vec!["1:1: return statements must be inside a function"]);
        assert_eq!(rerrs("break\n"), vec!["1:1: break statement must be inside a for loop"]);
        assert_eq!(rerrs("continue\n"), vec!["1:1: continue statement must be inside a for loop"]);
        assert_eq!(rerrs("a, b += 1\n"), vec!["1:6: cannot perform augmented assignment on a list or tuple expression"]);
        assert_eq!(rerrs("load('//:a.bzl', '_p')\n"), vec!["1:19: symbol '_p' is private and cannot be imported"]);
        assert_eq!(
            rerrs("load('//:a.bzl', 'x', x = 'y')\n"),
            vec!["1:23: load statement defines 'x' more than once"]
        );
        assert_eq!(rerrs("def f():\n  load('//:a.bzl', 'x')\n"), vec!["2:3: load statement not at top level"]);
        assert!(rerrs("for x in y:\n  break\n").is_empty());
    }
}
