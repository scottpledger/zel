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

//! Pure-Rust port of Bazel's JUnit test runner (`com.google.testing.junit.runner`):
//! - `JUnit4Options` & `JUnit4Config`
//! - `RegExTestCaseFilter`, `AndFilter`, `SuiteTrimmingFilter`
//! - `HashBackedShardingFilter`, `RoundRobinShardingFilter`, `ShardingEnvironment`, `ShardingFilters`
//! - `TestInstant`, `TestInterval`, `TestCaseNode`, `TestSuiteNode`, `TestSuiteModel`
//! - `XmlEscapers`, `XmlWriter`, `AntXmlResultWriter`
//! - `PrematureExitFile` & `BazelTestRunner` exit-code semantics.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zel_util::jar::{java_string_hash_code, utf16_cmp};
use zel_util::java_regex::Regex;

pub const EXIT_CODE_SUCCESS: i32 = 0;
pub const EXIT_CODE_TEST_FAILURE: i32 = 1;
pub const EXIT_CODE_BAD_SUITE_ARGS: i32 = 2;
pub const EXIT_CODE_SYSTEM_EXIT_DETECTED: i32 = 121;
pub const EXIT_CODE_OUT_OF_MEMORY: i32 = 137;

// ------------------------------------------------ XML Escapers & Writer

/// XML 1.0 content and attribute escapers matching Bazel's `XmlEscapers.java`.
pub struct XmlEscapers;

impl XmlEscapers {
    fn is_legal_xml_scalar(c: char) -> bool {
        let u = c as u32;
        u == 0x09
            || u == 0x0A
            || u == 0x0D
            || (0x20..=0xD7FF).contains(&u)
            || (0xE000..=0xFFFD).contains(&u)
            || (0x10000..=0x10FFFF).contains(&u)
    }

    /// Escapes character content (`&`, `<`, `>`), keeping `\t`, `\n`, `\r`, `'` and `"` raw and
    /// replacing illegal XML 1.0 control characters / noncharacters with `U+FFFD`.
    pub fn escape_content(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                c if Self::is_legal_xml_scalar(c) => out.push(c),
                _ => out.push('\u{FFFD}'),
            }
        }
        out
    }

    /// Escapes attribute values: content rules plus `'` -> `&apos;`, `"` -> `&quot;`,
    /// `\t` -> `&#x9;`, `\n` -> `&#xA;`, `\r` -> `&#xD;`.
    pub fn escape_attribute(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '\'' => out.push_str("&apos;"),
                '"' => out.push_str("&quot;"),
                '\t' => out.push_str("&#x9;"),
                '\n' => out.push_str("&#xA;"),
                '\r' => out.push_str("&#xD;"),
                c if Self::is_legal_xml_scalar(c) => out.push(c),
                _ => out.push('\u{FFFD}'),
            }
        }
        out
    }

    /// Sanitizes text for inclusion inside `<![CDATA[...]]>` by replacing illegal XML 1.0 scalars
    /// with `U+FFFD` and splitting `]]>` into `]]>]]<![CDATA[>` so the parsed character stream is
    /// preserved verbatim.
    pub fn sanitize_cdata(s: &str) -> String {
        let mut clean = String::with_capacity(s.len());
        for c in s.chars() {
            if Self::is_legal_xml_scalar(c) {
                clean.push(c);
            } else {
                clean.push('\u{FFFD}');
            }
        }
        clean.replace("]]>", "]]>]]<![CDATA[>")
    }
}

#[derive(Debug, Clone)]
struct OpenElement {
    name: String,
    tag_open: bool,
    has_child_elements: bool,
    has_characters: bool,
}

/// Streaming XML 1.0 writer matching `com.google.testing.junit.runner.model.XmlWriter`.
#[derive(Debug, Default)]
pub struct XmlWriter {
    buf: String,
    stack: Vec<OpenElement>,
    eol: String,
}

impl XmlWriter {
    pub fn new() -> Self {
        Self::with_eol("\n")
    }

    pub fn with_eol(eol: impl Into<String>) -> Self {
        Self {
            buf: String::new(),
            stack: Vec::new(),
            eol: eol.into(),
        }
    }

    pub fn start_document(&mut self) {
        self.buf.push_str("<?xml version='1.0' encoding='UTF-8'?>");
    }

    pub fn start_element(&mut self, name: &str) {
        let depth = self.stack.len();
        if let Some(parent) = self.stack.last_mut() {
            if parent.tag_open {
                self.buf.push('>');
                parent.tag_open = false;
            }
            parent.has_child_elements = true;
        }
        self.buf.push_str(&self.eol);
        for _ in 0..depth {
            self.buf.push_str("  ");
        }
        self.buf.push('<');
        self.buf.push_str(name);
        self.stack.push(OpenElement {
            name: name.to_string(),
            tag_open: true,
            has_child_elements: false,
            has_characters: false,
        });
    }

    pub fn write_attribute(&mut self, name: &str, value: &str) {
        self.buf.push(' ');
        self.buf.push_str(name);
        self.buf.push_str("='");
        self.buf.push_str(&XmlEscapers::escape_attribute(value));
        self.buf.push('\'');
    }

    pub fn write_int_attribute(&mut self, name: &str, value: i32) {
        self.buf.push(' ');
        self.buf.push_str(name);
        self.buf.push_str("='");
        self.buf.push_str(&value.to_string());
        self.buf.push('\'');
    }

    pub fn write_characters(&mut self, text: &str) {
        if let Some(cur) = self.stack.last_mut() {
            if cur.tag_open {
                self.buf.push('>');
                cur.tag_open = false;
            }
            cur.has_characters = true;
        }
        if !text.is_empty() {
            self.buf.push_str(&XmlEscapers::escape_content(text));
        }
    }

    pub fn end_element(&mut self) {
        let Some(el) = self.stack.pop() else {
            return;
        };
        if el.tag_open {
            self.buf.push_str(" />");
        } else {
            if el.has_child_elements && !el.has_characters {
                self.buf.push_str(&self.eol);
                for _ in 0..self.stack.len() {
                    self.buf.push_str("  ");
                }
            }
            self.buf.push_str("</");
            self.buf.push_str(&el.name);
            self.buf.push('>');
        }
    }

    pub fn finish(mut self) -> String {
        while !self.stack.is_empty() {
            self.end_element();
        }
        self.buf.push_str(&self.eol);
        self.buf
    }
}

/// Formats `millis / 1000.0` like Java's `String.valueOf(millis / 1000.0)` (`0` -> `"0.0"`,
/// `100` -> `"0.1"`, `1` -> `"0.001"`).
pub fn format_java_seconds(millis: u64) -> String {
    let val = (millis as f64) / 1000.0;
    let s = format!("{val}");
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}

// ------------------------------------------------ Timing (`TestInstant` & `TestInterval`)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestInstant {
    pub wall_millis: i64,
    pub monotonic_nanos: u128,
}

impl TestInstant {
    pub fn now() -> Self {
        let wall = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_millis() as i64,
            Err(e) => -(e.duration().as_millis() as i64),
        };
        Self {
            wall_millis: wall,
            monotonic_nanos: 0,
        }
    }
}

/// Monotonic + wall-clock interval (`TestInterval.java`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestInterval {
    pub start: TestInstant,
    pub end: TestInstant,
}

impl TestInterval {
    pub fn new(start: TestInstant, end: TestInstant) -> Result<Self, String> {
        if start.monotonic_nanos > end.monotonic_nanos {
            return Err("Start must be before end".to_string());
        }
        Ok(Self { start, end })
    }

    pub fn with_end(self, end: TestInstant) -> Result<Self, String> {
        Self::new(self.start, end)
    }

    pub fn duration_millis(&self) -> u64 {
        ((self.end.monotonic_nanos - self.start.monotonic_nanos) / 1_000_000) as u64
    }

    /// Unions two intervals using the earlier monotonic start and the later monotonic end.
    pub fn around(self, other: Self) -> Self {
        let start = if self.start.monotonic_nanos <= other.start.monotonic_nanos {
            self.start
        } else {
            other.start
        };
        let end = if self.end.monotonic_nanos >= other.end.monotonic_nanos {
            self.end
        } else {
            other.end
        };
        Self { start, end }
    }

    /// Formats `self.start.wall_millis` as UTC ISO-8601 with 3 fractional digits
    /// (`2016-08-20T16:15:34.000Z`).
    pub fn start_instant_to_iso8601(&self) -> String {
        let total_ms = self.start.wall_millis;
        let secs = total_ms.div_euclid(1000);
        let millis = total_ms.rem_euclid(1000) as u32;
        let days = secs.div_euclid(86_400);
        let rem_secs = secs.rem_euclid(86_400) as u32;
        let hour = rem_secs / 3600;
        let min = (rem_secs % 3600) / 60;
        let sec = rem_secs % 60;

        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = y + if month <= 2 { 1 } else { 0 };

        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{millis:03}Z")
    }
}

// ------------------------------------------------ Descriptions, Filters, & Sharding

/// JUnit `Description` counterpart representing either a leaf test method or a suite.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TestDescription {
    pub display_name: String,
    pub class_name: Option<String>,
    pub method_name: Option<String>,
    pub children: Vec<TestDescription>,
}

impl TestDescription {
    pub fn leaf(class_name: impl Into<String>, method_name: impl Into<String>) -> Self {
        let cn = class_name.into();
        let mn = method_name.into();
        let display_name = format!("{mn}({cn})");
        Self {
            display_name,
            class_name: Some(cn),
            method_name: Some(mn),
            children: Vec::new(),
        }
    }

    pub fn suite(name: impl Into<String>, children: Vec<TestDescription>) -> Self {
        Self {
            display_name: name.into(),
            class_name: None,
            method_name: None,
            children,
        }
    }

    pub fn is_suite(&self) -> bool {
        !self.children.is_empty() || self.method_name.is_none()
    }

    pub fn is_test(&self) -> bool {
        self.children.is_empty() && self.method_name.is_some()
    }

    pub fn collect_leaf_tests(&self, out: &mut Vec<TestDescription>) {
        if self.is_test() {
            out.push(self.clone());
        } else {
            for child in &self.children {
                child.collect_leaf_tests(out);
            }
        }
    }
}

/// JUnit `Filter` counterpart.
pub trait TestFilter {
    fn should_run(&self, description: &TestDescription) -> Result<bool, String>;
    fn describe(&self) -> String;
    fn is_unconditional_all(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AllFilter;

impl TestFilter for AllFilter {
    fn should_run(&self, _description: &TestDescription) -> Result<bool, String> {
        Ok(true)
    }

    fn describe(&self) -> String {
        "all tests".to_string()
    }

    fn is_unconditional_all(&self) -> bool {
        true
    }
}

/// Include/exclude regex filter backed by `zel_util::java_regex::Regex`
/// (`RegExTestCaseFilter.java`).
#[derive(Debug, Clone)]
pub struct RegExTestCaseFilter {
    pattern_str: String,
    pattern: Regex,
    include: bool,
}

impl RegExTestCaseFilter {
    pub fn include(pattern: &str) -> Result<Self, String> {
        Ok(Self {
            pattern_str: pattern.to_string(),
            pattern: Regex::new(pattern).map_err(|e| e.to_string())?,
            include: true,
        })
    }

    pub fn exclude(pattern: &str) -> Result<Self, String> {
        Ok(Self {
            pattern_str: pattern.to_string(),
            pattern: Regex::new(pattern).map_err(|e| e.to_string())?,
            include: false,
        })
    }

    fn target_name(desc: &TestDescription) -> String {
        match (desc.class_name.as_deref(), desc.method_name.as_deref()) {
            (Some(cn), Some(mn)) if !cn.trim().is_empty() && !mn.trim().is_empty() => {
                format!("{cn}#{mn}")
            }
            _ => desc.display_name.clone(),
        }
    }
}

impl TestFilter for RegExTestCaseFilter {
    fn should_run(&self, description: &TestDescription) -> Result<bool, String> {
        if !description.is_test() {
            return Ok(true);
        }
        let target = Self::target_name(description);
        let matched = self.pattern.find(&target).is_some();
        Ok(if self.include { matched } else { !matched })
    }

    fn describe(&self) -> String {
        if self.include {
            format!("RegEx[{}]", self.pattern_str)
        } else {
            format!("NOT RegEx[{}]", self.pattern_str)
        }
    }
}

/// Conjunction of two [`TestFilter`]s (`AndFilter.java` / `Filters.java`).
pub struct AndFilter {
    first: Box<dyn TestFilter>,
    second: Box<dyn TestFilter>,
}

impl AndFilter {
    pub fn create(first: Box<dyn TestFilter>, second: Box<dyn TestFilter>) -> Box<dyn TestFilter> {
        if first.is_unconditional_all() {
            second
        } else if second.is_unconditional_all() {
            first
        } else {
            Box::new(Self { first, second })
        }
    }
}

impl TestFilter for AndFilter {
    fn should_run(&self, description: &TestDescription) -> Result<bool, String> {
        Ok(self.first.should_run(description)? && self.second.should_run(description)?)
    }

    fn describe(&self) -> String {
        format!("{} AND {}", self.first.describe(), self.second.describe())
    }
}

/// Decorator pruning suites that have no runnable descendant leaf tests (`SuiteTrimmingFilter.java`).
pub struct SuiteTrimmingFilter {
    delegate: Box<dyn TestFilter>,
}

impl SuiteTrimmingFilter {
    pub fn new(delegate: Box<dyn TestFilter>) -> Self {
        Self { delegate }
    }
}

impl TestFilter for SuiteTrimmingFilter {
    fn should_run(&self, description: &TestDescription) -> Result<bool, String> {
        if !self.delegate.should_run(description)? {
            return Ok(false);
        }
        if description.is_test() {
            return Ok(true);
        }
        for child in &description.children {
            if self.should_run(child)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn describe(&self) -> String {
        self.delegate.describe()
    }
}

/// Hash-modulo sharding filter using Java's `String.hashCode()` (`HashBackedShardingFilter.java`).
#[derive(Debug, Clone)]
pub struct HashBackedShardingFilter {
    shard_index: i32,
    total_shards: i32,
}

impl HashBackedShardingFilter {
    pub fn new(shard_index: i32, total_shards: i32) -> Result<Self, String> {
        if shard_index < 0 || total_shards <= shard_index {
            return Err(format!(
                "Invalid shard bounds: shard_index={shard_index}, total_shards={total_shards}"
            ));
        }
        Ok(Self {
            shard_index,
            total_shards,
        })
    }
}

impl TestFilter for HashBackedShardingFilter {
    fn should_run(&self, description: &TestDescription) -> Result<bool, String> {
        if !description.is_test() {
            return Ok(true);
        }
        let hash = java_string_hash_code(&description.display_name);
        let mut shard = hash % self.total_shards;
        if shard < 0 {
            shard += self.total_shards;
        }
        if shard < 0 || shard >= self.total_shards {
            return Err(format!("Computed shard {shard} out of bounds"));
        }
        Ok(shard == self.shard_index)
    }

    fn describe(&self) -> String {
        "hash-backed sharding filter".to_string()
    }
}

/// Deterministic round-robin sharding filter sorting descriptions by `displayName` in UTF-16
/// code-unit order (`RoundRobinShardingFilter.java`).
#[derive(Debug, Clone)]
pub struct RoundRobinShardingFilter {
    desc_to_index: HashMap<TestDescription, usize>,
    shard_index: usize,
    total_shards: usize,
}

impl RoundRobinShardingFilter {
    pub fn new(
        test_descriptions: &[TestDescription],
        shard_index: i32,
        total_shards: i32,
    ) -> Result<Self, String> {
        if shard_index < 0 || total_shards <= shard_index {
            return Err(format!(
                "Invalid shard bounds: shard_index={shard_index}, total_shards={total_shards}"
            ));
        }
        for d in test_descriptions {
            if !d.is_test() {
                return Err(format!(
                    "Non-test description passed to RoundRobinShardingFilter: {}",
                    d.display_name
                ));
            }
        }
        let mut sorted = test_descriptions.to_vec();
        sorted.sort_by(|a, b| utf16_cmp(&a.display_name, &b.display_name));

        let mut desc_to_index = HashMap::with_capacity(sorted.len());
        for (idx, desc) in sorted.into_iter().enumerate() {
            // Later duplicate overwrites earlier index (`Map.put`).
            desc_to_index.insert(desc, idx);
        }
        Ok(Self {
            desc_to_index,
            shard_index: shard_index as usize,
            total_shards: total_shards as usize,
        })
    }
}

impl TestFilter for RoundRobinShardingFilter {
    fn should_run(&self, description: &TestDescription) -> Result<bool, String> {
        if !description.is_test() {
            return Ok(true);
        }
        let Some(&idx) = self.desc_to_index.get(description) else {
            return Err(format!(
                "Test description '{}' was not registered with RoundRobinShardingFilter",
                description.display_name
            ));
        };
        Ok(idx % self.total_shards == self.shard_index)
    }

    fn describe(&self) -> String {
        "round robin sharding filter".to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShardingStrategy {
    #[default]
    RoundRobin,
    Hash,
}

impl ShardingStrategy {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.to_ascii_uppercase().as_str() {
            "ROUND_ROBIN" => Ok(Self::RoundRobin),
            "HASH" => Ok(Self::Hash),
            _ => Err(format!(
                "Could not create custom sharding strategy class {raw}"
            )),
        }
    }
}

/// Environment reader for Bazel test sharding (`ShardingEnvironment.java`).
#[derive(Debug, Clone, Default)]
pub struct ShardingEnvironment {
    total_shards_raw: Option<String>,
    shard_index_raw: Option<String>,
    shard_status_file: Option<PathBuf>,
    strategy_property: Option<String>,
}

impl ShardingEnvironment {
    pub fn from_map(env: &BTreeMap<String, String>, strategy_property: Option<String>) -> Self {
        Self {
            total_shards_raw: env.get("TEST_TOTAL_SHARDS").cloned(),
            shard_index_raw: env.get("TEST_SHARD_INDEX").cloned(),
            shard_status_file: env.get("TEST_SHARD_STATUS_FILE").map(PathBuf::from),
            strategy_property,
        }
    }

    /// `true` whenever `TEST_TOTAL_SHARDS` is set in the environment (even if empty).
    pub fn is_sharding_enabled(&self) -> bool {
        self.total_shards_raw.is_some()
    }

    pub fn get_total_shards(&self) -> Result<i32, String> {
        match &self.total_shards_raw {
            None => Ok(1),
            Some(raw) => parse_java_int_no_trim(raw),
        }
    }

    pub fn get_shard_index(&self) -> Result<i32, String> {
        match &self.shard_index_raw {
            None => Ok(0),
            Some(raw) => parse_java_int_no_trim(raw),
        }
    }

    /// Creates `TEST_SHARD_STATUS_FILE` if absent, or updates its modification time if it already
    /// exists (`ShardingEnvironment.touchShardFile`).
    pub fn touch_shard_file(&self) -> Result<(), String> {
        let Some(path) = &self.shard_status_file else {
            return Ok(());
        };
        if path.as_os_str().is_empty() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Error writing shard file {}: {e}", path.display()))?;
            }
        }
        let existed = path.exists();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|e| format!("Error writing shard file {}: {e}", path.display()))?;
        if existed {
            let _ = file.set_modified(SystemTime::now());
        }
        Ok(())
    }
}

fn parse_java_int_no_trim(s: &str) -> Result<i32, String> {
    if s.is_empty() {
        return Err(format!("For input string: \"{s}\""));
    }
    let digits = s
        .strip_prefix('+')
        .or_else(|| s.strip_prefix('-'))
        .unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("For input string: \"{s}\""));
    }
    s.parse::<i32>()
        .map_err(|_| format!("For input string: \"{s}\""))
}

/// Factory building the active sharding filter (`ShardingFilters.java`).
#[derive(Debug, Clone, Default)]
pub struct ShardingFilters {
    default_strategy: ShardingStrategy,
}

impl ShardingFilters {
    pub fn new(default_strategy: ShardingStrategy) -> Self {
        Self { default_strategy }
    }

    pub fn create_sharding_filter(
        &self,
        env: &ShardingEnvironment,
        test_descriptions: &[TestDescription],
    ) -> Result<Box<dyn TestFilter>, String> {
        let strategy = match &env.strategy_property {
            Some(s) => ShardingStrategy::parse(s)?,
            None => self.default_strategy,
        };
        let idx = env.get_shard_index()?;
        let total = env.get_total_shards()?;
        match strategy {
            ShardingStrategy::Hash => Ok(Box::new(HashBackedShardingFilter::new(idx, total)?)),
            ShardingStrategy::RoundRobin => Ok(Box::new(RoundRobinShardingFilter::new(
                test_descriptions,
                idx,
                total,
            )?)),
        }
    }
}

// ------------------------------------------------ Options & Config (`JUnit4Options`, `JUnit4Config`)

/// Parsed JUnit 4 command-line options (`JUnit4Options.java`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JUnit4Options {
    pub test_include_filter: Option<String>,
    pub test_exclude_filter: Option<String>,
    pub unparsed_args: Vec<String>,
}

impl JUnit4Options {
    pub fn parse<I, S>(args: I, env: &BTreeMap<String, String>) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let raw: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
        let mut include_on_argv: Option<String> = None;
        let mut exclude: Option<String> = None;
        let mut unparsed = Vec::new();

        let mut i = 0;
        while i < raw.len() {
            let arg = &raw[i];
            i += 1;
            if let Some((flag, val)) = split_eq_flag(arg) {
                match flag {
                    "--test_filter" => include_on_argv = Some(val.to_string()),
                    "--test_exclude_filter" => exclude = Some(val.to_string()),
                    _ => unparsed.push(arg.clone()),
                }
            } else if arg == "--test_filter" || arg == "--test_exclude_filter" {
                let Some(val) = raw.get(i) else {
                    return Err(format!("No filter expression specified after {arg}"));
                };
                i += 1;
                if arg == "--test_filter" {
                    include_on_argv = Some(val.clone());
                } else {
                    exclude = Some(val.clone());
                }
            } else {
                unparsed.push(arg.clone());
            }
        }

        // `TESTBRIDGE_TEST_ONLY` is consulted iff `--test_filter` was not present on argv, so an
        // explicit `--test_filter=` overrides `TESTBRIDGE_TEST_ONLY`.
        let test_include_filter = match include_on_argv {
            Some(v) => Some(v),
            None => env.get("TESTBRIDGE_TEST_ONLY").cloned(),
        };

        Ok(Self {
            test_include_filter,
            test_exclude_filter: exclude,
            unparsed_args: unparsed,
        })
    }
}

fn split_eq_flag(arg: &str) -> Option<(&str, &str)> {
    let eq = arg.find('=')?;
    if eq > 0 {
        Some((&arg[..eq], &arg[eq + 1..]))
    } else {
        None
    }
}

/// Validated JUnit 4 configuration (`JUnit4Config.java`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JUnit4Config {
    pub should_run_fail_fast: bool,
    pub xml_output_path: Option<PathBuf>,
    pub include_filter: Option<String>,
    pub exclude_filter: Option<String>,
}

impl JUnit4Config {
    pub fn new(
        include_filter: Option<String>,
        exclude_filter: Option<String>,
        explicit_xml_path: Option<PathBuf>,
        api_version_property: Option<&str>,
        env: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let trimmed = api_version_property.unwrap_or("1").trim();
        let ok = trimmed.parse::<i32>().ok() == Some(1);
        if !ok {
            return Err(format!(
                "Unsupported JUnit Runner API version com.google.testing.junit.runner.apiVersion={trimmed} (must be \"1\")"
            ));
        }
        // Fail-fast is enabled iff `TESTBRIDGE_TEST_RUNNER_FAIL_FAST` is the exact string `"1"`.
        let should_run_fail_fast = env
            .get("TESTBRIDGE_TEST_RUNNER_FAIL_FAST")
            .is_some_and(|v| v == "1");
        let xml_output_path = explicit_xml_path.or_else(|| {
            env.get("XML_OUTPUT_FILE")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        });
        Ok(Self {
            should_run_fail_fast,
            xml_output_path,
            include_filter,
            exclude_filter,
        })
    }
}

// ------------------------------------------------ Suite Model & AntXmlResultWriter

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestCaseState {
    Initial,
    Pending,
    Started,
    Completed,
    Skipped,
    Suppressed,
    Filtered,
    Cancelled,
    Interrupted,
}

impl TestCaseState {
    pub const fn was_run(self) -> bool {
        matches!(self, Self::Interrupted | Self::Completed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureRecord {
    pub exception_type: String,
    pub message: Option<String>,
    pub stack_trace: String,
}

impl FailureRecord {
    pub fn new(
        exception_type: impl Into<String>,
        message: Option<String>,
        stack_trace: impl Into<String>,
    ) -> Self {
        Self {
            exception_type: exception_type.into(),
            message,
            stack_trace: stack_trace.into(),
        }
    }
}

/// Live leaf test case state machine (`TestCaseNode.java`).
#[derive(Debug, Clone)]
pub struct TestCaseNode {
    pub description: TestDescription,
    pub parent_suite_name: String,
    pub state: TestCaseState,
    pub interval: Option<TestInterval>,
    pub failures: Vec<FailureRecord>,
    pub properties: BTreeMap<String, String>,
}

impl TestCaseNode {
    pub fn new(description: TestDescription, parent_suite_name: impl Into<String>) -> Self {
        Self {
            description,
            parent_suite_name: parent_suite_name.into(),
            state: TestCaseState::Initial,
            interval: None,
            failures: Vec::new(),
            properties: BTreeMap::new(),
        }
    }

    pub fn filter_out(&mut self) {
        if self.state == TestCaseState::Initial {
            self.state = TestCaseState::Filtered;
        }
    }

    pub fn pending(&mut self) {
        if self.state == TestCaseState::Initial {
            self.state = TestCaseState::Pending;
        }
    }

    pub fn started(&mut self, now: TestInstant) {
        if matches!(self.state, TestCaseState::Initial | TestCaseState::Pending) {
            self.state = TestCaseState::Started;
            self.interval = Some(TestInterval {
                start: now,
                end: now,
            });
        }
    }

    pub fn finished(&mut self, now: TestInstant) {
        if self.state == TestCaseState::Started {
            self.state = TestCaseState::Completed;
            self.update_end(now);
        }
    }

    pub fn test_skipped(&mut self, now: TestInstant) {
        if self.state == TestCaseState::Started {
            self.state = TestCaseState::Skipped;
            self.update_end(now);
        }
    }

    pub fn test_suppressed(&mut self, now: TestInstant) {
        if matches!(self.state, TestCaseState::Initial | TestCaseState::Pending) {
            self.state = TestCaseState::Suppressed;
            self.update_end(now);
        }
    }

    pub fn test_failure(&mut self, failure: FailureRecord, now: TestInstant) {
        self.failures.push(failure);
        if matches!(self.state, TestCaseState::Initial | TestCaseState::Pending) {
            self.state = TestCaseState::Completed;
        }
        self.update_end(now);
    }

    pub fn test_interrupted(&mut self, now: TestInstant) {
        match self.state {
            TestCaseState::Started => {
                self.state = TestCaseState::Interrupted;
                self.failures.push(FailureRecord::new(
                    "java.lang.Exception",
                    Some("Test interrupted".to_string()),
                    "java.lang.Exception: Test interrupted\n",
                ));
                self.update_end(now);
            }
            TestCaseState::Initial | TestCaseState::Pending => {
                self.state = TestCaseState::Cancelled;
                self.failures.push(FailureRecord::new(
                    "java.lang.Exception",
                    Some("Test cancelled".to_string()),
                    "java.lang.Exception: Test cancelled\n",
                ));
                self.update_end(now);
            }
            _ => {}
        }
    }

    fn update_end(&mut self, now: TestInstant) {
        self.interval = Some(match self.interval {
            Some(iv) => iv.with_end(now).unwrap_or(TestInterval {
                start: iv.start,
                end: iv.start,
            }),
            None => TestInterval {
                start: now,
                end: now,
            },
        });
    }

    pub fn build_result(&self) -> ModelTestResult {
        let (name, class_name) = match (
            self.description.method_name.as_deref(),
            self.description.class_name.as_deref(),
        ) {
            (Some(m), Some(c)) => (m.to_string(), c.to_string()),
            (_, Some(c)) => (c.to_string(), self.parent_suite_name.clone()),
            _ => (
                self.description.display_name.clone(),
                self.parent_suite_name.clone(),
            ),
        };
        let status = if self.state == TestCaseState::Initial {
            TestCaseState::Filtered
        } else {
            self.state
        };
        let num_tests = i32::from(status != TestCaseState::Filtered);
        let num_failures =
            i32::from(status != TestCaseState::Filtered && !self.failures.is_empty());
        ModelTestResult {
            name,
            class_name,
            status,
            interval: if matches!(self.state, TestCaseState::Initial | TestCaseState::Pending) {
                None
            } else {
                self.interval
            },
            failures: self.failures.clone(),
            properties: self.properties.clone(),
            children: Vec::new(),
            num_tests,
            num_failures,
        }
    }
}

#[derive(Debug, Clone)]
pub enum SuiteChild {
    Suite(TestSuiteNode),
    Case(TestCaseNode),
}

/// Live suite node (`TestSuiteNode.java`).
#[derive(Debug, Clone)]
pub struct TestSuiteNode {
    pub description: TestDescription,
    pub properties: BTreeMap<String, String>,
    pub children: Vec<SuiteChild>,
}

impl TestSuiteNode {
    pub fn new(description: TestDescription, properties: BTreeMap<String, String>) -> Self {
        Self {
            description,
            properties,
            children: Vec::new(),
        }
    }

    pub fn test_failure(&mut self, failure: &FailureRecord, now: TestInstant) {
        for child in &mut self.children {
            match child {
                SuiteChild::Suite(s) => s.test_failure(failure, now),
                SuiteChild::Case(c) => c.test_failure(failure.clone(), now),
            }
        }
    }

    pub fn test_interrupted(&mut self, now: TestInstant) {
        for child in &mut self.children {
            match child {
                SuiteChild::Suite(s) => s.test_interrupted(now),
                SuiteChild::Case(c) => c.test_interrupted(now),
            }
        }
    }

    pub fn build_result(&self) -> ModelTestResult {
        let mut child_results = Vec::with_capacity(self.children.len());
        let mut num_tests = 0;
        let mut num_failures = 0;
        let mut union_interval: Option<TestInterval> = None;

        for child in &self.children {
            let r = match child {
                SuiteChild::Suite(s) => s.build_result(),
                SuiteChild::Case(c) => c.build_result(),
            };
            num_tests += r.num_tests;
            num_failures += r.num_failures;
            if let Some(iv) = r.interval {
                union_interval = Some(match union_interval {
                    Some(prev) => prev.around(iv),
                    None => iv,
                });
            }
            child_results.push(r);
        }

        ModelTestResult {
            name: self.description.display_name.clone(),
            class_name: String::new(),
            status: TestCaseState::Skipped,
            interval: union_interval,
            failures: Vec::new(),
            properties: self.properties.clone(),
            children: child_results,
            num_tests,
            num_failures,
        }
    }
}

/// Materialized result node consumed by [`AntXmlResultWriter`] (`TestResult.java`).
#[derive(Debug, Clone)]
pub struct ModelTestResult {
    pub name: String,
    pub class_name: String,
    pub status: TestCaseState,
    pub interval: Option<TestInterval>,
    pub failures: Vec<FailureRecord>,
    pub properties: BTreeMap<String, String>,
    pub children: Vec<ModelTestResult>,
    pub num_tests: i32,
    pub num_failures: i32,
}

impl ModelTestResult {
    pub fn is_suite(&self) -> bool {
        !self.children.is_empty() || self.class_name.is_empty()
    }
}

/// JUnit Ant XML formatter (`AntXmlResultWriter.java`).
#[derive(Debug, Default, Clone, Copy)]
pub struct AntXmlResultWriter;

impl AntXmlResultWriter {
    pub fn write_test_suites(&self, writer: &mut XmlWriter, root: &ModelTestResult) {
        writer.start_document();
        writer.start_element("testsuites");
        let mut next_id = 0i32;
        for child in &root.children {
            self.write_test_suite(writer, child, &root.failures, &mut next_id);
        }
        writer.end_element();
    }

    pub fn format_xml(&self, root: &ModelTestResult) -> String {
        let mut writer = XmlWriter::new();
        self.write_test_suites(&mut writer, root);
        writer.finish()
    }

    fn write_test_suite(
        &self,
        writer: &mut XmlWriter,
        suite: &ModelTestResult,
        parent_failures: &[FailureRecord],
        next_id: &mut i32,
    ) {
        let mut combined_failures = parent_failures.to_vec();
        combined_failures.extend(suite.failures.iter().cloned());

        let suite_id = *next_id;
        *next_id += 1;

        writer.start_element("testsuite");
        writer.write_attribute("name", &suite.name);
        let timestamp = suite
            .interval
            .map(|iv| iv.start_instant_to_iso8601())
            .unwrap_or_default();
        writer.write_attribute("timestamp", &timestamp);
        writer.write_attribute("hostname", "localhost");
        writer.write_int_attribute("tests", suite.num_tests);
        writer.write_int_attribute("failures", suite.num_failures);
        writer.write_int_attribute("errors", 0);
        let time_str = suite
            .interval
            .map(|iv| format_java_seconds(iv.duration_millis()))
            .unwrap_or_else(|| "0.0".to_string());
        writer.write_attribute("time", &time_str);
        writer.write_attribute("package", "");
        writer.write_int_attribute("id", suite_id);

        writer.start_element("properties");
        for (k, v) in &suite.properties {
            writer.start_element("property");
            writer.write_attribute("name", k);
            writer.write_attribute("value", v);
            writer.end_element();
        }
        writer.end_element();

        let mut nested_suites: Vec<&ModelTestResult> = Vec::new();
        for child in &suite.children {
            if !child.children.is_empty() {
                nested_suites.push(child);
            } else if child.class_name.is_empty()
                && child.status == TestCaseState::Skipped
                && child.failures.is_empty()
            {
                // Empty child suite: skip leaf testcase emission.
            } else if child.status != TestCaseState::Filtered {
                self.write_test_case(writer, child, &combined_failures);
            }
        }

        writer.start_element("system-out");
        writer.end_element();
        writer.start_element("system-err");
        writer.end_element();
        writer.end_element();

        // Nested suites with children are emitted as later sibling `<testsuite>` elements.
        for nested in nested_suites {
            self.write_test_suite(writer, nested, &combined_failures, next_id);
        }
    }

    fn write_test_case(
        &self,
        writer: &mut XmlWriter,
        case: &ModelTestResult,
        inherited_failures: &[FailureRecord],
    ) {
        writer.start_element("testcase");
        writer.write_attribute("name", &case.name);
        writer.write_attribute("classname", &case.class_name);
        let time_str = case
            .interval
            .map(|iv| format_java_seconds(iv.duration_millis()))
            .unwrap_or_else(|| "0.0".to_string());
        writer.write_attribute("time", &time_str);

        for f in inherited_failures.iter().chain(case.failures.iter()) {
            writer.start_element("failure");
            writer.write_attribute("message", f.message.as_deref().unwrap_or(""));
            writer.write_attribute("type", &f.exception_type);
            writer.write_characters(&f.stack_trace);
            writer.end_element();
        }

        if matches!(
            case.status,
            TestCaseState::Skipped | TestCaseState::Suppressed
        ) {
            writer.start_element("skipped");
            writer.end_element();
        }

        writer.end_element();
    }
}

/// Live in-memory suite model (`TestSuiteModel.java`).
pub struct TestSuiteModel {
    pub root: TestSuiteNode,
    pub sharding_filter: Option<Box<dyn TestFilter>>,
    wrote_xml: AtomicBool,
}

impl TestSuiteModel {
    /// Builds a `TestSuiteModel` from `root_description`, touching the shard file and installing
    /// the sharding filter when sharding is enabled (`TestSuiteModel.Builder.build`).
    pub fn build(
        suite_name: &str,
        root_description: &TestDescription,
        properties: BTreeMap<String, String>,
        sharding_env: &ShardingEnvironment,
        sharding_filters: &ShardingFilters,
    ) -> Result<Self, String> {
        let sharding_filter = if sharding_env.is_sharding_enabled() {
            let mut leaves = Vec::new();
            root_description.collect_leaf_tests(&mut leaves);
            sharding_env.touch_shard_file()?;
            let raw_filter = sharding_filters.create_sharding_filter(sharding_env, &leaves)?;
            Some(Box::new(SuiteTrimmingFilter::new(raw_filter)) as Box<dyn TestFilter>)
        } else {
            None
        };

        let top_desc = TestDescription::suite(suite_name, root_description.children.clone());
        let mut root = TestSuiteNode::new(top_desc, properties);
        if root_description.is_suite() {
            for child in &root_description.children {
                if let Some(node) =
                    Self::build_child(child, suite_name, sharding_filter.as_deref())?
                {
                    root.children.push(node);
                }
            }
        }

        Ok(Self {
            root,
            sharding_filter,
            wrote_xml: AtomicBool::new(false),
        })
    }

    fn build_child(
        desc: &TestDescription,
        parent_suite_name: &str,
        shard_filter: Option<&dyn TestFilter>,
    ) -> Result<Option<SuiteChild>, String> {
        if let Some(f) = shard_filter {
            if !f.should_run(desc)? {
                return Ok(None);
            }
        }
        if desc.is_test() {
            Ok(Some(SuiteChild::Case(TestCaseNode::new(
                desc.clone(),
                parent_suite_name,
            ))))
        } else {
            let mut suite = TestSuiteNode::new(desc.clone(), BTreeMap::new());
            for ch in &desc.children {
                if let Some(built) = Self::build_child(ch, &desc.display_name, shard_filter)? {
                    suite.children.push(built);
                }
            }
            if shard_filter.is_some() && suite.children.is_empty() {
                Ok(None)
            } else {
                Ok(Some(SuiteChild::Suite(suite)))
            }
        }
    }

    pub fn test_run_started(&mut self) {
        fn mark_pending(suite: &mut TestSuiteNode) {
            for child in &mut suite.children {
                match child {
                    SuiteChild::Suite(s) => mark_pending(s),
                    SuiteChild::Case(c) => {
                        if c.state != TestCaseState::Filtered {
                            c.pending();
                        }
                    }
                }
            }
        }
        mark_pending(&mut self.root);
    }

    pub fn apply_filter(&mut self, filter: &dyn TestFilter) -> Result<usize, String> {
        fn apply_rec(suite: &mut TestSuiteNode, filter: &dyn TestFilter) -> Result<usize, String> {
            let mut kept = 0;
            for child in &mut suite.children {
                match child {
                    SuiteChild::Suite(s) => kept += apply_rec(s, filter)?,
                    SuiteChild::Case(c) => {
                        if filter.should_run(&c.description)? {
                            kept += 1;
                        } else {
                            c.filter_out();
                        }
                    }
                }
            }
            Ok(kept)
        }
        apply_rec(&mut self.root, filter)
    }

    pub fn find_case_mut(&mut self, desc: &TestDescription) -> Option<&mut TestCaseNode> {
        fn find_rec<'a>(
            suite: &'a mut TestSuiteNode,
            desc: &TestDescription,
        ) -> Option<&'a mut TestCaseNode> {
            for child in &mut suite.children {
                match child {
                    SuiteChild::Suite(s) => {
                        if let Some(found) = find_rec(s, desc) {
                            return Some(found);
                        }
                    }
                    SuiteChild::Case(c) if &c.description == desc => return Some(c),
                    SuiteChild::Case(_) => {}
                }
            }
            None
        }
        find_rec(&mut self.root, desc)
    }

    /// Records a failure on `desc`; if `desc` is an undeclared dynamic test leaf, inserts it under
    /// the matching class suite (`TestSuiteModel.testFailure`).
    pub fn record_failure(
        &mut self,
        desc: &TestDescription,
        failure: FailureRecord,
        now: TestInstant,
    ) {
        if let Some(case) = self.find_case_mut(desc) {
            case.test_failure(failure, now);
            return;
        }
        fn find_suite_mut<'a>(
            suite: &'a mut TestSuiteNode,
            name: &str,
        ) -> Option<&'a mut TestSuiteNode> {
            if suite.description.display_name == name {
                return Some(suite);
            }
            for child in &mut suite.children {
                if let SuiteChild::Suite(s) = child {
                    if let Some(found) = find_suite_mut(s, name) {
                        return Some(found);
                    }
                }
            }
            None
        }

        if desc.is_test() {
            if let Some(cn) = desc.class_name.as_deref() {
                if let Some(parent_suite) = find_suite_mut(&mut self.root, cn) {
                    let mut dynamic_case = TestCaseNode::new(desc.clone(), cn);
                    dynamic_case.test_failure(failure, now);
                    parent_suite.children.push(SuiteChild::Case(dynamic_case));
                    return;
                }
            }
        }
        self.root.test_failure(&failure, now);
    }

    pub fn test_run_interrupted(&mut self, now: TestInstant) {
        self.root.test_interrupted(now);
    }

    /// Writes the XML report at most once (`AtomicBoolean.compareAndSet(false, true)`).
    pub fn write_as_xml_once(&self) -> Option<String> {
        if self
            .wrote_xml
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let res = self.root.build_result();
            Some(AntXmlResultWriter.format_xml(&res))
        } else {
            None
        }
    }
}

// ------------------------------------------------ PrematureExitFile & BazelTestRunner

/// Manages `TEST_PREMATURE_EXIT_FILE` (`BazelTestRunner.java`): creates the marker before tests
/// start and deletes it upon normal completion.
#[derive(Debug, Clone)]
pub struct PrematureExitFile {
    path: Option<PathBuf>,
}

impl PrematureExitFile {
    pub fn create_from_env(env: &BTreeMap<String, String>) -> io::Result<Self> {
        let path = env
            .get("TEST_PREMATURE_EXIT_FILE")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        if let Some(p) = &path {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::write(p, b"")?;
        }
        Ok(Self { path })
    }

    pub fn remove(&self) -> io::Result<()> {
        if let Some(p) = &self.path {
            if p.exists() {
                std::fs::remove_file(p)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum CaseExecutionOutcome {
    Passed {
        duration: Duration,
    },
    Skipped {
        duration: Duration,
    },
    Suppressed,
    Failed {
        duration: Duration,
        failure: FailureRecord,
    },
    SystemExitCalled,
    OutOfMemory,
}

/// In-process execution driver mirroring `BazelTestRunner.main` + `JUnit4Runner.run`.
pub struct BazelTestRunner;

impl BazelTestRunner {
    #[allow(clippy::too_many_arguments)]
    pub fn run_suite<F>(
        suite_class_name: Option<&str>,
        suite_exists: bool,
        root_description: &TestDescription,
        raw_args: &[String],
        env: &BTreeMap<String, String>,
        sharding_strategy_prop: Option<String>,
        api_version_prop: Option<&str>,
        mut execute_case: F,
    ) -> Result<i32, String>
    where
        F: FnMut(&TestDescription) -> CaseExecutionOutcome,
    {
        let Some(suite_name) = suite_class_name.filter(|s| !s.is_empty()) else {
            return Ok(EXIT_CODE_BAD_SUITE_ARGS);
        };
        if !suite_exists {
            return Ok(if raw_args.is_empty() {
                EXIT_CODE_BAD_SUITE_ARGS
            } else {
                EXIT_CODE_TEST_FAILURE
            });
        }

        let premature = PrematureExitFile::create_from_env(env).map_err(|e| e.to_string())?;
        let opts = JUnit4Options::parse(raw_args, env)?;
        let cfg = JUnit4Config::new(
            opts.test_include_filter.clone(),
            opts.test_exclude_filter.clone(),
            None,
            api_version_prop,
            env,
        )?;
        let sharding_env = ShardingEnvironment::from_map(env, sharding_strategy_prop);
        let sharding_filters = ShardingFilters::default();

        let mut model = TestSuiteModel::build(
            suite_name,
            root_description,
            BTreeMap::new(),
            &sharding_env,
            &sharding_filters,
        )?;

        let mut user_filter: Box<dyn TestFilter> = Box::new(AllFilter);
        let mut has_user_filter = false;
        if let Some(inc) = &cfg.include_filter {
            if !inc.is_empty() {
                user_filter =
                    AndFilter::create(user_filter, Box::new(RegExTestCaseFilter::include(inc)?));
                has_user_filter = true;
            }
        }
        if let Some(exc) = &cfg.exclude_filter {
            if !exc.is_empty() {
                user_filter =
                    AndFilter::create(user_filter, Box::new(RegExTestCaseFilter::exclude(exc)?));
                has_user_filter = true;
            }
        }

        let runnable_count = if has_user_filter {
            model.apply_filter(&*user_filter)?
        } else {
            let mut leaves = Vec::new();
            root_description.collect_leaf_tests(&mut leaves);
            leaves
                .iter()
                .filter(|d| model.find_case_mut(d).is_some())
                .count()
        };

        // When a user filter matches 0 tests:
        // - If sharding is enabled, allow no-op pass so shards with 0 matching tests do not fail!
        // - Otherwise, synthesize JUnit's `"No tests found matching ..."` failure.
        if has_user_filter && runnable_count == 0 && !sharding_env.is_sharding_enabled() {
            let now = TestInstant::now();
            let filter_err = TestDescription::leaf(
                "org.junit.runner.manipulation.Filter",
                "initializationError",
            );
            let msg = format!(
                "No tests found matching {} from {}",
                user_filter.describe(),
                suite_name
            );
            let mut err_suite = TestSuiteNode::new(
                TestDescription::suite(
                    "org.junit.runner.manipulation.Filter",
                    vec![filter_err.clone()],
                ),
                BTreeMap::new(),
            );
            let mut err_case =
                TestCaseNode::new(filter_err, "org.junit.runner.manipulation.Filter");
            err_case.test_failure(
                FailureRecord::new(
                    "java.lang.Exception",
                    Some(msg.clone()),
                    format!("java.lang.Exception: {msg}\n"),
                ),
                now,
            );
            err_suite.children.push(SuiteChild::Case(err_case));
            model.root.children.push(SuiteChild::Suite(err_suite));
            Self::flush_xml(&model, cfg.xml_output_path.as_deref());
            let _ = premature.remove();
            return Ok(EXIT_CODE_TEST_FAILURE);
        }

        model.test_run_started();

        let mut leaves = Vec::new();
        root_description.collect_leaf_tests(&mut leaves);
        let mut any_failed = false;
        let mut clock_nanos = 0u128;

        for leaf in &leaves {
            let should_exec = match model.find_case_mut(leaf) {
                Some(c) => c.state != TestCaseState::Filtered,
                None => false,
            };
            if !should_exec {
                continue;
            }
            let start = TestInstant {
                wall_millis: TestInstant::now().wall_millis,
                monotonic_nanos: clock_nanos,
            };
            match execute_case(leaf) {
                CaseExecutionOutcome::Suppressed => {
                    if let Some(c) = model.find_case_mut(leaf) {
                        c.test_suppressed(start);
                    }
                }
                CaseExecutionOutcome::Passed { duration } => {
                    clock_nanos += duration.as_nanos();
                    let end = TestInstant {
                        wall_millis: start.wall_millis + duration.as_millis() as i64,
                        monotonic_nanos: clock_nanos,
                    };
                    if let Some(c) = model.find_case_mut(leaf) {
                        c.started(start);
                        c.finished(end);
                    }
                }
                CaseExecutionOutcome::Skipped { duration } => {
                    clock_nanos += duration.as_nanos();
                    let end = TestInstant {
                        wall_millis: start.wall_millis + duration.as_millis() as i64,
                        monotonic_nanos: clock_nanos,
                    };
                    if let Some(c) = model.find_case_mut(leaf) {
                        c.started(start);
                        c.test_skipped(end);
                    }
                }
                CaseExecutionOutcome::Failed { duration, failure } => {
                    any_failed = true;
                    clock_nanos += duration.as_nanos();
                    let end = TestInstant {
                        wall_millis: start.wall_millis + duration.as_millis() as i64,
                        monotonic_nanos: clock_nanos,
                    };
                    if let Some(c) = model.find_case_mut(leaf) {
                        c.started(start);
                        c.test_failure(failure, end);
                        c.finished(end);
                    }
                    if cfg.should_run_fail_fast {
                        break;
                    }
                }
                CaseExecutionOutcome::SystemExitCalled => {
                    // Intentionally leave premature exit file in place and exit 121.
                    return Ok(EXIT_CODE_SYSTEM_EXIT_DETECTED);
                }
                CaseExecutionOutcome::OutOfMemory => {
                    return Ok(EXIT_CODE_OUT_OF_MEMORY);
                }
            }
        }

        Self::flush_xml(&model, cfg.xml_output_path.as_deref());
        let _ = premature.remove();
        Ok(if any_failed {
            EXIT_CODE_TEST_FAILURE
        } else {
            EXIT_CODE_SUCCESS
        })
    }

    fn flush_xml(model: &TestSuiteModel, xml_path: Option<&Path>) {
        if let Some(path) = xml_path {
            if let Some(xml) = model.write_as_xml_once() {
                // `ProvideXmlStreamFactory`: open for truncate; ignore FileNotFoundException when parent missing.
                let _ = std::fs::write(path, xml);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_status::{parse_test_xml, CaseStatus};

    #[test]
    fn junit4_options_and_config_edge_cases() {
        let mut env = BTreeMap::new();
        env.insert("TESTBRIDGE_TEST_ONLY".into(), "from_env".into());
        env.insert("TESTBRIDGE_TEST_RUNNER_FAIL_FAST".into(), "true".into());

        // Without `--test_filter` on argv, `TESTBRIDGE_TEST_ONLY` is used.
        let o1 = JUnit4Options::parse(["--unknown_flag"], &env).unwrap();
        assert_eq!(o1.test_include_filter.as_deref(), Some("from_env"));
        assert_eq!(o1.unparsed_args, vec!["--unknown_flag"]);

        // Explicit empty `--test_filter=` on argv overrides `TESTBRIDGE_TEST_ONLY`!
        let o2 =
            JUnit4Options::parse(["--test_filter=", "--test_exclude_filter=bad"], &env).unwrap();
        assert_eq!(o2.test_include_filter.as_deref(), Some(""));
        assert_eq!(o2.test_exclude_filter.as_deref(), Some("bad"));

        assert!(JUnit4Options::parse(["--test_filter"], &env).is_err());

        // `TESTBRIDGE_TEST_RUNNER_FAIL_FAST` requires exact `"1"`, not `"true"`.
        let c1 = JUnit4Config::new(None, None, None, Some(" 1 "), &env).unwrap();
        assert!(!c1.should_run_fail_fast);
        env.insert("TESTBRIDGE_TEST_RUNNER_FAIL_FAST".into(), "1".into());
        let c2 = JUnit4Config::new(None, None, None, None, &env).unwrap();
        assert!(c2.should_run_fail_fast);
        assert!(JUnit4Config::new(None, None, None, Some("2"), &env).is_err());
    }

    #[test]
    fn regex_suite_trimming_hash_and_round_robin_sharding_filters() {
        let t_a = TestDescription::leaf("com.pkg.AlphaTest", "testOne");
        let t_b = TestDescription::leaf("com.pkg.AlphaTest", "testTwo");
        let t_c = TestDescription::leaf("com.pkg.BetaTest", "testThree");
        let s_alpha = TestDescription::suite("com.pkg.AlphaTest", vec![t_a.clone(), t_b.clone()]);
        let s_beta = TestDescription::suite("com.pkg.BetaTest", vec![t_c.clone()]);
        let root = TestDescription::suite("AllTests", vec![s_alpha.clone(), s_beta.clone()]);

        let inc = RegExTestCaseFilter::include("AlphaTest#testTwo").unwrap();
        assert_eq!(inc.describe(), "RegEx[AlphaTest#testTwo]");
        let trim = SuiteTrimmingFilter::new(Box::new(inc));
        assert!(trim.should_run(&root).unwrap());
        assert!(trim.should_run(&s_alpha).unwrap());
        assert!(!trim.should_run(&s_beta).unwrap(), "BetaTest pruned");
        assert!(!trim.should_run(&t_a).unwrap());
        assert!(trim.should_run(&t_b).unwrap());

        // Round-robin sorts by UTF-16 displayName regardless of input order.
        let leaves_shuffled = vec![t_c.clone(), t_b.clone(), t_a.clone()];
        let rr0 = RoundRobinShardingFilter::new(&leaves_shuffled, 0, 2).unwrap();
        let rr1 = RoundRobinShardingFilter::new(&leaves_shuffled, 1, 2).unwrap();
        // Sorted display names:
        // 0: "testOne(com.pkg.AlphaTest)" -> shard 0
        // 1: "testThree(com.pkg.BetaTest)" -> shard 1
        // 2: "testTwo(com.pkg.AlphaTest)" -> shard 0
        assert!(rr0.should_run(&t_a).unwrap());
        assert!(rr1.should_run(&t_c).unwrap());
        assert!(rr0.should_run(&t_b).unwrap());

        // Hash-backed partitions every test cleanly across shards, including negative Java hashCode!
        let neg_leaf = TestDescription::leaf("polygenelubricants", "testNeg");
        assert!(java_string_hash_code("polygenelubricants") < 0);
        let h0 = HashBackedShardingFilter::new(0, 3).unwrap();
        let h1 = HashBackedShardingFilter::new(1, 3).unwrap();
        let h2 = HashBackedShardingFilter::new(2, 3).unwrap();
        let hits = [
            h0.should_run(&neg_leaf).unwrap(),
            h1.should_run(&neg_leaf).unwrap(),
            h2.should_run(&neg_leaf).unwrap(),
        ];
        assert_eq!(hits.iter().filter(|&&b| b).count(), 1);
    }

    #[test]
    fn ant_xml_writer_escapes_flattens_nested_suites_and_roundtrips_with_parse_test_xml() {
        let leaf1 = TestDescription::leaf("com.example.InnerTest", "passes");
        let leaf2 = TestDescription::leaf("com.example.InnerTest", "fails");
        let leaf3 = TestDescription::leaf("com.example.InnerTest", "skippedCase");

        let mut c1 = TestCaseNode::new(leaf1, "com.example.InnerTest");
        let t0 = TestInstant {
            wall_millis: 1_471_709_734_000, // 2016-08-20T16:15:34.000Z
            monotonic_nanos: 1_000_000_000,
        };
        let t1 = TestInstant {
            wall_millis: 1_471_709_734_250,
            monotonic_nanos: 1_250_000_000,
        };
        c1.started(t0);
        c1.finished(t1);

        let mut c2 = TestCaseNode::new(leaf2, "com.example.InnerTest");
        c2.started(t1);
        c2.test_failure(
            FailureRecord::new(
                "java.lang.AssertionError",
                Some("bad 'quote'\tand\nnewline\x01".into()),
                "at com.example.InnerTest.fails(InnerTest.java:20)\x00\n",
            ),
            TestInstant {
                wall_millis: 1_471_709_734_350,
                monotonic_nanos: 1_350_000_000,
            },
        );
        c2.finished(TestInstant {
            wall_millis: 1_471_709_734_350,
            monotonic_nanos: 1_350_000_000,
        });

        let mut c3 = TestCaseNode::new(leaf3, "com.example.InnerTest");
        c3.started(t1);
        c3.test_skipped(t1);

        let mut inner = TestSuiteNode::new(
            TestDescription::suite("com.example.InnerTest", vec![]),
            BTreeMap::from([("key".into(), "val".into())]),
        );
        inner.children = vec![
            SuiteChild::Case(c1),
            SuiteChild::Case(c2),
            SuiteChild::Case(c3),
        ];

        let mut outer = TestSuiteNode::new(
            TestDescription::suite("com.example.OuterSuite", vec![]),
            BTreeMap::new(),
        );
        outer.children.push(SuiteChild::Suite(inner));

        let mut root = TestSuiteNode::new(TestDescription::suite("root", vec![]), BTreeMap::new());
        root.children.push(SuiteChild::Suite(outer));

        let xml = AntXmlResultWriter.format_xml(&root.build_result());
        assert!(xml.starts_with("<?xml version='1.0' encoding='UTF-8'?>\n<testsuites>"));
        assert!(xml.contains("timestamp='2016-08-20T16:15:34.000Z'"));
        assert!(xml.contains("id='0'"));
        assert!(xml.contains("id='1'"));
        assert!(xml.contains("&#x9;"));
        assert!(xml.contains("&#xA;"));
        assert!(xml.contains("\u{FFFD}"));

        let parsed = parse_test_xml(&xml).unwrap().unwrap();
        // Two flattened sibling `<testsuite>` elements (`OuterSuite` id=0 and `InnerTest` id=1).
        assert_eq!(parsed.child.len(), 2);
        assert_eq!(
            parsed.child[1].name.as_deref(),
            Some("com.example.InnerTest")
        );
        assert_eq!(parsed.child[1].status, Some(CaseStatus::Failed));
        assert_eq!(parsed.child[1].child.len(), 3);
        assert_eq!(parsed.child[1].child[0].run_duration_millis, Some(250));
        assert_eq!(parsed.child[1].child[2].status, Some(CaseStatus::Skipped));
    }

    #[test]
    fn bazel_test_runner_sharded_empty_filter_noop_fail_fast_and_exit_codes() {
        let d = std::env::temp_dir().join(format!("zel-junit-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();

        let t1 = TestDescription::leaf("pkg.SampleTest", "testA");
        let t2 = TestDescription::leaf("pkg.SampleTest", "testB");
        let cls = TestDescription::suite("pkg.SampleTest", vec![t1.clone(), t2.clone()]);
        let root = TestDescription::suite("pkg.SampleTest", vec![cls]);

        // 1. Missing suite -> exit 2.
        assert_eq!(
            BazelTestRunner::run_suite(
                None,
                false,
                &root,
                &[],
                &BTreeMap::new(),
                None,
                None,
                |_| {
                    CaseExecutionOutcome::Passed {
                        duration: Duration::from_millis(1),
                    }
                }
            )
            .unwrap(),
            EXIT_CODE_BAD_SUITE_ARGS
        );

        // 2. Unsharded filter matching 0 tests -> exit 1 (`No tests found matching ...`).
        let xml_unsharded = d.join("unsharded.xml");
        let env_unsharded = BTreeMap::from([(
            "XML_OUTPUT_FILE".to_string(),
            xml_unsharded.to_string_lossy().into_owned(),
        )]);
        let rc_miss = BazelTestRunner::run_suite(
            Some("pkg.SampleTest"),
            true,
            &root,
            &["--test_filter=NonExistent".to_string()],
            &env_unsharded,
            None,
            None,
            |_| CaseExecutionOutcome::Passed {
                duration: Duration::from_millis(1),
            },
        )
        .unwrap();
        assert_eq!(rc_miss, EXIT_CODE_TEST_FAILURE);
        let xml_body = std::fs::read_to_string(&xml_unsharded).unwrap();
        assert!(xml_body.contains("No tests found matching"), "{xml_body}");

        // 3. Sharded + user filter leaving shard 1 with 0 tests -> touches shard file & exits 0!
        let shard_file = d.join("test.shard");
        let xml_sharded = d.join("sharded.xml");
        let env_sharded = BTreeMap::from([
            ("TEST_TOTAL_SHARDS".to_string(), "2".to_string()),
            ("TEST_SHARD_INDEX".to_string(), "1".to_string()),
            (
                "TEST_SHARD_STATUS_FILE".to_string(),
                shard_file.to_string_lossy().into_owned(),
            ),
            (
                "XML_OUTPUT_FILE".to_string(),
                xml_sharded.to_string_lossy().into_owned(),
            ),
            (
                "TEST_PREMATURE_EXIT_FILE".to_string(),
                d.join("premature").to_string_lossy().into_owned(),
            ),
        ]);
        // `testA` lands on shard 0 in round-robin, so shard 1 has 0 matching tests!
        let rc_shard_noop = BazelTestRunner::run_suite(
            Some("pkg.SampleTest"),
            true,
            &root,
            &["--test_filter=SampleTest#testA".to_string()],
            &env_sharded,
            None,
            None,
            |_| CaseExecutionOutcome::Passed {
                duration: Duration::from_millis(1),
            },
        )
        .unwrap();
        assert_eq!(rc_shard_noop, EXIT_CODE_SUCCESS);
        assert!(shard_file.exists(), "shard status file must be touched");
        assert!(!d.join("premature").exists(), "premature exit file removed");

        // 4. Fail-fast stops after the first failing test!
        let mut ran = 0usize;
        let env_ff = BTreeMap::from([("TESTBRIDGE_TEST_RUNNER_FAIL_FAST".into(), "1".into())]);
        let rc_ff = BazelTestRunner::run_suite(
            Some("pkg.SampleTest"),
            true,
            &root,
            &[],
            &env_ff,
            None,
            None,
            |_| {
                ran += 1;
                CaseExecutionOutcome::Failed {
                    duration: Duration::from_millis(5),
                    failure: FailureRecord::new("AssertionError", Some("boom".into()), "trace"),
                }
            },
        )
        .unwrap();
        assert_eq!(rc_ff, EXIT_CODE_TEST_FAILURE);
        assert_eq!(ran, 1, "fail-fast must stop after first failure");

        let _ = std::fs::remove_dir_all(&d);
    }
}
