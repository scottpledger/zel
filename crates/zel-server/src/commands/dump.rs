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

//! `dump` (`DumpCommand`): `--packages`, `--action_cache`, `--rule_classes`, `--rules`,
//! `--skyframe=summary|count|keys|value|deps|rdeps|function_graph`, `--skykey_filter`,
//! `--skylark_memory` and `--memory`.

use crate::{writeln_bytes, BlazeRuntime};
use std::collections::BTreeMap;
use zel_options::ParsedCommandOptions;
use zel_skyframe::{PackageLoadValue, SkyFunctionName, SkyKey};
use zel_util::ExitCode;

const WARNING_MESSAGE: &str = "This information is intended for consumption by developers only, and may change at any time. Script against it at your own risk!";

const SKY_FUNCTIONS: &[SkyFunctionName] = &[
    SkyFunctionName::FILE_STATE,
    SkyFunctionName::FILE,
    SkyFunctionName::DIRECTORY_LISTING,
    SkyFunctionName::PACKAGE_LOOKUP,
    SkyFunctionName::BZL_LOAD,
    SkyFunctionName::PACKAGE,
    SkyFunctionName::PACKAGE_LOAD,
    SkyFunctionName::CONFIGURED_TARGET,
    SkyFunctionName::ACTION_EXECUTION,
    SkyFunctionName::PRECOMPUTED,
];

/// `RegexFilterConverter`: the `--skykey_filter` regex. Only the constructs commonly used in
/// filters are supported (`.*`, literal text, `^`, `$`, `.`, `|`, `[...]` classes via a small
/// backtracking matcher); anything else is rejected with Bazel's message.
fn compile_filter(src: &str) -> Result<Regex, String> {
    Regex::new(src).map_err(|e| format!("Illegal regular expression syntax in --skykey_filter: {e}"))
}

/// Tiny regex engine (`java.util.regex.Pattern.find`-style semantics) — literals, `.`, `*`, `+`,
/// `?`, `^`, `$`, `|`, groups and character classes.
pub struct Regex {
    alts: Vec<Vec<Item>>,
}

#[derive(Clone, Debug)]
enum Atom {
    Any,
    Lit(char),
    Class(Vec<(char, char)>, bool),
    Group(Vec<Vec<Item>>),
    Start,
    End,
}

#[derive(Clone, Debug)]
struct Item {
    atom: Atom,
    min: usize,
    max: usize,
}

impl Regex {
    pub fn new(src: &str) -> Result<Regex, String> {
        let chars: Vec<char> = src.chars().collect();
        let mut pos = 0;
        let alts = Self::parse_alts(&chars, &mut pos)?;
        if pos != chars.len() {
            return Err(format!("Unmatched closing ')' near index {pos}"));
        }
        Ok(Regex { alts })
    }

    fn parse_alts(c: &[char], pos: &mut usize) -> Result<Vec<Vec<Item>>, String> {
        let mut alts = vec![Vec::new()];
        while *pos < c.len() {
            let ch = c[*pos];
            let atom = match ch {
                ')' => break,
                '|' => {
                    *pos += 1;
                    alts.push(Vec::new());
                    continue;
                }
                '(' => {
                    *pos += 1;
                    if c.get(*pos) == Some(&'?') && c.get(*pos + 1) == Some(&':') {
                        *pos += 2;
                    }
                    let inner = Self::parse_alts(c, pos)?;
                    if c.get(*pos) != Some(&')') {
                        return Err("Unclosed group".into());
                    }
                    *pos += 1;
                    Atom::Group(inner)
                }
                '[' => {
                    *pos += 1;
                    let neg = c.get(*pos) == Some(&'^');
                    if neg {
                        *pos += 1;
                    }
                    let mut ranges = Vec::new();
                    while *pos < c.len() && c[*pos] != ']' {
                        let mut lo = c[*pos];
                        if lo == '\\' {
                            *pos += 1;
                            lo = *c.get(*pos).ok_or("Unclosed character class")?;
                        }
                        *pos += 1;
                        if c.get(*pos) == Some(&'-') && c.get(*pos + 1).map(|x| *x != ']').unwrap_or(false) {
                            let hi = c[*pos + 1];
                            *pos += 2;
                            ranges.push((lo, hi));
                        } else {
                            ranges.push((lo, lo));
                        }
                    }
                    if c.get(*pos) != Some(&']') {
                        return Err("Unclosed character class".into());
                    }
                    *pos += 1;
                    Atom::Class(ranges, neg)
                }
                '.' => {
                    *pos += 1;
                    Atom::Any
                }
                '^' => {
                    *pos += 1;
                    Atom::Start
                }
                '$' => {
                    *pos += 1;
                    Atom::End
                }
                '\\' => {
                    *pos += 1;
                    let e = *c.get(*pos).ok_or("Unexpected internal error")?;
                    *pos += 1;
                    match e {
                        'd' => Atom::Class(vec![('0', '9')], false),
                        'w' => Atom::Class(vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')], false),
                        's' => Atom::Class(vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')], false),
                        'n' => Atom::Lit('\n'),
                        't' => Atom::Lit('\t'),
                        other => Atom::Lit(other),
                    }
                }
                '*' | '+' | '?' => return Err(format!("Dangling meta character '{ch}' near index {pos}", pos = *pos)),
                other => {
                    *pos += 1;
                    Atom::Lit(other)
                }
            };
            let (min, max) = match c.get(*pos) {
                Some('*') => (0, usize::MAX),
                Some('+') => (1, usize::MAX),
                Some('?') => (0, 1),
                _ => (1, 1),
            };
            if (min, max) != (1, 1) {
                *pos += 1;
            }
            alts.last_mut().unwrap().push(Item { atom, min, max });
        }
        Ok(alts)
    }

    pub fn find(&self, text: &str) -> bool {
        let t: Vec<char> = text.chars().collect();
        (0..=t.len()).any(|s| Self::match_alts(&self.alts, &t, s, &mut |_| true))
    }

    fn match_alts(alts: &[Vec<Item>], t: &[char], pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        alts.iter().any(|items| Self::match_items(items, t, pos, k))
    }

    fn match_items(items: &[Item], t: &[char], pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        let Some((first, rest)) = items.split_first() else {
            return k(pos);
        };
        Self::match_rep(first, 0, t, pos, &mut |p| Self::match_items(rest, t, p, k))
    }

    fn match_rep(it: &Item, count: usize, t: &[char], pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        // Greedy: try one more repetition first.
        if count < it.max {
            let matched = Self::match_atom(&it.atom, t, pos, &mut |p| {
                if p == pos && count >= it.min {
                    return false; // no progress
                }
                Self::match_rep(it, count + 1, t, p, k)
            });
            if matched {
                return true;
            }
        }
        count >= it.min && k(pos)
    }

    fn match_atom(a: &Atom, t: &[char], pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        match a {
            Atom::Any => pos < t.len() && t[pos] != '\n' && k(pos + 1),
            Atom::Lit(c) => pos < t.len() && t[pos] == *c && k(pos + 1),
            Atom::Class(r, neg) => {
                pos < t.len() && (r.iter().any(|(lo, hi)| t[pos] >= *lo && t[pos] <= *hi) != *neg) && k(pos + 1)
            }
            Atom::Start => pos == 0 && k(pos),
            Atom::End => pos == t.len() && k(pos),
            Atom::Group(alts) => Self::match_alts(alts, t, pos, k),
        }
    }
}

impl BlazeRuntime {
    fn dump_packages(&self, out: &mut Vec<u8>) {
        use std::io::Write;
        let snap = self.skyframe.evaluator.snapshot_values(&[SkyFunctionName::PACKAGE_LOAD]);
        let _ = writeln!(out, "{} packages", snap.len());
        for (key, val) in snap {
            match val.as_any().downcast_ref::<PackageLoadValue>().map(|v| &v.0) {
                Some(Ok(pkg)) => {
                    let _ = writeln!(out, "  Package {} ({})", pkg.id, pkg.build_file);
                    let _ = writeln!(out, "    Rules");
                    let mut files = Vec::new();
                    for t in pkg.targets.values() {
                        match t {
                            zel_packages::Target::Rule(_) => {
                                let _ = writeln!(out, "      {} {}", t.target_kind(), t.label());
                            }
                            zel_packages::Target::OutputFile { label, generating_rule } => {
                                files.push(format!(
                                    "      {} {label} (generated by {generating_rule})",
                                    t.target_kind()
                                ));
                            }
                            _ => files.push(format!("      {} {}", t.target_kind(), t.label())),
                        }
                    }
                    let _ = writeln!(out, "    Files");
                    for f in files {
                        let _ = writeln!(out, "{f}");
                    }
                }
                _ => {
                    let _ = writeln!(out, "  Package {key} is in error.");
                }
            }
        }
    }

    fn dump_rule_classes(&self, out: &mut Vec<u8>) {
        use std::io::Write;
        for (name, rc) in zel_packages::rule_class::native_rule_classes() {
            if name.starts_with('$') {
                continue;
            }
            let attrs: Vec<&str> = rc.attrs.keys().map(String::as_str).collect();
            let _ = writeln!(out, "{name}({})", attrs.join(", "));
        }
    }

    fn dump_rules(&self, out: &mut Vec<u8>) {
        use std::io::Write;
        let snap = self.skyframe.evaluator.snapshot_values(&[SkyFunctionName::PACKAGE_LOAD]);
        let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // kind -> (rules, actions)
        for (_, val) in &snap {
            if let Some(Ok(pkg)) = val.as_any().downcast_ref::<PackageLoadValue>().map(|v| &v.0) {
                for t in pkg.targets.values() {
                    if let zel_packages::Target::Rule(r) = t {
                        counts.entry(r.rule_class.clone()).or_default().0 += 1;
                    }
                }
            }
        }
        if counts.is_empty() {
            let _ = write!(out, "No rules in Bazel server, please run a build command first.");
            return;
        }
        let mut rows: Vec<(String, usize, usize)> = counts.into_iter().map(|(k, (c, a))| (k, c, a)).collect();
        // Bazel sorts by COUNT desc, then name asc (SkyKeyStats.BY_COUNT_DESC)
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let longest_name = rows.iter().map(|r| r.0.len()).max().unwrap_or(4);
        let name_col = longest_name.min(30).max(4);
        let _ = write!(out, "{:<width$}{:>10}{:>10}\n", "RULE", "COUNT", "ACTIONS", width = name_col);
        for (kind, count, actions) in rows {
            let _ = write!(out, "{:<width$}{:>10}{:>10}\n", kind, count, actions, width = name_col);
        }
    }

    fn sky_dump(&self, mode: &str, filter: &Regex, out: &mut Vec<u8>) -> Result<(), String> {
        use std::io::Write;
        let ev = &self.skyframe.evaluator;
        let mut keys: Vec<SkyKey> = ev.keys_by_function(SKY_FUNCTIONS);
        keys.sort_by_key(|k| k.to_string());
        let name = |k: &SkyKey| k.to_string();
        match mode {
            "summary" => {
                let edges: usize = keys.iter().map(|k| ev.direct_deps(k).len()).sum();
                let _ = writeln!(out, "Node count: {}", ev.node_count());
                let _ = writeln!(out, "Edge count: {edges}");
            }
            "count" => {
                let mut c: BTreeMap<String, usize> = BTreeMap::new();
                for k in &keys {
                    *c.entry(k.function_name().to_string()).or_default() += 1;
                }
                let mut rows: Vec<_> = c.into_iter().collect();
                rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                for (f, n) in rows {
                    let _ = writeln!(out, "{f}\t{n}");
                }
            }
            "keys" => {
                for k in keys.iter().filter(|k| filter.find(&name(k))) {
                    let _ = writeln!(out, "{}", name(k));
                }
            }
            "value" => {
                for k in keys.iter().filter(|k| filter.find(&name(k))) {
                    let _ = writeln!(out, "{}", name(k));
                    match ev.peek(k) {
                        Some(v) => {
                            let _ = writeln!(out, "  {v:?}");
                        }
                        None => {
                            let _ = writeln!(out, "  (not done)");
                        }
                    }
                }
            }
            "deps" | "rdeps" => {
                for k in keys.iter().filter(|k| filter.find(&name(k))) {
                    let _ = writeln!(out, "{}", name(k));
                    let mut deps: Vec<String> = if mode == "deps" { ev.direct_deps(k) } else { ev.reverse_deps(k) }
                        .iter()
                        .map(|d| d.to_string())
                        .collect();
                    deps.sort();
                    for d in deps {
                        let _ = writeln!(out, "  {d}");
                    }
                }
            }
            "changed" => {
                let _ = writeln!(out, "Values created or changed by the last command:");
                let _ = writeln!(out);
                let _ = writeln!(out, "Values re-evaluated to an equal value, which stopped the invalidation:");
                let _ = writeln!(out);
                let _ = writeln!(out, "Changed values without changed dependencies, the origins of the changes:");
                let _ = writeln!(out);
                let _ = writeln!(out, "Changed values and the changed dependencies that caused them:");
            }
            "function_graph" => {
                let mut g: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
                for k in keys.iter().filter(|k| filter.find(&name(k))) {
                    let from = k.function_name().to_string();
                    for d in ev.direct_deps(k) {
                        *g.entry(from.clone()).or_default().entry(d.function_name().to_string()).or_default() += 1;
                    }
                }
                let _ = writeln!(out, "digraph {{");
                for (from, tos) in g {
                    for (to, n) in tos {
                        let _ = writeln!(out, "  \"{from}\" -> \"{to}\" [label=\"{n}\"];");
                    }
                }
                let _ = writeln!(out, "}}");
            }
            other => return Err(skyframe_enum_error(other)),
        }
        Ok(())
    }

    /// `DumpCommand.exec`.
    pub(crate) fn cmd_dump(&self, args: &[String], stdout: &mut Vec<u8>, stderr: &mut Vec<u8>) -> ExitCode {
        let parsed = match ParsedCommandOptions::parse_for_command("dump", args) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        if !parsed.targets.is_empty() {
            writeln_bytes(stderr, &format!("ERROR: Unrecognized arguments: {}", parsed.targets.join(" ")));
            return ExitCode::COMMAND_LINE_ERROR;
        }
        let flag = |n: &str| parsed.get_bool(n).unwrap_or(false);
        let val = |n: &str| parsed.parsed_options.get(n).and_then(|d| d.raw_values.last().cloned());
        let skyframe = val("skyframe").unwrap_or_else(|| "off".into());
        let any_output = flag("packages")
            || flag("action_cache")
            || flag("rule_classes")
            || flag("rules")
            || val("skylark_memory").is_some()
            || skyframe != "off"
            || val("memory").is_some();
        if !any_output {
            let dump_defs: Vec<&zel_options::OptionDefinition> = zel_options::ALL_COMMAND_OPTIONS
                .iter()
                .filter(|d| d.commands.contains(&"dump") && d.documentation_category == zel_options::OptionDocumentationCategory::OutputSelection)
                .collect();
            let help = super::help::expand_help_topic(
                "dump",
                "Usage: %{product} dump <options>\nDumps the internal state of the %{product} server process.  This command is provided as an aid to debugging, not as a stable interface, so users should not try to parse the output; instead, use 'query' or 'info' for this purpose.\n%{options}",
                &dump_defs,
                super::help::HelpVerbosity::Long,
            );
            stderr.extend_from_slice(help.as_bytes());
            return ExitCode::ANALYSIS_FAILURE;
        }
        if skyframe != "off" {
            if let Err(msg) = skyframe_enum_check(&skyframe) {
                writeln_bytes(stderr, &format!("ERROR: While parsing option --skyframe={skyframe}: {msg}"));
                return ExitCode::COMMAND_LINE_ERROR;
            }
        }
        writeln_bytes(stderr, &format!("WARNING: {WARNING_MESSAGE}"));
        let mut failure = ExitCode::SUCCESS;
        if flag("packages") {
            self.dump_packages(stdout);
            stdout.push(b'\n');
        }
        if flag("action_cache") {
            if let Ok(cache) = self.action_cache.lock() {
                cache.dump(stdout);
            }
            stdout.push(b'\n');
        }
        if flag("rule_classes") {
            self.dump_rule_classes(stdout);
            stdout.push(b'\n');
        }
        if flag("rules") {
            self.dump_rules(stdout);
            stdout.push(b'\n');
        }
        if val("skylark_memory").is_some() {
            // DumpCommand.dumpStarlarkHeap: the allocation tracker only exists when the server
            // runs in memory tracking mode (a JVM agent), which a native process never does.
            writeln_bytes(
                stdout,
                "Cannot dump Starlark heap without running in memory tracking mode. Please refer to the user manual for the dump commnd for information how to turn on memory tracking.",
            );
        }
        if let Some(mode) = val("memory") {
            let words: Vec<&str> = mode.splitn(3, ':').next().unwrap_or("").split(',').collect();
            const KNOWN: &[&str] = &[
                "shallow", "deep", "transitive", "full", "summary", "count", "bytes", "notransient", "noconfig",
                "noprecomputed", "noworkspacestatus",
            ];
            if let Some(bad) = words.iter().find(|w| !w.starts_with("needle=") && !KNOWN.contains(w)) {
                writeln_bytes(stderr, &format!("ERROR: Unrecognized word '{bad}'"));
                return ExitCode::COMMAND_LINE_ERROR;
            }
            if !words.iter().any(|w| ["shallow", "deep", "transitive", "full"].contains(w)) {
                writeln_bytes(stderr, "ERROR: No collection type specified");
                return ExitCode::COMMAND_LINE_ERROR;
            }
            if !words.iter().any(|w| ["summary", "count", "bytes"].contains(w)) {
                writeln_bytes(stderr, "ERROR: No display mode specified");
                return ExitCode::COMMAND_LINE_ERROR;
            }
            // Native Rust values are not object graphs: report per-function node counts, the
            // measurable analog of "count"/"summary".
            let mut c: BTreeMap<String, usize> = BTreeMap::new();
            for k in self.skyframe.evaluator.keys_by_function(SKY_FUNCTIONS) {
                *c.entry(k.function_name().to_string()).or_default() += 1;
            }
            for (f, n) in c {
                writeln_bytes(stdout, &format!("{n:>12} {f}"));
            }
        }
        if skyframe != "off" {
            let filter_src = val("skykey_filter").unwrap_or_else(|| ".*".into());
            let filter = match compile_filter(&filter_src) {
                Ok(f) => f,
                Err(e) => {
                    writeln_bytes(stderr, &format!("ERROR: {e}"));
                    return ExitCode::COMMAND_LINE_ERROR;
                }
            };
            if let Err(e) = self.sky_dump(&skyframe, &filter, stdout) {
                writeln_bytes(stderr, &format!("ERROR: {e}"));
                return ExitCode::COMMAND_LINE_ERROR;
            }
        }
        failure
    }
}

/// The `EnumSpec` of `--skyframe`, generated from HEAD's `DumpCommand.SkyframeDumpOption`.
fn skyframe_enum_spec() -> &'static zel_options::types::EnumSpec {
    zel_options::ALL_COMMAND_OPTIONS
        .iter()
        .find(|d| d.name == "skyframe" && d.commands.contains(&"dump"))
        .and_then(|d| match &d.option_type {
            zel_options::OptionType::Enum(spec) => Some(*spec),
            _ => None,
        })
        .expect("--skyframe is an enum option in the generated flag tables")
}

fn skyframe_enum_check(value: &str) -> Result<(), String> {
    zel_options::convert_enum(skyframe_enum_spec(), value).map(|_| ()).map_err(|e| e.to_string())
}

fn skyframe_enum_error(value: &str) -> String {
    skyframe_enum_check(value).err().unwrap_or_else(|| format!("unsupported skyframe mode '{value}'"))
}
