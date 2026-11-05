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

//! Ports of the value converters of Bazel's option classes that validate their input
//! (`OptionOverrideConverter`, `RepositoryOverrideConverter`, `UUIDConverter`, `RegexFilterConverter`, enum
//! converters, ...). Each function cites the Java converter it mirrors; error texts are those of the Java
//! sources. The parser wraps them as `While parsing option --flag=value: <message>`.
//!
//! Conversion results are not needed beyond validation (canonicalisation uses the unconverted value, as Bazel's
//! `ParsedOptionDescription.getUnconvertedValue` does), so the converters return `OptionValue::String(raw)`.

use crate::converters::{convert_assignment, join_english_list};
use crate::definition::OptionDefinition;
use crate::enum_type_names::ENUM_TYPE_NAMES;
use crate::error::OptionsParsingError;
use crate::types::OptionValue;

type R = Result<OptionValue, OptionsParsingError>;

fn err(msg: impl Into<String>) -> OptionsParsingError {
    OptionsParsingError::new(msg)
}

fn ok(raw: &str) -> R {
    Ok(OptionValue::String(raw.to_string()))
}

/// Java's `Splitter.on(',').splitToList` (keeps empty strings, including for empty input).
fn guava_split(input: &str, sep: char) -> Vec<&str> {
    input.split(sep).collect()
}

/// `java.util.regex.Pattern.compile` syntax check. `Err` carries `PatternSyntaxException.getMessage()`.
fn java_pattern_check(pattern: &str) -> Result<(), String> {
    zel_util::java_regex::Regex::new(pattern).map(|_| ()).map_err(|e| e.to_string())
}

/// `CoreOptionConverters.convertOptionsLabel` with a null conversion context.
pub(crate) fn convert_options_label(input: &str) -> Result<(), OptionsParsingError> {
    let owned;
    let mut s = input;
    if !s.starts_with('/') && !s.starts_with('@') {
        owned = format!("//{input}");
        s = &owned;
    }
    zel_cmdline::Label::parse_canonical(s).map(|_| ()).map_err(|e| err(e.to_string()))
}

/// `RepositoryName.validateUserProvidedRepoName`: `[a-zA-Z0-9][-.\w]*`.
fn valid_user_provided_repo_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
}

/// `RepositoryName.validate` (`[\w\-.+]*`, with `.`/`..` rejected).
fn valid_canonical_repo_name(name: &str) -> bool {
    name.is_empty()
        || name == "_builtins"
        || (name != "." && name != ".." && name.chars().all(|c| c.is_ascii_alphanumeric() || "_-.+".contains(c)))
}

/// `RepositoryName.VALID_MODULE_NAME`: `[a-z]([a-z0-9._-]*[a-z0-9])?`.
fn valid_module_name(name: &str) -> bool {
    let b = name.as_bytes();
    if b.is_empty() || !b[0].is_ascii_lowercase() {
        return false;
    }
    if b.len() == 1 {
        return true;
    }
    let last = b[b.len() - 1];
    (last.is_ascii_lowercase() || last.is_ascii_digit())
        && b[1..].iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(c))
}

fn repository_injection(raw: &str) -> R {
    if !raw.contains('=') {
        return Err(err("Repository injections must be of the form 'repository-name=path'"));
    }
    let name = &raw[..raw.find('=').unwrap()];
    if !valid_user_provided_repo_name(name) {
        return Err(err("Invalid repository name given to inject"));
    }
    ok(raw)
}

fn repository_override(raw: &str) -> R {
    let Some(i) = raw.find('=') else {
        return Err(err("Repository overrides must be of the form 'repository-name=path'"));
    };
    let name = &raw[..i];
    let apparent = !name.is_empty() && !name.contains('+');
    let valid = if apparent { valid_user_provided_repo_name(name) } else { valid_canonical_repo_name(name) };
    if !valid {
        return Err(err("Invalid repository name given to override"));
    }
    ok(raw)
}

/// `RepositoryOptions.parseModuleNameAndPath`.
fn module_name_and_path(raw: &str, kind: &str) -> R {
    let Some(i) = raw.find('=') else {
        return Err(err(format!("{kind} must be of the form 'module-name=path'")));
    };
    let name = &raw[..i];
    if !valid_module_name(name) {
        return Err(err(format!(
            "invalid module name '{name}': valid names must 1) only contain lowercase letters (a-z), digits (0-9), dots (.), hyphens (-), and underscores (_); 2) begin with a lowercase letter; 3) end with a lowercase letter or digit."
        )));
    }
    ok(raw)
}

/// `ClientOptions.OptionOverrideConverter`.
fn option_override(raw: &str) -> R {
    const MSG: &str = "option overrides must be in form rcfile:command=option, where rcfile is a nonzero integer";
    let colon = raw.find(':');
    let assign = raw.find('=');
    let Some(colon) = colon else { return Err(err(MSG)) };
    match assign {
        Some(a) if a > colon + 1 => {}
        _ => return Err(err(MSG)),
    }
    match crate::converters::java_integer_value_of(&raw[..colon]) {
        Some(n) if n >= 0 => ok(raw),
        _ => Err(err(MSG)),
    }
}

/// `RegexFilter.RegexFilterConverter`.
fn regex_filter(input: &str) -> Result<(), OptionsParsingError> {
    if input.starts_with("--") {
        return Err(err(format!(
            "Failed to build filter: value looks like another flag ({input}). Either escape the value with \"\\-\\-\", or pass an explicit value to the flag."
        )));
    }
    let mut inclusions: Vec<String> = Vec::new();
    let mut exclusions: Vec<String> = Vec::new();
    // `input.split("(?<!\\\\),")`: split on ',' not preceded by a backslash; trailing empty strings are dropped.
    let chars: Vec<char> = input.chars().collect();
    let mut pieces: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, c) in chars.iter().enumerate() {
        if *c == ',' && !(i > 0 && chars[i - 1] == '\\') {
            pieces.push(std::mem::take(&mut cur));
        } else {
            cur.push(*c);
        }
    }
    pieces.push(cur);
    while pieces.len() > 1 && pieces.last().map(|p| p.is_empty()).unwrap_or(false) {
        pieces.pop();
    }
    if input.is_empty() {
        pieces = vec![String::new()];
    }
    for piece in pieces {
        let piece = piece.replace("\\,", ",").replace("\\@", "@");
        let (excluded, body) = match piece.chars().next() {
            Some('-') => (true, piece[1..].to_string()),
            Some('+') => (false, piece[1..].to_string()),
            _ => (false, piece.clone()),
        };
        if !body.is_empty() {
            if excluded { &mut exclusions } else { &mut inclusions }.push(body);
        }
    }
    for list in [inclusions, exclusions] {
        if list.is_empty() {
            continue;
        }
        // `takeUnionOfRegexes`: TreeSet-deduped, each wrapped in an atomic group.
        let mut deduped: Vec<String> = list;
        deduped.sort();
        deduped.dedup();
        let combined = format!("(?:(?>{}))", deduped.join(")|(?>"));
        java_pattern_check(&combined)
            .map_err(|m| err(format!("Failed to build valid regular expression: {m}")))?;
    }
    Ok(())
}

/// `Converters.RegexPatternConverter`.
fn regex_pattern(input: &str) -> R {
    java_pattern_check(input).map_err(|m| err(format!("Not a valid regular expression: {m}")))?;
    ok(input)
}

/// `Converters.AssignmentToListOfValuesConverter` with `AllowEmptyKeys.NO`.
fn assignment_to_list(
    raw: &str,
    key_check: impl Fn(&str) -> Result<(), OptionsParsingError>,
) -> R {
    let pos = raw.find('=');
    let Some(pos) = pos.filter(|p| *p > 0) else {
        return Err(err("Must be in the form of a 'key=value[,value]' assignment"));
    };
    let values = guava_split(&raw[pos + 1..], ',');
    if values.contains(&"") && values.len() > 1 {
        return Err(err("Variable definitions must not contain empty strings or leading / trailing commas"));
    }
    key_check(&raw[..pos])?;
    ok(raw)
}

/// `EnumFilterConverter` (`--test_size_filters`, `--test_timeout_filters`).
fn enum_filter(raw: &str, pretty: &str, all: &[&str]) -> R {
    if raw.is_empty() {
        return ok(raw);
    }
    let mut included: Vec<&str> = Vec::new();
    let mut excluded: Vec<&str> = Vec::new();
    for value in raw.split(',') {
        let exclude = value.starts_with('-');
        let s = if exclude { &value[1..] } else { value };
        let up = s.to_uppercase();
        match all.iter().find(|a| **a == up) {
            Some(a) => if exclude { &mut excluded } else { &mut included }.push(a),
            None => {
                return Err(err(format!("Invalid {pretty} filter '{value}' in the input '{raw}'")));
            }
        }
    }
    let result: Vec<&str> = if included.is_empty() {
        all.iter().copied().filter(|a| !excluded.contains(a)).collect()
    } else {
        included.into_iter().filter(|a| !excluded.contains(a)).collect()
    };
    if result.is_empty() {
        let mut cap = pretty.to_string();
        if let Some(f) = cap.get_mut(0..1) {
            f.make_ascii_uppercase();
        }
        return Err(err(format!("{cap} filter '{raw}' definition cannot match any tests")));
    }
    ok(raw)
}

/// `java.util.UUID.fromString` (JDK 17+): at most 36 chars, five `-`-separated hex groups.
fn java_uuid_ok(s: &str) -> bool {
    if s.chars().count() > 36 {
        return false;
    }
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    parts.iter().all(|p| {
        let digits = p.strip_prefix('+').unwrap_or(p);
        !digits.is_empty() && digits.len() <= 16 && digits.chars().all(|c| c.is_ascii_hexdigit())
    })
}

/// `CommonCommandOptions.UUIDConverter`.
fn uuid(raw: &str) -> R {
    if raw.is_empty() {
        return ok(raw);
    }
    if !java_uuid_ok(raw) {
        return Err(err(format!("Value '{raw}' is not a value UUID.")));
    }
    ok(raw)
}

/// `CommonCommandOptions.PrefixedUUIDConverter`.
fn prefixed_uuid(raw: &str) -> R {
    if raw.is_empty() {
        return ok(raw);
    }
    let units: Vec<u16> = raw.encode_utf16().collect();
    let good = units.len() >= 36 && java_uuid_ok(&String::from_utf16_lossy(&units[units.len() - 36..]));
    if !good {
        return Err(err(format!("Value '{raw}' does not end in a valid UUID.")));
    }
    ok(raw)
}

/// `DumpCommand.MemoryModeConverter`.
fn memory_mode(raw: &str) -> R {
    let items: Vec<&str> = raw.splitn(3, ':').collect();
    let mut collection = false;
    let mut full = false;
    let mut display = false;
    for word in items[0].split(',') {
        if word.starts_with("needle=") {
            continue;
        }
        match word {
            "shallow" | "deep" | "transitive" => collection = true,
            "full" => {
                collection = true;
                full = true;
            }
            "summary" | "count" | "bytes" => display = true,
            "notransient" | "noconfig" | "noprecomputed" | "noworkspacestatus" => {}
            _ => return Err(err(format!("Unrecognized word '{word}'"))),
        }
    }
    // `collectionMode` holds the *last* assigned mode in Java; `full` must reflect that.
    let last = items[0]
        .split(',')
        .filter(|w| ["shallow", "deep", "transitive", "full"].contains(w))
        .last();
    if let Some(l) = last {
        full = l == "full";
    }
    if !collection {
        return Err(err("No collection type specified"));
    }
    if !display {
        return Err(err("No display mode specified"));
    }
    if full {
        return ok(raw);
    }
    if items.len() != 3 {
        return Err(err("Should be in the form: <flags>:<node type>:<node>"));
    }
    if !["STARLARK_MODULE", "PACKAGE", "CONFIGURED_TARGET"].contains(&items[1].to_uppercase().as_str()) {
        return Err(err("Invalid subject type"));
    }
    ok(raw)
}

const EVENT_KINDS: &[&str] = &[
    "FATAL", "ERROR", "WARNING", "INFO", "PROGRESS", "DEBUG", "START", "FINISH", "SUBCOMMAND", "STDOUT", "STDERR",
    "PASS", "FAIL", "TIMEOUT", "CANCELLED", "DEPCHECKER",
];

fn enum_convert(type_name: &str, values: &[&str], input: &str) -> Result<(), OptionsParsingError> {
    if values.iter().any(|v| v.eq_ignore_ascii_case(input)) {
        return Ok(());
    }
    let lower: Vec<String> = values.iter().map(|v| v.to_ascii_lowercase()).collect();
    let refs: Vec<&str> = lower.iter().map(|s| s.as_str()).collect();
    Err(err(format!("Not a valid {type_name}: '{input}' (should be {})", join_english_list(&refs))))
}

/// `UiOptions.EventFiltersConverter`.
fn event_filters(raw: &str) -> R {
    if raw.is_empty() {
        return ok(raw);
    }
    for filter in raw.split(',') {
        if !filter.is_empty() {
            let body = filter.strip_prefix(['+', '-']).unwrap_or(filter);
            enum_convert("event kind", EVENT_KINDS, body)?;
        }
    }
    ok(raw)
}

/// `BuildEventProtocolOptions.BuildEventOutputGroupModeConverter`.
fn build_event_output_group_mode(raw: &str) -> R {
    let (_k, v) = convert_assignment(raw)?;
    enum_convert("Output group file reporting mode", &["NAMED_SET_OF_FILES_ONLY", "INLINE_ONLY", "BOTH"], &v)?;
    ok(raw)
}

/// `CppOptions.LibcTopLabelConverter`.
fn libc_top_label(raw: &str) -> R {
    if raw == "default" {
        return ok(raw);
    }
    if !raw.starts_with("//") {
        return Err(err("Not a label"));
    }
    convert_options_label(raw)?;
    ok(raw)
}

/// `ExecutionInfoModifier.Converter`.
fn modify_execution_info(raw: &str) -> R {
    if raw.is_empty() {
        return ok(raw);
    }
    for spec in raw.split(',') {
        // `^(?<pattern>.+)=(?<sign>[+-])(?<key>.+)$`: greedy pattern, so the *last* `=[+-]` with a non-empty key.
        let bytes = spec.as_bytes();
        let mut found: Option<usize> = None;
        for i in (1..bytes.len().saturating_sub(2)).rev() {
            if bytes[i] == b'=' && (bytes[i + 1] == b'+' || bytes[i + 1] == b'-') && i + 2 < bytes.len() {
                found = Some(i);
                break;
            }
        }
        let Some(i) = found else {
            return Err(err(format!("malformed expression '{spec}'")));
        };
        java_pattern_check(&spec[..i]).map_err(|m| err(format!("Not a valid regular expression: {m}")))?;
    }
    ok(raw)
}

/// `Md5Converter` (`RemoteAnalysisCachingOptions`).
fn md5(raw: &str) -> R {
    if raw.is_empty() {
        return ok(raw);
    }
    if raw.len() != 32 || !raw.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(err("Blaze checksum must be exactly 32 hex characters"));
    }
    ok(raw)
}

/// `Version.parse` validation.
fn version_ok(v: &str) -> Result<(), String> {
    if v.is_empty() {
        return Ok(());
    }
    let bad = || format!("bad version (does not match regex): {v}");
    let release_end = v.find(|c: char| !(c.is_ascii_alphanumeric() || c == '.')).unwrap_or(v.len());
    if release_end == 0 {
        return Err(bad());
    }
    let release = &v[..release_end];
    let mut rest = &v[release_end..];
    let mut prerelease = "";
    if let Some(r) = rest.strip_prefix('-') {
        let end = r.find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-')).unwrap_or(r.len());
        if end == 0 {
            return Err(bad());
        }
        prerelease = &r[..end];
        rest = &r[end..];
    }
    if let Some(r) = rest.strip_prefix('+') {
        if r.is_empty() || !r.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
            return Err(bad());
        }
        rest = "";
    }
    if !rest.is_empty() {
        return Err(bad());
    }
    for ident in release.split('.').chain(if prerelease.is_empty() { None } else { Some(prerelease) }.into_iter().flat_map(|p| p.split('.'))) {
        let numeric = !ident.is_empty() && ident.chars().all(|c| c.is_ascii_digit());
        if ident.is_empty() || (numeric && ident.parse::<i64>().is_err()) {
            return Err(format!("error parsing version: {v}"));
        }
    }
    Ok(())
}

/// `ModuleArg.ModuleArgConverter`.
fn module_arg(input: &str) -> Result<(), OptionsParsingError> {
    if input.eq_ignore_ascii_case("<root>") {
        return Ok(());
    }
    if let Some(name) = input.strip_prefix("@@") {
        if !valid_canonical_repo_name(name) {
            return Err(err(format!(
                "invalid argument '{input}': invalid repository name '{name}': repo names may contain only A-Z, a-z, 0-9, '-', '_', '.' and '+'"
            )));
        }
        return Ok(());
    }
    if let Some(name) = input.strip_prefix('@') {
        if !valid_user_provided_repo_name(name) {
            return Err(err(format!(
                "invalid argument '{input}': invalid user-provided repo name '{name}': valid names may contain only A-Z, a-z, 0-9, '-', '_', '.', and must start with a letter or a number"
            )));
        }
        return Ok(());
    }
    if let Some(at) = input.find('@') {
        let version = &input[at + 1..];
        if version.is_empty() {
            return Err(err(format!("invalid argument '{input}': use _ for the empty version")));
        }
        if version != "_" {
            version_ok(version).map_err(|m| err(format!("invalid argument '{input}': {m}")))?;
        }
    }
    Ok(())
}

/// `SeparatedOptionListConverter` (`allowEmptyValues == false`) over `,`.
fn comma_list_non_empty(raw: &str) -> Result<Vec<&str>, OptionsParsingError> {
    let items: Vec<&str> = if raw.is_empty() { Vec::new() } else { raw.split(',').collect() };
    if items.iter().any(|s| s.is_empty()) {
        if items.len() == 1 {
            return Ok(Vec::new());
        }
        return Err(err("Empty values are not allowed as part of this comma-separated list of options"));
    }
    Ok(items)
}

fn module_arg_list(raw: &str) -> R {
    for a in comma_list_non_empty(raw)? {
        module_arg(a)?;
    }
    ok(raw)
}

/// `ExtensionArg.ExtensionArgConverter`.
fn extension_arg(input: &str) -> Result<(), OptionsParsingError> {
    let Some(slash) = input.find('/') else {
        return Err(err(format!("Invalid argument {input}: missing .bzl label")));
    };
    match input.find('%') {
        Some(p) if p >= slash => {}
        _ => return Err(err(format!("Invalid argument {input}: missing extension name"))),
    }
    module_arg(&input[..slash])
}

fn extension_arg_list(raw: &str) -> R {
    for a in comma_list_non_empty(raw)? {
        extension_arg(a)?;
    }
    ok(raw)
}

/// `RemoteOptions.ScrubberConverter`: the value is a config file path.
fn scrubber(raw: &str) -> R {
    if raw.is_empty() {
        return ok(raw);
    }
    match std::fs::read(raw) {
        Ok(_) => ok(raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // `NoSuchFileException.getMessage()` is the path as `Paths.get` normalised it (`//` -> `/`).
            let mut norm = String::new();
            for c in raw.chars() {
                if !(c == '/' && norm.ends_with('/')) {
                    norm.push(c);
                }
            }
            if norm.len() > 1 && norm.ends_with('/') {
                norm.pop();
            }
            Err(err(format!("Failed to parse ScrubbingConfig: {norm}")))
        }
        Err(e) => Err(err(format!("Failed to parse ScrubbingConfig: {e}"))),
    }
}

/// `Converters.CaffeineSpecConverter` (`CaffeineSpec.parse`).
fn caffeine_spec(raw: &str) -> R {
    let bad = |m: String| Err(err(format!("Failed to parse CaffeineSpec: {m}")));
    for option in raw.split(',') {
        if option.is_empty() {
            continue;
        }
        let kv: Vec<&str> = option.split('=').collect();
        if kv.len() > 2 || kv.is_empty() {
            return bad(format!("key-value pair {option} with more than one equals sign"));
        }
        let key = kv[0];
        let value = kv.get(1).copied();
        fn needs_value<'v>(key: &str, v: Option<&'v str>) -> Result<&'v str, String> {
            match v {
                Some(v) if !v.is_empty() => Ok(v),
                _ => Err(format!("value of key {key} omitted")),
            }
        }
        match key {
            "initialCapacity" => match needs_value(key, value) {
                Ok(v) if v.parse::<i32>().is_ok() => {}
                Ok(v) => return bad(format!("key {key} value set to {v}, must be integer")),
                Err(m) => return bad(m),
            },
            "maximumSize" | "maximumWeight" => match needs_value(key, value) {
                Ok(v) if v.parse::<i64>().is_ok() => {}
                Ok(v) => return bad(format!("key {key} value set to {v}, must be integer")),
                Err(m) => return bad(m),
            },
            "expireAfterAccess" | "expireAfterWrite" | "refreshAfterWrite" => match needs_value(key, value) {
                Ok(v) => {
                    let (num, unit) = v.split_at(v.len().saturating_sub(1));
                    if num.is_empty()
                        || num.parse::<i64>().is_err()
                        || !["d", "h", "m", "s"].contains(&unit)
                    {
                        return bad(format!("key {key} value set to {v}, must be positive duration with a unit"));
                    }
                }
                Err(m) => return bad(m),
            },
            "weakKeys" | "weakValues" | "softValues" | "recordStats" => {
                if value.is_some() {
                    return bad(format!("key {key} does not take a value"));
                }
            }
            _ => return bad(format!("Unknown key {key}")),
        }
    }
    ok(raw)
}

/// `PackageOptions.DefaultVisibilityConverter` (the message really lacks the closing parenthesis).
fn default_visibility(raw: &str) -> R {
    match raw {
        "public" | "private" => ok(raw),
        _ => Err(err(format!("Not a valid default visibility: '{raw}' (should be 'public' or 'private'"))),
    }
}

/// `DigestHashFunction.DigestFunctionConverter`.
fn digest_function(raw: &str) -> R {
    const NAMES: &[&str] = &[
        "BLAKE3", "SHA-1", "SHA1", "SHA-256", "SHA256", "SHA-384", "SHA384", "SHA-512", "SHA512",
    ];
    if NAMES.iter().any(|n| n.eq_ignore_ascii_case(raw)) {
        return ok(raw);
    }
    Err(err(format!(
        "'{raw}' is not a valid hash function. Possible values are: BLAKE3, SHA-1, SHA-256, SHA-384, SHA-512"
    )))
}

/// `CppOptions.FissionOptionConverter`.
fn fission(raw: &str) -> R {
    if raw == "yes" || raw == "no" {
        return ok(raw);
    }
    for mode in raw.split(',') {
        enum_convert("compilation mode", &["FASTBUILD", "DBG", "OPT"], mode).map_err(|e| {
            // `CompilationMode.Converter` lists its values in lower case (`toString()`).
            err(e.message)
        })?;
    }
    ok(raw)
}

/// `WorkspaceStatusAction.OneLineStringConverter`.
fn one_line_string(raw: &str) -> R {
    if raw.contains('\n') {
        return Err(err("Value must not contain multiple lines"));
    }
    ok(raw)
}

/// `SandboxOptions.MountPairConverter`.
fn mount_pair(raw: &str) -> R {
    // `input.split("(?<!\\\\):")`: split on ':' not preceded by a backslash.
    let chars: Vec<char> = raw.chars().collect();
    let mut pieces: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, c) in chars.iter().enumerate() {
        if *c == ':' && !(i > 0 && chars[i - 1] == '\\') {
            pieces.push(std::mem::take(&mut cur));
        } else {
            cur.push(*c);
        }
    }
    pieces.push(cur);
    while pieces.len() > 1 && pieces.last().map(|p| p.is_empty()).unwrap_or(false) {
        pieces.pop(); // Java drops trailing empty strings
    }
    for p in &pieces {
        if p.trim().is_empty() {
            return Err(err(format!(
                "Input {raw} contains one or more empty paths. Input must be a single path to mount inside the sandbox or a mounting pair in the form of 'source:target'"
            )));
        }
    }
    if pieces.is_empty() || pieces.len() > 2 {
        return Err(err(
            "Input must be a single path to mount inside the sandbox or a mounting pair in the form of 'source:target'",
        ));
    }
    ok(raw)
}

/// `DynamicExecutionOptions.SignalListConverter`.
fn signal_list(raw: &str) -> R {
    if raw == "null" {
        return ok(raw);
    }
    for p in raw.split(',') {
        let t = p.trim();
        // Guava `Ints.tryParse`: optional '-', decimal digits, fits in an int.
        let good = !t.is_empty()
            && t.strip_prefix('-').unwrap_or(t).bytes().all(|b| b.is_ascii_digit())
            && t.parse::<i32>().map(|n| n > 0).unwrap_or(false);
        if !good {
            return Err(err(format!("No such signal {t}")));
        }
    }
    ok(raw)
}

/// `MemoryPressureOptions.GcThrashingLimitsConverter`.
fn gc_thrashing_limits(raw: &str) -> R {
    if raw.is_empty() {
        return ok(raw);
    }
    for part in raw.split(',') {
        let Some(colon) = part.find(':') else {
            return Err(err(format!("Expected <period>:<count>, got {part}")));
        };
        crate::converters::convert_duration(&part[..colon])?;
        crate::converters::convert_range(&part[colon + 1..], 1, i32::MAX)?;
    }
    ok(raw)
}

/// `DottedVersionConverter` (`DottedVersion.fromString`).
fn dotted_version(raw: &str) -> R {
    let illegal = || {
        err(format!(
            "Dotted version components must all start with the form \\d+([a-z0-9]*?)?(\\d+)? but got '{raw}'"
        ))
    };
    if raw.is_empty() {
        return Err(illegal());
    }
    let mut count = 0;
    for comp in raw.split('.') {
        // `[a-z]\w*` (case-insensitive) is a descriptive component that ends the numeric part.
        let mut cs = comp.chars();
        let descriptive = cs.next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false)
            && cs.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if descriptive {
            break;
        }
        let b = comp.as_bytes();
        if b.is_empty() || !b[0].is_ascii_digit() || !comp.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(illegal());
        }
        let lead: String = comp.chars().take_while(|c| c.is_ascii_digit()).collect();
        if lead.parse::<i32>().is_err() {
            return Err(illegal());
        }
        count += 1;
    }
    if count == 0 {
        return Err(illegal());
    }
    ok(raw)
}

/// `CoreOptionConverters.LabelListConverter` / `LabelOrderedSetConverter`.
fn label_list(raw: &str) -> R {
    for l in raw.split(',').filter(|s| !s.is_empty()) {
        convert_options_label(l)?;
    }
    ok(raw)
}

/// `CoreOptionConverters.LabelMapConverter`.
fn label_map(raw: &str) -> R {
    let mut seen: Vec<String> = Vec::new();
    for entry in raw.split(',').map(|e| e.trim()).filter(|e| !e.is_empty()) {
        let key = match entry.find('=') {
            None => entry.to_string(),
            Some(i) => {
                let value = &entry[i + 1..];
                if !value.is_empty() {
                    convert_options_label(value)?;
                }
                entry[..i].to_string()
            }
        };
        if seen.contains(&key) {
            return Err(err(format!("Key '{key}' appears twice")));
        }
        seen.push(key);
    }
    ok(raw)
}

/// `CoreOptionConverters.LabelToStringEntryConverter`.
fn label_to_string_entry(raw: &str) -> R {
    let eqs = raw.matches('=').count();
    if eqs != 1 || raw.starts_with('=') || raw.ends_with('=') {
        return Err(err(
            "Variable definitions must be in the form of a 'name=value' assignment. 'name' and 'value' must be non-empty and may not include '='.",
        ));
    }
    convert_options_label(&raw[..raw.find('=').unwrap()])?;
    ok(raw)
}

/// `CoreOptionConverters.EmptyToNullLabelConverter`.
fn empty_to_null_label(raw: &str) -> R {
    if !raw.is_empty() {
        convert_options_label(raw)?;
    }
    ok(raw)
}

/// `PerLabelOptions.PerLabelOptionsConverter`.
fn per_label_options(raw: &str) -> R {
    let chars: Vec<char> = raw.chars().collect();
    let mut at = None;
    for i in (0..chars.len()).rev() {
        if chars[i] == '@' && (i == 0 || chars[i - 1] != '\\') {
            at = Some(i);
            break;
        }
    }
    let filter: String = match at {
        Some(i) => chars[..i].iter().collect(),
        None => raw.to_string(),
    };
    regex_filter(&filter)?;
    ok(raw)
}

/// `RunUnderConverter` (`ShellUtils.tokenize` + label check).
fn run_under(raw: &str) -> R {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if c == '\\' => match chars.next() {
                Some(n) => cur.push(n),
                None => return Err(err("Not a valid command prefix unterminated escape")),
            },
            Some(_) => cur.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    has = true;
                }
                '\\' => match chars.next() {
                    Some(n) => {
                        cur.push(n);
                        has = true;
                    }
                    None => return Err(err("Not a valid command prefix unterminated escape")),
                },
                c if c.is_whitespace() => {
                    if has || !cur.is_empty() {
                        tokens.push(std::mem::take(&mut cur));
                        has = false;
                    }
                }
                c => cur.push(c),
            },
        }
    }
    if quote.is_some() {
        return Err(err("Not a valid command prefix unterminated quotation"));
    }
    if has || !cur.is_empty() {
        tokens.push(cur);
    }
    let Some(first) = tokens.first() else {
        return Err(err("Empty command"));
    };
    if first.starts_with("//") || first.starts_with('@') {
        convert_options_label(first).map_err(|e| err(format!("Not a valid label {}", e.message)))?;
    }
    ok(raw)
}

/// `CredentialHelperOptionConverter` (the scope's IDN check is not modelled).
fn credential_helper(raw: &str) -> R {
    let (scope, path) = match raw.find('=') {
        Some(i) => (Some(&raw[..i]), &raw[i + 1..]),
        None => (None, raw),
    };
    if let Some(s) = scope {
        if s.is_empty() {
            return Err(err("Credential helper scope must not be empty"));
        }
    }
    if path.is_empty() {
        return Err(err("Credential helper path must not be empty"));
    }
    ok(raw)
}

/// `ResourceConverter.IntegerConverter` based converters.
fn resource(spec: zel_util::resource_converter::IntegerSpec, raw: &str) -> R {
    spec.convert_detected(raw).map(|_| OptionValue::String(raw.to_string())).map_err(err)
}

/// `TestShardingStrategy.ShardingStrategyConverter`.
fn sharding_strategy(raw: &str) -> R {
    let lower = raw.to_ascii_lowercase();
    if lower == "explicit" || lower == "disabled" {
        return ok(raw);
    }
    if let Some(rest) = lower.strip_prefix("forced=") {
        let n = crate::converters::convert_int(&raw["forced=".len()..]).map_err(|e| err(e.message))?;
        let _ = rest;
        if n < 0 {
            return Err(err("Forced shards count cannot be negative."));
        }
        return ok(raw);
    }
    Err(err(format!(
        "Not a valid test sharding strategy: '{raw}' (should be explicit, disabled or forced=k where k is the number of shards to enforce)"
    )))
}

/// `MemoryProfileStableHeapParameters.Converter`.
fn memory_profile_stable_heap(raw: &str) -> R {
    let values: Vec<&str> = raw.split(',').collect();
    if values.len() % 2 != 0 {
        return Err(err("Expected even number of comma-separated integer values"));
    }
    let parse_err = || err("Expected even number of comma-separated integer values, could not parse integer in list");
    for pair in values.chunks(2) {
        let n = crate::converters::java_integer_value_of(pair[0]).ok_or_else(parse_err)?;
        let secs = crate::converters::java_integer_value_of(pair[1]).ok_or_else(parse_err)?;
        if n <= 0 {
            return Err(err("Number of times to GC must be positive"));
        }
        if secs < 0 {
            return Err(err("Number of seconds to sleep between GC's must be non-negative"));
        }
    }
    ok(raw)
}

/// `RemoteAnalysisCachingOptions.RemoteAnalysisCacheModeConverter`.
fn remote_analysis_cache_mode(raw: &str) -> R {
    const VALUES: &[&str] = &["UPLOAD", "DUMP_UPLOAD_MANIFEST_ONLY", "DOWNLOAD", "ASYNC_BIDI", "ASYNC_UPLOAD", "BIDI", "OFF"];
    if VALUES.iter().any(|v| v.eq_ignore_ascii_case(raw)) {
        return ok(raw);
    }
    Err(err(format!("Not a valid remote analysis cache mode: '{raw}' (should be {})", VALUES.join(", "))))
}

/// `ResourceConverter.AssignmentConverter` (`--experimental_sandbox_limits`).
fn resource_assignment(raw: &str) -> R {
    zel_util::resource_converter::convert_resource_assignment(
        raw,
        &zel_util::resource_converter::HostResources::detect_cached,
    )
    .map(|_| OptionValue::String(raw.to_string()))
    .map_err(err)
}

/// Converter classes whose (Java `EnumConverter`) conversion is handled by explicit code in `converters.rs`.
const EXPLICIT_ENUM_CONVERTERS: &[&str] =
    &["IncludeConfigFragmentsEnumConverter", "ShowRequiredConfigFragmentsConverter", "TransitionsConverter"];

fn enum_type_name(def: &OptionDefinition, converter: &str) -> &'static str {
    ENUM_TYPE_NAMES
        .iter()
        .find(|(flag, conv, _)| *flag == def.name && *conv == converter)
        .map(|(_, _, t)| *t)
        .unwrap_or(def.name)
}

/// Validating conversion for `OptionType::Custom` options; `None` when the converter is not modelled here.
pub(crate) fn convert_custom_ext(def: &OptionDefinition, name: &str, raw: &str) -> Option<R> {
    if !EXPLICIT_ENUM_CONVERTERS.contains(&name) && !def.enum_values.is_empty() && name.ends_with("Converter") {
        let converted = enum_convert(enum_type_name(def, name), def.enum_values, raw);
        // `BoolOrEnumConverter`: the enum constants win, otherwise the standard boolean spellings are accepted.
        if converted.is_err() && def.boolean_syntax && crate::converters::convert_boolean(raw).is_ok() {
            return Some(ok(raw));
        }
        return Some(converted.map(|_| OptionValue::String(raw.to_string())));
    }
    Some(match (name, def.name) {
        ("RepositoryInjectionConverter", _) => repository_injection(raw),
        ("RepositoryOverrideConverter", _) => repository_override(raw),
        ("ModuleOverrideConverter", _) => module_name_and_path(raw, "Module overrides"),
        ("ModuleInjectionConverter", _) => module_name_and_path(raw, "Module injections"),
        ("OptionOverrideConverter", _) => option_override(raw),
        ("RegexFilterAssignmentConverter", _) => assignment_to_list(raw, regex_filter),
        ("LabelToStringListConverter", _) => assignment_to_list(raw, convert_options_label),
        ("TestSizeFilterConverter", _) => enum_filter(raw, "test size", &["SMALL", "MEDIUM", "LARGE", "ENORMOUS"]),
        ("TestTimeoutFilterConverter", _) => enum_filter(raw, "test timeout", &["SHORT", "MODERATE", "LONG", "ETERNAL"]),
        ("LibcTopLabelConverter", _) => libc_top_label(raw),
        ("CommaSeparatedExtensionArgListConverter", _) => extension_arg_list(raw),
        ("CommaSeparatedModuleArgListConverter", _) => module_arg_list(raw),
        ("ModuleArgConverter", _) => module_arg(raw).map(|_| OptionValue::String(raw.to_string())),
        ("MemoryModeConverter", _) => memory_mode(raw),
        ("UUIDConverter", _) => uuid(raw),
        ("PrefixedUUIDConverter", _) => prefixed_uuid(raw),
        ("EventFiltersConverter", _) => event_filters(raw),
        ("ScrubberConverter", _) => scrubber(raw),
        ("Md5Converter", _) => md5(raw),
        ("BuildEventOutputGroupModeConverter", _) => build_event_output_group_mode(raw),
        ("RegexPatternConverter", _) => regex_pattern(raw),
        ("RegexFilterConverter", _) => regex_filter(raw).map(|_| OptionValue::String(raw.to_string())),
        ("Converter", "modify_execution_info") => modify_execution_info(raw),
        ("PlatformMappingKeyConverter", _) => platform_mapping_key(raw),
        ("HostPlatformConverter", _) => host_platform(raw),
        ("CommaSeparatedPackageNameListConverter", _) => package_name_list(raw),
        ("OptionConverter", "experimental_lazy_macro_expansion_packages") => lazy_macro_packages(raw),
        ("Converter", "experimental_tool_command_line") => tool_command_line(raw),
        ("ParallelismConverter", "experimental_include_scanning_parallelism") => {
            resource(zel_util::resource_converter::IntegerSpec::THREADS_MIN0, raw)
        }
        ("CaffeineSpecConverter", _) => caffeine_spec(raw),
        ("AssignmentConverter", "experimental_sandbox_limits") => resource_assignment(raw),
        ("ShardingStrategyConverter", _) => sharding_strategy(raw),
        ("RemoteAnalysisCacheModeConverter", _) => remote_analysis_cache_mode(raw),
        ("Converter", "memory_profile_stable_heap_parameters") => memory_profile_stable_heap(raw),
        ("DefaultVisibilityConverter", _) => default_visibility(raw),
        ("DigestFunctionConverter", _) => digest_function(raw),
        ("FissionOptionConverter", _) => fission(raw),
        ("OneLineStringConverter", _) => one_line_string(raw),
        ("MountPairConverter", _) => mount_pair(raw),
        ("SignalListConverter", _) => signal_list(raw),
        ("GcThrashingLimitsConverter", _) => gc_thrashing_limits(raw),
        ("DottedVersionConverter", _) => dotted_version(raw),
        ("LabelListConverter" | "LabelOrderedSetConverter", _) => label_list(raw),
        ("LabelMapConverter", _) => label_map(raw),
        ("LabelToStringEntryConverter", _) => label_to_string_entry(raw),
        ("EmptyToNullLabelConverter", _) => empty_to_null_label(raw),
        ("PerLabelOptionsConverter", _) => per_label_options(raw),
        ("RunUnderConverter", _) => run_under(raw),
        ("CredentialHelperOptionConverter", _) => credential_helper(raw),
        ("CpuResourceConverter", _) => resource(zel_util::resource_converter::IntegerSpec::CPU, raw),
        ("RamResourceConverter", _) => resource(zel_util::resource_converter::IntegerSpec::RAM, raw),
        ("ParallelismConverter", _) => resource(zel_util::resource_converter::IntegerSpec::THREADS_MIN1, raw),
        ("AsyncTreeDeletesConverter", _) => resource(zel_util::resource_converter::IntegerSpec::THREADS_MIN0, raw),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(r: R) -> String {
        r.unwrap_err().message
    }

    #[test]
    fn overrides() {
        assert_eq!(m(repository_injection("x")), "Repository injections must be of the form 'repository-name=path'");
        assert_eq!(m(repository_injection("_x=/p")), "Invalid repository name given to inject");
        assert!(repository_injection("x=/p").is_ok());
        assert_eq!(m(repository_override("x")), "Repository overrides must be of the form 'repository-name=path'");
        assert_eq!(m(repository_override("-x=/p")), "Invalid repository name given to override");
        assert!(repository_override("a+b=/p").is_ok());
        assert_eq!(
            m(module_name_and_path("Foo=/p", "Module overrides")),
            "invalid module name 'Foo': valid names must 1) only contain lowercase letters (a-z), digits (0-9), dots (.), hyphens (-), and underscores (_); 2) begin with a lowercase letter; 3) end with a lowercase letter or digit."
        );
    }

    #[test]
    fn option_override_and_uuid() {
        let msg = "option overrides must be in form rcfile:command=option, where rcfile is a nonzero integer";
        assert_eq!(m(option_override("x")), msg);
        assert_eq!(m(option_override("a:b=c")), msg);
        assert_eq!(m(option_override("-1:b=c")), msg);
        assert!(option_override("0:build=--x").is_ok());
        assert_eq!(m(uuid("x")), "Value 'x' is not a value UUID.");
        assert!(uuid("1-1-1-1-1").is_ok());
        assert!(uuid("123e4567-e89b-12d3-a456-426614174000").is_ok());
        assert_eq!(m(prefixed_uuid("short")), "Value 'short' does not end in a valid UUID.");
        assert!(prefixed_uuid("pre-123e4567-e89b-12d3-a456-426614174000").is_ok());
    }

    #[test]
    fn filters() {
        assert_eq!(m(enum_filter("x", "test size", &["SMALL"])), "Invalid test size filter 'x' in the input 'x'");
        assert!(enum_filter("small,-large", "test size", &["SMALL", "LARGE"]).is_ok());
        assert_eq!(
            m(enum_filter("-small", "test size", &["SMALL"])),
            "Test size filter '-small' definition cannot match any tests"
        );
        assert_eq!(m(regex_filter("--x").map(|_| OptionValue::Void)).starts_with("Failed to build filter"), true);
        assert!(m(regex_filter("(").map(|_| OptionValue::Void)).starts_with("Failed to build valid regular expression: Unclosed group"));
        assert_eq!(m(assignment_to_list("x", regex_filter)), "Must be in the form of a 'key=value[,value]' assignment");
    }

    #[test]
    fn memory_and_events() {
        assert_eq!(m(memory_mode("x")), "Unrecognized word 'x'");
        assert_eq!(m(memory_mode("deep")), "No display mode specified");
        assert_eq!(m(memory_mode("deep,count")), "Should be in the form: <flags>:<node type>:<node>");
        assert!(memory_mode("full,count").is_ok());
        assert_eq!(m(event_filters("x")), "Not a valid event kind: 'x' (should be fatal, error, warning, info, progress, debug, start, finish, subcommand, stdout, stderr, pass, fail, timeout, cancelled or depchecker)");
        assert!(event_filters("-info,+debug").is_ok());
    }

    #[test]
    fn execution_info() {
        assert_eq!(m(modify_execution_info("x")), "malformed expression 'x'");
        assert!(modify_execution_info("Foo=+key,Bar=-k").is_ok());
        assert_eq!(m(md5("x")), "Blaze checksum must be exactly 32 hex characters");
    }
}

/// Guava `BaseEncoding.base64().decode`: standard alphabet, trailing `=` padding optional in the
/// trimmed-length check; `None` for anything Guava rejects (`IllegalArgumentException`).
fn guava_base64_decode(input: &str) -> Option<Vec<u8>> {
    let trimmed = input.trim_end_matches('=');
    if input.len() - trimmed.len() > 2 {
        return None;
    }
    // Guava: the trimmed length must be a valid padding start position (len % 4 != 1); explicit
    // padding must complete a chunk of 4.
    if trimmed.len() % 4 == 1 || (input.len() != trimmed.len() && input.len() % 4 != 0) {
        return None;
    }
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in trimmed.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Protobuf wire-format well-formedness (what `Message.parseFrom` rejects with
/// `InvalidProtocolBufferException`, ignoring schema-specific checks).
fn wire_format_ok(mut b: &[u8]) -> bool {
    fn varint(b: &mut &[u8]) -> Option<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let byte = *b.get(i)?;
            v |= ((byte & 0x7f) as u64) << (7 * i);
            if byte & 0x80 == 0 {
                *b = &b[i + 1..];
                return Some(v);
            }
        }
        None
    }
    while !b.is_empty() {
        let Some(tag) = varint(&mut b) else { return false };
        if tag >> 3 == 0 {
            return false;
        }
        match tag & 7 {
            0 => {
                if varint(&mut b).is_none() {
                    return false;
                }
            }
            1 => {
                if b.len() < 8 {
                    return false;
                }
                b = &b[8..];
            }
            2 => {
                let Some(len) = varint(&mut b) else { return false };
                if len > b.len() as u64 {
                    return false;
                }
                b = &b[len as usize..];
            }
            5 => {
                if b.len() < 4 {
                    return false;
                }
                b = &b[4..];
            }
            _ => return false,
        }
    }
    true
}

/// `CommandLineEvent.ToolCommandLineEvent.Converter`: a plain string, or a base64 `CommandLine` proto.
fn tool_command_line(raw: &str) -> Result<OptionValue, OptionsParsingError> {
    if raw.is_empty() {
        return Ok(OptionValue::String(raw.to_string()));
    }
    if let Some(bytes) = guava_base64_decode(raw) {
        if !wire_format_ok(&bytes) {
            return Err(err(format!("Malformed value of --experimental_tool_command_line: {raw}")));
        }
    }
    Ok(OptionValue::String(raw.to_string()))
}

/// `PlatformOptions.PlatformMappingKeyConverter`.
fn platform_mapping_key(raw: &str) -> R {
    if !raw.is_empty() {
        crate::converters::convert_workspace_path(raw)?;
    }
    ok(raw)
}

/// `PlatformOptions.HostPlatformConverter` (a `LabelConverter`; empty selects the default).
fn host_platform(raw: &str) -> R {
    if !raw.is_empty() {
        convert_options_label(raw)?;
    }
    ok(raw)
}

fn parse_package_ids<'a>(items: impl Iterator<Item = &'a str>) -> Result<(), OptionsParsingError> {
    for s in items {
        zel_cmdline::validate_package_identifier(s).map_err(|e| err(e.to_string()))?;
    }
    Ok(())
}

/// `PackageOptions.CommaSeparatedPackageNameListConverter`.
fn package_name_list(raw: &str) -> R {
    if !raw.is_empty() {
        parse_package_ids(raw.split(','))?;
    }
    ok(raw)
}

/// `PackageOptions.LazyMacroExpansionPackages.OptionConverter`.
fn lazy_macro_packages(raw: &str) -> R {
    crate::converters::convert_custom("CommaSeparatedNonEmptyOptionListConverter", raw)?;
    let strings: Vec<&str> = raw.split(',').filter(|s| !s.is_empty()).collect();
    if !strings.contains(&"*") {
        parse_package_ids(strings.into_iter())?;
    }
    ok(raw)
}
