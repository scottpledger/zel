//! Reduced-classpath execution of `JavaCompileAction` (`--experimental_java_classpath=bazel`),
//! ported from `JavaCompileAction.java` / `JavaCompileActionContext.java`.
//!
//! The first attempt passes only the *direct* jars plus the jars named in the `.jdeps` files of
//! the direct dependencies (`getReducedClasspath`) together with
//! `--reduce_classpath_mode BAZEL_REDUCED`; JavaBuilder then reports a compilation error that may
//! be caused by the missing classpath entries by writing `requires_reduced_classpath_fallback`
//! into the `.jdeps` output (without producing the jar). The action is then re-run with the full
//! classpath and `BAZEL_FALLBACK`.

use std::collections::HashSet;
use std::io;
use zel_analysis::action::{render_param_file, ParamFileFormat};
use zel_analysis::JavaCompileSpec;

/// The parts of `deps.proto`'s `Dependencies` message that the executor needs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Jdeps {
    /// `dependency[].path`
    pub paths: Vec<String>,
    /// `requires_reduced_classpath_fallback` (field 5)
    pub requires_reduced_classpath_fallback: bool,
}

fn varint(b: &[u8], i: &mut usize) -> Result<u64, String> {
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

/// Walks the fields of a message, calling `f(field_number, wire_type, value)`; `value` is the
/// varint for wire type 0 and the payload slice for wire type 2 (empty otherwise).
fn fields<'a>(b: &'a [u8], mut f: impl FnMut(u64, u64, u64, &'a [u8]) -> Result<(), String>) -> Result<(), String> {
    let mut i = 0;
    while i < b.len() {
        let tag = varint(b, &mut i)?;
        let (field, wire) = (tag >> 3, tag & 7);
        match wire {
            0 => {
                let v = varint(b, &mut i)?;
                f(field, wire, v, &[])?;
            }
            1 => i = i.checked_add(8).filter(|e| *e <= b.len()).ok_or("truncated fixed64")?,
            2 => {
                let len = varint(b, &mut i)? as usize;
                let end = i.checked_add(len).filter(|e| *e <= b.len()).ok_or("truncated field")?;
                f(field, wire, 0, &b[i..end])?;
                i = end;
            }
            5 => i = i.checked_add(4).filter(|e| *e <= b.len()).ok_or("truncated fixed32")?,
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
                fields(payload, |f, w, _, p| {
                    if f == 1 && w == 2 {
                        path = Some(String::from_utf8_lossy(p).into_owned());
                    }
                    Ok(())
                })?;
                out.paths.push(path.ok_or("Dependency without path")?);
            }
            (5, 0) => out.requires_reduced_classpath_fallback = v != 0,
            _ => {}
        }
        Ok(())
    })?;
    Ok(out)
}

/// `JavaCompileAction.getReducedClasspath`: the entries of the full classpath that are direct
/// jars or are named by the `.jdeps` of a direct dependency, in classpath order.
pub fn reduced_classpath(spec: &JavaCompileSpec, read: &dyn Fn(&str) -> io::Result<Vec<u8>>) -> io::Result<Vec<String>> {
    let mut direct: HashSet<String> = spec.direct_jars.iter().cloned().collect();
    for dep in &spec.dependency_artifacts {
        let bytes = read(dep)?;
        let jdeps = parse_jdeps(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{dep}: {e}")))?;
        direct.extend(jdeps.paths);
    }
    Ok(spec.transitive_inputs.iter().filter(|p| direct.contains(*p)).cloned().collect())
}

/// Content of the classpath param file for one attempt (`getReducedSpawn`).
pub fn classpath_param_file(spec: &JavaCompileSpec, classpath: &[String], fallback: bool) -> String {
    let mut args: Vec<String> = Vec::new();
    if !classpath.is_empty() {
        args.push("--classpath".into());
        args.extend(classpath.iter().cloned());
    }
    args.push("--reduce_classpath_mode".into());
    args.push(
        if spec.classpath_mode == "BAZEL_NO_FALLBACK" {
            "NONE"
        } else if fallback {
            "BAZEL_FALLBACK"
        } else {
            "BAZEL_REDUCED"
        }
        .into(),
    );
    render_param_file(&args, ParamFileFormat::Multiline)
}

/// The inputs of one attempt: `mandatoryInputs` plus `classpath` (the full classpath for the
/// fallback), i.e. everything except the unused classpath entries and the dependency `.jdeps`
/// files, plus the classpath param file.
pub fn attempt_inputs(spec: &JavaCompileSpec, all_inputs: &[String], classpath: &[String]) -> Vec<String> {
    let trans: HashSet<&str> = spec.transitive_inputs.iter().map(String::as_str).collect();
    let deps: HashSet<&str> = spec.dependency_artifacts.iter().map(String::as_str).collect();
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
    fn parses_jdeps_paths_and_fallback_flag() {
        let mut b = dep("bazel-out/k8-fastbuild/bin/a/liba-hjar.jar", 0);
        b.extend(dep("b/libb.jar", 1));
        b.extend([0x12, 3, b'/', b'/', b'x']); // rule_label (ignored)
        b.extend([0x18, 1]); // success
        b.extend([0x28, 1]); // requires_reduced_classpath_fallback
        let j = parse_jdeps(&b).unwrap();
        assert_eq!(j.paths, vec!["bazel-out/k8-fastbuild/bin/a/liba-hjar.jar", "b/libb.jar"]);
        assert!(j.requires_reduced_classpath_fallback);
        assert!(!parse_jdeps(&dep("x.jar", 0)).unwrap().requires_reduced_classpath_fallback);
        assert!(parse_jdeps(&[0x0a, 0x7f]).is_err());
    }

    fn spec() -> JavaCompileSpec {
        JavaCompileSpec {
            classpath_mode: "BAZEL".into(),
            classpath_arg_index: 2,
            classpath_params_path: "out/x.jar-2.params".into(),
            transitive_inputs: vec!["a.jar".into(), "b.jar".into(), "c.jar".into(), "d.jar".into()],
            direct_jars: vec!["a.jar".into()],
            dependency_artifacts: vec!["a.jdeps".into()],
            mandatory_inputs: vec!["A.java".into(), "boot.jar".into()],
            deps_proto: "out/x.jdeps".into(),
        }
    }

    #[test]
    fn reduces_to_direct_jars_and_jdeps_of_direct_deps() {
        let jd = dep("c.jar", 0);
        let read = move |p: &str| -> io::Result<Vec<u8>> {
            assert_eq!(p, "a.jdeps");
            Ok(jd.clone())
        };
        let cp = reduced_classpath(&spec(), &read).unwrap();
        assert_eq!(cp, vec!["a.jar", "c.jar"]);
        let missing = |_: &str| -> io::Result<Vec<u8>> { Err(io::Error::new(io::ErrorKind::NotFound, "gone")) };
        assert!(reduced_classpath(&spec(), &missing).is_err());
    }

    #[test]
    fn param_file_and_inputs_per_attempt() {
        let s = spec();
        let reduced = vec!["a.jar".to_string(), "c.jar".to_string()];
        assert_eq!(classpath_param_file(&s, &reduced, false), "--classpath\na.jar\nc.jar\n--reduce_classpath_mode\nBAZEL_REDUCED\n");
        assert_eq!(classpath_param_file(&s, &s.transitive_inputs, true), "--classpath\na.jar\nb.jar\nc.jar\nd.jar\n--reduce_classpath_mode\nBAZEL_FALLBACK\n");
        assert_eq!(classpath_param_file(&s, &[], false), "--reduce_classpath_mode\nBAZEL_REDUCED\n");
        let mut nf = s.clone();
        nf.classpath_mode = "BAZEL_NO_FALLBACK".into();
        assert!(classpath_param_file(&nf, &reduced, false).ends_with("NONE\n"));
        let all: Vec<String> = ["A.java", "boot.jar", "tool.jar", "a.jar", "b.jar", "c.jar", "d.jar", "a.jdeps"].iter().map(|s| s.to_string()).collect();
        assert_eq!(attempt_inputs(&s, &all, &reduced), vec!["A.java", "boot.jar", "tool.jar", "a.jar", "c.jar", "out/x.jar-2.params"]);
        assert_eq!(attempt_inputs(&s, &all, &s.transitive_inputs).len(), 8); // all but a.jdeps, plus params
    }
}
