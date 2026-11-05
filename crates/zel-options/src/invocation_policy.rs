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

//! Invocation policy definition, decoding (text and binary proto), and enforcement.
//! Mirrors `src/main/protobuf/invocation_policy.proto` and `InvocationPolicyEnforcer.java`.

use crate::error::OptionsParsingError;
use crate::rc::get_command_ancestry;
use zel_proto::wire;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigOrder {
    Undefined = 0,
    Prepend = 1,
    Append = 2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPolicy {
    pub name: String,
    pub order: ConfigOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetValueBehavior {
    Undefined = 0,
    AllowOverrides = 1,
    Append = 2,
    FinalValueIgnoreOverrides = 3,
    FinalValueThrowOnOverride = 4,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetValue {
    pub flag_values: Vec<String>,
    pub behavior: SetValueBehavior,
    /// Whether `behavior` was explicitly present (proto2 field presence; `behavior: UNDEFINED` is echoed).
    pub behavior_set: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseDefault;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplacementValue {
    NewValue(String),
    UseDefault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisallowValues {
    pub disallowed_values: Vec<String>,
    pub replacement: Option<ReplacementValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowValues {
    pub allowed_values: Vec<String>,
    pub replacement: Option<ReplacementValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyOperation {
    SetValue(SetValue),
    UseDefault(UseDefault),
    DisallowValues(DisallowValues),
    AllowValues(AllowValues),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagPolicy {
    pub flag_name: String,
    pub commands: Vec<String>,
    pub operation: Option<PolicyOperation>,
    pub custom_error_message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InvocationPolicy {
    pub flag_policies: Vec<FlagPolicy>,
    pub config_policies: Vec<ConfigPolicy>,
}

impl InvocationPolicy {
    /// Parses an invocation policy from either base64-encoded binary proto or text proto format.
    pub fn parse(input: &str) -> Result<Self, OptionsParsingError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Ok(Self::default());
        }

        // 1. Try base64 binary proto
        if let Ok(bytes) = base64_decode(trimmed) {
            if let Ok(policy) = decode_binary_invocation_policy(&bytes) {
                return Ok(policy);
            }
        }

        // 2. Try text format proto
        parse_text_invocation_policy(trimmed)
    }

    /// Checks if this policy applies to the given command.
    pub fn applies_to_command(commands: &[String], current_command: &str) -> bool {
        if commands.is_empty() {
            return true;
        }
        let hierarchy = get_command_ancestry(current_command);
        commands.iter().any(|cmd| hierarchy.contains(&cmd.as_str()) || cmd == current_command)
    }
}

// ---------------------------------------------------------------------------
// Base64 decoder
// ---------------------------------------------------------------------------

pub fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    let clean: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;

    for c in clean.chars() {
        if c == '=' {
            break;
        }
        let val = match c {
            'A'..='Z' => (c as u8 - b'A') as u32,
            'a'..='z' => (c as u8 - b'a' + 26) as u32,
            '0'..='9' => (c as u8 - b'0' + 52) as u32,
            '+' => 62,
            '/' => 63,
            _ => return Err(format!("Invalid base64 character: '{c}'")),
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Binary wire decoder
// ---------------------------------------------------------------------------

pub fn decode_binary_invocation_policy(buf: &[u8]) -> Result<InvocationPolicy, OptionsParsingError> {
    let mut policy = InvocationPolicy::default();
    let mut pos = 0;

    while pos < buf.len() {
        let (field, wire_type) = wire::decode_tag(buf, &mut pos)
            .map_err(|e| OptionsParsingError::new(format!("Policy proto decode error: {e}")))?;

        match (field, wire_type) {
            (1, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                let sub = wire::decode_bytes(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?;
                policy.flag_policies.push(decode_flag_policy(sub)?);
            }
            (3, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                let sub = wire::decode_bytes(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?;
                policy.config_policies.push(decode_config_policy(sub)?);
            }
            (_, wire::WIRE_TYPE_VARINT) => {
                let _ = wire::decode_varint(buf, &mut pos);
            }
            (_, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                let _ = wire::decode_bytes(buf, &mut pos);
            }
            _ => {
                return Err(OptionsParsingError::new(format!(
                    "Unsupported wire type {wire_type} for field {field}"
                )));
            }
        }
    }
    Ok(policy)
}

fn decode_config_policy(buf: &[u8]) -> Result<ConfigPolicy, OptionsParsingError> {
    let mut name = String::new();
    let mut order = ConfigOrder::Undefined;
    let mut pos = 0;

    while pos < buf.len() {
        let (field, wire_type) = wire::decode_tag(buf, &mut pos)
            .map_err(|e| OptionsParsingError::new(e.0))?;
        match (field, wire_type) {
            (1, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                name = wire::decode_string(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?;
            }
            (2, wire::WIRE_TYPE_VARINT) => {
                let v = wire::decode_varint(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?;
                order = match v {
                    1 => ConfigOrder::Prepend,
                    2 => ConfigOrder::Append,
                    _ => ConfigOrder::Undefined,
                };
            }
            _ => {
                skip_field(wire_type, buf, &mut pos)?;
            }
        }
    }
    Ok(ConfigPolicy { name, order })
}

fn decode_flag_policy(buf: &[u8]) -> Result<FlagPolicy, OptionsParsingError> {
    let mut flag_name = String::new();
    let mut commands = Vec::new();
    let mut operation = None;
    let mut custom_error_message = None;
    let mut pos = 0;

    while pos < buf.len() {
        let (field, wire_type) = wire::decode_tag(buf, &mut pos)
            .map_err(|e| OptionsParsingError::new(e.0))?;
        match (field, wire_type) {
            (1, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                flag_name = wire::decode_string(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?;
            }
            (2, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                commands.push(wire::decode_string(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?);
            }
            (3, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                let sub = wire::decode_bytes(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?;
                operation = Some(PolicyOperation::SetValue(decode_set_value(sub)?));
            }
            (4, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                let _ = wire::decode_bytes(buf, &mut pos);
                operation = Some(PolicyOperation::UseDefault(UseDefault));
            }
            (5, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                let sub = wire::decode_bytes(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?;
                operation = Some(PolicyOperation::DisallowValues(decode_disallow_values(sub)?));
            }
            (6, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                let sub = wire::decode_bytes(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?;
                operation = Some(PolicyOperation::AllowValues(decode_allow_values(sub)?));
            }
            (7, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                custom_error_message = Some(wire::decode_string(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?);
            }
            _ => {
                skip_field(wire_type, buf, &mut pos)?;
            }
        }
    }

    Ok(FlagPolicy {
        flag_name,
        commands,
        operation,
        custom_error_message,
    })
}

fn decode_set_value(buf: &[u8]) -> Result<SetValue, OptionsParsingError> {
    let mut flag_values = Vec::new();
    let mut behavior = SetValueBehavior::Undefined;
    let mut behavior_set = false;
    let mut pos = 0;

    while pos < buf.len() {
        let (field, wire_type) = wire::decode_tag(buf, &mut pos)
            .map_err(|e| OptionsParsingError::new(e.0))?;
        match (field, wire_type) {
            (1, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                flag_values.push(wire::decode_string(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?);
            }
            (4, wire::WIRE_TYPE_VARINT) => {
                let v = wire::decode_varint(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?;
                behavior_set = true;
                behavior = match v {
                    1 => SetValueBehavior::AllowOverrides,
                    2 => SetValueBehavior::Append,
                    3 => SetValueBehavior::FinalValueIgnoreOverrides,
                    4 => SetValueBehavior::FinalValueThrowOnOverride,
                    _ => SetValueBehavior::Undefined,
                };
            }
            _ => skip_field(wire_type, buf, &mut pos)?,
        }
    }
    Ok(SetValue { flag_values, behavior, behavior_set })
}

fn decode_disallow_values(buf: &[u8]) -> Result<DisallowValues, OptionsParsingError> {
    let mut disallowed_values = Vec::new();
    let mut replacement = None;
    let mut pos = 0;

    while pos < buf.len() {
        let (field, wire_type) = wire::decode_tag(buf, &mut pos)
            .map_err(|e| OptionsParsingError::new(e.0))?;
        match (field, wire_type) {
            (1, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                disallowed_values.push(wire::decode_string(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?);
            }
            (3, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                replacement = Some(ReplacementValue::NewValue(
                    wire::decode_string(buf, &mut pos)
                        .map_err(|e| OptionsParsingError::new(e.0))?,
                ));
            }
            (4, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                let _ = wire::decode_bytes(buf, &mut pos);
                replacement = Some(ReplacementValue::UseDefault);
            }
            _ => skip_field(wire_type, buf, &mut pos)?,
        }
    }
    Ok(DisallowValues {
        disallowed_values,
        replacement,
    })
}

fn decode_allow_values(buf: &[u8]) -> Result<AllowValues, OptionsParsingError> {
    let mut allowed_values = Vec::new();
    let mut replacement = None;
    let mut pos = 0;

    while pos < buf.len() {
        let (field, wire_type) = wire::decode_tag(buf, &mut pos)
            .map_err(|e| OptionsParsingError::new(e.0))?;
        match (field, wire_type) {
            (1, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                allowed_values.push(wire::decode_string(buf, &mut pos)
                    .map_err(|e| OptionsParsingError::new(e.0))?);
            }
            (3, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                replacement = Some(ReplacementValue::NewValue(
                    wire::decode_string(buf, &mut pos)
                        .map_err(|e| OptionsParsingError::new(e.0))?,
                ));
            }
            (4, wire::WIRE_TYPE_LENGTH_DELIMITED) => {
                let _ = wire::decode_bytes(buf, &mut pos);
                replacement = Some(ReplacementValue::UseDefault);
            }
            _ => skip_field(wire_type, buf, &mut pos)?,
        }
    }
    Ok(AllowValues {
        allowed_values,
        replacement,
    })
}

fn skip_field(wire_type: u32, buf: &[u8], pos: &mut usize) -> Result<(), OptionsParsingError> {
    match wire_type {
        wire::WIRE_TYPE_VARINT => {
            let _ = wire::decode_varint(buf, pos).map_err(|e| OptionsParsingError::new(e.0))?;
        }
        wire::WIRE_TYPE_LENGTH_DELIMITED => {
            let _ = wire::decode_bytes(buf, pos).map_err(|e| OptionsParsingError::new(e.0))?;
        }
        wire::WIRE_TYPE_32BIT => {
            *pos += 4;
        }
        wire::WIRE_TYPE_64BIT => {
            *pos += 8;
        }
        _ => {
            return Err(OptionsParsingError::new(format!(
                "Invalid wire type {wire_type}"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Text format proto parser (`TextFormat.merge` semantics for `blaze.invocation_policy`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(String),
    Colon,
    Open(char),
    Close(char),
    LBracket,
    RBracket,
    Comma,
    Semi,
}

fn malformed() -> OptionsParsingError {
    OptionsParsingError::new("text format parse error")
}

fn tokenize_proto_text(input: &str) -> Result<Vec<Tok>, OptionsParsingError> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut toks = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == ':' {
            toks.push(Tok::Colon);
            i += 1;
        } else if c == '{' || c == '<' {
            toks.push(Tok::Open(c));
            i += 1;
        } else if c == '}' || c == '>' {
            toks.push(Tok::Close(c));
            i += 1;
        } else if c == '[' {
            toks.push(Tok::LBracket);
            i += 1;
        } else if c == ']' {
            toks.push(Tok::RBracket);
            i += 1;
        } else if c == ',' {
            toks.push(Tok::Comma);
            i += 1;
        } else if c == ';' {
            toks.push(Tok::Semi);
            i += 1;
        } else if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            let mut bytes: Vec<u8> = Vec::new();
            let mut closed = false;
            while i < chars.len() {
                let ch = chars[i];
                i += 1;
                if ch == quote {
                    closed = true;
                    break;
                }
                if ch == '\n' {
                    return Err(malformed());
                }
                if ch != '\\' {
                    let mut buf = [0u8; 4];
                    bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    continue;
                }
                let Some(&e) = chars.get(i) else { return Err(malformed()) };
                i += 1;
                match e {
                    'a' => bytes.push(7),
                    'b' => bytes.push(8),
                    'f' => bytes.push(12),
                    'n' => bytes.push(b'\n'),
                    'r' => bytes.push(b'\r'),
                    't' => bytes.push(b'\t'),
                    'v' => bytes.push(11),
                    '\\' | '\'' | '"' | '?' => bytes.push(e as u8),
                    '0'..='7' => {
                        let mut v = e.to_digit(8).unwrap();
                        for _ in 0..2 {
                            match chars.get(i).and_then(|d| d.to_digit(8)) {
                                Some(d) => {
                                    v = v * 8 + d;
                                    i += 1;
                                }
                                None => break,
                            }
                        }
                        bytes.push(v as u8);
                    }
                    'x' | 'X' => {
                        let mut v = match chars.get(i).and_then(|d| d.to_digit(16)) {
                            Some(d) => d,
                            None => return Err(malformed()),
                        };
                        i += 1;
                        if let Some(d) = chars.get(i).and_then(|d| d.to_digit(16)) {
                            v = v * 16 + d;
                            i += 1;
                        }
                        bytes.push(v as u8);
                    }
                    _ => return Err(malformed()),
                }
            }
            if !closed {
                return Err(malformed());
            }
            toks.push(Tok::Str(String::from_utf8(bytes).map_err(|_| malformed())?));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let st = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            toks.push(Tok::Ident(chars[st..i].iter().collect()));
        } else if c.is_ascii_digit() || c == '-' || c == '.' {
            let st = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '.' | '+' | '-' | '_')) {
                i += 1;
            }
            toks.push(Tok::Num(chars[st..i].iter().collect()));
        } else {
            return Err(malformed());
        }
    }
    Ok(toks)
}

#[derive(Debug, Clone)]
enum Node {
    Str(String),
    Ident(String),
    Num(String),
    Msg(Vec<(String, Node)>),
}

fn parse_fields(t: &[Tok], i: &mut usize, closing: Option<char>) -> Result<Vec<(String, Node)>, OptionsParsingError> {
    let mut out = Vec::new();
    loop {
        match (t.get(*i), closing) {
            (None, None) => return Ok(out),
            (None, Some(_)) => return Err(malformed()),
            (Some(Tok::Close(c)), Some(want)) if *c == want => {
                *i += 1;
                return Ok(out);
            }
            _ => {}
        }
        let Some(Tok::Ident(name)) = t.get(*i) else { return Err(malformed()) };
        let name = name.clone();
        *i += 1;
        let has_colon = matches!(t.get(*i), Some(Tok::Colon));
        if has_colon {
            *i += 1;
        }
        let parse_value = |i: &mut usize| -> Result<Node, OptionsParsingError> {
            match t.get(*i) {
                Some(Tok::Open(c)) => {
                    let close = if *c == '{' { '}' } else { '>' };
                    *i += 1;
                    Ok(Node::Msg(parse_fields(t, i, Some(close))?))
                }
                Some(Tok::Str(_)) if has_colon => {
                    let mut s = String::new();
                    while let Some(Tok::Str(x)) = t.get(*i) {
                        s.push_str(x);
                        *i += 1;
                    }
                    Ok(Node::Str(s))
                }
                Some(Tok::Ident(x)) if has_colon => {
                    *i += 1;
                    Ok(Node::Ident(x.clone()))
                }
                Some(Tok::Num(x)) if has_colon => {
                    *i += 1;
                    Ok(Node::Num(x.clone()))
                }
                _ => Err(malformed()),
            }
        };
        if has_colon && matches!(t.get(*i), Some(Tok::LBracket)) {
            *i += 1;
            if matches!(t.get(*i), Some(Tok::RBracket)) {
                *i += 1;
            } else {
                loop {
                    out.push((name.clone(), parse_value(i)?));
                    match t.get(*i) {
                        Some(Tok::Comma) => *i += 1,
                        Some(Tok::RBracket) => {
                            *i += 1;
                            break;
                        }
                        _ => return Err(malformed()),
                    }
                }
            }
        } else {
            out.push((name, parse_value(i)?));
        }
        if matches!(t.get(*i), Some(Tok::Comma | Tok::Semi)) {
            *i += 1;
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Str,
    /// A message field; the payload names its schema (see `schema_by_name`).
    Msg(&'static str),
    Enum(&'static [(&'static str, i32)]),
}

struct Field {
    name: &'static str,
    kind: Kind,
    repeated: bool,
    oneof: Option<&'static str>,
}

const fn f(name: &'static str, kind: Kind, repeated: bool, oneof: Option<&'static str>) -> Field {
    Field { name, kind, repeated, oneof }
}

const ORDER_ENUM: &[(&str, i32)] = &[("UNDEFINED", 0), ("PREPEND", 1), ("APPEND", 2)];
const BEHAVIOR_ENUM: &[(&str, i32)] = &[
    ("UNDEFINED", 0),
    ("ALLOW_OVERRIDES", 1),
    ("APPEND", 2),
    ("FINAL_VALUE_IGNORE_OVERRIDES", 3),
    ("FINAL_VALUE_THROW_ON_OVERRIDE", 4),
];

static INVOCATION_POLICY: &[Field] = &[
    f("flag_policies", Kind::Msg("FlagPolicy"), true, None),
    f("strategy_policy", Kind::Msg("StrategyPolicy"), false, None),
    f("config_policies", Kind::Msg("ConfigPolicy"), true, None),
];
static CONFIG_POLICY: &[Field] =
    &[f("name", Kind::Str, false, None), f("order", Kind::Enum(ORDER_ENUM), false, None)];
static FLAG_POLICY: &[Field] = &[
    f("flag_name", Kind::Str, false, None),
    f("commands", Kind::Str, true, None),
    f("set_value", Kind::Msg("SetValue"), false, Some("operation")),
    f("use_default", Kind::Msg("UseDefault"), false, Some("operation")),
    f("disallow_values", Kind::Msg("DisallowValues"), false, Some("operation")),
    f("allow_values", Kind::Msg("AllowValues"), false, Some("operation")),
    f("custom_error_message", Kind::Str, false, None),
];
static SET_VALUE: &[Field] =
    &[f("flag_value", Kind::Str, true, None), f("behavior", Kind::Enum(BEHAVIOR_ENUM), false, None)];
static USE_DEFAULT: &[Field] = &[];
static DISALLOW_VALUES: &[Field] = &[
    f("disallowed_values", Kind::Str, true, None),
    f("new_value", Kind::Str, false, Some("replacement_value")),
    f("use_default", Kind::Msg("UseDefault"), false, Some("replacement_value")),
];
static ALLOW_VALUES: &[Field] = &[
    f("allowed_values", Kind::Str, true, None),
    f("new_value", Kind::Str, false, Some("replacement_value")),
    f("use_default", Kind::Msg("UseDefault"), false, Some("replacement_value")),
];
static STRATEGY_POLICY: &[Field] = &[
    f("mnemonic_policy", Kind::Msg("MnemonicPolicy"), false, None),
    f("dynamic_remote_policy", Kind::Msg("MnemonicPolicy"), false, None),
    f("dynamic_local_policy", Kind::Msg("MnemonicPolicy"), false, None),
];
static MNEMONIC_POLICY: &[Field] = &[
    f("default_allowlist", Kind::Str, true, None),
    f("strategy_allowlist", Kind::Msg("StrategiesForMnemonic"), true, None),
];
static STRATEGIES_FOR_MNEMONIC: &[Field] =
    &[f("mnemonic", Kind::Str, false, None), f("strategy", Kind::Str, true, None)];

fn schema_by_name(name: &str) -> &'static [Field] {
    match name {
        "FlagPolicy" => FLAG_POLICY,
        "ConfigPolicy" => CONFIG_POLICY,
        "SetValue" => SET_VALUE,
        "UseDefault" => USE_DEFAULT,
        "DisallowValues" => DISALLOW_VALUES,
        "AllowValues" => ALLOW_VALUES,
        "StrategyPolicy" => STRATEGY_POLICY,
        "MnemonicPolicy" => MNEMONIC_POLICY,
        _ => STRATEGIES_FOR_MNEMONIC,
    }
}

/// Type-checks `fields` against `schema` and merges them as `TextFormat.merge` does with the default
/// `ALLOW_SINGULAR_OVERWRITES` policy: an unknown field or a wrong value kind is an error; a repeated
/// singular scalar keeps the last value; a repeated singular message is merged; setting one member
/// of a oneof clears the others.
fn normalize(fields: &[(String, Node)], schema: &[Field]) -> Result<Vec<(String, Node)>, OptionsParsingError> {
    let mut acc: Vec<(String, Node)> = Vec::new();
    for (name, node) in fields {
        let Some(fd) = schema.iter().find(|fd| fd.name == name) else { return Err(malformed()) };
        match (fd.kind, node) {
            (Kind::Str, Node::Str(_)) => {}
            (Kind::Enum(vals), Node::Ident(x)) => {
                if !vals.iter().any(|(n, _)| n == x) {
                    return Err(malformed());
                }
            }
            (Kind::Enum(vals), Node::Num(x)) => match x.parse::<i32>() {
                Ok(v) if vals.iter().any(|(_, n)| *n == v) => {}
                _ => return Err(malformed()),
            },
            (Kind::Msg(_), Node::Msg(_)) => {}
            _ => return Err(malformed()),
        }
        if let Some(o) = fd.oneof {
            let others: Vec<&str> =
                schema.iter().filter(|g| g.oneof == Some(o) && g.name != fd.name).map(|g| g.name).collect();
            acc.retain(|(n, _)| !others.contains(&n.as_str()));
        }
        if !fd.repeated {
            if let (Kind::Msg(_), Node::Msg(new_children)) = (fd.kind, node) {
                if let Some((_, Node::Msg(existing))) = acc.iter_mut().find(|(n, _)| n == name) {
                    existing.extend(new_children.iter().cloned());
                    continue;
                }
            }
            acc.retain(|(n, _)| n != name);
        }
        acc.push((name.clone(), node.clone()));
    }
    // Recurse into message fields (after merging, so merged children are normalized together).
    let mut out = Vec::with_capacity(acc.len());
    for (name, node) in acc {
        let fd = schema.iter().find(|fd| fd.name == name).unwrap();
        match (fd.kind, node) {
            (Kind::Msg(sub), Node::Msg(children)) => {
                out.push((name, Node::Msg(normalize(&children, schema_by_name(sub))?)));
            }
            (_, node) => out.push((name, node)),
        }
    }
    Ok(out)
}

fn strs(fields: &[(String, Node)], name: &str) -> Vec<String> {
    fields
        .iter()
        .filter(|(n, _)| n == name)
        .filter_map(|(_, v)| if let Node::Str(s) = v { Some(s.clone()) } else { None })
        .collect()
}

fn one_str(fields: &[(String, Node)], name: &str) -> Option<String> {
    strs(fields, name).pop()
}

fn msg<'a>(fields: &'a [(String, Node)], name: &str) -> Option<&'a Vec<(String, Node)>> {
    fields.iter().find(|(n, _)| n == name).and_then(|(_, v)| if let Node::Msg(m) = v { Some(m) } else { None })
}

fn enum_number(fields: &[(String, Node)], name: &str, vals: &[(&str, i32)]) -> i32 {
    match fields.iter().find(|(n, _)| n == name).map(|(_, v)| v) {
        Some(Node::Ident(x)) => vals.iter().find(|(n, _)| n == x).map(|(_, v)| *v).unwrap_or(0),
        Some(Node::Num(x)) => x.parse().unwrap_or(0),
        _ => 0,
    }
}

fn replacement(fields: &[(String, Node)]) -> Option<ReplacementValue> {
    if let Some(v) = one_str(fields, "new_value") {
        Some(ReplacementValue::NewValue(v))
    } else if msg(fields, "use_default").is_some() {
        Some(ReplacementValue::UseDefault)
    } else {
        None
    }
}

pub fn parse_text_invocation_policy(input: &str) -> Result<InvocationPolicy, OptionsParsingError> {
    let tokens = tokenize_proto_text(input)?;
    let mut i = 0;
    let fields = parse_fields(&tokens, &mut i, None)?;
    let fields = normalize(&fields, INVOCATION_POLICY)?;
    let mut policy = InvocationPolicy::default();
    for (name, node) in &fields {
        let Node::Msg(m) = node else { continue };
        match name.as_str() {
            "flag_policies" => {
                let operation = if let Some(sv) = msg(m, "set_value") {
                    Some(PolicyOperation::SetValue(SetValue {
                        flag_values: strs(sv, "flag_value"),
                        behavior_set: sv.iter().any(|(n, _)| n == "behavior"),
                        behavior: match enum_number(sv, "behavior", BEHAVIOR_ENUM) {
                            1 => SetValueBehavior::AllowOverrides,
                            2 => SetValueBehavior::Append,
                            3 => SetValueBehavior::FinalValueIgnoreOverrides,
                            4 => SetValueBehavior::FinalValueThrowOnOverride,
                            _ => SetValueBehavior::Undefined,
                        },
                    }))
                } else if msg(m, "use_default").is_some() {
                    Some(PolicyOperation::UseDefault(UseDefault))
                } else if let Some(d) = msg(m, "disallow_values") {
                    Some(PolicyOperation::DisallowValues(DisallowValues {
                        disallowed_values: strs(d, "disallowed_values"),
                        replacement: replacement(d),
                    }))
                } else if let Some(a) = msg(m, "allow_values") {
                    Some(PolicyOperation::AllowValues(AllowValues {
                        allowed_values: strs(a, "allowed_values"),
                        replacement: replacement(a),
                    }))
                } else {
                    None
                };
                policy.flag_policies.push(FlagPolicy {
                    flag_name: one_str(m, "flag_name").unwrap_or_default(),
                    commands: strs(m, "commands"),
                    operation,
                    custom_error_message: one_str(m, "custom_error_message"),
                });
            }
            "config_policies" => policy.config_policies.push(ConfigPolicy {
                name: one_str(m, "name").unwrap_or_default(),
                order: match enum_number(m, "order", ORDER_ENUM) {
                    1 => ConfigOrder::Prepend,
                    2 => ConfigOrder::Append,
                    _ => ConfigOrder::Undefined,
                },
            }),
            _ => {}
        }
    }
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_text_invocation_policy() {
        let text = r#"
flag_policies {
  flag_name: "test_output"
  commands: "test"
  set_value {
    flag_value: "all"
    behavior: FINAL_VALUE_IGNORE_OVERRIDES
  }
}
config_policies {
  name: "opt"
  order: PREPEND
}
"#;
        let policy = InvocationPolicy::parse(text).unwrap();
        assert_eq!(policy.flag_policies.len(), 1);
        assert_eq!(policy.flag_policies[0].flag_name, "test_output");
        assert_eq!(policy.flag_policies[0].commands, vec!["test"]);
        if let Some(PolicyOperation::SetValue(ref sv)) = policy.flag_policies[0].operation {
            assert_eq!(sv.flag_values, vec!["all"]);
            assert_eq!(sv.behavior, SetValueBehavior::FinalValueIgnoreOverrides);
        } else {
            panic!("Expected SetValue");
        }

        assert_eq!(policy.config_policies.len(), 1);
        assert_eq!(policy.config_policies[0].name, "opt");
        assert_eq!(policy.config_policies[0].order, ConfigOrder::Prepend);
    }
}

#[cfg(test)]
mod strict_text_tests {
    use super::*;

    fn bad(t: &str) {
        assert!(InvocationPolicy::parse(t).is_err(), "should be malformed: {t}");
    }

    #[test]
    fn rejects_truncated_and_unknown() {
        bad("flag_policies{");
        bad("flag_policies{flag_name:\"jobs\"");
        bad(r#"flag_policies{flag_name:"jobs" allow_values{allowed_values:"4" default_value:"4"}}"#);
        bad(r#"flag_policies{flag_name:"jobs" bogus:"x"}"#);
        bad(r#"flag_policies{flag_name:jobs}"#);
        bad(r#"flag_policies{flag_name:"jobs" set_value{behavior:NOPE}}"#);
        bad(r#"flag_policies{flag_name:"jobs" commands:"b"#);
        bad("xyz");
        bad("flag_policies: 3");
    }

    #[test]
    fn accepts_valid_variants() {
        let p = InvocationPolicy::parse(
            r#"flag_policies { flag_name: "jobs" commands: ["build", "test"]; set_value < flag_value: 'a' "b" behavior: 2 > }
               config_policies { name: "x" order: APPEND }
               strategy_policy { mnemonic_policy { default_allowlist: "local" } }"#,
        )
        .unwrap();
        assert_eq!(p.flag_policies[0].commands, vec!["build", "test"]);
        match &p.flag_policies[0].operation {
            Some(PolicyOperation::SetValue(sv)) => {
                assert_eq!(sv.flag_values, vec!["ab"]);
                assert_eq!(sv.behavior, SetValueBehavior::Append);
            }
            o => panic!("{o:?}"),
        }
    }
}
