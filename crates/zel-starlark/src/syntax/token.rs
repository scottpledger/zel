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

use crate::bigint::BigInt;
use crate::syntax::location::Location;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Identifiers and Literals
    Ident(String),
    Int(BigInt),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),

    // Keywords
    Def,
    If,
    Elif,
    Else,
    For,
    In,
    NotIn,
    Return,
    Pass,
    Break,
    Continue,
    Load,
    Lambda,
    And,
    Or,
    Not,
    True,
    False,
    None,

    // Delimiters
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Dot,
    Semicolon,

    // Assignment & Augmented Assignment
    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    SlashSlashAssign,
    PercentAssign,
    AmpAssign,
    PipeAssign,
    CaretAssign,
    ShlAssign,
    ShrAssign,

    // Operators
    Plus,
    Minus,
    Star,
    StarStar,
    Slash,
    SlashSlash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    Shr,

    // Comparisons
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,

    // Indentation & Whitespace
    Newline,
    Indent,
    Dedent,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub location: Location,
}
