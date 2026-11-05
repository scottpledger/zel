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

//! Port of `com.google.devtools.build.lib.util.ResourceConverter` (and the Java number parsing it
//! relies on: `Integer.decode`, Guava `Ints.tryParse` / `Doubles.tryParse`, `Float.parseFloat`).
//!
//! This lives in `zel-util` so that both the options parser (`zel-options`, which validates the
//! flags at parse time exactly like Bazel's `ParsedOptionDescription.getConvertedValue`) and the
//! execution phase (`zel-exec`, which evaluates the raw flag strings against the host) share one
//! implementation, including Bazel's exact error messages.

use std::sync::OnceLock;

pub const AUTO_KEYWORD: &str = "auto";
pub const HOST_CPUS_KEYWORD: &str = "HOST_CPUS";
pub const HOST_RAM_KEYWORD: &str = "HOST_RAM";

/// `BuildRequestOptions.MAX_JOBS`.
pub const MAX_JOBS: i32 = 5000;

/// `ResourceConverter.FLAG_SYNTAX`.
pub const FLAG_SYNTAX: &str = "an integer, or a keyword (\"auto\", \"HOST_CPUS\", \"HOST_RAM\"), optionally followed by an operation ([-|*]<float>) eg. \"auto\", \"HOST_CPUS*.5\"";

/// Detected host resources (`LocalHostComputeResources`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostResources {
    pub cpus: f64,
    pub memory_mb: f64,
}

impl HostResources {
    pub fn detect() -> Self {
        let cpus = std::thread::available_parallelism().map(|n| n.get() as f64).unwrap_or(1.0);
        Self { cpus, memory_mb: detect_memory_mb() }
    }

    /// Like [`detect`](Self::detect) but probed once per process.
    pub fn detect_cached() -> Self {
        static HOST: OnceLock<HostResources> = OnceLock::new();
        *HOST.get_or_init(HostResources::detect)
    }

    /// `ResourceConverter.HOST_CPUS_SUPPLIER`.
    pub fn host_cpus(&self) -> i32 {
        self.cpus.ceil() as i32
    }

    /// `ResourceConverter.HOST_RAM_SUPPLIER`.
    pub fn host_ram_mb(&self) -> i32 {
        self.memory_mb.ceil() as i32
    }
}

fn detect_memory_mb() -> f64 {
    #[cfg(target_os = "macos")]
    {
        if let Ok(o) = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "hw.memsize"]).output() {
            if let Ok(b) = String::from_utf8_lossy(&o.stdout).trim().parse::<f64>() {
                return b / (1024.0 * 1024.0);
            }
        }
    }
    if let Ok(t) = std::fs::read_to_string("/proc/meminfo") {
        for l in t.lines() {
            if let Some(r) = l.strip_prefix("MemTotal:") {
                if let Ok(kb) = r.trim().trim_end_matches("kB").trim().parse::<f64>() {
                    return kb / 1024.0;
                }
            }
        }
    }
    // Same fallback as `LocalHostCapacity` when probing fails.
    1024.0
}

/// Which keywords a converter accepts (the `keywords` map handed to `ResourceConverter`), in
/// the map's iteration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keywords {
    /// `IntegerConverter(Supplier auto, ..)`: `auto`, `HOST_CPUS`, `HOST_RAM`.
    AutoCpusRam,
    /// `AssignmentConverter`'s inner `DoubleConverter`: `HOST_CPUS`, `HOST_RAM` (no `auto`).
    CpusRam,
    /// `CpuResourceConverter`: `HOST_CPUS` only.
    Cpus,
    /// `RamResourceConverter`: `HOST_RAM` only.
    Ram,
}

impl Keywords {
    pub fn names(self) -> &'static [&'static str] {
        match self {
            Keywords::AutoCpusRam => &[AUTO_KEYWORD, HOST_CPUS_KEYWORD, HOST_RAM_KEYWORD],
            Keywords::CpusRam => &[HOST_CPUS_KEYWORD, HOST_RAM_KEYWORD],
            Keywords::Cpus => &[HOST_CPUS_KEYWORD],
            Keywords::Ram => &[HOST_RAM_KEYWORD],
        }
    }
}

/// What `auto` evaluates to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoValue {
    HostCpus,
    /// `() -> 0` (`--local_test_jobs`, worker flags).
    Zero,
}

/// An `IntegerConverter` configuration (`ResourceConverter.IntegerConverter` subclasses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegerSpec {
    pub keywords: Keywords,
    pub auto: AutoValue,
    pub min: i32,
    pub max: i32,
    /// `BuildRequestOptions.JobsConverter`: `%d`-formatted "at least" message and values above
    /// `max` are capped instead of rejected.
    pub jobs_style: bool,
}

impl IntegerSpec {
    pub const fn new(keywords: Keywords, auto: AutoValue, min: i32, max: i32) -> Self {
        Self { keywords, auto, min, max, jobs_style: false }
    }

    /// `JobsConverter` (`--jobs`).
    pub const JOBS: IntegerSpec =
        IntegerSpec { keywords: Keywords::AutoCpusRam, auto: AutoValue::HostCpus, min: 1, max: MAX_JOBS, jobs_style: true };
    /// `LocalTestJobsConverter` (`--local_test_jobs`).
    pub const LOCAL_TEST_JOBS: IntegerSpec = IntegerSpec::new(Keywords::AutoCpusRam, AutoValue::Zero, 0, i32::MAX);
    /// `LoadingPhaseThreadCountConverter`, `BuildRequestOptions.ThreadConverter`,
    /// `PackageOptions.ParallelismConverter`.
    pub const THREADS_MIN1: IntegerSpec = IntegerSpec::new(Keywords::AutoCpusRam, AutoValue::HostCpus, 1, i32::MAX);
    /// `IncludeScanningOptions.ParallelismConverter`, `SandboxOptions.AsyncTreeDeletesConverter`.
    pub const THREADS_MIN0: IntegerSpec = IntegerSpec::new(Keywords::AutoCpusRam, AutoValue::HostCpus, 0, i32::MAX);
    /// `CpuResourceConverter`.
    pub const CPU: IntegerSpec = IntegerSpec::new(Keywords::Cpus, AutoValue::HostCpus, 0, i32::MAX);
    /// `RamResourceConverter`.
    pub const RAM: IntegerSpec = IntegerSpec::new(Keywords::Ram, AutoValue::HostCpus, 0, i32::MAX);
    /// `WorkerOptions.MultiResourceConverter.valueConverter`: `IntegerConverter(() -> 0, 0, MAX)`.
    pub const WORKER_VALUE: IntegerSpec = IntegerSpec::new(Keywords::AutoCpusRam, AutoValue::Zero, 0, i32::MAX);
}

/// A `DoubleConverter` configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DoubleSpec {
    pub keywords: Keywords,
    pub min: f64,
    pub max: f64,
}

impl DoubleSpec {
    /// `AssignmentConverter`'s value converter (`[0, Double.MAX_VALUE]`, `HOST_CPUS`/`HOST_RAM`).
    pub const ASSIGNMENT: DoubleSpec = DoubleSpec { keywords: Keywords::CpusRam, min: 0.0, max: f64::MAX };
}

fn keyword_value(name: &str, auto: AutoValue, host: &dyn Fn() -> HostResources) -> f64 {
    match name {
        AUTO_KEYWORD => match auto {
            AutoValue::HostCpus => host().host_cpus() as f64,
            AutoValue::Zero => 0.0,
        },
        HOST_CPUS_KEYWORD => host().host_cpus() as f64,
        HOST_RAM_KEYWORD => host().host_ram_mb() as f64,
        _ => unreachable!("unknown keyword {name}"),
    }
}

/// `ResourceConverter.getTypeDescription()` (overridden by `CpuResourceConverter` and
/// `RamResourceConverter`).
pub fn type_description(keywords: Keywords) -> String {
    match keywords {
        Keywords::Cpus => format!("an integer, or \"{HOST_CPUS_KEYWORD}\", optionally followed by [-|*]<float>."),
        Keywords::Ram => {
            format!("an integer number of MBs, or \"{HOST_RAM_KEYWORD}\", optionally followed by [-|*]<float>.")
        }
        _ => {
            let names = keywords.names();
            format!(
                "an integer, or a keyword (\"{}\"), optionally followed by an operation ([-|*]<float>) eg. \"{}\", \"{HOST_CPUS_KEYWORD}*.5\"",
                names.join("\", \""),
                names[0]
            )
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Java number parsing.
// ---------------------------------------------------------------------------------------------

/// `Integer.decode(String)`; `None` on `NumberFormatException`.
pub fn java_decode_int(input: &str) -> Option<i32> {
    // Anything outside the int range is a NumberFormatException (so is "2147483648").
    java_decode_long(input).and_then(|v| i32::try_from(v).ok())
}

/// `Long.decode(String)`; `None` on `NumberFormatException`.
pub fn java_decode_long(input: &str) -> Option<i64> {
    let (neg, rest) = if let Some(r) = input.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = input.strip_prefix('+') {
        (false, r)
    } else {
        (false, input)
    };
    let (radix, digits) = if let Some(r) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        (16, r)
    } else if let Some(r) = rest.strip_prefix('#') {
        (16, r)
    } else if rest.len() > 1 && rest.starts_with('0') {
        (8, &rest[1..])
    } else {
        (10, rest)
    };
    // A second sign after the prefix is rejected by Java.
    if digits.is_empty() || digits.starts_with('-') || digits.starts_with('+') {
        return None;
    }
    if !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let mag = i128::from_str_radix(digits, radix).ok()?;
    let v = if neg { -mag } else { mag };
    i64::try_from(v).ok()
}

/// Guava `Ints.tryParse(String)`: optional leading `-`, then decimal digits, within int range.
pub fn guava_try_parse_int(input: &str) -> Option<i32> {
    let (neg, digits) = match input.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, input),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mag: i64 = digits.parse().ok()?;
    let v = if neg { -mag } else { mag };
    i32::try_from(v).ok()
}

/// The grammar accepted by `Double.parseDouble` after whitespace trimming (and, without the
/// trimming, by Guava's `Doubles.tryParse` pre-check): `[+-]?(NaN|Infinity|decimal|hex)`.
fn parse_java_floating(s: &str) -> Option<f64> {
    let (neg, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let sign = |v: f64| if neg { -v } else { v };
    if body == "NaN" {
        return Some(f64::NAN);
    }
    if body == "Infinity" {
        return Some(sign(f64::INFINITY));
    }
    // Optional trailing type suffix.
    let core = match body.as_bytes().last() {
        Some(b'f' | b'F' | b'd' | b'D') => &body[..body.len() - 1],
        _ => body,
    };
    if let Some(hex) = core.strip_prefix("0x").or_else(|| core.strip_prefix("0X")) {
        // HexSignificand BinaryExponent (the `p` exponent is mandatory).
        let p = hex.find(['p', 'P'])?;
        let (sig, exp) = (&hex[..p], &hex[p + 1..]);
        let (int_part, frac_part) = match sig.split_once('.') {
            Some((i, f)) => (i, f),
            None => (sig, ""),
        };
        if int_part.is_empty() && frac_part.is_empty() {
            return None;
        }
        if !int_part.chars().all(|c| c.is_ascii_hexdigit()) || !frac_part.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let exp_digits = exp.strip_prefix(['+', '-']).unwrap_or(exp);
        if exp_digits.is_empty() || !exp_digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let e: i32 = exp.parse().ok().unwrap_or(if exp.starts_with('-') { i32::MIN / 2 } else { i32::MAX / 2 });
        let mut m = 0f64;
        for c in int_part.chars() {
            m = m * 16.0 + c.to_digit(16)? as f64;
        }
        let mut scale = 1.0 / 16.0;
        for c in frac_part.chars() {
            m += c.to_digit(16)? as f64 * scale;
            scale /= 16.0;
        }
        return Some(sign(m * 2f64.powi(e)));
    }
    // Decimal: digits [. digits] | . digits, then optional exponent.
    let (mant, exp) = match core.find(['e', 'E']) {
        Some(i) => (&core[..i], Some(&core[i + 1..])),
        None => (core, None),
    };
    let (ip, fp) = match mant.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (mant, None),
    };
    let has_digits = !ip.is_empty() || fp.map(|f| !f.is_empty()).unwrap_or(false);
    if !has_digits || !ip.bytes().all(|b| b.is_ascii_digit()) || !fp.unwrap_or("").bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if let Some(e) = exp {
        let d = e.strip_prefix(['+', '-']).unwrap_or(e);
        if d.is_empty() || !d.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    let text = format!("{}{}{}{}", if ip.is_empty() { "0" } else { ip }, if fp.is_some() { "." } else { "" }, fp.unwrap_or(""), exp.map(|e| format!("e{e}")).unwrap_or_default());
    // Rust's parser rejects a trailing '.', which Java accepts.
    let text = text.replace(".e", ".0e");
    let text = if text.ends_with('.') { format!("{text}0") } else { text };
    text.parse::<f64>().ok().map(sign)
}

/// `Double.parseDouble(String)` (trims whitespace, i.e. chars `<= ' '`).
pub fn java_parse_double(input: &str) -> Option<f64> {
    parse_java_floating(input.trim_matches(|c: char| c <= ' '))
}

/// Guava `Doubles.tryParse(String)` succeeds (no trimming).
pub fn guava_try_parse_double(input: &str) -> Option<f64> {
    if input.chars().next().map(|c| c <= ' ').unwrap_or(false) || input.chars().last().map(|c| c <= ' ').unwrap_or(false) {
        return None;
    }
    parse_java_floating(input)
}

/// `Float.parseFloat(String)`.
pub fn java_parse_float(input: &str) -> Option<f32> {
    // Parsed through f64 then rounded; double rounding can differ in the last ulp for exotic
    // inputs but never for the short operands this is used with.
    java_parse_double(input).map(|v| v as f32)
}

/// Java's `String.format("%f", v)`.
pub fn java_format_f(v: f64) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v.is_infinite() {
        if v > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
    } else if v.abs() >= 1e15 {
        // Java prints the shortest-repr digits followed by zeros, like Rust's `Display`.
        format!("{v}.000000")
    } else {
        format!("{v:.6}")
    }
}

// ---------------------------------------------------------------------------------------------
// ResourceConverter.
// ---------------------------------------------------------------------------------------------

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// Does `s` match `(?:.[0-9]+)?` entirely (Java `.` excludes line terminators)?
fn matches_dot_digits(s: &str) -> bool {
    let mut it = s.chars();
    match it.next() {
        None => true,
        Some(c) if is_line_terminator(c) => false,
        Some(_) => {
            let rest = it.as_str();
            !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
        }
    }
}

/// Does `s` match `[0-9]?(?:.[0-9]+)?` entirely (with regex backtracking semantics)?
fn matches_operand(s: &str) -> bool {
    if matches_dot_digits(s) {
        return true;
    }
    match s.chars().next() {
        Some(c) if c.is_ascii_digit() => matches_dot_digits(&s[1..]),
        _ => false,
    }
}

/// `ResourceConverter.convertKeyword`.
pub fn convert_keyword(
    input: &str,
    keywords: Keywords,
    auto: AutoValue,
    host: &dyn Fn() -> HostResources,
) -> Result<f64, String> {
    // validInputPattern: (?<keyword>auto|HOST_CPUS|HOST_RAM)(?<expression>[-*][0-9]?(?:.[0-9]+)?)?
    let found = keywords.names().iter().find_map(|k| input.strip_prefix(k).map(|rest| (*k, rest)));
    if let Some((kw, rest)) = found {
        if rest.is_empty() {
            return Ok(keyword_value(kw, auto, host));
        }
        let mut chars = rest.chars();
        let op = chars.next().unwrap();
        if (op == '-' || op == '*') && matches_operand(chars.as_str()) {
            let operand_text = chars.as_str();
            let operand = java_parse_float(operand_text).ok_or_else(|| format!("'{operand_text} is not a float"))?;
            let first = keyword_value(kw, auto, host);
            return Ok(if op == '-' { first - operand as f64 } else { first * operand as f64 });
        }
    }
    Err(format!(
        "Parameter '{input}' does not follow correct syntax. This flag takes {}.",
        type_description(keywords)
    ))
}

impl IntegerSpec {
    /// `ResourceConverter.checkAndLimit` with the subclass overrides.
    fn check_and_limit(&self, value: i32) -> Result<i32, String> {
        if self.jobs_style {
            if value < self.min {
                return Err(format!("Value '({value})' must be at least {}.", self.min));
            }
            if value > self.max {
                // Bazel logs a warning and caps.
                return Ok(self.max);
            }
            return Ok(value);
        }
        if value < self.min {
            return Err(format!(
                "Value '({})' must be at least {}.",
                java_format_f(value as f64),
                java_format_f(self.min as f64)
            ));
        }
        if value > self.max {
            return Err(format!(
                "Value '({})' cannot be greater than {}.",
                java_format_f(value as f64),
                java_format_f(self.max as f64)
            ));
        }
        Ok(value)
    }

    /// `ResourceConverter.IntegerConverter.convert`.
    pub fn convert(&self, input: &str, host: &dyn Fn() -> HostResources) -> Result<i32, String> {
        if guava_try_parse_int(input).is_some() {
            // `Converters.IntegerConverter` (`Integer.decode`).
            let v = java_decode_int(input).ok_or_else(|| format!("'{input}' is not an int"))?;
            self.check_and_limit(v)
        } else {
            let d = convert_keyword(input, self.keywords, self.auto, host)?;
            // `(int) Math.round(double)`: floor(x + 0.5) as long, narrowed to int.
            let rounded = (d + 0.5).floor() as i64;
            self.check_and_limit(rounded as i32)
        }
    }

    pub fn convert_with_host(&self, input: &str, host: &HostResources) -> Result<i32, String> {
        self.convert(input, &|| *host)
    }

    pub fn convert_detected(&self, input: &str) -> Result<i32, String> {
        self.convert(input, &HostResources::detect_cached)
    }
}

impl DoubleSpec {
    fn check_and_limit(&self, value: f64) -> Result<f64, String> {
        // `Double.compareTo`: NaN is greater than everything (including +Infinity).
        let cmp_min = if value.is_nan() { std::cmp::Ordering::Greater } else { value.total_cmp(&self.min) };
        let cmp_min = if value == self.min { std::cmp::Ordering::Equal } else { cmp_min };
        if cmp_min == std::cmp::Ordering::Less {
            return Err(format!(
                "Value '({})' must be at least {}.",
                java_format_f(value),
                java_format_f(self.min)
            ));
        }
        let over = value.is_nan() || (value > self.max);
        if over {
            return Err(format!(
                "Value '({})' cannot be greater than {}.",
                java_format_f(value),
                java_format_f(self.max)
            ));
        }
        Ok(value)
    }

    /// `ResourceConverter.DoubleConverter.convert`: numeric literals are range-checked, keyword
    /// expressions are not.
    pub fn convert(&self, input: &str, host: &dyn Fn() -> HostResources) -> Result<f64, String> {
        if guava_try_parse_double(input).is_some() {
            // `Converters.DoubleConverter` (`Double.parseDouble`).
            let v = java_parse_double(input).ok_or_else(|| format!("'{input}' is not a double"))?;
            self.check_and_limit(v)
        } else {
            convert_keyword(input, self.keywords, AutoValue::HostCpus, host)
        }
    }
}

/// `Converters.AssignmentConverter.convert` (`name=value`, `name` non-empty).
pub fn split_assignment(input: &str) -> Result<(&str, &str), String> {
    match input.find('=') {
        Some(pos) if pos > 0 => Ok((&input[..pos], &input[pos + 1..])),
        _ => Err("Variable definitions must be in the form of a 'name=value' assignment".to_string()),
    }
}

/// `ResourceConverter.AssignmentConverter.getTypeDescription()`.
pub fn assignment_type_description() -> String {
    format!("a named double, 'name=value', where value is {}", type_description(Keywords::CpusRam))
}

/// `ResourceConverter.AssignmentConverter.convert` (`--local_resources=name=value`).
pub fn convert_resource_assignment(
    input: &str,
    host: &dyn Fn() -> HostResources,
) -> Result<(String, f64), String> {
    let (name, value) = split_assignment(input)?;
    Ok((name.to_string(), DoubleSpec::ASSIGNMENT.convert(value, host)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> HostResources {
        HostResources { cpus: 8.0, memory_mb: 16384.0 }
    }
    fn int(spec: IntegerSpec, s: &str) -> Result<i32, String> {
        spec.convert_with_host(s, &host())
    }

    #[test]
    fn jobs_forms() {
        assert_eq!(int(IntegerSpec::JOBS, "auto"), Ok(8));
        assert_eq!(int(IntegerSpec::JOBS, "HOST_CPUS"), Ok(8));
        assert_eq!(int(IntegerSpec::JOBS, "HOST_CPUS*.5"), Ok(4));
        assert_eq!(int(IntegerSpec::JOBS, "HOST_CPUS-2"), Ok(6));
        assert_eq!(int(IntegerSpec::JOBS, "HOST_CPUS*0.5"), Ok(4));
        assert_eq!(int(IntegerSpec::JOBS, "auto*.3"), Ok(2)); // 2.4 rounds to 2
        assert_eq!(int(IntegerSpec::JOBS, "auto*.5"), Ok(4));
        assert_eq!(int(IntegerSpec::JOBS, "HOST_RAM*.001"), Ok(16)); // 16.384
        assert_eq!(int(IntegerSpec::JOBS, "12"), Ok(12));
        assert_eq!(int(IntegerSpec::JOBS, "99999"), Ok(5000));
        assert_eq!(int(IntegerSpec::JOBS, "010"), Ok(8)); // Integer.decode: octal
        assert_eq!(int(IntegerSpec::JOBS, "0"), Err("Value '(0)' must be at least 1.".into()));
        assert_eq!(int(IntegerSpec::JOBS, "-3"), Err("Value '(-3)' must be at least 1.".into()));
        assert_eq!(int(IntegerSpec::JOBS, "HOST_CPUS-8"), Err("Value '(0)' must be at least 1.".into()));
        assert_eq!(int(IntegerSpec::JOBS, "09"), Err("'09' is not an int".into()));
    }

    #[test]
    fn jobs_errors() {
        let syntax = "an integer, or a keyword (\"auto\", \"HOST_CPUS\", \"HOST_RAM\"), optionally followed by an operation ([-|*]<float>) eg. \"auto\", \"HOST_CPUS*.5\"";
        for bad in ["lots", "1.5", "+4", "0x10", "HOST_CPUS/2", "HOST_CPUS*", "auto ", "AUTO", "HOST_CPUS*.5x", "99999999999"] {
            let expect = if bad == "HOST_CPUS*" {
                "' is not a float".to_string()
            } else {
                format!("Parameter '{bad}' does not follow correct syntax. This flag takes {syntax}.")
            };
            assert_eq!(int(IntegerSpec::JOBS, bad), Err(expect), "{bad}");
        }
        // Regex matches, Float.parseFloat does not.
        assert_eq!(int(IntegerSpec::JOBS, "HOST_CPUS*1x5"), Err("'1x5 is not a float".into()));
        assert_eq!(syntax, type_description(Keywords::AutoCpusRam));
        assert_eq!(syntax, FLAG_SYNTAX);
    }

    #[test]
    fn local_test_jobs_auto_is_zero() {
        assert_eq!(int(IntegerSpec::LOCAL_TEST_JOBS, "auto"), Ok(0));
        assert_eq!(int(IntegerSpec::LOCAL_TEST_JOBS, "0"), Ok(0));
        assert_eq!(int(IntegerSpec::LOCAL_TEST_JOBS, "HOST_CPUS*.5"), Ok(4));
        assert_eq!(
            int(IntegerSpec::LOCAL_TEST_JOBS, "-1"),
            Err("Value '(-1.000000)' must be at least 0.000000.".into())
        );
        assert_eq!(int(IntegerSpec::LOCAL_TEST_JOBS, "99999"), Ok(99999)); // no cap
    }

    #[test]
    fn cpu_ram_specs() {
        assert_eq!(int(IntegerSpec::CPU, "HOST_CPUS-1"), Ok(7));
        assert_eq!(int(IntegerSpec::RAM, "HOST_RAM*.5"), Ok(8192));
        assert_eq!(
            int(IntegerSpec::CPU, "auto"),
            Err("Parameter 'auto' does not follow correct syntax. This flag takes an integer, or \"HOST_CPUS\", optionally followed by [-|*]<float>..".into())
        );
        assert_eq!(
            int(IntegerSpec::RAM, "HOST_CPUS"),
            Err("Parameter 'HOST_CPUS' does not follow correct syntax. This flag takes an integer number of MBs, or \"HOST_RAM\", optionally followed by [-|*]<float>..".into())
        );
    }

    #[test]
    fn assignments() {
        let h = host();
        let conv = |s: &str| convert_resource_assignment(s, &|| h);
        assert_eq!(conv("cpu=HOST_CPUS-1"), Ok(("cpu".into(), 7.0)));
        assert_eq!(conv("cpu=2"), Ok(("cpu".into(), 2.0)));
        assert_eq!(conv("memory=HOST_RAM*.5"), Ok(("memory".into(), 8192.0)));
        assert_eq!(conv("gpu=1.5"), Ok(("gpu".into(), 1.5)));
        assert_eq!(conv("x=1e1"), Ok(("x".into(), 10.0)));
        assert_eq!(conv("x=2f"), Ok(("x".into(), 2.0)));
        assert_eq!(conv("x=.5"), Ok(("x".into(), 0.5)));
        // Keyword expressions are not range checked (DoubleConverter.convertKeyword).
        assert_eq!(conv("cpu=HOST_CPUS-100"), Ok(("cpu".into(), -92.0)));
        let err = "Variable definitions must be in the form of a 'name=value' assignment";
        assert_eq!(conv("cpu"), Err(err.into()));
        assert_eq!(conv("=3"), Err(err.into()));
        assert_eq!(conv("cpu=-1"), Err("Value '(-1.000000)' must be at least 0.000000.".into()));
        // `auto` is not a keyword for assignments.
        assert_eq!(
            conv("cpu=auto"),
            Err("Parameter 'auto' does not follow correct syntax. This flag takes an integer, or a keyword (\"HOST_CPUS\", \"HOST_RAM\"), optionally followed by an operation ([-|*]<float>) eg. \"HOST_CPUS\", \"HOST_CPUS*.5\".".into())
        );
        assert_eq!(
            conv("cpu=NaN"),
            Err(format!("Value '(NaN)' cannot be greater than 17976931348623157{}.000000.", "0".repeat(292)))
        );
    }

    #[test]
    fn java_parsing() {
        assert_eq!(java_decode_int("0x10"), Some(16));
        assert_eq!(java_decode_int("#10"), Some(16));
        assert_eq!(java_decode_int("-0x10"), Some(-16));
        assert_eq!(java_decode_int("+5"), Some(5));
        assert_eq!(java_decode_int("010"), Some(8));
        assert_eq!(java_decode_int("0"), Some(0));
        assert_eq!(java_decode_int("-2147483648"), Some(i32::MIN));
        assert_eq!(java_decode_int("2147483648"), None);
        assert_eq!(java_decode_int(""), None);
        assert_eq!(java_decode_int("1_0"), None);
        assert_eq!(java_parse_double(" 1.5 "), Some(1.5));
        assert_eq!(java_parse_double("5."), Some(5.0));
        assert_eq!(java_parse_double("1e3"), Some(1000.0));
        assert_eq!(java_parse_double("0x1p3"), Some(8.0));
        assert_eq!(java_parse_double("inf"), None);
        assert_eq!(java_parse_double("nan"), None);
        assert_eq!(java_parse_double("Infinity"), Some(f64::INFINITY));
        assert_eq!(java_parse_double("-Infinity"), Some(f64::NEG_INFINITY));
        assert_eq!(java_parse_double("e5"), None);
        assert_eq!(java_parse_double("."), None);
        assert_eq!(java_parse_double("1d"), Some(1.0));
        assert_eq!(guava_try_parse_double(" 1"), None);
    }
}
