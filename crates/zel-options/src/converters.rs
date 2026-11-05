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

//! Typed converters matching Bazel's `Converter.java` and `Converters.java`.

use crate::error::OptionsParsingError;
use crate::types::{EnumSpec, OptionType, OptionValue, TriState};
use std::collections::BTreeMap;
use std::time::Duration;
use zel_util::resource_converter::{self as rc, DoubleSpec, HostResources, IntegerSpec};

const ENABLED_REPS: &[&str] = &["true", "1", "yes", "t", "y"];
const DISABLED_REPS: &[&str] = &["false", "0", "no", "f", "n"];

fn err(msg: impl Into<String>) -> OptionsParsingError {
    OptionsParsingError::new(msg)
}

/// `Converters.BooleanConverter`: the error message quotes the *lower-cased* input.
pub fn convert_boolean(input: &str) -> Result<bool, OptionsParsingError> {
    let lower = input.to_ascii_lowercase();
    if ENABLED_REPS.contains(&lower.as_str()) {
        Ok(true)
    } else if DISABLED_REPS.contains(&lower.as_str()) {
        Ok(false)
    } else {
        Err(err(format!("'{lower}' is not a boolean")))
    }
}

/// `Converters.TriStateConverter` (Bazel 9.3.2).
pub fn convert_tristate(input: &str) -> Result<TriState, OptionsParsingError> {
    let lower = input.to_ascii_lowercase();
    if lower == "auto" {
        Ok(TriState::Auto)
    } else if ENABLED_REPS.contains(&lower.as_str()) {
        Ok(TriState::Yes)
    } else if DISABLED_REPS.contains(&lower.as_str()) {
        Ok(TriState::No)
    } else {
        // 9.3.2 `Converters.TriStateConverter`.
        Err(err(format!("'{lower}' is not a boolean")))
    }
}

/// `Converters.IntegerConverter` (`Integer.decode`: decimal, `0x`/`#` hex, leading-`0` octal).
pub fn convert_int(input: &str) -> Result<i32, OptionsParsingError> {
    rc::java_decode_int(input).ok_or_else(|| err(format!("'{input}' is not an int")))
}

/// `Converters.LongConverter` (`Long.decode`).
pub fn convert_long(input: &str) -> Result<i64, OptionsParsingError> {
    rc::java_decode_long(input).ok_or_else(|| err(format!("'{input}' is not a long")))
}

/// `Converters.DoubleConverter` (`Double.parseDouble`).
pub fn convert_double(input: &str) -> Result<f64, OptionsParsingError> {
    rc::java_parse_double(input).ok_or_else(|| err(format!("'{input}' is not a double")))
}

/// `Converters.DurationConverter`: `0`, or `^([0-9]+)(d|h|m|s|ms|ns)$`. A bare number other than
/// `0` is illegal (unlike `RemoteDurationConverter`).
pub fn convert_duration(input: &str) -> Result<Duration, OptionsParsingError> {
    if input == "0" {
        return Ok(Duration::ZERO);
    }
    let illegal = || err(format!("Illegal duration '{input}'."));
    let digits_end = input.bytes().position(|b| !b.is_ascii_digit()).unwrap_or(input.len());
    if digits_end == 0 {
        return Err(illegal());
    }
    let (num_part, unit) = input.split_at(digits_end);
    // `Long.parseLong` overflow is a `NumberFormatException` in Java; reject likewise.
    let num: i64 = num_part.parse().map_err(|_| illegal())?;
    let num = num as u64;
    match unit {
        "d" => Ok(Duration::from_secs(num.saturating_mul(86400))),
        "h" => Ok(Duration::from_secs(num.saturating_mul(3600))),
        "m" => Ok(Duration::from_secs(num.saturating_mul(60))),
        "s" => Ok(Duration::from_secs(num)),
        "ms" => Ok(Duration::from_millis(num)),
        "ns" => Ok(Duration::from_nanos(num)),
        _ => Err(illegal()),
    }
}

/// `CommonRemoteOptions.RemoteDurationConverter`: like [`convert_duration`] but a bare
/// non-negative integer means seconds.
pub fn convert_remote_duration(input: &str) -> Result<Duration, OptionsParsingError> {
    if !input.is_empty() && input.bytes().all(|b| b.is_ascii_digit()) {
        return convert_duration(&format!("{input}s"));
    }
    convert_duration(input)
}

/// `Converters.ByteSizeConverter`: `[0-9]+[KMGT]?` (upper-case multiplier only).
pub fn convert_bytesize(input: &str) -> Result<u64, OptionsParsingError> {
    let invalid = || err(format!("Invalid size: {input}"));
    let (num_part, mult) = match input.as_bytes().last() {
        Some(b'K') => (&input[..input.len() - 1], 1024i64),
        Some(b'M') => (&input[..input.len() - 1], 1024 * 1024),
        Some(b'G') => (&input[..input.len() - 1], 1024 * 1024 * 1024),
        Some(b'T') => (&input[..input.len() - 1], 1024i64 * 1024 * 1024 * 1024),
        _ => (input, 1),
    };
    if num_part.is_empty() || !num_part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let value: i64 = num_part.parse().map_err(|_| invalid())?;
    value.checked_mul(mult).map(|v| v as u64).ok_or_else(invalid)
}

/// `Converters.AssignmentConverter`.
pub fn convert_assignment(input: &str) -> Result<(String, String), OptionsParsingError> {
    match input.find('=') {
        Some(idx) if idx > 0 => Ok((input[..idx].to_string(), input[idx + 1..].to_string())),
        _ => Err(err(
            "Variable definitions must be in the form of a 'name=value' assignment",
        )),
    }
}

/// `CoreOptionConverters.FlagAliasConverter` (without the final label resolution).
pub fn convert_flag_alias(input: &str) -> Result<(String, String), OptionsParsingError> {
    let idx = match input.find('=') {
        Some(i) if i > 0 => i,
        _ => {
            return Err(err(
                "Flag alias definitions must be in the form of a 'name=label' assignment",
            ))
        }
    };
    let alias = &input[..idx];
    let label = &input[idx + 1..];

    // `Pattern.matches("\\w*", shortForm)`: ASCII word characters only.
    if !alias.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        return Err(err(format!(
            "{alias} should only consist of word characters to be a valid alias name."
        )));
    }
    if label.contains('=') {
        return Err(err("--flag_alias does not support flag value assignment."));
    }
    // `OptionsParser.STARLARK_SKIPPED_PREFIXES`.
    let long_form = format!("--{label}");
    if !["--//", "--no//", "--@", "--no@"].iter().any(|p| long_form.starts_with(p)) {
        return Err(err("--flag_alias only supports Starlark build settings."));
    }
    Ok((alias.to_string(), label.to_string()))
}

/// `Converters.joinEnglishList`.
pub fn join_english_list(items: &[&str]) -> String {
    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        if !out.is_empty() {
            out.push_str(if i + 1 < items.len() { ", " } else { " or " });
        }
        out.push_str(item);
    }
    if out.is_empty() { "nothing".to_string() } else { out }
}

/// `EnumConverter.convert` / `BoolOrEnumConverter.convert`; returns the matched constant.
pub fn convert_enum(spec: &EnumSpec, input: &str) -> Result<&'static str, OptionsParsingError> {
    if let Some(v) = spec.values.iter().find(|v| v.eq_ignore_ascii_case(input)) {
        return Ok(v);
    }
    if let (Some(t), Some(f)) = (spec.true_value, spec.false_value) {
        if let Ok(b) = convert_boolean(input) {
            return Ok(if b { t } else { f });
        }
    }
    Err(err(format!(
        "Not a valid {}: '{input}' (should be {})",
        spec.type_name,
        join_english_list(spec.values)
    )))
}

pub fn convert_compilation_mode(input: &str) -> Result<String, OptionsParsingError> {
    const SPEC: EnumSpec = EnumSpec {
        type_name: "compilation mode",
        values: &["fastbuild", "dbg", "opt"],
        true_value: None,
        false_value: None,
    };
    convert_enum(&SPEC, input).map(str::to_string)
}

/// `Converters.CommaSeparatedOptionListConverter`: empty input is the empty list, empty elements
/// are preserved.
pub fn convert_comma_list(input: &str) -> Vec<String> {
    split_list(input, ',')
}

fn split_list(input: &str, sep: char) -> Vec<String> {
    if input.is_empty() {
        Vec::new()
    } else {
        input.split(sep).map(|s| s.to_string()).collect()
    }
}

/// `SeparatedOptionListConverter` with `allowEmptyValues == false`.
fn split_list_non_empty(input: &str, sep: char, description: &str) -> Result<Vec<String>, OptionsParsingError> {
    let result = split_list(input, sep);
    if result.iter().any(|s| s.is_empty()) {
        if result.len() == 1 {
            return Ok(Vec::new());
        }
        return Err(err(format!("Empty values are not allowed as part of this {description}")));
    }
    Ok(result)
}

fn string_list(items: Vec<String>) -> OptionValue {
    OptionValue::List(items.into_iter().map(OptionValue::String).collect())
}

/// `java.lang.Integer.parseInt` (optional sign, ASCII decimal digits).
fn java_parse_int(input: &str) -> Option<i32> {
    let digits = input.strip_prefix(['+', '-']).unwrap_or(input);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Rust's `i32` parser accepts a leading `+` like `Integer.parseInt`; overflow yields `None`.
    input.parse::<i32>().ok()
}

/// `Integer.valueOf(String)` (same grammar as `Integer.parseInt`).
pub(crate) fn java_integer_value_of(input: &str) -> Option<i32> {
    java_parse_int(input)
}

/// `Converters.RangeConverter`.
pub fn convert_range(input: &str, min: i32, max: i32) -> Result<i32, OptionsParsingError> {
    let value = java_parse_int(input).ok_or_else(|| err(format!("'{input}' is not an int")))?;
    if value < min {
        Err(err(format!("'{input}' should be >= {min}")))
    } else if value > max {
        Err(err(format!("'{input}' should be <= {max}")))
    } else {
        Ok(value)
    }
}

/// `TestConfiguration.RunsPerTestConverter` / `ExecutionOptions.TestAttemptsConverter`
/// (both `PerLabelOptionsConverter`s): `N`, (`default` for attempts) or `regex@N`.
fn convert_per_label_attempts(input: &str, attempts: bool) -> Result<OptionValue, OptionsParsingError> {
    let validate = |v: &str| -> Result<(), OptionsParsingError> {
        if attempts {
            if v != "default" {
                let n = java_parse_int(v).ok_or_else(|| err(format!("'{input}' has a non-numeric value")))?;
                if n < 1 {
                    return Err(err(format!("'{v}' should be >= 1")));
                } else if n > 10 {
                    return Err(err(format!("'{v}' should be <= 10")));
                }
            }
        } else {
            let n = java_parse_int(v).ok_or_else(|| err(format!("'{input}' has a non-numeric value")))?;
            if n <= 0 {
                return Err(err(format!("'{input}' has a value < 1")));
            }
        }
        Ok(())
    };
    // Whole input as an integer (`parseAsInteger`).
    if java_parse_int(input).is_some() || (attempts && input == "default") {
        if attempts {
            validate(input)?;
        } else if java_parse_int(input).map_or(false, |n| n <= 0) {
            return Err(err(format!("'{input}' should be >= 1")));
        }
        return Ok(OptionValue::String(input.to_string()));
    }
    // `parseAsRegex`: last unescaped '@' separates the filter from the (single) option.
    let bytes: Vec<char> = input.chars().collect();
    let at = (0..bytes.len()).rev().find(|&i| bytes[i] == '@' && (i == 0 || bytes[i - 1] != '\\'));
    let opts: Vec<String> = match at {
        None => Vec::new(),
        Some(i) => {
            let piece: String = bytes[i + 1..].iter().collect();
            let mut out = Vec::new();
            let mut cur = String::new();
            let pc: Vec<char> = piece.chars().collect();
            for (k, c) in pc.iter().enumerate() {
                if *c == ',' && (k == 0 || pc[k - 1] != '\\') {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.push(*c);
                }
            }
            out.push(cur);
            out.into_iter().filter(|o| !o.trim().is_empty()).map(|o| o.replace("\\,", ",").replace("\\@", "@")).collect()
        }
    };
    if opts.len() != 1 {
        return Err(err(format!("'{input}' has multiple runs for a single pattern")));
    }
    validate(&opts[0])?;
    Ok(OptionValue::String(input.to_string()))
}

/// `TestTimeout.TestTimeoutConverter`; the map is keyed by timeout label with whole seconds.
fn convert_test_timeout(input: &str) -> Result<OptionValue, OptionsParsingError> {
    let mut values: Vec<i64> = Vec::new();
    // `Splitter.on(',').limit(6)`.
    for token in input.splitn(6, ',') {
        if !token.is_empty() || values.len() > 1 {
            let v = java_parse_int(token).ok_or_else(|| err(format!("'{input}' is not an int")))?;
            values.push(v as i64);
        }
    }
    let mut timeouts: Vec<Option<i64>> = vec![None; 4];
    match values.len() {
        1 => timeouts.iter_mut().for_each(|t| *t = Some(values[0])),
        4 => timeouts.iter_mut().zip(&values).for_each(|(t, v)| *t = Some(*v)),
        _ => return Err(err("Invalid number of comma-separated entries")),
    }
    let defaults = [("short", 60), ("moderate", 300), ("long", 900), ("eternal", 3600)];
    let mut map = BTreeMap::new();
    for ((label, default), t) in defaults.iter().zip(timeouts) {
        let secs = match t {
            Some(v) if v > 0 => v,
            _ => *default,
        };
        map.insert((*label).to_string(), secs.to_string());
    }
    Ok(OptionValue::Map(map))
}

/// `TestResourcesConverter`: `name=value` or `name=v1,v2,v3,v4` (small, medium, large, enormous).
fn convert_test_resources(input: &str) -> Result<OptionValue, OptionsParsingError> {
    let (name, value) = convert_assignment(input)?;
    let host = HostResources::detect_cached;
    let mut values = Vec::new();
    for s in value.split(',') {
        values.push(DoubleSpec::ASSIGNMENT.convert(s, &host).map_err(err)?);
    }
    if values.len() != 1 && values.len() != 4 {
        return Err(err(format!("Invalid number of comma-separated entries in {input}")));
    }
    let amounts = (0..4).map(|i| OptionValue::Double(values[i.min(values.len() - 1)])).collect();
    Ok(OptionValue::Entry(name, Box::new(OptionValue::List(amounts))))
}

/// `WorkerOptions.MultiResourceConverter`: `[name=]value` where `auto` means "unset".
fn convert_multi_resource(input: &str) -> Result<OptionValue, OptionsParsingError> {
    let unset = |name: &str| OptionValue::Entry(name.to_string(), Box::new(OptionValue::Void));
    if input == "null" || input == "auto" {
        return Ok(unset(""));
    }
    let value = |v: &str| -> Result<OptionValue, OptionsParsingError> {
        Ok(OptionValue::Int(IntegerSpec::WORKER_VALUE.convert_detected(v).map_err(err)?))
    };
    match input.find('=') {
        None => Ok(OptionValue::Entry(String::new(), Box::new(value(input)?))),
        Some(pos) => {
            let (name, v) = (&input[..pos], &input[pos + 1..]);
            if v == "auto" {
                Ok(unset(name))
            } else {
                Ok(OptionValue::Entry(name.to_string(), Box::new(value(v)?)))
            }
        }
    }
}

/// `OptionsUtils.WorkspacePathFragmentConverter` (`PathFragment` normalizes `.` and `a/..`).
pub(crate) fn convert_workspace_path(input: &str) -> Result<OptionValue, OptionsParsingError> {
    if input.starts_with('/') {
        return Err(err(format!("Expected relative path but got '{input}'.")));
    }
    let mut stack: Vec<&str> = Vec::new();
    for seg in input.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(stack.last(), Some(l) if *l != "..") {
                    stack.pop();
                } else {
                    stack.push("..");
                }
            }
            s => stack.push(s),
        }
    }
    if stack.contains(&"..") {
        return Err(err(format!(
            "Expected path without uplevel references ('..') but got '{input}'."
        )));
    }
    Ok(OptionValue::String(input.to_string()))
}

/// Converters selected by `OptionType::Custom(name)` (Java class simple names).
pub(crate) fn convert_custom(name: &str, raw: &str) -> Result<OptionValue, OptionsParsingError> {
    match name {
        "NonNegativeIntegerConverter" => Ok(OptionValue::Int(convert_range(raw, 0, i32::MAX)?)),
        "PositiveIntegerConverter" => Ok(OptionValue::Int(convert_range(raw, 1, i32::MAX)?)),
        "PercentageConverter" => Ok(OptionValue::Int(convert_range(raw, 0, 100)?)),
        "ProgressReportIntervalConverter" => Ok(OptionValue::Int(convert_range(raw, 0, 3600)?)),
        "OptionalPercentageConverter" => {
            if raw == "-1" {
                Ok(OptionValue::Int(-1))
            } else {
                Ok(OptionValue::Int(convert_range(raw, 0, 100)?))
            }
        }
        "MaxStdoutErrBytesConverter" => {
            const MAX: i32 = (i32::MAX - 8) >> 1;
            let v = convert_range(raw, -1, MAX)?;
            Ok(OptionValue::Int(if v >= 0 { v } else { MAX }))
        }
        "LogLevelConverter" => {
            // `Converters.LogLevelConverter`: levels 0..=6.
            match raw.parse::<i32>() {
                Ok(l) if (0..=6).contains(&l) => Ok(OptionValue::Int(l)),
                _ => Err(err(format!("Not a log level: {raw}"))),
            }
        }
        "RemoteDurationConverter" => Ok(OptionValue::Duration(convert_remote_duration(raw)?)),
        "CommaSeparatedNonEmptyOptionListConverter" => Ok(string_list(split_list_non_empty(
            raw,
            ',',
            "comma-separated list of options",
        )?)),
        "CommaSeparatedOptionSetConverter" => {
            let mut v = split_list(raw, ',');
            v.sort();
            v.dedup();
            Ok(string_list(v))
        }
        "ColonSeparatedOptionListConverter" => Ok(string_list(split_list(raw, ':'))),
        "StringToStringListConverter" => {
            // `AssignmentToListOfValuesConverter` with `AllowEmptyKeys.YES`.
            let (key, rest) = match raw.find('=') {
                Some(pos) if pos > 0 => (&raw[..pos], &raw[pos + 1..]),
                Some(_) => ("", &raw[1..]),
                None => ("", raw),
            };
            let mut values: Vec<&str> = rest.split(',').collect();
            if values.contains(&"") {
                if values.len() == 1 {
                    values.clear();
                } else {
                    return Err(err(
                        "Variable definitions must not contain empty strings or leading / trailing commas",
                    ));
                }
            }
            Ok(OptionValue::Entry(
                key.to_string(),
                Box::new(OptionValue::List(values.into_iter().map(|v| OptionValue::String(v.to_string())).collect())),
            ))
        }
        "AssignmentConverter" => {
            let (k, v) = convert_assignment(raw)?;
            Ok(OptionValue::Entry(k, Box::new(OptionValue::String(v))))
        }
        "FlagAliasConverter" => {
            let (k, v) = convert_flag_alias(raw)?;
            Ok(OptionValue::Entry(k, Box::new(OptionValue::Label(v))))
        }
        "RunsPerTestConverter" => convert_per_label_attempts(raw, false),
        "TestAttemptsConverter" => convert_per_label_attempts(raw, true),
        "TestTimeoutConverter" => convert_test_timeout(raw),
        "TestResourcesConverter" => convert_test_resources(raw),
        "MultiResourceConverter" => convert_multi_resource(raw),
        "AbsolutePathFragmentConverter" => {
            if raw.starts_with('/') {
                Ok(OptionValue::String(raw.to_string()))
            } else {
                Err(err(format!("Not an absolute path: '{raw}'")))
            }
        }
        "WorkspacePathFragmentConverter" => convert_workspace_path(raw),
        "SlimProfileConverter" => {
            let lower = raw.to_ascii_lowercase();
            if ENABLED_REPS.contains(&lower.as_str()) || DISABLED_REPS.contains(&lower.as_str()) {
                return Ok(OptionValue::String(raw.to_string()));
            }
            convert_bytesize(raw).map_err(|_| {
                err(format!("Expected a boolean or a size in bytes (e.g. 5M), but got: {raw}"))
            })?;
            Ok(OptionValue::String(raw.to_string()))
        }
        "MnemonicBooleanConverter" => {
            let (k, v) = match raw.find('=') {
                None => (String::new(), convert_boolean(raw)?),
                Some(pos) => (raw[..pos].to_string(), convert_boolean(&raw[pos + 1..])?),
            };
            Ok(OptionValue::Entry(k, Box::new(OptionValue::Boolean(v))))
        }
        "TransitionsConverter" => {
            let lower = raw.to_ascii_lowercase();
            if matches!(lower.as_str(), "none" | "lite" | "full") {
                Ok(OptionValue::String(lower))
            } else {
                Err(err(format!("Not a valid transition verbosity: '{raw}' (should be full, lite or none)")))
            }
        }
        "IncludeConfigFragmentsEnumConverter" | "ShowRequiredConfigFragmentsConverter" => {
            let lower = raw.to_ascii_lowercase();
            if matches!(lower.as_str(), "off" | "direct" | "transitive") {
                Ok(OptionValue::String(lower))
            } else {
                Err(err(format!("Not a valid include config fragments provider option: '{raw}' (should be off, direct or transitive)")))
            }
        }
        // Converters we do not model: pass the string through unchanged.
        _ => Ok(OptionValue::String(raw.to_string())),
    }
}

/// Converts `raw` for the option `def`: like [`convert_value`], but also runs the validating ports of the
/// Java converters in `converters_ext` for `OptionType::Custom` options.
pub fn convert_option(raw: &str, def: &crate::definition::OptionDefinition) -> Result<OptionValue, OptionsParsingError> {
    if let OptionType::Custom(name) = &def.option_type {
        if let Some(r) = crate::converters_ext::convert_custom_ext(def, name, raw) {
            return r;
        }
    }
    convert_value(raw, &def.option_type)
}

/// Dispatches raw string to an `OptionValue` according to `OptionType`.
pub fn convert_value(
    raw: &str,
    target_type: &OptionType,
) -> Result<OptionValue, OptionsParsingError> {
    match target_type {
        OptionType::Boolean => Ok(OptionValue::Boolean(convert_boolean(raw)?)),
        OptionType::TriState => Ok(OptionValue::TriState(convert_tristate(raw)?)),
        OptionType::Int => Ok(OptionValue::Int(convert_int(raw)?)),
        OptionType::Long => Ok(OptionValue::Long(convert_long(raw)?)),
        OptionType::Double => Ok(OptionValue::Double(convert_double(raw)?)),
        OptionType::String => Ok(OptionValue::String(raw.to_string())),
        OptionType::Duration => Ok(OptionValue::Duration(convert_duration(raw)?)),
        OptionType::ByteSize => Ok(OptionValue::ByteSize(convert_bytesize(raw)?)),
        OptionType::Label => {
            crate::converters_ext::convert_options_label(raw)?;
            Ok(OptionValue::Label(raw.to_string()))
        }
        OptionType::ListString => Ok(string_list(convert_comma_list(raw))),
        OptionType::ListLabel => {
            let items: Vec<OptionValue> =
                convert_comma_list(raw).into_iter().map(OptionValue::Label).collect();
            Ok(OptionValue::List(items))
        }
        OptionType::Map => {
            let (k, v) = convert_assignment(raw)?;
            let mut map = BTreeMap::new();
            map.insert(k, v);
            Ok(OptionValue::Map(map))
        }
        OptionType::Void => Ok(OptionValue::Void),
        OptionType::Enum(spec) => Ok(OptionValue::String(convert_enum(spec, raw)?.to_string())),
        OptionType::Resource(spec) => {
            Ok(OptionValue::Int(spec.convert_detected(raw).map_err(err)?))
        }
        OptionType::ResourceAssignment => {
            let (name, v) = rc::convert_resource_assignment(raw, &HostResources::detect_cached)
                .map_err(err)?;
            Ok(OptionValue::Entry(name, Box::new(OptionValue::Double(v))))
        }
        OptionType::Custom(name) => convert_custom(name, raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg<T: std::fmt::Debug>(r: Result<T, OptionsParsingError>) -> String {
        r.unwrap_err().message
    }

    #[test]
    fn test_converters() {
        assert!(convert_boolean("true").unwrap());
        assert!(convert_boolean("1").unwrap());
        assert!(convert_boolean("yes").unwrap());
        assert!(convert_boolean("T").unwrap());
        assert!(!convert_boolean("false").unwrap());
        assert!(!convert_boolean("0").unwrap());
        assert!(!convert_boolean("No").unwrap());
        // Bazel quotes the lower-cased input.
        assert_eq!(msg(convert_boolean("BoGuS")), "'bogus' is not a boolean");
        assert_eq!(msg(convert_boolean("")), "'' is not a boolean");

        assert_eq!(convert_tristate("auto").unwrap(), TriState::Auto);
        assert_eq!(convert_tristate("AUTO").unwrap(), TriState::Auto);
        assert_eq!(convert_tristate("true").unwrap(), TriState::Yes);
        assert_eq!(convert_tristate("no").unwrap(), TriState::No);
        assert_eq!(
            msg(convert_tristate("Maybe")),
            "'maybe' is not a boolean"
        );
    }

    #[test]
    fn integers_follow_java_decode() {
        assert_eq!(convert_int("42").unwrap(), 42);
        assert_eq!(convert_int("-7").unwrap(), -7);
        assert_eq!(convert_int("+7").unwrap(), 7);
        assert_eq!(convert_int("0x1F").unwrap(), 31);
        assert_eq!(convert_int("#1F").unwrap(), 31);
        assert_eq!(convert_int("010").unwrap(), 8);
        assert_eq!(msg(convert_int("09")), "'09' is not an int");
        assert_eq!(msg(convert_int("1.5")), "'1.5' is not an int");
        assert_eq!(msg(convert_int("")), "'' is not an int");
        assert_eq!(msg(convert_int("2147483648")), "'2147483648' is not an int");
        assert_eq!(convert_int("-2147483648").unwrap(), i32::MIN);
        assert_eq!(convert_long("10000000000").unwrap(), 10_000_000_000);
        assert_eq!(msg(convert_long("x")), "'x' is not a long");
        assert_eq!(convert_double("3.5").unwrap(), 3.5);
        assert_eq!(convert_double("1e2").unwrap(), 100.0);
        assert_eq!(convert_double(" 2 ").unwrap(), 2.0);
        assert_eq!(msg(convert_double("inf")), "'inf' is not a double");
        assert_eq!(msg(convert_double("abc")), "'abc' is not a double");
    }

    #[test]
    fn durations_match_converters_java() {
        let d = |s: &str| convert_duration(s).unwrap();
        assert_eq!(d("0"), Duration::ZERO);
        assert_eq!(d("30s"), Duration::from_secs(30));
        assert_eq!(d("5m"), Duration::from_secs(300));
        assert_eq!(d("2h"), Duration::from_secs(7200));
        assert_eq!(d("1d"), Duration::from_secs(86400));
        assert_eq!(d("100ms"), Duration::from_millis(100));
        assert_eq!(d("7ns"), Duration::from_nanos(7));
        assert_eq!(d("0s"), Duration::ZERO);
        assert_eq!(d("007s"), Duration::from_secs(7));
        // A bare number other than 0 has no unit, which Bazel rejects.
        for bad in ["120", "xyz", "", "s", "5 s", "5S", "-5s", "1.5s", "5sec", "+5s", "5s\n"] {
            assert_eq!(msg(convert_duration(bad)), format!("Illegal duration '{bad}'."), "{bad:?}");
        }
        assert_eq!(msg(convert_duration("99999999999999999999s")), "Illegal duration '99999999999999999999s'.");
        // RemoteDurationConverter: unit-less means seconds.
        assert_eq!(convert_remote_duration("60").unwrap(), Duration::from_secs(60));
        assert_eq!(convert_remote_duration("0").unwrap(), Duration::ZERO);
        assert_eq!(convert_remote_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(msg(convert_remote_duration("x")), "Illegal duration 'x'.");
    }

    #[test]
    fn byte_sizes_are_integers_with_uppercase_suffix() {
        assert_eq!(convert_bytesize("1024").unwrap(), 1024);
        assert_eq!(convert_bytesize("10K").unwrap(), 10240);
        assert_eq!(convert_bytesize("5M").unwrap(), 5 * 1024 * 1024);
        assert_eq!(convert_bytesize("2G").unwrap(), 2 * 1024 * 1024 * 1024);
        assert_eq!(convert_bytesize("1T").unwrap(), 1024u64.pow(4));
        for bad in ["", "K", "1.5G", "10k", "10KB", " 10", "-1", "1 K", "10X"] {
            assert_eq!(msg(convert_bytesize(bad)), format!("Invalid size: {bad}"), "{bad:?}");
        }
        assert_eq!(msg(convert_bytesize("9223372036854775807T")), "Invalid size: 9223372036854775807T");
        assert_eq!(msg(convert_bytesize("99999999999999999999")), "Invalid size: 99999999999999999999");
    }

    #[test]
    fn assignments_and_aliases() {
        let (k, v) = convert_assignment("FOO=BAR=BAZ").unwrap();
        assert_eq!((k.as_str(), v.as_str()), ("FOO", "BAR=BAZ"));
        let (k, v) = convert_assignment("FOO=").unwrap();
        assert_eq!((k.as_str(), v.as_str()), ("FOO", ""));
        let want = "Variable definitions must be in the form of a 'name=value' assignment";
        assert_eq!(msg(convert_assignment("NO_EQUALS")), want);
        assert_eq!(msg(convert_assignment("=v")), want);

        assert_eq!(convert_flag_alias("x=//a:b").unwrap(), ("x".to_string(), "//a:b".to_string()));
        assert_eq!(
            msg(convert_flag_alias("x")),
            "Flag alias definitions must be in the form of a 'name=label' assignment"
        );
        assert_eq!(
            msg(convert_flag_alias("=//a:b")),
            "Flag alias definitions must be in the form of a 'name=label' assignment"
        );
        assert_eq!(
            msg(convert_flag_alias("a-b=//a:b")),
            "a-b should only consist of word characters to be a valid alias name."
        );
        assert_eq!(msg(convert_flag_alias("a=//a:b=c")), "--flag_alias does not support flag value assignment.");
        assert_eq!(msg(convert_flag_alias("a=cpu")), "--flag_alias only supports Starlark build settings.");
    }

    #[test]
    fn enums() {
        assert_eq!(convert_compilation_mode("OPT").unwrap(), "opt");
        assert_eq!(
            msg(convert_compilation_mode("fast")),
            "Not a valid compilation mode: 'fast' (should be fastbuild, dbg or opt)"
        );
        assert_eq!(join_english_list(&[]), "nothing");
        assert_eq!(join_english_list(&["a"]), "a");
        assert_eq!(join_english_list(&["a", "b"]), "a or b");
        assert_eq!(join_english_list(&["a", "b", "c"]), "a, b or c");
        const SUB: EnumSpec = EnumSpec {
            type_name: "subcommand option",
            values: &["true", "pretty_print", "false"],
            true_value: Some("true"),
            false_value: Some("false"),
        };
        assert_eq!(convert_enum(&SUB, "Pretty_Print").unwrap(), "pretty_print");
        assert_eq!(convert_enum(&SUB, "1").unwrap(), "true");
        assert_eq!(convert_enum(&SUB, "no").unwrap(), "false");
        assert_eq!(
            msg(convert_enum(&SUB, "x")),
            "Not a valid subcommand option: 'x' (should be true, pretty_print or false)"
        );
    }

    #[test]
    fn per_label_attempt_converters() {
        let r = |s: &str| convert_custom("RunsPerTestConverter", s);
        let a = |s: &str| convert_custom("TestAttemptsConverter", s);
        assert!(r("3").is_ok() && r("//foo/.*,-//foo/bar/.*@3").is_ok());
        assert_eq!(msg(r("0")), "'0' should be >= 1");
        assert_eq!(msg(r("-1")), "'-1' should be >= 1");
        assert_eq!(msg(r("abc")), "'abc' has multiple runs for a single pattern");
        assert_eq!(msg(r("x@0")), "'x@0' has a value < 1");
        assert_eq!(msg(r("x@y")), "'x@y' has a non-numeric value");
        assert!(a("default").is_ok() && a("10").is_ok() && a("//:t@2").is_ok());
        assert_eq!(msg(a("0")), "'0' should be >= 1");
        assert_eq!(msg(a("11")), "'11' should be <= 10");
        assert_eq!(msg(a("abc")), "'abc' has multiple runs for a single pattern");
    }

    #[test]
    fn lists() {
        assert_eq!(convert_comma_list(""), Vec::<String>::new());
        assert_eq!(convert_comma_list("a,,b,"), vec!["a", "", "b", ""]);
        let ne = |s: &str| convert_custom("CommaSeparatedNonEmptyOptionListConverter", s);
        assert_eq!(ne("").unwrap(), OptionValue::List(vec![]));
        assert_eq!(ne("a,b").unwrap(), string_list(vec!["a".into(), "b".into()]));
        let want = "Empty values are not allowed as part of this comma-separated list of options";
        assert_eq!(msg(ne("a,,b")), want);
        assert_eq!(msg(ne(",")), want);
        assert_eq!(msg(ne("a,")), want);
        assert_eq!(
            convert_custom("CommaSeparatedOptionSetConverter", "b,a,b,,c").unwrap(),
            string_list(vec!["".into(), "a".into(), "b".into(), "c".into()])
        );
        assert_eq!(
            convert_custom("ColonSeparatedOptionListConverter", "%workspace%:/x::y").unwrap(),
            string_list(vec!["%workspace%".into(), "/x".into(), "".into(), "y".into()])
        );
        let sl = |s: &str| convert_custom("StringToStringListConverter", s);
        assert_eq!(
            sl("Genrule=local,sandboxed").unwrap(),
            OptionValue::Entry("Genrule".into(), Box::new(string_list(vec!["local".into(), "sandboxed".into()])))
        );
        assert_eq!(
            sl("local").unwrap(),
            OptionValue::Entry("".into(), Box::new(string_list(vec!["local".into()])))
        );
        assert_eq!(sl("G=").unwrap(), OptionValue::Entry("G".into(), Box::new(OptionValue::List(vec![]))));
        let want = "Variable definitions must not contain empty strings or leading / trailing commas";
        assert_eq!(msg(sl("G=a,,b")), want);
        assert_eq!(msg(sl("G=,a")), want);
    }

    #[test]
    fn ranges() {
        assert_eq!(convert_range("5", 0, 10).unwrap(), 5);
        assert_eq!(msg(convert_range("-1", 0, 10)), "'-1' should be >= 0");
        assert_eq!(msg(convert_range("11", 0, 10)), "'11' should be <= 10");
        assert_eq!(msg(convert_range("a", 0, 10)), "'a' is not an int");
        assert_eq!(msg(convert_range("0x5", 0, 10)), "'0x5' is not an int");
        let c = |n: &str, s: &str| convert_custom(n, s);
        assert_eq!(c("PercentageConverter", "100").unwrap(), OptionValue::Int(100));
        assert_eq!(msg(c("PercentageConverter", "101")), "'101' should be <= 100");
        assert_eq!(msg(c("PositiveIntegerConverter", "0")), "'0' should be >= 1");
        assert_eq!(msg(c("ProgressReportIntervalConverter", "3601")), "'3601' should be <= 3600");
        assert_eq!(c("OptionalPercentageConverter", "-1").unwrap(), OptionValue::Int(-1));
        assert_eq!(
            c("MaxStdoutErrBytesConverter", "-1").unwrap(),
            OptionValue::Int((i32::MAX - 8) >> 1)
        );
        assert_eq!(msg(c("LogLevelConverter", "7")), "Not a log level: 7");
    }

    #[test]
    fn test_timeout() {
        let t = |s: &str| match convert_test_timeout(s).unwrap() {
            OptionValue::Map(m) => m.into_iter().collect::<Vec<_>>(),
            _ => unreachable!(),
        };
        let to_owned = |v: Vec<(&str, &str)>| -> Vec<(String, String)> {
            v.into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
        };
        let all = |n: &str| -> Vec<(String, String)> {
            ["eternal", "long", "moderate", "short"].iter().map(|l| (l.to_string(), n.to_string())).collect()
        };
        assert_eq!(t("30"), all("30"));
        assert_eq!(t("30,"), all("30"));
        assert_eq!(
            t("1,2,3,4"),
            to_owned(vec![("eternal", "4"), ("long", "3"), ("moderate", "2"), ("short", "1")])
        );
        // Non-positive entries fall back to the label's default.
        assert_eq!(t("-1"), to_owned(vec![("eternal", "3600"), ("long", "900"), ("moderate", "300"), ("short", "60")]));
        assert_eq!(msg(convert_test_timeout("1,2")), "Invalid number of comma-separated entries");
        assert_eq!(msg(convert_test_timeout("abc")), "'abc' is not an int");
        assert_eq!(msg(convert_test_timeout("30s")), "'30s' is not an int");
    }

    #[test]
    fn resource_flag_types() {
        use zel_util::resource_converter::IntegerSpec as S;
        let host = HostResources::detect_cached();
        let cpus = host.host_cpus();
        assert_eq!(convert_value("auto", &OptionType::Resource(S::JOBS)).unwrap(), OptionValue::Int(cpus.min(5000)));
        assert_eq!(convert_value("auto", &OptionType::Resource(S::LOCAL_TEST_JOBS)).unwrap(), OptionValue::Int(0));
        assert_eq!(convert_value("7", &OptionType::Resource(S::JOBS)).unwrap(), OptionValue::Int(7));
        assert_eq!(
            msg(convert_value("0", &OptionType::Resource(S::JOBS))),
            "Value '(0)' must be at least 1."
        );
        assert_eq!(
            convert_value("cpu=3", &OptionType::ResourceAssignment).unwrap(),
            OptionValue::Entry("cpu".into(), Box::new(OptionValue::Double(3.0)))
        );
        assert_eq!(
            msg(convert_value("cpu", &OptionType::ResourceAssignment)),
            "Variable definitions must be in the form of a 'name=value' assignment"
        );
        let mr = |s: &str| convert_multi_resource(s).unwrap();
        assert_eq!(mr("auto"), OptionValue::Entry("".into(), Box::new(OptionValue::Void)));
        assert_eq!(mr("4"), OptionValue::Entry("".into(), Box::new(OptionValue::Int(4))));
        assert_eq!(mr("Javac=2"), OptionValue::Entry("Javac".into(), Box::new(OptionValue::Int(2))));
        assert_eq!(mr("Javac=auto"), OptionValue::Entry("Javac".into(), Box::new(OptionValue::Void)));
        assert_eq!(
            msg(convert_multi_resource("Javac=-1")),
            "Value '(-1.000000)' must be at least 0.000000."
        );
        let tr = convert_test_resources("memory=10,30,60,100").unwrap();
        assert_eq!(
            tr,
            OptionValue::Entry(
                "memory".into(),
                Box::new(OptionValue::List(vec![10.0, 30.0, 60.0, 100.0].into_iter().map(OptionValue::Double).collect()))
            )
        );
        assert_eq!(
            msg(convert_test_resources("memory=1,2")),
            "Invalid number of comma-separated entries in memory=1,2"
        );
    }

    #[test]
    fn paths() {
        assert_eq!(
            msg(convert_custom("AbsolutePathFragmentConverter", "rel/x")),
            "Not an absolute path: 'rel/x'"
        );
        assert!(convert_custom("AbsolutePathFragmentConverter", "/abs").is_ok());
        assert_eq!(
            msg(convert_workspace_path("/abs")),
            "Expected relative path but got '/abs'."
        );
        assert_eq!(
            msg(convert_workspace_path("a/../../b")),
            "Expected path without uplevel references ('..') but got 'a/../../b'."
        );
        assert!(convert_workspace_path("a/../b").is_ok());
    }
}
