//! Bazel's `ActionKeyComputer.getKey` (aquery's `ActionKey:`), computed with the exact
//! `com.google.devtools.build.lib.util.Fingerprint` framing (protobuf `CodedOutputStream`:
//! varint-length-prefixed strings, varint ints, one-byte booleans; SHA-256).
//!
//! Verified against the real Bazel 9.3.2 server jar (`scripts/cq_harness/actionkey`): the key
//! of a `FileWriteAction`, a `SpawnAction` (Starlark `run_shell`) and a `GenRuleAction` is
//! reproduced bit-for-bit.
//!
//! Layout of Bazel 9.3.2 (the spec): `PlatformInfo.addTo` fingerprints `remoteExecutionProperties`,
//! `PlatformRule.DEFAULT_MISSING_TOOLCHAIN_ERROR` ends with a period, and `ActionEnvironment.addTo`
//! fingerprints only the fixed map and the inherited names (no unset names).

use zel_analysis::{Action, ActionKind};

const FILE_WRITE_GUID: &str = "332877c7-ca9f-4731-b387-54f620408522";
const SPAWN_GUID: &str = "ebd6fce3-093e-45ee-adb6-bf513b602f0d";
const SYMLINK_GUID: &str = "7f4fab4d-d0a7-4f0f-8649-1d0337a21fee";
const UNRESOLVED_SYMLINK_GUID: &str = "0f302651-602c-404b-881c-58913193cfe7";
const TEMPLATE_GUID: &str = "786c1fe0-dca8-407a-b108-e1ecd6d1bc7f";
const PARAM_FILE_UUID: &str = "106c1389-88d7-4cc1-8f05-f8a61fd8f7b1";
/// `PlatformRule.DEFAULT_MISSING_TOOLCHAIN_ERROR` (9.3.2, with the trailing period).
const MISSING_TOOLCHAIN_ERROR: &str =
    "For more information on platforms or toolchains see https://bazel.build/concepts/platforms-intro.";

/// `util.Fingerprint`.
#[derive(Default)]
pub struct JFp {
    b: Vec<u8>,
}

impl JFp {
    pub fn new() -> Self {
        Self::default()
    }

    fn varint(&mut self, mut n: u64) {
        loop {
            if n < 0x80 {
                self.b.push(n as u8);
                return;
            }
            self.b.push((n & 0x7f) as u8 | 0x80);
            n >>= 7;
        }
    }

    pub fn add_int(&mut self, v: i32) -> &mut Self {
        // `writeInt32NoTag`: negative values are sign-extended to ten bytes.
        self.varint(v as i64 as u64);
        self
    }

    pub fn add_long(&mut self, v: u64) -> &mut Self {
        // `writeInt64NoTag`
        self.varint(v);
        self
    }

    pub fn add_string(&mut self, s: &str) -> &mut Self {
        self.varint(s.len() as u64);
        self.b.extend_from_slice(s.as_bytes());
        self
    }

    pub fn add_bool(&mut self, v: bool) -> &mut Self {
        self.b.push(u8::from(v));
        self
    }

    pub fn add_nullable_string(&mut self, s: Option<&str>) -> &mut Self {
        match s {
            None => self.add_bool(false),
            Some(s) => self.add_bool(true).add_string(s),
        }
    }

    pub fn add_strings<S: AsRef<str>>(&mut self, v: &[S]) -> &mut Self {
        self.add_int(v.len() as i32);
        for s in v {
            self.add_string(s.as_ref());
        }
        self
    }

    pub fn add_string_map<'a, I: ExactSizeIterator<Item = (&'a String, &'a String)>>(&mut self, m: I) -> &mut Self {
        self.add_int(m.len() as i32);
        for (k, v) in m {
            self.add_string(k).add_string(v);
        }
        self
    }

    /// `addUUID(UUID.fromString(s))`: least significant bits first, then the most significant.
    pub fn add_uuid(&mut self, uuid: &str) -> &mut Self {
        let hex: String = uuid.chars().filter(|c| *c != '-').collect();
        let v = u128::from_str_radix(&hex, 16).unwrap_or(0);
        self.add_long(v as u64).add_long((v >> 64) as u64)
    }

    pub fn add_raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.b.extend_from_slice(bytes);
        self
    }

    pub fn digest(&self) -> [u8; 32] {
        zel_util::sha256::digest(&self.b)
    }

    pub fn hex_digest(&self) -> String {
        zel_util::to_hex(&zel_util::sha256::digest(&self.b))
    }
}

const EMPTY_SET_DIGEST: i32 = 104_395_303;
const REPO_MAPPING_GUID: &str = "458e351c-4d30-433d-b927-da6cddd4737f";

/// `NestedSetFingerprintCache.addNestedSetToFingerprint` with the element mapper `f`.
fn add_nested_set<E: Clone + PartialEq>(fp: &mut JFp, set: &zel_analysis::nested_set::NSet<E>, f: &dyn Fn(&E, &mut JFp)) {
    use zel_analysis::nested_set::NNode;
    fn node<E>(fp: &mut JFp, n: &NNode<E>, f: &dyn Fn(&E, &mut JFp)) {
        match n {
            NNode::Leaf(e) => f(e, fp),
            NNode::Arr(children) => {
                let mut sub = JFp::new();
                for c in children.iter() {
                    node(&mut sub, c, f);
                }
                let d = sub.digest();
                fp.add_raw(&d);
            }
        }
    }
    match &set.children {
        None => {
            fp.add_int(EMPTY_SET_DIGEST);
        }
        Some(c) => {
            fp.add_int(set.order);
            node(fp, c, f);
        }
    }
}

/// `TestRunnerAction.computeKey`.
fn add_test_runner_key(fp: &mut JFp, t: &zel_analysis::TestRunnerKey, exec_info: &std::collections::BTreeMap<String, String>) {
    fp.add_string("cc41f9d0-47a6-11e7-8726-eb6ce83a8cc8");
    for a in &t.args {
        fp.add_bool(true).add_string(a);
    }
    fp.add_bool(false);
    fp.add_string(&t.test_filter).add_bool(t.fail_fast).add_string(&t.run_under);
    fp.add_string_map(t.coverage_env.iter());
    for e in [&t.extra_env, &t.action_env, &t.test_env] {
        fp.add_string_map(e.0.iter());
        fp.add_strings(&e.1);
    }
    fp.add_string(&t.size).add_string(&t.timeout_category).add_string(&t.timeout);
    fp.add_strings(&t.tags);
    fp.add_bool(t.remotable);
    fp.add_int(t.shard_num).add_int(t.total_shards).add_int(t.run_number).add_int(t.total_runs);
    fp.add_bool(t.coverage).add_bool(t.zip_undeclared);
    fp.add_string_map(exec_info.iter());
    fp.add_nullable_string(None);
}

/// `RepoMappingManifestAction.computeKey`.
fn add_repo_mapping_key(fp: &mut JFp, info: &zel_analysis::ActionKeyInfo) {
    let zel_analysis::ActionKeyInfo::RepoMapping { packages, runfiles_owner_repos, has_runfiles_symlinks, root_symlink_segments, workspace_name, compact } = info else { return };
    fp.add_uuid(REPO_MAPPING_GUID);
    add_nested_set(fp, packages, &|p, fp| {
        // `repoMappingFingerprintCache`: size, then (apparent name, canonical name) pairs.
        let mut m = JFp::new();
        m.add_int(p.mapping.len() as i32);
        for (a, c) in &p.mapping {
            m.add_string(a).add_string(c);
        }
        fp.add_string(&p.repo).add_string(&m.hex_digest());
    });
    add_nested_set(fp, runfiles_owner_repos, &|e, fp| {
        fp.add_string(&e.1);
    });
    fp.add_bool(*has_runfiles_symlinks);
    add_nested_set(fp, root_symlink_segments, &|e, fp| {
        fp.add_string(&e.1);
    });
    fp.add_string(workspace_name);
    fp.add_bool(*compact);
}

/// The host platform (`@@platforms//host:host`) as `PlatformInfo.addTo` fingerprints it.
#[allow(dead_code)]
fn add_host_platform(fp: &mut JFp, label: &str) {
    let os = match std::env::consts::OS {
        "macos" => "osx",
        other => other,
    };
    add_platform(fp, label, std::env::consts::ARCH, os);
}

/// `PlatformInfo.addTo` of a `platform` with the `cpu`/`os` constraint values.
fn add_platform(fp: &mut JFp, label: &str, cpu: &str, os: &str) {
    fp.add_bool(true);
    fp.add_string(label);
    fp.add_bool(false); // no parent constraint collection
    fp.add_int(2);
    for (setting, value) in [
        ("@@platforms//cpu:cpu".to_string(), format!("@@platforms//cpu:{cpu}")),
        ("@@platforms//os:os".to_string(), format!("@@platforms//os:{os}")),
    ] {
        fp.add_string(&setting).add_string(&value);
    }
    fp.add_nullable_string(Some("")); // remoteExecutionProperties
    fp.add_int(0); // execProperties
    fp.add_int(0); // flags
    fp.add_int(0); // requiredSettings
    fp.add_int(0); // allowedToolchainTypes
    fp.add_bool(false); // checkToolchainTypes
    fp.add_nullable_string(Some(MISSING_TOOLCHAIN_ERROR));
}

/// `ActionKeyComputer.getKey` for `a`, executed on `exec_platform` (label).
pub fn action_key(a: &Action, exec_platform: Option<&str>) -> String {
    let os = match std::env::consts::OS {
        "macos" => "osx",
        other => other,
    };
    action_key_for(a, exec_platform.map(|l| (l, std::env::consts::ARCH, os)))
}

/// `action_key` with an explicit `(label, cpu, os)` execution platform.
pub fn action_key_for(a: &Action, exec_platform: Option<(&str, &str, &str)>) -> String {
    let mut fp = JFp::new();
    // `RunfilesTreeAction.computeKey` contributes nothing (only the inputs matter).
    let runfiles_tree = a.mnemonic == "RunfilesTree";
    match &a.kind {
        _ if runfiles_tree => {}
        ActionKind::FileWrite { .. } if matches!(a.key_info.as_deref(), Some(zel_analysis::ActionKeyInfo::RepoMapping { .. })) => {
            add_repo_mapping_key(&mut fp, a.key_info.as_ref().unwrap());
        }
        ActionKind::Spawn { .. } if matches!(a.key_info.as_deref(), Some(zel_analysis::ActionKeyInfo::TestRunner(_))) => {
            if let Some(zel_analysis::ActionKeyInfo::TestRunner(t)) = a.key_info.as_deref() {
                add_test_runner_key(&mut fp, t, &a.execution_requirements);
            }
        }
        ActionKind::FileWrite { content, is_executable } => {
            let bytes = content.render();
            fp.add_string(FILE_WRITE_GUID)
                .add_bool(*is_executable)
                .add_string(&String::from_utf8_lossy(&bytes));
        }
        ActionKind::Spawn { .. } => {
            fp.add_string(SPAWN_GUID);
            if let ActionKind::Spawn { executable, arguments, .. } = &a.kind {
                // `CommandLines`: the executable line, then every `Args` line followed by its
                // `ParamFileInfo` (plain string arguments only; `Args` objects with
                // `format_each`/`map_each` have their own fingerprint in Bazel).
                fp.add_string(executable);
                for args in arguments {
                    for s in &args.args {
                        fp.add_string(s);
                    }
                    if let Some(p) = &args.param_file {
                        fp.add_uuid(PARAM_FILE_UUID);
                        fp.add_string(&p.flag_format);
                        fp.add_string(match p.format {
                            zel_analysis::ParamFileFormat::Shell => "SHELL_QUOTED",
                            _ => "UNQUOTED",
                        });
                    }
                }
            }
            fp.add_string(&a.mnemonic);
            // `ActionEnvironment.addTo` (9.3.2): fixed map + inherited names.
            match &a.action_env {
                Some(e) => {
                    fp.add_string_map(e.fixed_env().iter());
                    let inh: Vec<&String> = e.inherited_env().iter().collect();
                    fp.add_strings(&inh);
                }
                None => {
                    fp.add_string_map(a.env.iter());
                    fp.add_int(0);
                }
            }
            fp.add_string_map(a.execution_requirements.iter());
        }
        ActionKind::Symlink { target_file, target_path, .. } => match (target_file, target_path) {
            // `SymlinkAction.computeKey`: the GUID, plus the input path only when it differs from the
            // input artifact (`use_exec_root_for_source`, not modelled: always null here).
            (Some(_), _) | (None, None) => {
                fp.add_string(SYMLINK_GUID);
            }
            // `UnresolvedSymlinkAction.computeKey` (the explicit `target_type` is not recorded).
            (None, Some(p)) => {
                fp.add_string(UNRESOLVED_SYMLINK_GUID).add_string(p).add_string("UNSPECIFIED");
            }
        },
        ActionKind::TemplateExpand { template, substitutions, is_executable, computed_substitutions } => {
            fp.add_string(TEMPLATE_GUID);
            fp.add_string(if *is_executable { "true" } else { "false" });
            fp.add_string(&format!("ARTIFACT: {}", template.exec_path.path_string()));
            fp.add_int((substitutions.len() + computed_substitutions.len()) as i32);
            for (k, v) in substitutions {
                fp.add_string(k).add_string(v);
            }
            for (k, v) in computed_substitutions {
                fp.add_string(k).add_string(v);
            }
        }
    }
    // `SymlinkAction`, `SolibSymlinkAction` and `RunfilesTreeAction` are platform agnostic: they
    // override `getExecutionPlatform` with `PlatformInfo.EMPTY_PLATFORM_INFO`.
    let agnostic = matches!(a.mnemonic.as_str(), "Symlink" | "ExecutableSymlink" | "SolibSymlink" | "RunfilesTree");
    match exec_platform {
        _ if agnostic => {
            fp.add_bool(true);
            fp.add_string("@@bazel_tools//tools:internal_platform");
            fp.add_bool(false); // no parent constraint collection
            fp.add_int(0);
            fp.add_nullable_string(Some("")); // remoteExecutionProperties
            fp.add_int(0).add_int(0).add_int(0).add_int(0);
            fp.add_bool(false);
            fp.add_nullable_string(None);
        }
        Some((label, cpu, os)) => add_platform(&mut fp, label, cpu, os),
        None => {
            fp.add_bool(false);
        }
    }
    fp.add_int(0); // getExecProperties() (empty map)
    fp.add_int(0); // ACTION_KEY_UNIQUIFIER
    fp.hex_digest()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use zel_analysis::{CommandLineArgs, FileContent};
    use zel_cmdline::Label;

    fn key_on_mac(a: &Action) -> String {
        // `action_key` with the aarch64/osx host platform fixed (the tests must not depend on the machine).
        action_key_for(a, Some(("@@platforms//host:host", "aarch64", "osx")))
    }

    fn owner() -> Label {
        Label::parse_canonical("@@//:x").unwrap()
    }

    #[test]
    fn filewrite_keys_match_bazel_9_3_2() {
        // aquery of `ctx.actions.write(...)` in Bazel 9.3.2 (see scripts/cq_harness/actionkey).
        let a = Action::new(
            owner(),
            "FileWrite",
            ActionKind::FileWrite { content: FileContent::Literal("#!/bin/sh\ntrue\n".into()), is_executable: true },
        );
        assert_eq!(key_on_mac(&a), "c60c798cc3bc62836511bbeff4652c740baf83705716113dbf218e878fd7a46a");
        let b = Action::new(
            owner(),
            "FileWrite",
            ActionKind::FileWrite { content: FileContent::Literal("hello\n".into()), is_executable: false },
        );
        assert_eq!(key_on_mac(&b), "c2deb0bea159fada2a4c523f4e462702506abd451f590e52880764e05e701f92");
    }

    fn spawn(mnemonic: &str, args: Vec<&str>, env: &[(&str, &str)]) -> Action {
        let mut a = Action::new(
            owner(),
            mnemonic,
            ActionKind::Spawn {
                executable: "/bin/bash".into(),
                arguments: vec![CommandLineArgs { args: args.into_iter().map(String::from).collect(), param_file: None }],
                shell_command: None,
            },
        );
        a.env = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>();
        a
    }

    #[test]
    fn spawn_action_keys_match_bazel_9_3_2() {
        // `ctx.actions.run_shell(command = "echo hi > <out>", mnemonic = "Foo")`.
        let a = spawn("Foo", vec!["-c", "echo hi > bazel-out/darwin_arm64-fastbuild/bin/sh.txt"], &[]);
        assert_eq!(key_on_mac(&a), "7cae808420194240877b674491e7ee0b5c299911923f48b307818bbc720e9b49");
        // `genrule(cmd = "echo hi > $@")`: 9.3.2's strict action environment.
        let g = spawn(
            "Genrule",
            vec!["-c", "source external/bazel_tools/tools/genrule/genrule-setup.sh; echo hi > bazel-out/darwin_arm64-fastbuild/bin/o.txt"],
            &[("PATH", "/bin:/usr/bin:/usr/local/bin")],
        );
        assert_eq!(key_on_mac(&g), "63704e266eda4547e7e31cbba63adc3d112c38d0cfa7973022081fcc0392d709");
    }

    #[test]
    fn fingerprint_framing() {
        // `Fingerprint.addUUID`: `writeInt64NoTag` of the least, then the most significant bits.
        let mut fp = JFp::new();
        fp.add_uuid("106c1389-88d7-4cc1-8f05-f8a61fd8f7b1");
        assert_eq!(fp.b.len(), 19);
        assert_eq!(fp.hex_digest(), "23e1b636b669bb504ba6a2464c1089c10a515aaba302ce27fd1cb7c7d1f3b724");
        let mut fp = JFp::new();
        fp.add_int(-1);
        assert_eq!(fp.b.len(), 10); // negative int32: sign-extended ten-byte varint
    }
}
