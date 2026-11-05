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

//! `config` (`ConfigCommand` + `ConfigCommandOutputFormatter`): lists the configurations in the
//! Skyframe graph, dumps one (`config <hash>`) or diffs two.

use crate::{writeln_bytes, BlazeRuntime};
use std::collections::BTreeMap;
use zel_analysis::BuildConfigurationKey;
use zel_options::ParsedCommandOptions;
use zel_skyframe::{SkyFunctionName, SkyKey};
use zel_util::ExitCode;

struct ConfigForOutput {
    hash: String,
    is_exec: bool,
    mnemonic: String,
    display_mnemonic: String,
    /// Fragment options class (fully qualified) → option → value, sorted.
    fragments: BTreeMap<String, BTreeMap<String, String>>,
}

/// `BuildConfigurationValue` as `ConfigCommand` shows it: every option of every fragment options
/// class present in the (normalized) `BuildOptions`, `ConfigurationForOutput.getTransitionSuffix`.
fn for_output(mnemonic: String, exec: bool, o: &zel_analysis::build_options::BuildOptions) -> ConfigForOutput {
    let mut fragments: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (cls, opts) in &o.fragments {
        let m = fragments.entry((*cls).to_string()).or_default();
        for (k, v) in opts {
            // `--define` lives in the `user-defined` section below (`CoreOptions.commandLineBuildVariables`).
            if *k == "define" && matches!(v, zel_analysis::build_options::ConfigValue::List(l) if l.is_empty()) {
                continue;
            }
            m.insert((*k).to_string(), v.display());
        }
    }
    let mut user: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in &o.starlark {
        user.insert(k.clone(), v.clone());
    }
    fragments.insert("user-defined".to_string(), user);
    let has_test = o.fragments.contains_key(zel_analysis::build_options::TEST_OPTIONS);
    let suffix = if exec {
        " (exec)"
    } else if !has_test {
        " (test-trimmed)"
    } else {
        ""
    };
    ConfigForOutput {
        hash: o.checksum(),
        is_exec: exec,
        display_mnemonic: format!("{mnemonic}{suffix}"),
        mnemonic,
        fragments,
    }
}

/// `--define`/native options of the command line as typed Java values.
pub(crate) fn native_settings(
    parsed: &ParsedCommandOptions,
    canon: &dyn Fn(&str) -> Result<String, String>,
) -> BTreeMap<String, zel_analysis::NativeSetting> {
    let host_cpu = zel_analysis::build_options::auto_host_cpu();
    let mut out = BTreeMap::new();
    for (name, d) in &parsed.parsed_options {
        let Some((_, opt)) = zel_analysis::build_options::find_option(name) else { continue };
        if let Ok(value) = zel_analysis::build_options::convert_raws(opt, &d.raw_values, canon, host_cpu) {
            out.insert(name.clone(), zel_analysis::NativeSetting { raw: String::new(), value });
        }
    }
    out
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '\u{8}' => o.push_str("\\b"),
            '\u{c}' => o.push_str("\\f"),
            // Gson's HTML-safe escaping (`GsonBuilder.disableHtmlEscaping` is not used).
            '<' | '>' | '&' | '=' | '\'' | '\u{2028}' | '\u{2029}' => o.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn json_config(c: &ConfigForOutput) -> String {
    let frags: Vec<String> = zel_analysis::build_options::CONFIG_FRAGMENTS
        .iter()
        .map(|(f, opts)| {
            let o: Vec<String> = opts.iter().map(|o| json_str(o)).collect();
            format!("{{\"name\":{},\"fragmentOptions\":[{}]}}", json_str(f), o.join(","))
        })
        .collect();
    let opts: Vec<String> = c
        .fragments
        .iter()
        .map(|(f, o)| {
            let kv: Vec<String> = o.iter().map(|(k, v)| format!("{}:{}", json_str(k), json_str(v))).collect();
            format!("{{\"name\":{},\"options\":{{{}}}}}", json_str(f), kv.join(","))
        })
        .collect();
    format!(
        "{{\"skyKey\":{},\"configHash\":{},\"mnemonic\":{},\"isExec\":{},\"fragments\":[{}],\"fragmentOptions\":[{}]}}",
        json_str(&format!("BuildConfigurationKey[{}]", c.hash)),
        json_str(&c.hash),
        json_str(&c.mnemonic),
        c.is_exec,
        frags.join(","),
        opts.join(",")
    )
}

impl BlazeRuntime {
    fn find_configurations(&self) -> Vec<ConfigForOutput> {
        let registered = zel_analysis::registered_configurations();
        let mut out: Vec<ConfigForOutput> = Vec::new();
        for (_, k) in registered {
            if k.is_exec {
                out.push(for_output(k.config_display_mnemonic(), true, &k.build_options()));
                continue;
            }
            if k.core_only {
                out.push(for_output("fastbuild-noconfig".to_string(), false, &k.build_options()));
                continue;
            }
            out.push(for_output(k.base_mnemonic(), false, &k.untrimmed_build_options()));
            out.push(for_output(k.base_mnemonic(), false, &k.build_options()));
        }
        if !out.is_empty() {
            // `NoConfigTransition`: only CoreOptions at defaults.
            let nc = zel_analysis::build_options::BuildOptions::no_config(zel_analysis::build_options::auto_host_cpu());
            let mode = nc.get("compilation_mode").map(|v| v.display()).unwrap_or_default();
            out.push(for_output(format!("{mode}-noconfig"), false, &nc));
        }
        out.sort_by(|a, b| a.hash.cmp(&b.hash));
        out.dedup_by(|a, b| a.hash == b.hash);
        out
    }

    /// `ConfigCommand.exec`.
    pub(crate) fn cmd_config(&self, args: &[String], stdout: &mut Vec<u8>, stderr: &mut Vec<u8>) -> ExitCode {
        let parsed = match ParsedCommandOptions::parse_for_command("config", args) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        let configs = self.find_configurations();
        if configs.is_empty() {
            writeln_bytes(
                stderr,
                "ERROR: No configurations found. This can happen if the 'config' subcommand is used after files, including their metadata, have changed since the last invocation of another subcommand. Try running a 'build' or 'cquery' directly followed by 'config'.",
            );
            return ExitCode::COMMAND_LINE_ERROR;
        }
        let residue = &parsed.targets;
        if residue.len() == 1 {
            writeln_bytes(stderr, &format!("INFO: Displaying config with id {}", residue[0]));
        }
        let json = parsed.get_str("output") == Some("json");
        let dump_all = parsed.get_bool("dump_all").unwrap_or(false);
        let text_config = |out: &mut Vec<u8>, c: &ConfigForOutput| {
            writeln_bytes(out, &format!("BuildConfigurationValue {}:", c.hash));
            writeln_bytes(out, &format!("Skyframe Key: BuildConfigurationKey[{}]", c.hash));
            let mut frags = String::new();
            for (f, opts) in zel_analysis::build_options::CONFIG_FRAGMENTS {
                frags.push_str(&format!("{f}: [{}], ", opts.join(",")));
            }
            writeln_bytes(out, &format!("Fragments: {frags}"));
            for (f, opts) in &c.fragments {
                writeln_bytes(out, &format!("FragmentOptions {f} {{"));
                for (k, v) in opts {
                    writeln_bytes(out, &format!("  {k}: {v}"));
                }
                writeln_bytes(out, "}");
            }
        };
        let get = |prefix: &str| -> Result<&ConfigForOutput, String> {
            let hash_matches: Vec<&ConfigForOutput> = configs.iter().filter(|c| c.hash.starts_with(prefix)).collect();
            let mn_matches: Vec<&ConfigForOutput> = configs
                .iter()
                .filter(|c| c.display_mnemonic.starts_with(prefix) || c.mnemonic.starts_with(prefix))
                .collect();
            let mut combined: Vec<&ConfigForOutput> = hash_matches.clone();
            for m in &mn_matches {
                if !combined.iter().any(|c| c.hash == m.hash) {
                    combined.push(m);
                }
            }
            match combined.len() {
                1 => Ok(combined[0]),
                0 => Err(format!("No configuration found with ID prefix {prefix}")),
                _ if mn_matches.is_empty() => Err(format!(
                    "Configuration identifier '{prefix}' is ambiguous.\n'{prefix}' is a prefix of multiple configurations:\n {}\n\nUse a sufficient prefix to uniquely identify one configuration.",
                    hash_matches.iter().map(|c| c.hash.as_str()).collect::<Vec<_>>().join("\n ")
                )),
                _ => Err("Multiple configurations match this prefix. Please choose a specific checksum to disambiguate".into()),
            }
        };
        let residue = &parsed.targets;
        match residue.len() {
            0 if dump_all => {
                if json {
                    let all: Vec<String> = configs.iter().map(json_config).collect();
                    writeln_bytes(stdout, &format!("[{}]", all.join(",")));
                } else {
                    for c in &configs {
                        text_config(stdout, c);
                    }
                }
            }
            0 => {
                if json {
                    let ids: Vec<String> = configs.iter().map(|c| json_str(&c.hash)).collect();
                    writeln_bytes(stdout, &format!("{{\"configuration-IDs\":[{}]}}", ids.join(",")));
                } else {
                    writeln_bytes(stdout, "Available configurations:");
                    for c in &configs {
                        writeln_bytes(stdout, &format!("{} {}", c.hash, c.display_mnemonic));
                    }
                }
            }
            1 => {
                match get(&residue[0]) {
                    Ok(c) => {
                        if json {
                            writeln_bytes(stdout, &json_config(c));
                        } else {
                            text_config(stdout, c);
                        }
                    }
                    Err(m) => {
                        writeln_bytes(stderr, &format!("ERROR: {m}"));
                        return ExitCode::COMMAND_LINE_ERROR;
                    }
                }
            }
            2 => {
                writeln_bytes(
                    stderr,
                    &format!("INFO: Displaying diff between configs {} and {}", residue[0], residue[1]),
                );
                let (c1, c2) = match (get(&residue[0]), get(&residue[1])) {
                    (Ok(a), Ok(b)) => (a, b),
                    (Err(m), _) | (_, Err(m)) => {
                        writeln_bytes(stderr, &format!("ERROR: {m}"));
                        return ExitCode::COMMAND_LINE_ERROR;
                    }
                };
                let mut diffs: BTreeMap<String, BTreeMap<String, (Option<String>, Option<String>)>> = BTreeMap::new();
                let names: std::collections::BTreeSet<&String> =
                    c1.fragments.keys().chain(c2.fragments.keys()).collect();
                for f in names {
                    let o1 = c1.fragments.get(f);
                    let o2 = c2.fragments.get(f);
                    let keys: std::collections::BTreeSet<&String> =
                        o1.into_iter().chain(o2).flat_map(|m| m.keys()).collect();
                    for k in keys {
                        let v1 = o1.and_then(|m| m.get(k)).cloned();
                        let v2 = o2.and_then(|m| m.get(k)).cloned();
                        if v1 != v2 {
                            diffs.entry(f.clone()).or_default().insert(k.clone(), (v1, v2));
                        }
                    }
                }
                let s = |o: &Option<String>| o.clone().unwrap_or_else(|| "null".into());
                if json {
                    let frags: Vec<String> = diffs
                        .iter()
                        .map(|(f, m)| {
                            let od: Vec<String> = m
                                .iter()
                                .map(|(k, (a, b))| {
                                    format!("{}:{{\"first\":{},\"second\":{}}}", json_str(k), json_str(&s(a)), json_str(&s(b)))
                                })
                                .collect();
                            format!("{{\"name\":{},\"optionsDiff\":{{{}}}}}", json_str(f), od.join(","))
                        })
                        .collect();
                    writeln_bytes(
                        stdout,
                        &format!(
                            "{{\"configHash1\":{},\"configHash2\":{},\"fragmentsDiff\":[{}]}}",
                            json_str(&c1.hash),
                            json_str(&c2.hash),
                            frags.join(",")
                        ),
                    );
                } else {
                    writeln_bytes(stdout, &format!("Displaying diff between configs {} and {}", c1.hash, c2.hash));
                    for (f, m) in &diffs {
                        writeln_bytes(stdout, &format!("FragmentOptions {f} {{"));
                        for (k, (a, b)) in m {
                            writeln_bytes(stdout, &format!("  {k}: {}, {}", s(a), s(b)));
                        }
                        writeln_bytes(stdout, "}");
                    }
                }
            }
            _ => {
                writeln_bytes(stderr, "ERROR: Too many config ids.");
                return ExitCode::COMMAND_LINE_ERROR;
            }
        }
        ExitCode::SUCCESS
    }
}
