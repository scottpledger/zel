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

//! A pure-Rust backtracking regular-expression engine with `java.util.regex.Pattern` semantics
//! (as used by `filter()`, `kind()`, `attr()` and friends in Bazel's query language).
//!
//! Supported: literals, `.`, character classes (ranges, negation, nesting, `&&` intersection,
//! POSIX and Unicode-category `\p{..}`), predefined classes (`\d \w \s \h \D \W \S \H`), anchors
//! (`^ $ \A \z \Z \b \B \G`), capturing / non-capturing / atomic / named groups, look-ahead and
//! look-behind, back-references, greedy / lazy / possessive quantifiers, inline flags
//! (`(?i) (?s) (?m) (?x) (?u) (?-i) (?i:...)`), `\Q..\E` quoting, and `\uXXXX`, `\xHH`, `\0nn`,
//! `\t \n \r \f \a \e \cX` escapes.

use std::fmt;

/// `PatternSyntaxException`-style error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexError {
    pub description: String,
    pub index: Option<usize>,
    pub pattern: String,
}

impl fmt::Display for RegexError {
    /// Same layout as `PatternSyntaxException.getMessage()`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.description)?;
        if let Some(i) = self.index {
            write!(f, " near index {i}")?;
        }
        write!(f, "\n{}", self.pattern)?;
        if let Some(i) = self.index {
            // `index < pattern.length()` (UTF-16 units): no caret for errors at the end of the pattern.
            if i < self.pattern.encode_utf16().count() {
                write!(f, "\n{}^", " ".repeat(i))?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
enum ClassNode {
    Range(char, char),
    Pred(Pred),
    Union(Vec<ClassNode>),
    Intersect(Vec<ClassNode>),
    Negate(Box<ClassNode>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pred {
    Digit,
    NotDigit,
    Word,
    NotWord,
    Space,
    NotSpace,
    HSpace,
    NotHSpace,
    VSpace,
    NotVSpace,
    Lower,
    Upper,
    Ascii,
    Alpha,
    Alnum,
    Punct,
    Graph,
    Print,
    Blank,
    Cntrl,
    XDigit,
    UniLetter,
    UniUpper,
    UniLower,
    UniDigit,
    UniNumber,
    UniSpace,
    UniPunct,
    UniAlnum,
    NotUniLetter,
    Any,
    JavaWhitespace,
    JavaLetter,
    JavaLetterOrDigit,
    JavaDigit,
    JavaUpper,
    JavaLower,
}

fn pred_matches(p: Pred, c: char) -> bool {
    use Pred::*;
    match p {
        Digit => c.is_ascii_digit(),
        NotDigit => !c.is_ascii_digit(),
        Word => c.is_ascii_alphanumeric() || c == '_',
        NotWord => !(c.is_ascii_alphanumeric() || c == '_'),
        Space => matches!(c, ' ' | '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r'),
        NotSpace => !matches!(c, ' ' | '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r'),
        HSpace => matches!(
            c,
            ' ' | '\t' | '\u{A0}' | '\u{1680}' | '\u{180e}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
        ),
        NotHSpace => !pred_matches(HSpace, c),
        VSpace => matches!(c, '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'),
        NotVSpace => !pred_matches(VSpace, c),
        Lower => c.is_ascii_lowercase(),
        Upper => c.is_ascii_uppercase(),
        Ascii => (c as u32) < 0x80,
        Alpha => c.is_ascii_alphabetic(),
        Alnum => c.is_ascii_alphanumeric(),
        Punct => c.is_ascii_punctuation(),
        Graph => c.is_ascii_graphic(),
        Print => c.is_ascii_graphic() || c == ' ',
        Blank => c == ' ' || c == '\t',
        Cntrl => c.is_ascii_control(),
        XDigit => c.is_ascii_hexdigit(),
        UniLetter => c.is_alphabetic() && !c.is_numeric(),
        UniUpper => c.is_uppercase(),
        UniLower => c.is_lowercase(),
        UniDigit => c.is_numeric() && c.to_digit(10).is_some() || (c.is_numeric() && !c.is_ascii()),
        UniNumber => c.is_numeric(),
        UniSpace => c.is_whitespace(),
        UniPunct => c.is_ascii_punctuation() || (!c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace() && !c.is_control()),
        UniAlnum => c.is_alphanumeric(),
        NotUniLetter => !(c.is_alphabetic() && !c.is_numeric()),
        Any => true,
        JavaWhitespace => c.is_whitespace() && !matches!(c, '\u{A0}' | '\u{2007}' | '\u{202F}'),
        JavaLetter => c.is_alphabetic(),
        JavaLetterOrDigit => c.is_alphanumeric(),
        JavaDigit => c.is_numeric(),
        JavaUpper => c.is_uppercase(),
        JavaLower => c.is_lowercase(),
    }
}

fn class_matches(n: &ClassNode, c: char, ci: bool) -> bool {
    match n {
        ClassNode::Range(a, b) => {
            if *a <= c && c <= *b {
                return true;
            }
            if ci {
                for alt in case_variants(c) {
                    if *a <= alt && alt <= *b {
                        return true;
                    }
                }
            }
            false
        }
        ClassNode::Pred(p) => {
            if pred_matches(*p, c) {
                return true;
            }
            if ci && matches!(p, Pred::Lower | Pred::Upper | Pred::UniLower | Pred::UniUpper | Pred::JavaLower | Pred::JavaUpper) {
                return case_variants(c).into_iter().any(|x| pred_matches(*p, x));
            }
            false
        }
        ClassNode::Union(v) => v.iter().any(|x| class_matches(x, c, ci)),
        ClassNode::Intersect(v) => v.iter().all(|x| class_matches(x, c, ci)),
        ClassNode::Negate(x) => !class_matches(x, c, ci),
    }
}

fn case_variants(c: char) -> Vec<char> {
    let mut v = Vec::new();
    for x in c.to_lowercase() {
        if x != c {
            v.push(x);
        }
    }
    for x in c.to_uppercase() {
        if x != c {
            v.push(x);
        }
    }
    v
}

#[derive(Debug, Clone)]
enum Node {
    Empty,
    Char(char, bool),
    Any { dotall: bool, unix_lines: bool },
    Class(ClassNode, bool),
    LineStart { multiline: bool },
    LineEnd { multiline: bool },
    InputStart,
    InputEnd,
    InputEndZ,
    WordB,
    NotWordB,
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Group(Box<Node>, usize),
    NonCapture(Box<Node>),
    Atomic(Box<Node>),
    Repeat { node: Box<Node>, min: usize, max: usize, kind: RepKind },
    Backref(usize, bool),
    Look { node: Box<Node>, ahead: bool, negate: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepKind {
    Greedy,
    Lazy,
    Possessive,
}

/// A compiled pattern.
#[derive(Debug, Clone)]
pub struct Regex {
    root: Node,
    groups: usize,
    pattern: String,
}

#[derive(Clone, Copy)]
struct Flags {
    ci: bool,
    dotall: bool,
    multiline: bool,
    comments: bool,
    unix_lines: bool,
    literal: bool,
}

struct Parser<'a> {
    src: &'a [char],
    pos: usize,
    groups: usize,
    pattern: &'a str,
    flags: Flags,
    named: Vec<(String, usize)>,
}

const NO_MAX: usize = usize::MAX;

impl<'a> Parser<'a> {
    fn err<T>(&self, desc: &str, idx: usize) -> Result<T, RegexError> {
        Err(RegexError { description: desc.to_string(), index: Some(idx), pattern: self.pattern.to_string() })
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn skip_comments(&mut self) {
        if !self.flags.comments {
            return;
        }
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => self.pos += 1,
                Some('#') => {
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        if c == '\n' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }

    fn parse_alt(&mut self) -> Result<Node, RegexError> {
        let mut alts = vec![self.parse_concat()?];
        while self.eat('|') {
            alts.push(self.parse_concat()?);
        }
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn parse_concat(&mut self) -> Result<Node, RegexError> {
        let mut items: Vec<Node> = Vec::new();
        loop {
            self.skip_comments();
            match self.peek() {
                None | Some('|') | Some(')') => break,
                _ => {}
            }
            let atom_start = self.pos;
            let atom = self.parse_atom()?;
            let Some(atom) = atom else { continue };
            let node = self.parse_quant(atom, atom_start)?;
            items.push(node);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap(),
            _ => Node::Concat(items),
        })
    }

    fn parse_quant(&mut self, atom: Node, atom_start: usize) -> Result<Node, RegexError> {
        let mut node = atom;
        loop {
            self.skip_comments();
            let (min, max) = match self.peek() {
                Some('*') => {
                    self.pos += 1;
                    (0, NO_MAX)
                }
                Some('+') => {
                    self.pos += 1;
                    (1, NO_MAX)
                }
                Some('?') => {
                    self.pos += 1;
                    (0, 1)
                }
                Some('{') => {
                    let save = self.pos;
                    match self.try_parse_braces() {
                        Some(r) => r?,
                        None => {
                            self.pos = save;
                            return self.err("Illegal repetition", save);
                        }
                    }
                }
                _ => return Ok(node),
            };
            let kind = if self.eat('?') {
                RepKind::Lazy
            } else if self.eat('+') {
                RepKind::Possessive
            } else {
                RepKind::Greedy
            };
            let _ = atom_start;
            node = Node::Repeat { node: Box::new(node), min, max, kind };
        }
    }

    /// Parses `{n}`, `{n,}`, `{n,m}` at `self.pos`.
    #[allow(clippy::type_complexity)]
    fn try_parse_braces(&mut self) -> Option<Result<(usize, usize), RegexError>> {
        let start = self.pos;
        self.pos += 1;
        let n = self.parse_digits()?;
        let (min, max) = if self.eat(',') {
            if self.peek() == Some('}') {
                (n, NO_MAX)
            } else {
                let m = self.parse_digits()?;
                (n, m)
            }
        } else {
            (n, n)
        };
        if !self.eat('}') {
            return None;
        }
        if max < min {
            return Some(self.err("Illegal repetition range", start + 1));
        }
        Some(Ok((min, max)))
    }

    fn parse_digits(&mut self) -> Option<usize> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if start == self.pos {
            return None;
        }
        self.src[start..self.pos].iter().collect::<String>().parse().ok()
    }

    fn parse_atom(&mut self) -> Result<Option<Node>, RegexError> {
        let start = self.pos;
        let c = self.src[self.pos];
        self.pos += 1;
        if self.flags.literal {
            return Ok(Some(Node::Char(c, self.flags.ci)));
        }
        Ok(Some(match c {
            '.' => Node::Any { dotall: self.flags.dotall, unix_lines: self.flags.unix_lines },
            '^' => Node::LineStart { multiline: self.flags.multiline },
            '$' => Node::LineEnd { multiline: self.flags.multiline },
            '*' | '+' | '?' => {
                return self.err(&format!("Dangling meta character '{c}'"), start);
            }
            '{' => {
                return self.err("Illegal repetition", start);
            }
            '[' => {
                let cls = self.parse_class(start)?;
                Node::Class(cls, self.flags.ci)
            }
            '(' => return self.parse_group(start),
            '\\' => return self.parse_escape(start),
            _ => Node::Char(c, self.flags.ci),
        }))
    }

    fn parse_group(&mut self, start: usize) -> Result<Option<Node>, RegexError> {
        let saved = self.flags;
        let node;
        if self.eat('?') {
            let Some(c) = self.peek() else {
                return self.err("Unknown group type", self.pos);
            };
            self.pos += 1;
            match c {
                ':' => {
                    node = Node::NonCapture(Box::new(self.parse_alt()?));
                }
                '>' => {
                    node = Node::Atomic(Box::new(self.parse_alt()?));
                }
                '=' | '!' => {
                    let n = self.parse_alt()?;
                    node = Node::Look { node: Box::new(n), ahead: true, negate: c == '!' };
                }
                '<' => match self.peek() {
                    Some('=') | Some('!') => {
                        let neg = self.peek() == Some('!');
                        self.pos += 1;
                        let n = self.parse_alt()?;
                        node = Node::Look { node: Box::new(n), ahead: false, negate: neg };
                    }
                    _ => {
                        let name_start = self.pos;
                        while matches!(self.peek(), Some(ch) if ch.is_ascii_alphanumeric()) {
                            self.pos += 1;
                        }
                        let name: String = self.src[name_start..self.pos].iter().collect();
                        if name.is_empty() || !name.chars().next().unwrap().is_ascii_alphabetic() {
                            return self.err("capturing group name does not start with a Latin letter", self.pos);
                        }
                        if !self.eat('>') {
                            return self.err("named capturing group is missing trailing '>'", self.pos);
                        }
                        self.groups += 1;
                        let idx = self.groups;
                        self.named.push((name, idx));
                        let n = self.parse_alt()?;
                        node = Node::Group(Box::new(n), idx);
                    }
                },
                _ => {
                    // inline flags
                    self.pos -= 1;
                    let mut on = true;
                    let mut f = self.flags;
                    loop {
                        let Some(ch) = self.peek() else {
                            return self.err("Unknown inline modifier", self.pos);
                        };
                        self.pos += 1;
                        match ch {
                            '-' => on = false,
                            'i' => f.ci = on,
                            's' => f.dotall = on,
                            'm' => f.multiline = on,
                            'x' => f.comments = on,
                            'd' => f.unix_lines = on,
                            'u' | 'U' => {}
                            ')' => {
                                // Applies to the rest of the enclosing group.
                                self.flags = f;
                                return Ok(None);
                            }
                            ':' => {
                                self.flags = f;
                                let n = self.parse_alt()?;
                                if !self.eat(')') {
                                    return self.err("Unclosed group", self.src.len());
                                }
                                self.flags = saved;
                                return Ok(Some(Node::NonCapture(Box::new(n))));
                            }
                            _ => return self.err("Unknown inline modifier", self.pos - 1),
                        }
                    }
                }
            }
        } else {
            self.groups += 1;
            let idx = self.groups;
            let n = self.parse_alt()?;
            node = Node::Group(Box::new(n), idx);
        }
        if !self.eat(')') {
            return self.err("Unclosed group", self.src.len());
        }
        let _ = start;
        self.flags = saved;
        Ok(Some(node))
    }

    fn parse_hex(&mut self, n: usize, at: usize) -> Result<char, RegexError> {
        let mut v = 0u32;
        for _ in 0..n {
            let Some(c) = self.peek().and_then(|c| c.to_digit(16)) else {
                return self.err("Illegal hexadecimal escape sequence", at);
            };
            self.pos += 1;
            v = v * 16 + c;
        }
        char::from_u32(v).map_or_else(|| self.err("Illegal hexadecimal escape sequence", at), Ok)
    }

    /// Escapes valid both inside and outside classes. Returns a char literal or a predicate.
    fn parse_escape_common(&mut self, start: usize, in_class: bool) -> Result<Esc, RegexError> {
        let Some(c) = self.peek() else {
            return self.err("Unexpected internal error", self.pos.saturating_sub(0));
        };
        self.pos += 1;
        Ok(match c {
            'd' => Esc::Pred(Pred::Digit),
            'D' => Esc::Pred(Pred::NotDigit),
            'w' => Esc::Pred(Pred::Word),
            'W' => Esc::Pred(Pred::NotWord),
            's' => Esc::Pred(Pred::Space),
            'S' => Esc::Pred(Pred::NotSpace),
            'h' => Esc::Pred(Pred::HSpace),
            'H' => Esc::Pred(Pred::NotHSpace),
            'v' => Esc::Pred(Pred::VSpace),
            'V' => Esc::Pred(Pred::NotVSpace),
            'p' | 'P' => {
                let neg = c == 'P';
                let name = if self.eat('{') {
                    let s = self.pos;
                    while matches!(self.peek(), Some(ch) if ch != '}') {
                        self.pos += 1;
                    }
                    let n: String = self.src[s..self.pos].iter().collect();
                    if !self.eat('}') {
                        return self.err("Unclosed character family", self.src.len());
                    }
                    n
                } else if let Some(ch) = self.peek() {
                    self.pos += 1;
                    ch.to_string()
                } else {
                    return self.err("Illegal character family", self.pos);
                };
                let Some(p) = property(&name) else {
                    return self.err(&format!("Unknown character property name {{{name}}}"), self.pos - 1);
                };
                if neg {
                    Esc::Node(ClassNode::Negate(Box::new(ClassNode::Pred(p))))
                } else {
                    Esc::Pred(p)
                }
            }
            't' => Esc::Ch('\t'),
            'n' => Esc::Ch('\n'),
            'r' => Esc::Ch('\r'),
            'f' => Esc::Ch('\u{0C}'),
            'a' => Esc::Ch('\u{07}'),
            'e' => Esc::Ch('\u{1B}'),
            '0' => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 3 {
                    match self.peek().and_then(|c| c.to_digit(8)) {
                        Some(d) if v * 8 + d <= 0o377 => {
                            v = v * 8 + d;
                            self.pos += 1;
                            n += 1;
                        }
                        _ => break,
                    }
                }
                if n == 0 {
                    return self.err("Illegal octal escape sequence", self.pos);
                }
                Esc::Ch(char::from_u32(v).unwrap())
            }
            'x' => {
                if self.eat('{') {
                    let s = self.pos;
                    while matches!(self.peek(), Some(ch) if ch != '}') {
                        self.pos += 1;
                    }
                    let h: String = self.src[s..self.pos].iter().collect();
                    self.pos += 1;
                    match u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                        Some(ch) => Esc::Ch(ch),
                        None => return self.err("Illegal hexadecimal escape sequence", s),
                    }
                } else {
                    Esc::Ch(self.parse_hex(2, self.pos)?)
                }
            }
            'u' => Esc::Ch(self.parse_hex(4, self.pos)?),
            'c' => {
                let Some(ch) = self.peek() else {
                    return self.err("Illegal control escape sequence", self.pos);
                };
                self.pos += 1;
                Esc::Ch(char::from_u32((ch as u32) ^ 64).unwrap_or(ch))
            }
            'b' if in_class => return self.err("Illegal/unsupported escape sequence", self.pos - 1),
            c if c.is_ascii_alphabetic() => {
                if in_class {
                    return self.err("Illegal/unsupported escape sequence", self.pos - 1);
                }
                Esc::NotCommon(c)
            }
            c => {
                let _ = start;
                Esc::Ch(c)
            }
        })
    }

    fn parse_escape(&mut self, start: usize) -> Result<Option<Node>, RegexError> {
        if self.pos >= self.src.len() {
            return self.err("Unexpected internal error", self.pos);
        }
        let ci = self.flags.ci;
        let c = self.src[self.pos];
        // Back-references and quoting are outside-class only.
        if c.is_ascii_digit() && c != '0' {
            self.pos += 1;
            let mut n = c.to_digit(10).unwrap() as usize;
            while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
                let nn = n * 10 + d as usize;
                if nn > self.groups {
                    break;
                }
                n = nn;
                self.pos += 1;
            }
            if n > self.groups {
                // Java: back-reference to a group that does not exist never matches
                // (it is a syntax error only for forward references at compile time in new JDKs).
                return Ok(Some(Node::Backref(n, ci)));
            }
            return Ok(Some(Node::Backref(n, ci)));
        }
        if c == 'k' {
            self.pos += 1;
            if !self.eat('<') {
                return self.err("\\k is not followed by '<' for named capturing group", self.pos);
            }
            let s = self.pos;
            while matches!(self.peek(), Some(ch) if ch != '>') {
                self.pos += 1;
            }
            let name: String = self.src[s..self.pos].iter().collect();
            self.pos += 1;
            let Some((_, idx)) = self.named.iter().find(|(n, _)| *n == name) else {
                return self.err(&format!("named capturing group <{name}> does not exist"), self.pos - 1);
            };
            return Ok(Some(Node::Backref(*idx, ci)));
        }
        if c == 'Q' {
            self.pos += 1;
            let mut items = Vec::new();
            while self.pos < self.src.len() {
                if self.src[self.pos] == '\\' && self.src.get(self.pos + 1) == Some(&'E') {
                    self.pos += 2;
                    break;
                }
                items.push(Node::Char(self.src[self.pos], ci));
                self.pos += 1;
            }
            return Ok(Some(match items.len() {
                0 => return Ok(None),
                1 => items.pop().unwrap(),
                _ => Node::NonCapture(Box::new(Node::Concat(items))),
            }));
        }
        if c == 'E' {
            self.pos += 1;
            return Ok(None);
        }
        match self.parse_escape_common(start, false)? {
            Esc::Ch(ch) => Ok(Some(Node::Char(ch, ci))),
            Esc::Pred(p) => Ok(Some(Node::Class(ClassNode::Pred(p), ci))),
            Esc::Node(n) => Ok(Some(Node::Class(n, ci))),
            Esc::NotCommon(ch) => Ok(Some(match ch {
                'A' => Node::InputStart,
                'z' => Node::InputEnd,
                'Z' => Node::InputEndZ,
                'G' => Node::InputStart,
                'b' => Node::WordB,
                'B' => Node::NotWordB,
                'R' => Node::Alt(vec![
                    Node::Concat(vec![Node::Char('\r', false), Node::Char('\n', false)]),
                    Node::Class(ClassNode::Pred(Pred::VSpace), false),
                ]),
                _ => return self.err("Illegal/unsupported escape sequence", self.pos - 1),
            })),
        }
    }

    /// Parses a class body after the opening `[` (consumed). Returns the class node.
    fn parse_class(&mut self, start: usize) -> Result<ClassNode, RegexError> {
        let negate = self.eat('^');
        let mut items: Vec<ClassNode> = Vec::new();
        let mut intersect: Vec<ClassNode> = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                return self.err("Unclosed character class", self.src.len().saturating_sub(1));
            };
            match c {
                ']' if !first => {
                    self.pos += 1;
                    break;
                }
                '[' => {
                    self.pos += 1;
                    let inner = self.parse_class(self.pos - 1)?;
                    items.push(inner);
                }
                '&' if self.src.get(self.pos + 1) == Some(&'&') => {
                    self.pos += 2;
                    intersect.push(ClassNode::Union(std::mem::take(&mut items)));
                }
                _ => {
                    let lo = self.class_atom()?;
                    match lo {
                        ClassAtom::Ch(a) => {
                            // Range?
                            if self.peek() == Some('-')
                                && self.src.get(self.pos + 1).is_some_and(|n| *n != ']' && *n != '[')
                                && !(self.src.get(self.pos + 1) == Some(&'&') && self.src.get(self.pos + 2) == Some(&'&'))
                            {
                                self.pos += 1;
                                let at = self.pos;
                                match self.class_atom()? {
                                    ClassAtom::Ch(b) => {
                                        if b < a {
                                            return self.err("Illegal character range", at);
                                        }
                                        items.push(ClassNode::Range(a, b));
                                    }
                                    ClassAtom::Node(_) => {
                                        return self.err("Illegal character range", at);
                                    }
                                }
                            } else {
                                items.push(ClassNode::Range(a, a));
                            }
                        }
                        ClassAtom::Node(n) => items.push(n),
                    }
                }
            }
            first = false;
        }
        let body = if intersect.is_empty() {
            ClassNode::Union(items)
        } else {
            intersect.push(ClassNode::Union(items));
            ClassNode::Intersect(intersect)
        };
        let _ = start;
        Ok(if negate { ClassNode::Negate(Box::new(body)) } else { body })
    }

    fn class_atom(&mut self) -> Result<ClassAtom, RegexError> {
        let c = self.src[self.pos];
        self.pos += 1;
        if c == '\\' {
            if self.pos >= self.src.len() {
                return self.err("Unclosed character class", self.pos);
            }
            if self.peek() == Some('Q') {
                self.pos += 1;
                let mut v = Vec::new();
                while self.pos < self.src.len() {
                    if self.src[self.pos] == '\\' && self.src.get(self.pos + 1) == Some(&'E') {
                        self.pos += 2;
                        break;
                    }
                    v.push(ClassNode::Range(self.src[self.pos], self.src[self.pos]));
                    self.pos += 1;
                }
                return Ok(ClassAtom::Node(ClassNode::Union(v)));
            }
            return Ok(match self.parse_escape_common(self.pos - 1, true)? {
                Esc::Ch(ch) => ClassAtom::Ch(ch),
                Esc::Pred(p) => ClassAtom::Node(ClassNode::Pred(p)),
                Esc::Node(n) => ClassAtom::Node(n),
                Esc::NotCommon(_) => unreachable!(),
            });
        }
        Ok(ClassAtom::Ch(c))
    }
}

enum Esc {
    Ch(char),
    Pred(Pred),
    Node(ClassNode),
    NotCommon(char),
}

enum ClassAtom {
    Ch(char),
    Node(ClassNode),
}

fn property(name: &str) -> Option<Pred> {
    use Pred::*;
    let n = name.strip_prefix("Is").unwrap_or(name);
    Some(match name {
        "Lower" => Lower,
        "Upper" => Upper,
        "ASCII" => Ascii,
        "Alpha" => Alpha,
        "Digit" => Digit,
        "Alnum" => Alnum,
        "Punct" => Punct,
        "Graph" => Graph,
        "Print" => Print,
        "Blank" => Blank,
        "Cntrl" => Cntrl,
        "XDigit" => XDigit,
        "Space" => Space,
        "javaLowerCase" => JavaLower,
        "javaUpperCase" => JavaUpper,
        "javaWhitespace" => JavaWhitespace,
        "javaMirrored" => Any,
        "javaLetter" => JavaLetter,
        "javaDigit" => JavaDigit,
        "javaLetterOrDigit" => JavaLetterOrDigit,
        "javaAlphabetic" => JavaLetter,
        "L" | "IsL" | "IsLetter" | "Letter" => UniLetter,
        "Lu" | "IsLu" | "IsUppercase" | "Uppercase" => UniUpper,
        "Ll" | "IsLl" | "IsLowercase" | "Lowercase" => UniLower,
        "N" | "Nd" | "IsN" | "IsNd" | "IsDigit" => if name.ends_with('d') { UniDigit } else { UniNumber },
        "P" | "IsP" | "IsPunctuation" => UniPunct,
        "Z" | "Zs" | "IsWhite_Space" | "IsWhiteSpace" => UniSpace,
        "IsAlphabetic" | "Alphabetic" => JavaLetter,
        "IsAlnum" => UniAlnum,
        "IsLatin" => UniLetter,
        _ => match n {
            "Alphabetic" => JavaLetter,
            _ => return None,
        },
    })
}

impl Regex {
    /// `Pattern.compile(pattern)`.
    pub fn new(pattern: &str) -> Result<Regex, RegexError> {
        let src: Vec<char> = pattern.chars().collect();
        let mut p = Parser {
            src: &src,
            pos: 0,
            groups: 0,
            pattern,
            flags: Flags { ci: false, dotall: false, multiline: false, comments: false, unix_lines: false, literal: false },
            named: Vec::new(),
        };
        let root = p.parse_alt()?;
        if p.pos < src.len() {
            // Only an unmatched ')' can stop the top-level parse.
            return p.err("Unmatched closing ')'", p.pos.saturating_sub(1));
        }
        Ok(Regex { root, groups: p.groups, pattern: pattern.to_string() })
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// `matcher.find()`: whether the pattern matches somewhere in `text`.
    pub fn is_match(&self, text: &str) -> bool {
        self.find(text).is_some()
    }

    /// `matcher.matches()`: whole-input match.
    pub fn full_match(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        let mut caps = vec![None; self.groups + 1];
        let m = Matcher { chars: &chars };
        m.node(&self.root, 0, &mut caps, &mut |p, _| p == chars.len())
    }

    /// First match as `(start, end)` char offsets.
    pub fn find(&self, text: &str) -> Option<(usize, usize)> {
        let chars: Vec<char> = text.chars().collect();
        let m = Matcher { chars: &chars };
        for start in 0..=chars.len() {
            let mut caps = vec![None; self.groups + 1];
            let mut end = None;
            if m.node(&self.root, start, &mut caps, &mut |p, _| {
                end = Some(p);
                true
            }) {
                return Some((start, end.unwrap()));
            }
        }
        None
    }
}

type Caps = Vec<Option<(usize, usize)>>;

struct Matcher<'a> {
    chars: &'a [char],
}

fn eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase()) || a.to_uppercase().eq(b.to_uppercase())
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

impl<'a> Matcher<'a> {
    fn node(&self, n: &Node, pos: usize, caps: &mut Caps, k: &mut dyn FnMut(usize, &mut Caps) -> bool) -> bool {
        let cs = self.chars;
        match n {
            Node::Empty => k(pos, caps),
            Node::Char(c, ci) => {
                if pos < cs.len() && (cs[pos] == *c || (*ci && eq_ci(cs[pos], *c))) {
                    k(pos + 1, caps)
                } else {
                    false
                }
            }
            Node::Any { dotall, unix_lines } => {
                if pos >= cs.len() {
                    return false;
                }
                let c = cs[pos];
                let line_term = if *unix_lines {
                    c == '\n'
                } else {
                    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
                };
                if *dotall || !line_term {
                    k(pos + 1, caps)
                } else {
                    false
                }
            }
            Node::Class(cls, ci) => {
                if pos < cs.len() && class_matches(cls, cs[pos], *ci) {
                    k(pos + 1, caps)
                } else {
                    false
                }
            }
            Node::LineStart { multiline } => {
                let ok = pos == 0
                    || (*multiline
                        && pos <= cs.len()
                        && matches!(cs[pos - 1], '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
                        && !(cs[pos - 1] == '\r' && pos < cs.len() && cs[pos] == '\n')
                        && pos != cs.len());
                if ok {
                    k(pos, caps)
                } else {
                    false
                }
            }
            Node::LineEnd { multiline } => {
                let n = cs.len();
                let is_term = |c: char| matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}');
                let ok = if pos == n {
                    true
                } else if *multiline {
                    is_term(cs[pos]) && !(cs[pos] == '\n' && pos > 0 && cs[pos - 1] == '\r')
                } else {
                    // Before a final line terminator.
                    (pos == n - 1 && is_term(cs[pos])) || (pos == n - 2 && cs[pos] == '\r' && cs[pos + 1] == '\n')
                };
                if ok {
                    k(pos, caps)
                } else {
                    false
                }
            }
            Node::InputStart => pos == 0 && k(pos, caps),
            Node::InputEnd => pos == cs.len() && k(pos, caps),
            Node::InputEndZ => {
                let n = cs.len();
                let is_term = |c: char| matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}');
                let ok = pos == n
                    || (pos == n - 1 && is_term(cs[pos]))
                    || (pos + 2 == n && cs[pos] == '\r' && cs[pos + 1] == '\n');
                ok && k(pos, caps)
            }
            Node::WordB | Node::NotWordB => {
                let before = pos > 0 && is_word(cs[pos - 1]);
                let after = pos < cs.len() && is_word(cs[pos]);
                let at = before != after;
                if at == matches!(n, Node::WordB) {
                    k(pos, caps)
                } else {
                    false
                }
            }
            Node::Concat(items) => self.seq(items, 0, pos, caps, k),
            Node::Alt(alts) => {
                for a in alts {
                    let saved = caps.clone();
                    if self.node(a, pos, caps, k) {
                        return true;
                    }
                    *caps = saved;
                }
                false
            }
            Node::NonCapture(inner) => self.node(inner, pos, caps, k),
            Node::Group(inner, idx) => {
                let idx = *idx;
                let old = caps[idx];
                let r = self.node(inner, pos, caps, &mut |e, caps: &mut Caps| {
                    let prev = caps[idx];
                    caps[idx] = Some((pos, e));
                    if k(e, caps) {
                        true
                    } else {
                        caps[idx] = prev;
                        false
                    }
                });
                if !r {
                    caps[idx] = old;
                }
                r
            }
            Node::Atomic(inner) => {
                let mut end = None;
                if self.node(inner, pos, caps, &mut |e, _| {
                    end = Some(e);
                    true
                }) {
                    k(end.unwrap(), caps)
                } else {
                    false
                }
            }
            Node::Repeat { node, min, max, kind } => match kind {
                RepKind::Greedy => self.rep_greedy(node, *min, *max, 0, pos, caps, k),
                RepKind::Lazy => self.rep_lazy(node, *min, *max, 0, pos, caps, k),
                RepKind::Possessive => {
                    let mut cur = pos;
                    let mut count = 0;
                    while count < *max {
                        let mut end = None;
                        let ok = self.node(node, cur, caps, &mut |e, _| {
                            end = Some(e);
                            true
                        });
                        if !ok {
                            break;
                        }
                        let e = end.unwrap();
                        count += 1;
                        if e == cur {
                            break;
                        }
                        cur = e;
                    }
                    if count < *min {
                        return false;
                    }
                    k(cur, caps)
                }
            },
            Node::Backref(idx, ci) => {
                let Some(Some((s, e))) = caps.get(*idx).copied() else {
                    return false;
                };
                let len = e - s;
                if pos + len > cs.len() {
                    return false;
                }
                for i in 0..len {
                    let (a, b) = (cs[s + i], cs[pos + i]);
                    if !(a == b || (*ci && eq_ci(a, b))) {
                        return false;
                    }
                }
                k(pos + len, caps)
            }
            Node::Look { node, ahead, negate } => {
                let matched = if *ahead {
                    let mut c2 = caps.clone();
                    let r = self.node(node, pos, &mut c2, &mut |_, _| true);
                    if r && !*negate {
                        *caps = c2;
                    }
                    r
                } else {
                    // Look-behind: try every start position ending exactly at `pos`.
                    let mut found = false;
                    for start in (0..=pos).rev() {
                        let mut c2 = caps.clone();
                        if self.node(node, start, &mut c2, &mut |e, _| e == pos) {
                            found = true;
                            break;
                        }
                    }
                    found
                };
                if matched != *negate {
                    k(pos, caps)
                } else {
                    false
                }
            }
        }
    }

    fn seq(&self, items: &[Node], i: usize, pos: usize, caps: &mut Caps, k: &mut dyn FnMut(usize, &mut Caps) -> bool) -> bool {
        if i == items.len() {
            return k(pos, caps);
        }
        self.node(&items[i], pos, caps, &mut |p, caps: &mut Caps| self.seq(items, i + 1, p, caps, k))
    }

    #[allow(clippy::too_many_arguments)]
    fn rep_greedy(
        &self,
        node: &Node,
        min: usize,
        max: usize,
        count: usize,
        pos: usize,
        caps: &mut Caps,
        k: &mut dyn FnMut(usize, &mut Caps) -> bool,
    ) -> bool {
        if count < max {
            let saved = caps.clone();
            let r = self.node(node, pos, caps, &mut |p, caps: &mut Caps| {
                if p == pos && count >= min {
                    // Empty iteration: stop looping.
                    return false;
                }
                self.rep_greedy(node, min, max, count + 1, p, caps, k)
            });
            if r {
                return true;
            }
            *caps = saved;
        }
        if count >= min {
            return k(pos, caps);
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn rep_lazy(
        &self,
        node: &Node,
        min: usize,
        max: usize,
        count: usize,
        pos: usize,
        caps: &mut Caps,
        k: &mut dyn FnMut(usize, &mut Caps) -> bool,
    ) -> bool {
        if count >= min && k(pos, caps) {
            return true;
        }
        if count < max {
            return self.node(node, pos, caps, &mut |p, caps: &mut Caps| {
                if p == pos && count >= min {
                    return false;
                }
                self.rep_lazy(node, min, max, count + 1, p, caps, k)
            });
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(p: &str, s: &str) -> bool {
        Regex::new(p).unwrap().is_match(s)
    }

    #[test]
    fn basics() {
        assert!(f("abc", "xxabcxx"));
        assert!(!f("^abc", "xabc"));
        assert!(f("abc$", "xabc"));
        assert!(f("a.c", "abc"));
        assert!(f("a+b*", "aaa"));
        assert!(f("^(foo|bar)baz$", "barbaz"));
        assert!(!f("^(foo|bar)baz$", "bazbaz"));
        assert!(f("x{2,3}", "axxb"));
        assert!(!f("x{2,3}", "axb"));
        assert!(f("^x{2}$", "xx"));
        assert!(f("//foo:.*_test", "//foo:bar_test"));
        assert!(f("\\.cc$", "a/b.cc"));
        assert!(f("\\bmanual\\b", "[manual, no-sandbox]"));
        assert!(!f("\\bmanual\\b", "[manualx]"));
    }

    #[test]
    fn classes() {
        assert!(f("^[a-c]+$", "abcabc"));
        assert!(!f("^[^a-c]+$", "abd"));
        assert!(f("^[a-z&&[^aeiou]]+$", "xyz"));
        assert!(!f("^[a-z&&[^aeiou]]+$", "xya"));
        assert!(f("^\\w+$", "ab_9"));
        assert!(f("^\\p{Alpha}+$", "abc"));
        assert!(f("[]a]", "]"));
        assert!(f("[\\d-]", "-"));
        assert!(f("^[a\\-z]+$", "a-z"));
    }

    #[test]
    fn groups_and_backrefs() {
        assert!(f("^(a+)\\1$", "aaaa"));
        assert!(!f("^(a+)\\1$", "aaa"));
        assert!(f("(?i)ABC", "xabcx"));
        assert!(f("(?i:a)b", "Ab"));
        assert!(!f("(?i:a)b", "AB"));
        assert!(f("foo(?=bar)", "foobar"));
        assert!(!f("foo(?!bar)", "foobar"));
        assert!(f("(?<=foo)bar", "foobar"));
        assert!(!f("(?<!foo)bar", "foobar"));
        assert!(f("^(?<x>ab)\\k<x>$", "abab"));
        assert!(f("^(?:ab)+$", "ababab"));
    }

    #[test]
    fn quantifiers() {
        assert!(f("^a*?b$", "aaab"));
        assert!(!f("^a*+a$", "aaa"));
        assert!(f("^(a|ab)(c|bcd)$", "abcd"));
        assert!(f("^(a*)*$", "aaaa"));
        assert!(f("^(a?)*b$", "aab"));
    }

    #[test]
    fn quoting_and_escapes() {
        assert!(f("\\Qa.b\\E", "a.b"));
        assert!(!f("\\Qa.b\\E", "axb"));
        assert!(f("\\x41\\u0042", "AB"));
        assert!(f("a\\tb", "a\tb"));
    }

    #[test]
    fn syntax_errors() {
        let e = Regex::new("(ab").unwrap_err();
        // `PatternSyntaxException.getMessage()` has no caret when the index is past the pattern end.
        assert_eq!(e.to_string(), "Unclosed group near index 3\n(ab");
        let e = Regex::new("*a").unwrap_err();
        assert_eq!(e.to_string(), "Dangling meta character '*' near index 0\n*a\n^");
        let e = Regex::new("[a").unwrap_err();
        assert_eq!(e.description, "Unclosed character class");
        let e = Regex::new("a)").unwrap_err();
        assert_eq!(e.description, "Unmatched closing ')'");
        assert!(Regex::new("a{2,1}").is_err());
        assert!(Regex::new("\\p{Nope}").is_err());
    }

    #[test]
    fn multiline_and_dotall() {
        assert!(f("(?m)^b$", "a\nb\nc"));
        assert!(!f("^b$", "a\nb\nc"));
        assert!(f("(?s)a.b", "a\nb"));
        assert!(!f("a.b", "a\nb"));
        assert!(f("a$", "a\n"));
    }
}
