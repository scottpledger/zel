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

//! Legacy (pre-engine) analysis of `copy_file` and `rust_*` rules: these drive real tools
//! (`/bin/cp`, `rustc`). Every other rule class goes through the typed engine in `native.rs`.

use std::collections::BTreeMap;
use zel_analysis::{
    ActionAnalysisMetadata, Artifact, BuildConfigurationKey, ConfiguredTarget, DefaultInfo,
    OutputGroupInfo,
};
use zel_cmdline::Label;
use zel_packages::RuleInstance;

pub(crate) fn analyze_legacy(
    rule: &RuleInstance,
    config: &BuildConfigurationKey,
) -> Result<ConfiguredTarget, String> {
    let cfg_mnemonic = config.mnemonic();
    let pkg_id = rule.label.package_identifier();
    let pkg_frag = pkg_id.package_fragment();

    let mut src_artifacts = Vec::new();
    for s in &rule.srcs {
        let clean = s.strip_prefix(':').unwrap_or(s);
        let rel = if clean.starts_with("//") {
            if let Ok(lbl) = Label::parse_canonical(clean) {
                lbl.package_identifier()
                    .package_fragment()
                    .get_relative_str(lbl.name())
            } else {
                pkg_frag.get_relative_str(clean)
            }
        } else {
            pkg_frag.get_relative_str(clean)
        };
        src_artifacts.push(Artifact::source(rel, rule.label.clone()));
    }

    let mut out_artifacts = Vec::new();
    for o in &rule.outs {
        let clean = o.strip_prefix(':').unwrap_or(o);
        let rel = pkg_frag.get_relative_str(clean);
        out_artifacts.push(Artifact::derived(&cfg_mnemonic, rel, rule.label.clone()));
    }

    let mut actions = Vec::new();
    let mut executable: Option<Artifact> = None;

    match rule.rule_class.as_str() {
        "copy_file" => {
            if out_artifacts.is_empty() {
                let default_out = pkg_frag.get_relative_str(rule.label.name());
                out_artifacts.push(Artifact::derived(
                    &cfg_mnemonic,
                    default_out,
                    rule.label.clone(),
                ));
            }
            if let (Some(src), Some(dst)) = (src_artifacts.first(), out_artifacts.first()) {
                actions.push(ActionAnalysisMetadata::SpawnAction {
                    owner: rule.label.clone(),
                    mnemonic: "CopyFile".to_string(),
                    progress_message: format!("Copying {} to {}", src.exec_path, dst.exec_path),
                    argv: vec![
                        "/bin/cp".to_string(),
                        "-f".to_string(),
                        src.exec_path.to_string(),
                        dst.exec_path.to_string(),
                    ],
                    env: BTreeMap::new(),
                    inherited_env: Default::default(),
                    unset_env: Default::default(),
                    inputs: vec![src.clone()],
                    outputs: vec![dst.clone()],
                    execution_info: BTreeMap::new(),
                    java_compile: None,
                });
            }
        }
        "rust_binary" | "rust_test" | "rust_library" | "rust_shared_library" => {
            let is_bin = rule.rule_class == "rust_binary" || rule.rule_class == "rust_test";
            let crate_name = rule.label.name().replace('-', "_");
            let out_name = if is_bin {
                rule.label.name().to_string()
            } else if rule.rule_class == "rust_shared_library" {
                format!("lib{crate_name}.dylib")
            } else {
                format!("lib{crate_name}.rlib")
            };
            let out_rel = pkg_frag.get_relative_str(&out_name);
            let out_art = Artifact::derived(&cfg_mnemonic, out_rel, rule.label.clone());

            let crate_root = src_artifacts
                .iter()
                .find(|a| {
                    let b = a.exec_path.base_name();
                    b == "main.rs" || b == "lib.rs"
                })
                .or_else(|| src_artifacts.first());

            if let Some(root_src) = crate_root {
                let mut argv = vec![
                    "rustc".to_string(),
                    "--edition=2021".to_string(),
                    format!("--crate-name={crate_name}"),
                    root_src.exec_path.to_string(),
                    "-o".to_string(),
                    out_art.exec_path.to_string(),
                ];
                if rule.rule_class == "rust_library" {
                    argv.push("--crate-type=rlib".to_string());
                } else if rule.rule_class == "rust_shared_library" {
                    argv.push("--crate-type=cdylib".to_string());
                } else if rule.rule_class == "rust_test" {
                    argv.push("--test".to_string());
                }
                let mut action_inputs = src_artifacts.clone();
                if !rule.deps.is_empty() {
                    let bin_pkg_dir = format!("bazel-out/{cfg_mnemonic}/bin/{pkg_frag}");
                    argv.push("-L".to_string());
                    argv.push(format!("dependency={bin_pkg_dir}"));
                    for dep_str in &rule.deps {
                        let dep_name = dep_str
                            .rsplit(':')
                            .next()
                            .unwrap_or(dep_str)
                            .trim_start_matches("//")
                            .replace('-', "_");
                        let dep_crate_name = dep_name
                            .strip_prefix("zel_client_lib")
                            .map(|_| "zel_client")
                            .unwrap_or(&dep_name);
                        let dep_rlib_rel =
                            pkg_frag.get_relative_str(&format!("lib{dep_name}.rlib"));
                        let dep_art =
                            Artifact::derived(&cfg_mnemonic, dep_rlib_rel, rule.label.clone());
                        argv.push("--extern".to_string());
                        argv.push(format!("{dep_crate_name}={}", dep_art.exec_path));
                        action_inputs.push(dep_art);
                    }
                }
                actions.push(ActionAnalysisMetadata::SpawnAction {
                    owner: rule.label.clone(),
                    mnemonic: "Rustc".to_string(),
                    progress_message: format!("Compiling Rust {} {}", rule.rule_class, rule.label),
                    argv,
                    env: BTreeMap::new(),
                    inherited_env: Default::default(),
                    unset_env: Default::default(),
                    inputs: action_inputs,
                    outputs: vec![out_art.clone()],
                    execution_info: BTreeMap::new(),
                    java_compile: None,
                });
            } else {
                actions.push(ActionAnalysisMetadata::FileWriteAction {
                    owner: rule.label.clone(),
                    output: out_art.clone(),
                    content: Vec::new(),
                    is_executable: is_bin,
                });
            }
            if is_bin {
                executable = Some(out_art.clone());
            }
            out_artifacts = vec![out_art];
        }
        other => {
            return Err(format!(
                "{}: analysis of rule class '{other}' is not supported (no toolchain-backed implementation)",
                rule.label
            ));
        }
    }
    let mut groups = BTreeMap::new();
    groups.insert("default".to_string(), out_artifacts.clone());

    let mut ct = ConfiguredTarget::new(rule.label.clone(), config.clone(), rule.rule_class.clone());
    ct.default_info = DefaultInfo {
        files: out_artifacts,
        data_runfiles: src_artifacts,
        default_runfiles: Vec::new(),
        executable,
    };
    ct.output_groups = OutputGroupInfo { groups };
    ct.actions = actions;
    ct.is_test = rule.is_test_rule();
    ct.testonly = rule.testonly;
    Ok(ct)
}
