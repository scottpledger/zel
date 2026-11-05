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
use crate::syntax::token::{Token, TokenKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexerError {
    pub message: String,
    pub location: Location,
}

impl std::fmt::Display for LexerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.location, self.message)
    }
}

impl std::error::Error for LexerError {}

pub struct Lexer<'a> {
    file_name: String,
    _src: &'a str,
    chars: Vec<(usize, char)>, // (byte_offset, char)
    pos: usize,
    line: usize,
    col: usize,
    paren_depth: usize,
    indent_stack: Vec<usize>,
    at_line_start: bool,
}

impl<'a> Lexer<'a> {
    pub fn new(file_name: impl Into<String>, src: &'a str) -> Self {
        let chars: Vec<(usize, char)> = src.char_indices().collect();
        Self {
            file_name: file_name.into(),
            _src: src,
            chars,
            pos: 0,
            line: 1,
            col: 1,
            paren_depth: 0,
            indent_stack: vec![0],
            at_line_start: true,
        }
    }

    fn loc(&self) -> Location {
        Location::new(self.file_name.clone(), self.line, self.col)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).map(|&(_, c)| c)
    }

    fn peek_next(&self) -> Option<char> {
        self.chars.get(self.pos + 1).map(|&(_, c)| c)
    }

    fn peek_n(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).map(|&(_, c)| c)
    }

    fn advance(&mut self) -> Option<char> {
        if self.pos < self.chars.len() {
            let (_, c) = self.chars[self.pos];
            self.pos += 1;
            if c == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            Some(c)
        } else {
            None
        }
    }

    pub fn lex_all(&mut self) -> Result<Vec<Token>, LexerError> {
        let mut tokens = Vec::new();

        while self.pos < self.chars.len() {
            if self.at_line_start && self.paren_depth == 0 {
                let mut indent = 0;
                let _start_pos = self.pos;
                while let Some(c) = self.peek() {
                    if c == ' ' {
                        indent += 1;
                        self.advance();
                    } else if c == '\t' {
                        indent += 8 - (indent % 8);
                        self.advance();
                    } else {
                        break;
                    }
                }

                // If blank line or comment line, ignore indentation
                if let Some(c) = self.peek() {
                    if c == '#' || c == '\n' || c == '\r' {
                        if c == '#' {
                            while let Some(ch) = self.peek() {
                                if ch == '\n' {
                                    break;
                                }
                                self.advance();
                            }
                        }
                        if let Some('\r') = self.peek() {
                            self.advance();
                        }
                        if let Some('\n') = self.peek() {
                            self.advance();
                        }
                        continue;
                    }
                } else {
                    // EOF reached
                    break;
                }

                self.at_line_start = false;
                let cur_indent = *self.indent_stack.last().unwrap_or(&0);
                if indent > cur_indent {
                    self.indent_stack.push(indent);
                    tokens.push(Token {
                        kind: TokenKind::Indent,
                        location: self.loc(),
                    });
                } else {
                    while indent < *self.indent_stack.last().unwrap_or(&0) {
                        self.indent_stack.pop();
                        tokens.push(Token {
                            kind: TokenKind::Dedent,
                            location: self.loc(),
                        });
                    }
                    if indent != *self.indent_stack.last().unwrap_or(&0) {
                        return Err(LexerError {
                            message: "unindent does not match any outer indentation level".to_string(),
                            location: self.loc(),
                        });
                    }
                }
            }

            self.at_line_start = false;
            let c = match self.peek() {
                Some(c) => c,
                None => break,
            };

            // Line continuation with backslash: `\` followed by newline
            if c == '\\' {
                let loc = self.loc();
                self.advance();
                if let Some('\r') = self.peek() {
                    self.advance();
                }
                if let Some('\n') = self.peek() {
                    self.advance();
                    continue;
                } else {
                    return Err(LexerError {
                        message: "unexpected character after line continuation '\\'".to_string(),
                        location: loc,
                    });
                }
            }

            // Whitespace inside line
            if c == ' ' || c == '\t' {
                self.advance();
                continue;
            }

            // Comments
            if c == '#' {
                while let Some(ch) = self.peek() {
                    if ch == '\n' {
                        break;
                    }
                    self.advance();
                }
                continue;
            }

            // Newlines
            if c == '\n' || c == '\r' {
                let loc = self.loc();
                if c == '\r' {
                    self.advance();
                }
                if let Some('\n') = self.peek() {
                    self.advance();
                }
                self.at_line_start = true;
                if self.paren_depth == 0 {
                    // Avoid duplicate newlines
                    if !tokens.is_empty() && tokens.last().map(|t| &t.kind) != Some(&TokenKind::Newline) {
                        tokens.push(Token {
                            kind: TokenKind::Newline,
                            location: loc,
                        });
                    }
                }
                continue;
            }

            // String or bytes literals
            if (c == 'r' || c == 'R') && matches!(self.peek_next(), Some('"') | Some('\'')) {
                let loc = self.loc();
                self.advance(); // r
                let s = self.lex_string(true)?;
                tokens.push(Token {
                    kind: TokenKind::Str(s),
                    location: loc,
                });
                continue;
            }
            if (c == 'b' || c == 'B') && matches!(self.peek_next(), Some('"') | Some('\'')) {
                let loc = self.loc();
                self.advance(); // b
                let s = self.lex_string(false)?;
                tokens.push(Token {
                    kind: TokenKind::Bytes(s.into_bytes()),
                    location: loc,
                });
                continue;
            }
            if ((c == 'r' || c == 'R') && (self.peek_next() == Some('b') || self.peek_next() == Some('B')))
                || ((c == 'b' || c == 'B') && (self.peek_next() == Some('r') || self.peek_next() == Some('R')))
            {
                if matches!(self.peek_n(2), Some('"') | Some('\'')) {
                    let loc = self.loc();
                    self.advance(); // r or b
                    self.advance(); // b or r
                    let s = self.lex_string(true)?;
                    tokens.push(Token {
                        kind: TokenKind::Bytes(s.into_bytes()),
                        location: loc,
                    });
                    continue;
                }
            }
            if c == '"' || c == '\'' {
                let loc = self.loc();
                let s = self.lex_string(false)?;
                tokens.push(Token {
                    kind: TokenKind::Str(s),
                    location: loc,
                });
                continue;
            }

            // Numbers: Hex, Octal, Binary, Decimal or Float
            if c.is_ascii_digit() || (c == '.' && self.peek_next().map(|n| n.is_ascii_digit()).unwrap_or(false)) {
                let loc = self.loc();
                let tok = self.lex_number()?;
                tokens.push(Token {
                    kind: tok,
                    location: loc,
                });
                continue;
            }

            // Identifiers and Keywords
            if c.is_ascii_alphabetic() || c == '_' {
                let loc = self.loc();
                let ident = self.lex_ident();
                let kind = match ident.as_str() {
                    "def" => TokenKind::Def,
                    "if" => TokenKind::If,
                    "elif" => TokenKind::Elif,
                    "else" => TokenKind::Else,
                    "for" => TokenKind::For,
                    "in" => TokenKind::In,
                    "return" => TokenKind::Return,
                    "pass" => TokenKind::Pass,
                    "break" => TokenKind::Break,
                    "continue" => TokenKind::Continue,
                    "load" => TokenKind::Load,
                    "lambda" => TokenKind::Lambda,
                    "and" => TokenKind::And,
                    "or" => TokenKind::Or,
                    "not" => TokenKind::Not,
                    "True" => TokenKind::True,
                    "False" => TokenKind::False,
                    "None" => TokenKind::None,
                    _ => TokenKind::Ident(ident),
                };
                tokens.push(Token {
                    kind,
                    location: loc,
                });
                continue;
            }

            // Operators and Delimiters
            let loc = self.loc();
            let tok = match c {
                '(' => {
                    self.advance();
                    self.paren_depth += 1;
                    TokenKind::LParen
                }
                ')' => {
                    self.advance();
                    self.paren_depth = self.paren_depth.saturating_sub(1);
                    TokenKind::RParen
                }
                '[' => {
                    self.advance();
                    self.paren_depth += 1;
                    TokenKind::LBracket
                }
                ']' => {
                    self.advance();
                    self.paren_depth = self.paren_depth.saturating_sub(1);
                    TokenKind::RBracket
                }
                '{' => {
                    self.advance();
                    self.paren_depth += 1;
                    TokenKind::LBrace
                }
                '}' => {
                    self.advance();
                    self.paren_depth = self.paren_depth.saturating_sub(1);
                    TokenKind::RBrace
                }
                ',' => {
                    self.advance();
                    TokenKind::Comma
                }
                ':' => {
                    self.advance();
                    TokenKind::Colon
                }
                ';' => {
                    self.advance();
                    TokenKind::Semicolon
                }
                '.' => {
                    self.advance();
                    TokenKind::Dot
                }
                '+' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::PlusAssign
                    } else {
                        TokenKind::Plus
                    }
                }
                '-' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::MinusAssign
                    } else {
                        TokenKind::Minus
                    }
                }
                '*' => {
                    self.advance();
                    if self.peek() == Some('*') {
                        self.advance();
                        TokenKind::StarStar
                    } else if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::StarAssign
                    } else {
                        TokenKind::Star
                    }
                }
                '/' => {
                    self.advance();
                    if self.peek() == Some('/') {
                        self.advance();
                        if self.peek() == Some('=') {
                            self.advance();
                            TokenKind::SlashSlashAssign
                        } else {
                            TokenKind::SlashSlash
                        }
                    } else if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::SlashAssign
                    } else {
                        TokenKind::Slash
                    }
                }
                '%' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::PercentAssign
                    } else {
                        TokenKind::Percent
                    }
                }
                '&' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::AmpAssign
                    } else {
                        TokenKind::Amp
                    }
                }
                '|' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::PipeAssign
                    } else {
                        TokenKind::Pipe
                    }
                }
                '^' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::CaretAssign
                    } else {
                        TokenKind::Caret
                    }
                }
                '~' => {
                    self.advance();
                    TokenKind::Tilde
                }
                '=' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::EqEq
                    } else {
                        TokenKind::Assign
                    }
                }
                '!' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::NotEq
                    } else {
                        return Err(LexerError {
                            message: "unexpected character '!'".to_string(),
                            location: loc,
                        });
                    }
                }
                '<' => {
                    self.advance();
                    if self.peek() == Some('<') {
                        self.advance();
                        if self.peek() == Some('=') {
                            self.advance();
                            TokenKind::ShlAssign
                        } else {
                            TokenKind::Shl
                        }
                    } else if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::Le
                    } else {
                        TokenKind::Lt
                    }
                }
                '>' => {
                    self.advance();
                    if self.peek() == Some('>') {
                        self.advance();
                        if self.peek() == Some('=') {
                            self.advance();
                            TokenKind::ShrAssign
                        } else {
                            TokenKind::Shr
                        }
                    } else if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::Ge
                    } else {
                        TokenKind::Gt
                    }
                }
                other => {
                    return Err(LexerError {
                        message: format!("unexpected character '{other}'"),
                        location: loc,
                    });
                }
            };

            tokens.push(Token {
                kind: tok,
                location: loc,
            });
        }

        // Emit final Newline if needed
        if !tokens.is_empty() && tokens.last().map(|t| &t.kind) != Some(&TokenKind::Newline) {
            tokens.push(Token {
                kind: TokenKind::Newline,
                location: self.loc(),
            });
        }

        // Emit Dedents for any open indentation levels
        while self.indent_stack.len() > 1 {
            self.indent_stack.pop();
            tokens.push(Token {
                kind: TokenKind::Dedent,
                location: self.loc(),
            });
        }

        tokens.push(Token {
            kind: TokenKind::Eof,
            location: self.loc(),
        });
        Ok(tokens)
    }

    fn lex_ident(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }
        s
    }

    fn lex_number(&mut self) -> Result<TokenKind, LexerError> {
        let loc = self.loc();
        let mut s = String::new();

        // Check for 0x, 0o, 0b
        if self.peek() == Some('0') {
            match self.peek_next() {
                Some('x') | Some('X') => {
                    s.push(self.advance().unwrap());
                    s.push(self.advance().unwrap());
                    while let Some(c) = self.peek() {
                        if c.is_ascii_hexdigit() {
                            s.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    let val = BigInt::from_str_auto(&s).map_err(|e| LexerError {
                        message: e,
                        location: loc,
                    })?;
                    return Ok(TokenKind::Int(val));
                }
                Some('o') | Some('O') => {
                    s.push(self.advance().unwrap());
                    s.push(self.advance().unwrap());
                    while let Some(c) = self.peek() {
                        if ('0'..='7').contains(&c) {
                            s.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    let val = BigInt::from_str_auto(&s).map_err(|e| LexerError {
                        message: e,
                        location: loc,
                    })?;
                    return Ok(TokenKind::Int(val));
                }
                Some('b') | Some('B') => {
                    s.push(self.advance().unwrap());
                    s.push(self.advance().unwrap());
                    while let Some(c) = self.peek() {
                        if c == '0' || c == '1' {
                            s.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    let val = BigInt::from_str_auto(&s).map_err(|e| LexerError {
                        message: e,
                        location: loc,
                    })?;
                    return Ok(TokenKind::Int(val));
                }
                _ => {}
            }
        }

        let mut is_float = false;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }

        if self.peek() == Some('.') {
            // Check if '.' is followed by attribute identifier like in (1).method()
            // In Starlark, `1.` or `1.5` or `.5` is a float.
            // But `1.func()` would not be a float if next char is alphabetic and no digit.
            let next_after_dot = self.peek_next();
            if next_after_dot.map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false) {
                // Not float, dot is attr!
            } else {
                is_float = true;
                s.push(self.advance().unwrap()); // consume '.'
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        s.push(c);
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
        }

        if let Some('e' | 'E') = self.peek() {
            is_float = true;
            s.push(self.advance().unwrap());
            if let Some('+' | '-') = self.peek() {
                s.push(self.advance().unwrap());
            }
            let mut has_exp_digits = false;
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    has_exp_digits = true;
                    s.push(c);
                    self.advance();
                } else {
                    break;
                }
            }
            if !has_exp_digits {
                return Err(LexerError {
                    message: "exponent has no digits".to_string(),
                    location: loc,
                });
            }
        }

        if is_float {
            let val = s.parse::<f64>().map_err(|e| LexerError {
                message: format!("invalid float literal '{s}': {e}"),
                location: loc,
            })?;
            Ok(TokenKind::Float(val))
        } else {
            let val = BigInt::from_str_radix(&s, 10).map_err(|e| LexerError {
                message: format!("invalid integer literal '{s}': {e}"),
                location: loc,
            })?;
            Ok(TokenKind::Int(val))
        }
    }

    fn lex_string(&mut self, raw: bool) -> Result<String, LexerError> {
        let loc = self.loc();
        let quote = self.advance().unwrap();
        let is_triple = self.peek() == Some(quote) && self.peek_next() == Some(quote);
        if is_triple {
            self.advance();
            self.advance();
        }

        let mut out = String::new();
        while let Some(c) = self.peek() {
            if is_triple {
                if c == quote && self.peek_next() == Some(quote) && self.peek_n(2) == Some(quote) {
                    self.advance();
                    self.advance();
                    self.advance();
                    return Ok(out);
                }
            } else if c == quote {
                self.advance();
                return Ok(out);
            } else if c == '\n' && !is_triple {
                return Err(LexerError {
                    message: "unclosed string literal at end of line".to_string(),
                    location: loc,
                });
            }

            if !raw && c == '\\' {
                self.advance();
                let esc = match self.peek() {
                    Some(ch) => ch,
                    None => {
                        return Err(LexerError {
                            message: "unclosed string literal after '\\'".to_string(),
                            location: loc,
                        })
                    }
                };
                self.advance();
                match esc {
                    'a' => out.push('\x07'),
                    'b' => out.push('\x08'),
                    'f' => out.push('\x0c'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'v' => out.push('\x0b'),
                    '\\' => out.push('\\'),
                    '\'' => out.push('\''),
                    '"' => out.push('"'),
                    '\n' => {
                        // Escaped newline -> ignored!
                    }
                    'x' => {
                        // 2 hex digits
                        let h1 = self.advance().ok_or_else(|| LexerError {
                            message: "incomplete \\x escape".to_string(),
                            location: loc.clone(),
                        })?;
                        let h2 = self.advance().ok_or_else(|| LexerError {
                            message: "incomplete \\x escape".to_string(),
                            location: loc.clone(),
                        })?;
                        let byte = u8::from_str_radix(&format!("{h1}{h2}"), 16).map_err(|_| LexerError {
                            message: format!("invalid \\x escape: \\x{h1}{h2}"),
                            location: loc.clone(),
                        })?;
                        out.push(byte as char);
                    }
                    'u' => {
                        // 4 hex digits
                        let mut hex = String::new();
                        for _ in 0..4 {
                            hex.push(self.advance().ok_or_else(|| LexerError {
                                message: "incomplete \\u escape".to_string(),
                                location: loc.clone(),
                            })?);
                        }
                        let code = u32::from_str_radix(&hex, 16).map_err(|_| LexerError {
                            message: format!("invalid \\u escape: \\u{hex}"),
                            location: loc.clone(),
                        })?;
                        let ch = char::from_u32(code).unwrap_or('\u{FFFD}');
                        out.push(ch);
                    }
                    'U' => {
                        // 8 hex digits
                        let mut hex = String::new();
                        for _ in 0..8 {
                            hex.push(self.advance().ok_or_else(|| LexerError {
                                message: "incomplete \\U escape".to_string(),
                                location: loc.clone(),
                            })?);
                        }
                        let code = u32::from_str_radix(&hex, 16).map_err(|_| LexerError {
                            message: format!("invalid \\U escape: \\U{hex}"),
                            location: loc.clone(),
                        })?;
                        let ch = char::from_u32(code).unwrap_or('\u{FFFD}');
                        out.push(ch);
                    }
                    '0'..='7' => {
                        // 1 to 3 octal digits
                        let mut oct = String::new();
                        oct.push(esc);
                        for _ in 0..2 {
                            if let Some(next_c) = self.peek() {
                                if ('0'..='7').contains(&next_c) {
                                    oct.push(next_c);
                                    self.advance();
                                } else {
                                    break;
                                }
                            }
                        }
                        let code = u32::from_str_radix(&oct, 8).map_err(|_| LexerError {
                            message: format!("invalid octal escape: \\{oct}"),
                            location: loc.clone(),
                        })?;
                        let ch = char::from_u32(code).unwrap_or('\u{FFFD}');
                        out.push(ch);
                    }
                    other => {
                        // Bazel: standard python escape rules
                        out.push('\\');
                        out.push(other);
                    }
                }
            } else {
                out.push(c);
                self.advance();
            }
        }

        Err(LexerError {
            message: "unclosed string literal".to_string(),
            location: loc,
        })
    }
}
