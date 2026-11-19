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

//! `info` (`InfoCommand` and `runtime/commands/info/*`): every info key, its description,
//! hidden/requires-workspace flags, ordering, `--show_make_env` and key validation.

use super::{not_in_workspace_message, product_name};
use crate::{writeln_bytes, BlazeRuntime};
use std::collections::BTreeMap;
use zel_options::ParsedCommandOptions;
use zel_util::ExitCode;

/// Bazel's `StringUtilities.prettyPrintBytes`.
pub fn pretty_print_bytes(bytes: u64) -> String {
    if bytes < 10_000 {
        format!("{bytes}B")
    } else if bytes < 10_000_000 {
        format!("{}KB", bytes / 1_000)
    } else if bytes < 100_000_000_000 {
        format!("{}MB", bytes / 1_000_000)
    } else {
        format!("{}GB", bytes / 1_000_000_000)
    }
}

/// One `InfoItem`.
pub struct InfoItem {
    pub name: String,
    pub description: String,
    pub hidden: bool,
    pub requires_workspace: bool,
    /// Raw bytes (already newline-terminated for text values), like `InfoItem.get`.
    pub value: Vec<u8>,
}

fn text(v: impl AsRef<str>) -> Vec<u8> {
    let mut b = v.as_ref().as_bytes().to_vec();
    b.push(b'\n');
    b
}

/// Names and descriptions of all hardwired + module info items (for `help info-keys` and the
/// completion script): `(name, description)` in key order.
pub fn info_key_descriptions() -> Vec<(String, String)> {
    let p = product_name();
    let mut v: Vec<(&str, String)> = vec![
        ("workspace", "The working directory of the server.".into()),
        ("install_base", "The installation base directory.".into()),
        ("install_md5", "The logical checksum of the Blaze server.".into()),
        (
            "output_base",
            format!("A directory for shared {p} state as well as tool and strategy specific subdirectories."),
        ),
        ("execution_root", "A directory that makes all input and output files visible to the build.".into()),
        ("output_path", "Output directory".into()),
        (
            "client-env",
            "The specifications that need to be added to the project-specific rc file to freeze the current client environment"
                .into(),
        ),
        ("bazel-bin", "Configuration dependent directory for binaries.".into()),
        ("bazel-genfiles", "Configuration dependent directory for generated files.".into()),
        ("bazel-testlogs", "Configuration dependent directory for logs from a test run.".into()),
        ("release", format!("{p} release identifier")),
        ("server_pid", format!("{p} process id")),
        ("server_log", format!("{p} server log path")),
        ("package_path", "The search path for resolving package labels.".into()),
        (
            "used-heap-size",
            "The amount of used memory in bytes. Note that this is not a good indicator of the actual memory use, as it includes any remaining inaccessible memory."
                .into(),
        ),
        ("used-heap-size-after-gc", "The amount of used memory in bytes after a call to System.gc().".into()),
        (
            "committed-heap-size",
            "The amount of memory in bytes that is committed for the Java virtual machine to use".into(),
        ),
        (
            "max-heap-size",
            "The maximum amount of memory in bytes that can be used for memory management.".into(),
        ),
        ("gc-time", "The approximate accumulated time spend on garbage collection.".into()),
        ("gc-count", "Number of garbage collection runs.".into()),
        ("java-runtime", "Name and version of the current Java runtime environment.".into()),
        ("java-vm", "Name and version of the current Java virtual machine.".into()),
        ("java-home", "Location of the current Java runtime.".into()),
        (
            "character-encoding",
            "Information about the character encoding used by the running JVM.".into(),
        ),
        ("build-language", "A protobuffer with the build language structure".into()),
        ("default-package-path", "The default package path".into()),
        ("starlark-semantics", "The effective set of Starlark semantics option values.".into()),
        ("worker_metrics", "persistent worker metrics".into()),
        ("local_resources", "Auto detected local resources".into()),
        // Module-provided items.
        ("command_log", "Location of the log containing the output from the build commands.".into()),
        ("repository_cache", "The location of the repository download cache used".into()),
        (
            "peak-heap-size",
            "The peak amount of used memory in bytes after any full GC during the most recent invocation.".into(),
        ),
    ];
    v.sort_by(|a, b| a.0.cmp(b.0));
    v.into_iter().map(|(k, d)| (k.to_string(), d)).collect()
}

/// The hardwired info item names in declaration order (`getHardwiredInfoItemNames`), used by the
/// bash completion variables.
pub fn hardwired_info_item_names() -> Vec<String> {
    let p = product_name();
    [
        "workspace",
        "install_base",
        "install_md5",
        "output_base",
        "execution_root",
        "output_path",
        "client-env",
        &format!("{p}-bin"),
        &format!("{p}-genfiles"),
        &format!("{p}-testlogs"),
        "release",
        "server_pid",
        "server_log",
        "package_path",
        "used-heap-size",
        "used-heap-size-after-gc",
        "committed-heap-size",
        "max-heap-size",
        "gc-time",
        "gc-count",
        "java-runtime",
        "java-vm",
        "java-home",
        "character-encoding",
        "build-language",
        "default-package-path",
        "starlark-semantics",
        "worker_metrics",
        "local_resources",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn field_bytes(out: &mut Vec<u8>, field: u32, data: &[u8]) {
    varint(out, ((field as u64) << 3) | 2);
    varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

/// `BuildLanguage` proto (`build.proto`): one `RuleDefinition` per native rule class, sorted by
/// name, with `name`, `type` and `mandatory` for each attribute.
fn build_language_proto() -> Vec<u8> {
    use zel_packages::attr::AttrType;
    let mut out = Vec::new();
    for (name, rc) in zel_packages::rule_class::native_rule_classes() {
        if name.starts_with('$') {
            continue;
        }
        let mut rule = Vec::new();
        field_bytes(&mut rule, 1, name.as_bytes());
        for (aname, attr) in &rc.attrs {
            let mut a = Vec::new();
            field_bytes(&mut a, 1, aname.as_bytes());
            let disc: u64 = match attr.ty {
                AttrType::Int => 1,
                AttrType::String => 2,
                AttrType::Label => 3,
                AttrType::Output => 4,
                AttrType::StringList => 5,
                AttrType::LabelList => 6,
                AttrType::OutputList => 7,
                AttrType::StringDict => 10,
                AttrType::StringListDict => 13,
                AttrType::Bool => 14,
                AttrType::Tristate => 15,
                AttrType::IntList => 16,
                AttrType::LabelKeyedStringDict => 21,
                AttrType::StringKeyedLabelDict => 19,
                AttrType::LabelListDict => 12,
            };
            varint(&mut a, 2 << 3);
            varint(&mut a, disc);
            varint(&mut a, 3 << 3);
            varint(&mut a, attr.mandatory as u64);
            if matches!(
                attr.ty,
                AttrType::StringList
                    | AttrType::LabelList
                    | AttrType::OutputList
                    | AttrType::IntList
                    | AttrType::StringDict
                    | AttrType::StringListDict
                    | AttrType::LabelKeyedStringDict
                    | AttrType::StringKeyedLabelDict
                    | AttrType::LabelListDict
            ) {
                varint(&mut a, 6 << 3);
                varint(&mut a, (!attr.non_empty) as u64);
            }
            if attr.ty == AttrType::Label {
                varint(&mut a, 7 << 3);
                varint(&mut a, attr.allow_single_file as u64);
                varint(&mut a, 10 << 3);
                varint(&mut a, attr.executable as u64);
            }
            varint(&mut a, 11 << 3);
            varint(&mut a, attr.configurable as u64);
            if attr.nodep_set == Some(true)
                || matches!(
                    attr.ty,
                    AttrType::Label
                        | AttrType::LabelList
                        | AttrType::LabelKeyedStringDict
                        | AttrType::StringKeyedLabelDict
                        | AttrType::LabelListDict
                )
            {
                varint(&mut a, 12 << 3);
                varint(&mut a, attr.nodep as u64);
            }
            if attr.tool_dep.unwrap_or(attr.cfg_exec) {
                varint(&mut a, 13 << 3);
                varint(&mut a, 1);
            }
            field_bytes(&mut rule, 2, &a);
        }
        field_bytes(&mut out, 1, &rule);
    }
    out
}

impl BlazeRuntime {
    pub(crate) fn in_workspace(&self) -> bool {
        !self.dirs.workspace_dir.path_string().is_empty()
    }

    fn collect_info_items(&self, parsed: &ParsedCommandOptions) -> BTreeMap<String, InfoItem> {
        let p = product_name();
        let desc: BTreeMap<String, String> = info_key_descriptions().into_iter().collect();
        let cpu = parsed
            .get_str("cpu")
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Self::host_cpu().to_string());
        let mode = parsed.compilation_mode.clone();
        let cfg_mnemonic = self
            .build_config_key(parsed, &self.make_loader(parsed))
            .map(|k| k.mnemonic())
            .unwrap_or_else(|_| format!("{cpu}-{mode}"));
        let dirs = &self.dirs;
        let _ws = self.in_workspace();
        let pkg_path = {
            let v = parsed
                .parsed_options
                .get("package_path")
                .map(|d| d.raw_values.clone())
                .unwrap_or_default();
            if v.is_empty() {
                "%workspace%".to_string()
            } else {
                v.join(":")
            }
        };
        let server_log = {
            let link = dirs.server_log();
            match std::fs::read_link(link.path_string()) {
                Ok(t) => {
                    let t = if t.is_absolute() {
                        t
                    } else {
                        std::path::Path::new(dirs.output_base.path_string()).join(t)
                    };
                    t.display().to_string()
                }
                Err(_) => link.to_string(),
            }
        };
        let rss = crate::sys::resident_bytes();
        let host = zel_util::resource_converter::HostResources::detect_cached();
        let client_env = self.client_env();
        let mut client_env_text = String::new();
        // `env.getAllowlistedActionEnv()` / `getAllowlistedTestEnv()`: last-op-wins allowlist of
        // variables named without a value (`NAME` adds; `NAME=val` or `=NAME` removes), sorted.
        for (flag, label) in [("action_env", "action_env"), ("test_env", "test_env")] {
            if let Some(d) = parsed.parsed_options.get(flag) {
                let mut inherited = std::collections::BTreeSet::new();
                for raw in &d.raw_values {
                    if let Some(unset) = raw.strip_prefix('=') {
                        inherited.remove(unset);
                    } else if let Some((k, _)) = raw.split_once('=') {
                        inherited.remove(k);
                    } else if !raw.starts_with('-') && !raw.is_empty() {
                        inherited.insert(raw.clone());
                    }
                }
                for name in inherited {
                    if let Some(v) = client_env.get(&name) {
                        client_env_text.push_str(&format!("build --{label}={name}={v}\n"));
                    }
                }
            }
        }
        let install_md5 = super::install_md5()
            .map(str::to_string)
            .unwrap_or_else(|| dirs.install_base.base_name().to_string());
        let mk = |name: &str, hidden: bool, requires_ws: bool, value: Vec<u8>| InfoItem {
            name: name.to_string(),
            description: desc.get(name).cloned().unwrap_or_default(),
            hidden,
            requires_workspace: requires_ws,
            value,
        };
        let logical_path = |pf: &zel_vfs::PathFragment| -> String { pf.path_string().to_string() };
        let embedded_jdk = dirs.install_base.get_relative_str("embedded_tools/jdk");
        let java_home = logical_path(&embedded_jdk);
        let main_execroot = dirs.execroot_base().get_relative_str("_main");
        let main_output_path = main_execroot.get_relative_str("bazel-out");
        let cfg_out_dir = main_output_path.get_relative_str(&cfg_mnemonic);
        let bin_dir = cfg_out_dir.get_relative_str("bin");
        let testlogs_dir = cfg_out_dir.get_relative_str("testlogs");

        // `JavaRuntimeInfoItem` / `JavaVirtualMachineInfoItem` print `java.runtime.*` / `java.vm.*` properties.
        // Zel is a native executable without a JVM: the same templates are filled with the native runtime's own
        // properties (documented deviation, README "Deviations from Bazel").
        let (java_runtime, java_vm) = {
            let version = env!("CARGO_PKG_VERSION");
            let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
            (
                format!("Zel native runtime (build {version}) by The Zel Authors"),
                format!(
                    "Zel native code (build {version}, {platform} compiled) by The Zel Authors"
                ),
            )
        };
        // `CharacterEncodingInfoItem`: the client starts the server JVM with `-Dfile.encoding=ISO-8859-1`
        // (blaze.cc), which fixes `file.encoding`/default charset; the JNU
        // (file name) encoding follows the process locale exactly like the JVM's `sun.jnu.encoding`.
        let character_encoding = {
            let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
                .iter()
                .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
                .unwrap_or_default();
            let lower = locale.to_ascii_lowercase();
            let jnu = if lower.contains("utf-8") || lower.contains("utf8") {
                "UTF-8"
            } else if locale.is_empty() || locale == "C" || locale == "POSIX" {
                "ANSI_X3.4-1968"
            } else {
                locale.split('.').nth(1).unwrap_or("UTF-8")
            };
            format!(
                "file.encoding = ISO-8859-1, defaultCharset = ISO-8859-1, sun.jnu.encoding = {jnu}"
            )
        };
        // `StarlarkSemantics.toString()` of the `BuildLanguageOptions` semantics (non-default pairs only).
        let starlark_semantics =
            zel_options::starlark_semantics_string(&|flag| parsed.get_bool(flag), &|flag| {
                parsed
                    .parsed_options
                    .get(flag)
                    .map(|d| d.raw_values.clone())
                    .unwrap_or_default()
            });
        let worker_metrics = crate::exec_phase::worker_metrics_text(dirs.output_base.path_string());

        // Real measurable process memory:
        // Zel is native Rust, not a JVM. We report actual process memory (RSS)
        // rather than fabricated JVM heap constants.
        let used_heap = rss;
        let committed_heap = rss;
        // A JVM's default maximum heap is a quarter of the physical memory (`-XX:MaxRAMPercentage=25`).
        let max_heap_bytes = (host.memory_mb as u64) * 1024 * 1024 / 4;
        let used_after_gc = rss;

        let mut items: Vec<InfoItem> = vec![
            mk(
                "workspace",
                false,
                true,
                text(logical_path(&dirs.workspace_dir)),
            ),
            mk(
                "install_base",
                false,
                false,
                text(logical_path(&dirs.install_base)),
            ),
            mk("install_md5", true, false, text(install_md5)),
            mk(
                "output_base",
                false,
                false,
                text(logical_path(&dirs.output_base)),
            ),
            mk(
                "execution_root",
                false,
                true,
                text(logical_path(&main_execroot)),
            ),
            mk(
                "output_path",
                false,
                false,
                text(logical_path(&main_output_path)),
            ),
            mk("client-env", true, false, text(client_env_text)),
            mk(
                &format!("{p}-bin"),
                false,
                true,
                text(logical_path(&bin_dir)),
            ),
            mk(
                &format!("{p}-genfiles"),
                false,
                true,
                text(logical_path(&bin_dir)),
            ),
            mk(
                &format!("{p}-testlogs"),
                false,
                true,
                text(logical_path(&testlogs_dir)),
            ),
            mk("release", false, false, text(crate::release_name())),
            mk(
                "server_pid",
                false,
                false,
                text(self.server_pid.to_string()),
            ),
            mk("server_log", false, false, text(&server_log)),
            mk("package_path", false, true, text(pkg_path.clone())),
            mk(
                "used-heap-size",
                false,
                false,
                text(pretty_print_bytes(used_heap)),
            ),
            mk(
                "used-heap-size-after-gc",
                true,
                false,
                text(pretty_print_bytes(used_after_gc)),
            ),
            mk(
                "committed-heap-size",
                false,
                false,
                text(pretty_print_bytes(committed_heap)),
            ),
            mk(
                "max-heap-size",
                false,
                false,
                text(pretty_print_bytes(max_heap_bytes)),
            ),
            // Honest Zel semantics: Zel has no JVM and no garbage collector.
            // Garbage collection runs and accumulated time are 0.
            mk("gc-time", false, false, text("0ms")),
            mk("gc-count", false, false, text("0")),
            mk("java-runtime", false, false, text(java_runtime)),
            mk("java-vm", false, false, text(java_vm)),
            mk("java-home", false, false, text(java_home)),
            mk("character-encoding", false, false, text(character_encoding)),
            mk("build-language", true, true, build_language_proto()),
            mk("default-package-path", true, true, text(pkg_path)),
            mk("starlark-semantics", true, false, text(starlark_semantics)),
            mk("worker_metrics", true, false, text(worker_metrics)),
            mk(
                "local_resources",
                false,
                false,
                text(format!(
                    "RAM={}MB, CPU={:.1}",
                    host.truncated_ram_mb(),
                    host.cpus
                )),
            ),
            mk(
                "command_log",
                false,
                false,
                text(logical_path(&dirs.command_log())),
            ),
            mk(
                "repository_cache",
                false,
                false,
                text(logical_path(
                    &dirs.output_user_root.get_relative_str("cache/repos/v1"),
                )),
            ),
            mk("peak-heap-size", true, false, text("unknown")),
        ];
        let mut map = BTreeMap::new();
        for i in items.drain(..) {
            map.insert(i.name.clone(), i);
        }
        map
    }

    /// Variables of the default configuration's `getMakeEnvironment()` that do not depend on a
    /// C++ toolchain (the toolchain-derived `CC`, `AR`, ... are not modelled).
    fn make_environment(&self, parsed: &ParsedCommandOptions) -> Vec<(String, String)> {
        let cpu = parsed
            .get_str("cpu")
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Self::host_cpu().to_string());
        let mode = &parsed.compilation_mode;
        let cfg_mnemonic = self
            .build_config_key(parsed, &self.make_loader(parsed))
            .map(|k| k.mnemonic())
            .unwrap_or_else(|_| format!("{cpu}-{mode}"));
        let target_cpu = match cpu.as_str() {
            "darwin_arm64" => "aarch64",
            "darwin_x86_64" => "x86_64",
            other => other,
        };
        let mut map = BTreeMap::from([
            (
                "BINDIR".to_string(),
                format!("bazel-out/{cfg_mnemonic}/bin"),
            ),
            ("COMPILATION_MODE".to_string(), mode.clone()),
            (
                "GENDIR".to_string(),
                format!("bazel-out/{cfg_mnemonic}/bin"),
            ),
            ("TARGET_CPU".to_string(), target_cpu.to_string()),
        ]);
        if let Some(d) = parsed.parsed_options.get("define") {
            for raw in &d.raw_values {
                let (k, v) = raw.split_once('=').unwrap_or((raw.as_str(), ""));
                if !k.is_empty() {
                    map.insert(k.to_string(), v.to_string());
                }
            }
        }
        map.into_iter().collect()
    }

    /// `InfoCommand.exec`.
    pub(crate) fn cmd_info(
        &self,
        args: &[String],
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
        extensions: &mut Vec<zel_proto::Any>,
    ) -> ExitCode {
        let parsed = match ParsedCommandOptions::parse_for_command("info", args) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        let in_ws = self.in_workspace();
        let mut items = self.collect_info_items(&parsed);
        let residue = &parsed.targets;
        if parsed.show_make_env {
            if !in_ws {
                let msg = "option '--show_make_env' is only supported from within a workspace (below a directory having a MODULE.bazel file).\nSee documentation at https://bazel.build/concepts/build-ref#workspace";
                writeln_bytes(stderr, &format!("ERROR: {msg}"));
                return ExitCode::COMMAND_LINE_ERROR;
            }
            for (k, v) in self.make_environment(&parsed) {
                items.insert(
                    k.clone(),
                    InfoItem {
                        name: k,
                        description: v.clone(),
                        hidden: false,
                        requires_workspace: false,
                        value: text(v),
                    },
                );
            }
        }
        let print_keys = residue.len() != 1;
        let is_proto_output = args.iter().any(|a| {
            a.eq_ignore_ascii_case("--info_output_type=response_proto")
                || a.eq_ignore_ascii_case("-info_output_type=response_proto")
        }) || parsed
            .get_str("info_output_type")
            .is_some_and(|s| s.eq_ignore_ascii_case("response_proto"));

        let (mut unknown, mut unsupported) = (Vec::<String>::new(), Vec::<String>::new());
        if is_proto_output {
            let mut info_items = Vec::new();
            if residue.is_empty() {
                for item in items.values() {
                    if item.hidden || (!in_ws && item.requires_workspace) {
                        continue;
                    }
                    info_items.push(zel_proto::command_server::InfoItem {
                        key: item.name.clone(),
                        value: item.value.clone(),
                    });
                }
            } else {
                for key in residue {
                    match items.get(key) {
                        Some(item) => {
                            if !in_ws && item.requires_workspace {
                                if !unsupported.contains(key) {
                                    unsupported.push(key.clone());
                                }
                            } else {
                                info_items.push(zel_proto::command_server::InfoItem {
                                    key: item.name.clone(),
                                    value: item.value.clone(),
                                });
                            }
                        }
                        None => {
                            if !unknown.contains(key) {
                                unknown.push(key.clone());
                            }
                        }
                    }
                }
            }
            let client_cwd = parsed
                .get_str("client_cwd")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| self.dirs.workspace_dir.path_string());
            let mut path_to_replace = vec![
                zel_proto::command_server::PathToReplace {
                    r#type: zel_proto::command_server::PathToReplaceType::OutputBase,
                    value: self.dirs.output_base.path_string().as_bytes().to_vec(),
                },
                zel_proto::command_server::PathToReplace {
                    r#type: zel_proto::command_server::PathToReplaceType::BuildWorkingDirectory,
                    value: client_cwd.as_bytes().to_vec(),
                },
            ];
            if in_ws {
                path_to_replace.push(zel_proto::command_server::PathToReplace {
                    r#type: zel_proto::command_server::PathToReplaceType::BuildWorkspaceDirectory,
                    value: self.dirs.workspace_dir.path_string().as_bytes().to_vec(),
                });
            }
            let info_resp = zel_proto::command_server::InfoResponse {
                path_to_replace,
                info_item: info_items,
                print_keys,
            };
            let any = zel_proto::Any {
                type_url: "type.googleapis.com/command_server.InfoResponse".to_string(),
                value: info_resp.encode_to_vec(),
            };
            extensions.push(any);
        } else if residue.is_empty() {
            for item in items.values() {
                if item.hidden || (!in_ws && item.requires_workspace) {
                    continue;
                }
                if print_keys {
                    stdout.extend_from_slice(item.name.as_bytes());
                    stdout.extend_from_slice(b": ");
                }
                stdout.extend_from_slice(&item.value);
            }
            return ExitCode::SUCCESS;
        } else {
            for key in residue {
                match items.get(key) {
                    Some(item) => {
                        if !in_ws && item.requires_workspace {
                            if !unsupported.contains(key) {
                                unsupported.push(key.clone());
                            }
                            continue;
                        }
                        if print_keys {
                            stdout.extend_from_slice(item.name.as_bytes());
                            stdout.extend_from_slice(b": ");
                        }
                        stdout.extend_from_slice(&item.value);
                    }
                    None => {
                        if !unknown.contains(key) {
                            unknown.push(key.clone());
                        }
                    }
                }
            }
        }
        if !unknown.is_empty() {
            let names: Vec<&str> = items.keys().map(String::as_str).collect();
            let msg = format!(
                "unknown key(s): {}",
                unknown
                    .iter()
                    .map(|k| format!(
                        "'{k}'{}",
                        zel_options::spellcheck::did_you_mean(k, names.iter().copied())
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            writeln_bytes(stderr, &format!("ERROR: {msg}"));
            return ExitCode::COMMAND_LINE_ERROR;
        }
        if !unsupported.is_empty() {
            let msg = format!(
                "key(s) only supported from within a workspace (below a directory having a MODULE.bazel file): {}\nSee documentation at https://bazel.build/concepts/build-ref#workspace",
                unsupported.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(", ")
            );
            writeln_bytes(stderr, &format!("ERROR: {msg}"));
            return ExitCode::COMMAND_LINE_ERROR;
        }
        let _ = not_in_workspace_message;
        ExitCode::SUCCESS
    }
}
