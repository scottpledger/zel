//! Reduced-classpath execution of `JavaCompileAction` (`--experimental_java_classpath=bazel`),
//! ported from `JavaCompileAction.java` / `JavaCompileActionContext.java`, plus full
//! `blaze_deps.Dependencies` (`.jdeps`) protobuf decoding and encoding.

use std::collections::HashSet;
use std::io;
use zel_analysis::action::{render_param_file, ParamFileFormat};
use zel_analysis::JavaCompileSpec;

/// `blaze_deps.Dependency.Kind` (`deps.proto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DependencyKind {
    #[default]
    Explicit = 0,
    Implicit = 1,
    Unused = 2,
    Incomplete = 3,
}

impl DependencyKind {
    pub fn from_wire(v: u64) -> Result<Self, String> {
        match v {
            0 => Ok(Self::Explicit),
            1 => Ok(Self::Implicit),
            2 => Ok(Self::Unused),
            3 => Ok(Self::Incomplete),
            other => Err(format!("unknown Dependency.Kind {other}")),
        }
    }

    /// Returns `true` for kinds retained in reduced classpaths (`EXPLICIT`, `IMPLICIT`,
    /// `INCOMPLETE`) and `false` for `UNUSED`.
    pub const fn is_used(self) -> bool {
        !matches!(self, Self::Unused)
    }
}

/// `blaze_deps.SourceLocation` (`deps.proto`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceLocation {
    pub path: String,
    pub line: Option<i32>,
    pub column: Option<i32>,
}

/// `blaze_deps.Dependency` (`deps.proto`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub path: String,
    pub kind: DependencyKind,
    pub locations: Vec<SourceLocation>,
}

/// Parsed `blaze_deps.Dependencies` message (`.jdeps`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Jdeps {
    /// Full `dependency` list in wire order, including `UNUSED` entries.
    pub dependencies: Vec<Dependency>,
    /// Convenience list of `dependency[].path` restricted to used kinds
    /// (`EXPLICIT`, `IMPLICIT`, `INCOMPLETE`).
    pub paths: Vec<String>,
    /// `rule_label` (field 2).
    pub rule_label: Option<String>,
    /// `success` (field 3).
    pub success: Option<bool>,
    /// `contained_package` (field 4).
    pub contained_packages: Vec<String>,
    /// `requires_reduced_classpath_fallback` (field 5).
    pub requires_reduced_classpath_fallback: bool,
}

pub(crate) fn varint(b: &[u8], i: &mut usize) -> Result<u64, String> {
    let mut v: u64 = 0;
    let mut shift = 0;
    loop {
        let byte = *b.get(*i).ok_or("truncated varint")?;
        *i += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
        if shift > 63 {
            return Err("varint too long".into());
        }
    }
}

pub(crate) fn push_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(((v as u8) & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub(crate) fn push_bytes_field(out: &mut Vec<u8>, field: u32, payload: &[u8]) {
    push_varint(out, u64::from((field << 3) | 2));
    push_varint(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

pub(crate) fn push_varint_field(out: &mut Vec<u8>, field: u32, val: u64) {
    push_varint(out, u64::from(field << 3));
    push_varint(out, val);
}

/// Walks the fields of a message, calling `f(field_number, wire_type, value, payload)`.
pub(crate) fn fields<'a>(
    b: &'a [u8],
    mut f: impl FnMut(u64, u64, u64, &'a [u8]) -> Result<(), String>,
) -> Result<(), String> {
    let mut i = 0;
    while i < b.len() {
        let tag = varint(b, &mut i)?;
        let (field, wire) = (tag >> 3, tag & 7);
        match wire {
            0 => {
                let v = varint(b, &mut i)?;
                f(field, wire, v, &[])?;
            }
            1 => {
                i = i
                    .checked_add(8)
                    .filter(|e| *e <= b.len())
                    .ok_or("truncated fixed64")?
            }
            2 => {
                let len = varint(b, &mut i)? as usize;
                let end = i
                    .checked_add(len)
                    .filter(|e| *e <= b.len())
                    .ok_or("truncated field")?;
                f(field, wire, 0, &b[i..end])?;
                i = end;
            }
            5 => {
                i = i
                    .checked_add(4)
                    .filter(|e| *e <= b.len())
                    .ok_or("truncated fixed32")?
            }
            w => return Err(format!("unsupported wire type {w}")),
        }
    }
    Ok(())
}

/// Parses a `.jdeps` file (`Deps.Dependencies.parseFrom`).
pub fn parse_jdeps(bytes: &[u8]) -> Result<Jdeps, String> {
    let mut out = Jdeps::default();
    fields(bytes, |field, wire, v, payload| {
        match (field, wire) {
            (1, 2) => {
                let mut path = None;
                let mut kind = DependencyKind::Explicit;
                let mut locations = Vec::new();
                fields(payload, |f, w, val, p| {
                    match (f, w) {
                        (1, 2) => path = Some(String::from_utf8_lossy(p).into_owned()),
                        (2, 0) => kind = DependencyKind::from_wire(val)?,
                        (3, 2) => {
                            let mut loc = SourceLocation::default();
                            let mut has_path = false;
                            fields(p, |lf, lw, lv, lp| {
                                match (lf, lw) {
                                    (1, 2) => {
                                        loc.path = String::from_utf8_lossy(lp).into_owned();
                                        has_path = true;
                                    }
                                    (2, 0) => loc.line = Some(lv as i64 as i32),
                                    (3, 0) => loc.column = Some(lv as i64 as i32),
                                    _ => {}
                                }
                                Ok(())
                            })?;
                            if has_path {
                                locations.push(loc);
                            }
                        }
                        _ => {}
                    }
                    Ok(())
                })?;
                let path = path.ok_or("Dependency without path")?;
                if kind.is_used() {
                    out.paths.push(path.clone());
                }
                out.dependencies.push(Dependency {
                    path,
                    kind,
                    locations,
                });
            }
            (2, 2) => out.rule_label = Some(String::from_utf8_lossy(payload).into_owned()),
            (3, 0) => out.success = Some(v != 0),
            (4, 2) => out
                .contained_packages
                .push(String::from_utf8_lossy(payload).into_owned()),
            (5, 0) => out.requires_reduced_classpath_fallback = v != 0,
            _ => {}
        }
        Ok(())
    })?;
    Ok(out)
}

/// Encodes a [`Jdeps`] message to `blaze_deps.Dependencies` protobuf wire bytes.
pub fn encode_jdeps(jdeps: &Jdeps) -> Vec<u8> {
    let mut out = Vec::new();
    for dep in &jdeps.dependencies {
        let mut d = Vec::new();
        push_bytes_field(&mut d, 1, dep.path.as_bytes());
        push_varint_field(&mut d, 2, dep.kind as u64);
        for loc in &dep.locations {
            let mut l = Vec::new();
            push_bytes_field(&mut l, 1, loc.path.as_bytes());
            if let Some(line) = loc.line {
                push_varint_field(&mut l, 2, line as i64 as u64);
            }
            if let Some(col) = loc.column {
                push_varint_field(&mut l, 3, col as i64 as u64);
            }
            push_bytes_field(&mut d, 3, &l);
        }
        push_bytes_field(&mut out, 1, &d);
    }
    if let Some(label) = &jdeps.rule_label {
        push_bytes_field(&mut out, 2, label.as_bytes());
    }
    if let Some(success) = jdeps.success {
        push_varint_field(&mut out, 3, u64::from(success));
    }
    for pkg in &jdeps.contained_packages {
        push_bytes_field(&mut out, 4, pkg.as_bytes());
    }
    if jdeps.requires_reduced_classpath_fallback {
        push_varint_field(&mut out, 5, 1);
    }
    out
}

/// Normalizes a path string for `java.nio.file.Path`-equivalent equality checks (`\` -> `/`,
/// collapsing duplicate `/` and `.` segments, and removing trailing `/` on non-root paths).
pub fn normalize_jdeps_path(p: &str) -> String {
    let unified = p.replace('\\', "/");
    let leading_slash = unified.starts_with('/');
    let mut segs: Vec<&str> = Vec::new();
    for seg in unified.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            if let Some(&last) = segs.last() {
                if last != ".." {
                    segs.pop();
                    continue;
                }
            }
            if !leading_slash {
                segs.push("..");
            }
        } else {
            segs.push(seg);
        }
    }
    if segs.is_empty() {
        if leading_slash {
            "/".to_string()
        } else {
            String::new()
        }
    } else if leading_slash {
        format!("/{}", segs.join("/"))
    } else {
        segs.join("/")
    }
}

/// `JavaCompileAction.getReducedClasspath`: the entries of the full classpath that are direct
/// jars or are named (with a non-`UNUSED` kind) by the `.jdeps` of a direct dependency, in
/// classpath order.
pub fn reduced_classpath(
    spec: &JavaCompileSpec,
    read: &dyn Fn(&str) -> io::Result<Vec<u8>>,
) -> io::Result<Vec<String>> {
    let mut direct: HashSet<String> = spec
        .direct_jars
        .iter()
        .map(|p| normalize_jdeps_path(p))
        .collect();
    for dep in &spec.dependency_artifacts {
        let bytes = read(dep)?;
        let jdeps = parse_jdeps(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{dep}: {e}")))?;
        for p in &jdeps.paths {
            direct.insert(normalize_jdeps_path(p));
        }
    }
    Ok(spec
        .transitive_inputs
        .iter()
        .filter(|p| direct.contains(&normalize_jdeps_path(p)))
        .cloned()
        .collect())
}

/// Content of the classpath param file for one attempt (`getReducedSpawn`).
pub fn classpath_param_file(
    spec: &JavaCompileSpec,
    classpath: &[String],
    fallback: bool,
) -> String {
    let mut args: Vec<String> = Vec::new();
    if !classpath.is_empty() {
        args.push("--classpath".into());
        args.extend(classpath.iter().cloned());
    }
    let mode = if fallback {
        "BAZEL_FALLBACK"
    } else if spec.classpath_mode == "BAZEL_NO_FALLBACK" {
        "NONE"
    } else {
        "BAZEL_REDUCED"
    };
    args.push("--reduce_classpath_mode".into());
    args.push(mode.into());
    render_param_file(&args, ParamFileFormat::Multiline)
}

/// The inputs of one attempt: `mandatoryInputs` plus `classpath` (the full classpath for the
/// fallback), i.e. everything except the unused classpath entries and the dependency `.jdeps`
/// files, plus the classpath param file.
pub fn attempt_inputs(
    spec: &JavaCompileSpec,
    all_inputs: &[String],
    classpath: &[String],
) -> Vec<String> {
    let trans: HashSet<&str> = spec.transitive_inputs.iter().map(String::as_str).collect();
    let deps: HashSet<&str> = spec
        .dependency_artifacts
        .iter()
        .map(String::as_str)
        .collect();
    let mandatory: HashSet<&str> = spec.mandatory_inputs.iter().map(String::as_str).collect();
    let cp: HashSet<&str> = classpath.iter().map(String::as_str).collect();
    let mut out: Vec<String> = all_inputs
        .iter()
        .filter(|p| {
            let p = p.as_str();
            mandatory.contains(p) || cp.contains(p) || (!trans.contains(p) && !deps.contains(p))
        })
        .cloned()
        .collect();
    out.push(spec.classpath_params_path.clone());
    out
}

fn strip_cfg_segment(p: &str) -> Option<String> {
    let rest = p.strip_prefix("bazel-out/")?;
    let (_, after_cfg) = rest.split_once('/')?;
    Some(format!("bazel-out/cfg/{after_cfg}"))
}

/// `JavaCompileAction.createFullOutputDeps`: maps stripped `bazel-out/cfg/...` dependency paths
/// in `bytes` back to their original `bazel-out/<mnemonic>/...` paths from `classpath_inputs`
/// (retaining the first seen path on collisions).
pub fn create_full_output_deps(
    bytes: &[u8],
    classpath_inputs: &[String],
) -> Result<Vec<u8>, String> {
    let mut reverse_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for orig in classpath_inputs {
        if let Some(mapped) = strip_cfg_segment(orig) {
            reverse_map.entry(mapped).or_insert_with(|| orig.clone());
        }
    }
    if reverse_map.is_empty() {
        return Ok(bytes.to_vec());
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut changed = false;
    let mut i = 0;
    while i < bytes.len() {
        let tag_start = i;
        let tag = varint(bytes, &mut i)?;
        let (field, wire) = (tag >> 3, tag & 7);
        match (field, wire) {
            (1, 2) => {
                let len = varint(bytes, &mut i)? as usize;
                let end = i
                    .checked_add(len)
                    .filter(|e| *e <= bytes.len())
                    .ok_or("truncated Dependency")?;
                let payload = &bytes[i..end];
                i = end;
                let mut inner_out = Vec::with_capacity(payload.len());
                let mut j = 0;
                while j < payload.len() {
                    let itag_start = j;
                    let itag = varint(payload, &mut j)?;
                    let (ifield, iwire) = (itag >> 3, itag & 7);
                    match (ifield, iwire) {
                        (1, 2) => {
                            let ilen = varint(payload, &mut j)? as usize;
                            let iend = j
                                .checked_add(ilen)
                                .filter(|e| *e <= payload.len())
                                .ok_or("truncated Dependency.path")?;
                            let pstr = String::from_utf8_lossy(&payload[j..iend]);
                            j = iend;
                            if let Some(orig) = reverse_map.get(pstr.as_ref()) {
                                changed = true;
                                push_varint(&mut inner_out, itag);
                                push_varint(&mut inner_out, orig.len() as u64);
                                inner_out.extend_from_slice(orig.as_bytes());
                            } else {
                                inner_out.extend_from_slice(&payload[itag_start..iend]);
                            }
                        }
                        (_, 0) => {
                            let _ = varint(payload, &mut j)?;
                            inner_out.extend_from_slice(&payload[itag_start..j]);
                        }
                        (_, 1) => {
                            let iend = j
                                .checked_add(8)
                                .filter(|e| *e <= payload.len())
                                .ok_or("truncated fixed64")?;
                            inner_out.extend_from_slice(&payload[itag_start..iend]);
                            j = iend;
                        }
                        (_, 2) => {
                            let ilen = varint(payload, &mut j)? as usize;
                            let iend = j
                                .checked_add(ilen)
                                .filter(|e| *e <= payload.len())
                                .ok_or("truncated field")?;
                            inner_out.extend_from_slice(&payload[itag_start..iend]);
                            j = iend;
                        }
                        (_, 5) => {
                            let iend = j
                                .checked_add(4)
                                .filter(|e| *e <= payload.len())
                                .ok_or("truncated fixed32")?;
                            inner_out.extend_from_slice(&payload[itag_start..iend]);
                            j = iend;
                        }
                        (_, w) => return Err(format!("unsupported wire type {w}")),
                    }
                }
                push_varint(&mut out, tag);
                push_varint(&mut out, inner_out.len() as u64);
                out.extend_from_slice(&inner_out);
            }
            (_, 0) => {
                let _ = varint(bytes, &mut i)?;
                out.extend_from_slice(&bytes[tag_start..i]);
            }
            (_, 1) => {
                let end = i
                    .checked_add(8)
                    .filter(|e| *e <= bytes.len())
                    .ok_or("truncated fixed64")?;
                out.extend_from_slice(&bytes[tag_start..end]);
                i = end;
            }
            (_, 2) => {
                let len = varint(bytes, &mut i)? as usize;
                let end = i
                    .checked_add(len)
                    .filter(|e| *e <= bytes.len())
                    .ok_or("truncated field")?;
                out.extend_from_slice(&bytes[tag_start..end]);
                i = end;
            }
            (_, 5) => {
                let end = i
                    .checked_add(4)
                    .filter(|e| *e <= bytes.len())
                    .ok_or("truncated fixed32")?;
                out.extend_from_slice(&bytes[tag_start..end]);
                i = end;
            }
            (_, w) => return Err(format!("unsupported wire type {w}")),
        }
    }
    Ok(if changed { out } else { bytes.to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(path: &str, kind: u8) -> Vec<u8> {
        let mut inner = vec![0x0a, path.len() as u8];
        inner.extend(path.as_bytes());
        inner.extend([0x10, kind]);
        let mut out = vec![0x0a, inner.len() as u8];
        out.extend(inner);
        out
    }

    #[test]
    fn parses_and_encodes_full_jdeps_excluding_unused_from_paths() {
        let original = Jdeps {
            dependencies: vec![
                Dependency {
                    path: "bazel-out/k8-fastbuild/bin/a/liba-hjar.jar".into(),
                    kind: DependencyKind::Explicit,
                    locations: vec![SourceLocation {
                        path: "pkg/A.java".into(),
                        line: Some(12),
                        column: Some(5),
                    }],
                },
                Dependency {
                    path: "b/libb.jar".into(),
                    kind: DependencyKind::Implicit,
                    locations: vec![],
                },
                Dependency {
                    path: "u/unused.jar".into(),
                    kind: DependencyKind::Unused,
                    locations: vec![],
                },
                Dependency {
                    path: "i/incomplete.jar".into(),
                    kind: DependencyKind::Incomplete,
                    locations: vec![],
                },
            ],
            paths: vec![
                "bazel-out/k8-fastbuild/bin/a/liba-hjar.jar".into(),
                "b/libb.jar".into(),
                "i/incomplete.jar".into(),
            ],
            rule_label: Some("//pkg:x".into()),
            success: Some(true),
            contained_packages: vec!["com.example".into()],
            requires_reduced_classpath_fallback: true,
        };
        let encoded = encode_jdeps(&original);
        let decoded = parse_jdeps(&encoded).unwrap();
        assert_eq!(decoded, original);

        assert!(
            !parse_jdeps(&dep("x.jar", 0))
                .unwrap()
                .requires_reduced_classpath_fallback
        );
        assert!(parse_jdeps(&[0x0a, 0x7f]).is_err());
    }

    fn spec() -> JavaCompileSpec {
        JavaCompileSpec {
            classpath_mode: "BAZEL".into(),
            classpath_arg_index: 2,
            classpath_params_path: "out/x.jar-2.params".into(),
            transitive_inputs: vec![
                "a.jar".into(),
                "b.jar".into(),
                "c.jar".into(),
                "d.jar".into(),
            ],
            direct_jars: vec!["a.jar".into()],
            dependency_artifacts: vec!["a.jdeps".into()],
            mandatory_inputs: vec!["A.java".into(), "boot.jar".into()],
            deps_proto: "out/x.jdeps".into(),
        }
    }

    #[test]
    fn reduces_to_direct_jars_and_used_jdeps_with_normalized_paths() {
        let mut jd = dep("./c.jar/", 0); // EXPLICIT with `./` and trailing `/`
        jd.extend(dep("b.jar", 2)); // UNUSED must be omitted!
        jd.extend(dep("pkg//../d.jar", 3)); // INCOMPLETE with `//` and `..`
        let read = move |p: &str| -> io::Result<Vec<u8>> {
            assert_eq!(p, "a.jdeps");
            Ok(jd.clone())
        };
        let cp = reduced_classpath(&spec(), &read).unwrap();
        assert_eq!(cp, vec!["a.jar", "c.jar", "d.jar"]);
        let missing = |_: &str| -> io::Result<Vec<u8>> {
            Err(io::Error::new(io::ErrorKind::NotFound, "gone"))
        };
        assert!(reduced_classpath(&spec(), &missing).is_err());
    }

    #[test]
    fn param_file_and_inputs_per_attempt() {
        let s = spec();
        let reduced = vec!["a.jar".to_string(), "c.jar".to_string()];
        assert_eq!(
            classpath_param_file(&s, &reduced, false),
            "--classpath\na.jar\nc.jar\n--reduce_classpath_mode\nBAZEL_REDUCED\n"
        );
        assert_eq!(
            classpath_param_file(&s, &s.transitive_inputs, true),
            "--classpath\na.jar\nb.jar\nc.jar\nd.jar\n--reduce_classpath_mode\nBAZEL_FALLBACK\n"
        );
        assert_eq!(
            classpath_param_file(&s, &[], false),
            "--reduce_classpath_mode\nBAZEL_REDUCED\n"
        );
        let mut nf = s.clone();
        nf.classpath_mode = "BAZEL_NO_FALLBACK".into();
        assert!(classpath_param_file(&nf, &reduced, false).ends_with("NONE\n"));
        let all: Vec<String> = [
            "A.java", "boot.jar", "tool.jar", "a.jar", "b.jar", "c.jar", "d.jar", "a.jdeps",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            attempt_inputs(&s, &all, &reduced),
            vec![
                "A.java",
                "boot.jar",
                "tool.jar",
                "a.jar",
                "c.jar",
                "out/x.jar-2.params"
            ]
        );
        assert_eq!(attempt_inputs(&s, &all, &s.transitive_inputs).len(), 8);
    }

    #[test]
    fn create_full_output_deps_restores_first_seen_config_paths() {
        let mapped_jdeps = dep("bazel-out/cfg/bin/pkg/libdep-hjar.jar", 0);
        let cp = vec![
            "bazel-out/k8-fastbuild/bin/pkg/libdep-hjar.jar".to_string(),
            "bazel-out/k8-opt/bin/pkg/libdep-hjar.jar".to_string(),
        ];
        let full = create_full_output_deps(&mapped_jdeps, &cp).unwrap();
        let parsed = parse_jdeps(&full).unwrap();
        assert_eq!(
            parsed.paths,
            vec!["bazel-out/k8-fastbuild/bin/pkg/libdep-hjar.jar".to_string()]
        );
    }
}
