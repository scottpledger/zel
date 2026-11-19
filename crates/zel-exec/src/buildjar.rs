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

//! Native Rust port of Bazel's `buildjar` and `genclass` subsystems (`OptionsParser`,
//! `JavaLibraryBuildRequest`, `SimpleJavaLibraryBuilder`, `ReducedClasspathJavaLibraryBuilder`,
//! `VanillaJavaBuilder`, `StrictJavaDeps`, `Manifest` proto, and `GenClass`).

use crate::java_compile::{
    encode_jdeps, fields, normalize_jdeps_path, parse_jdeps, push_bytes_field, push_varint_field,
    Dependency, DependencyKind, Jdeps,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use zel_analysis::javac_options::{JavacOptions, WerrorCustomOption};
use zel_util::jar::{parse_manifest_main_attributes, split_java_lines, JarCreator, MANIFEST_NAME};
use zel_util::singlejar::{ConcatenateStrategy, CustomMergeStrategy};
use zel_util::zip::ZipReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StrictDepsMode {
    #[default]
    Off,
    Warn,
    Error,
}

impl StrictDepsMode {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.to_ascii_uppercase().as_str() {
            "OFF" => Ok(Self::Off),
            "WARN" => Ok(Self::Warn),
            "ERROR" | "DEFAULT" | "STRICT" => Ok(Self::Error),
            _ => Err(format!("unknown strict_java_deps mode '{raw}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReduceClasspathMode {
    #[default]
    None,
    BazelReduced,
    BazelFallback,
    JavaBuilderReduced,
}

impl ReduceClasspathMode {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "NONE" => Ok(Self::None),
            "BAZEL_REDUCED" => Ok(Self::BazelReduced),
            "BAZEL_FALLBACK" => Ok(Self::BazelFallback),
            "JAVABUILDER_REDUCED" => Ok(Self::JavaBuilderReduced),
            _ => Err(format!("unknown reduce_classpath_mode '{raw}'")),
        }
    }
}

/// Expands `@paramfile` arguments recursively per `OptionsParser.expandArguments`:
/// - `@@foo` unescapes to `@foo` (`substring(1)`) without file lookup;
/// - `@path` reads UTF-8 lines (`\r\n`, `\n`, and lone `\r`), skips only `line.is_empty()`,
///   does not trim or unquote, and recurses on nested `@` / `@@` entries.
pub fn expand_options_args<I, S>(work_dir: &Path, raw_args: I) -> io::Result<Vec<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = Vec::new();
    for arg in raw_args {
        expand_one_arg(work_dir, arg.as_ref(), &mut out, 0)?;
    }
    Ok(out)
}

fn expand_one_arg(
    work_dir: &Path,
    arg: &str,
    out: &mut Vec<String>,
    depth: usize,
) -> io::Result<()> {
    if depth > 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "OptionsParser @-file recursion limit exceeded",
        ));
    }
    if let Some(escaped) = arg.strip_prefix("@@") {
        out.push(format!("@{escaped}"));
        return Ok(());
    }
    if let Some(rel) = arg.strip_prefix('@') {
        let p = resolve_in_work_dir(work_dir, rel);
        let bytes = std::fs::read(&p)?;
        let text = std::str::from_utf8(&bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("malformed UTF-8 in parameter file '{}': {e}", p.display()),
            )
        })?;
        for line in split_java_lines(text) {
            if !line.is_empty() {
                expand_one_arg(work_dir, line, out, depth + 1)?;
            }
        }
        return Ok(());
    }
    out.push(arg.to_string());
    Ok(())
}

fn resolve_in_work_dir(work_dir: &Path, p: &str) -> PathBuf {
    let pb = Path::new(p);
    if pb.is_absolute() || work_dir.as_os_str().is_empty() {
        pb.to_path_buf()
    } else {
        work_dir.join(pb)
    }
}

/// Parsed JavaBuilder command line (`OptionsParser.java`).
#[derive(Debug, Clone, Default)]
pub struct OptionsParser {
    pub javac_opts: Vec<String>,
    pub direct_jars: Vec<String>,
    pub strict_java_deps: StrictDepsMode,
    pub fix_deps_tool: Option<String>,
    pub reduce_classpath_mode: ReduceClasspathMode,
    pub output_deps_proto: Option<String>,
    pub deps_artifacts: Vec<String>,
    pub output_manifest_proto: Option<String>,
    pub source_files: Vec<String>,
    pub source_jars: Vec<String>,
    pub class_path: Vec<String>,
    pub boot_class_path: Vec<String>,
    pub system_module_dir: Option<String>,
    pub source_path: Vec<String>,
    pub ext_class_path: Vec<String>,
    pub processor_path: Vec<String>,
    pub processor_names: Vec<String>,
    pub output_jar: Option<String>,
    pub native_header_output: Option<String>,
    pub class_dir: Option<String>,
    pub temp_dir: Option<String>,
    pub generated_sources_output_jar: Option<String>,
    pub compress_jar: bool,
    pub target_label: Option<String>,
    pub injecting_rule_kind: Option<String>,
    pub post_processors: Vec<(String, Vec<String>)>,
}

impl OptionsParser {
    pub fn parse<I, S>(work_dir: &Path, raw_args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::parse_with_path_separator(work_dir, raw_args, if cfg!(windows) { ';' } else { ':' })
    }

    pub fn parse_with_path_separator<I, S>(
        work_dir: &Path,
        raw_args: I,
        path_sep: char,
    ) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let args = expand_options_args(work_dir, raw_args).map_err(|e| e.to_string())?;
        let mut p = Self::default();
        let mut i = 0;

        let next_arg = |args: &[String], idx: &mut usize, flag: &str| -> Result<String, String> {
            let v = args
                .get(*idx)
                .ok_or_else(|| format!("expected argument after {flag}"))?;
            *idx += 1;
            Ok(v.clone())
        };

        let collect_until_dash = |args: &[String], idx: &mut usize, out: &mut Vec<String>| {
            while let Some(v) = args.get(*idx) {
                if v.starts_with('-') {
                    break;
                }
                out.push(v.clone());
                *idx += 1;
            }
        };

        while i < args.len() {
            let arg = args[i].as_str();
            i += 1;
            match arg {
                "--javacopts" => {
                    let mut raw_jopts = Vec::new();
                    let mut terminated = false;
                    while i < args.len() {
                        let tok = &args[i];
                        i += 1;
                        if tok == "--" {
                            terminated = true;
                            break;
                        }
                        raw_jopts.push(tok.clone());
                    }
                    if !terminated {
                        return Err("--javacopts must be terminated by '--'".to_string());
                    }
                    let mut j = 0;
                    while j < raw_jopts.len() {
                        if raw_jopts[j] == "-sourcepath" {
                            let sp = raw_jopts
                                .get(j + 1)
                                .ok_or_else(|| "missing value after -sourcepath".to_string())?;
                            for entry in sp.split(path_sep) {
                                let trimmed = entry.trim();
                                if !trimmed.is_empty() {
                                    p.source_path.push(trimmed.to_string());
                                }
                            }
                            j += 2;
                        } else {
                            p.javac_opts.push(raw_jopts[j].clone());
                            j += 1;
                        }
                    }
                }
                "--direct_dependencies" => collect_until_dash(&args, &mut i, &mut p.direct_jars),
                "--strict_java_deps" => {
                    let v = next_arg(&args, &mut i, arg)?;
                    p.strict_java_deps = StrictDepsMode::parse(&v)?;
                }
                "--experimental_fix_deps_tool" => {
                    p.fix_deps_tool = Some(next_arg(&args, &mut i, arg)?);
                }
                "--reduce_classpath_mode" => {
                    let v = next_arg(&args, &mut i, arg)?;
                    p.reduce_classpath_mode = ReduceClasspathMode::parse(&v)?;
                }
                "--output_deps_proto" | "--output_deps" => {
                    p.output_deps_proto = Some(next_arg(&args, &mut i, arg)?);
                }
                "--deps_artifacts" => collect_until_dash(&args, &mut i, &mut p.deps_artifacts),
                "--output_manifest_proto" => {
                    p.output_manifest_proto = Some(next_arg(&args, &mut i, arg)?);
                }
                "--sources" => collect_until_dash(&args, &mut i, &mut p.source_files),
                "--source_jars" => collect_until_dash(&args, &mut i, &mut p.source_jars),
                "--classpath" => collect_until_dash(&args, &mut i, &mut p.class_path),
                "--bootclasspath" => collect_until_dash(&args, &mut i, &mut p.boot_class_path),
                "--system" => p.system_module_dir = Some(next_arg(&args, &mut i, arg)?),
                "--sourcepath" => collect_until_dash(&args, &mut i, &mut p.source_path),
                "--extclasspath" | "--extdir" => {
                    collect_until_dash(&args, &mut i, &mut p.ext_class_path)
                }
                "--processorpath" => collect_until_dash(&args, &mut i, &mut p.processor_path),
                "--processors" => {
                    while let Some(v) = args.get(i) {
                        if v.starts_with('-') {
                            break;
                        }
                        if v.contains(',') {
                            return Err(format!("processor argument may not contain commas: {v}"));
                        }
                        p.processor_names.push(v.clone());
                        i += 1;
                    }
                }
                "--output" => p.output_jar = Some(next_arg(&args, &mut i, arg)?),
                "--native_header_output" => {
                    p.native_header_output = Some(next_arg(&args, &mut i, arg)?)
                }
                "--classdir" => p.class_dir = Some(next_arg(&args, &mut i, arg)?),
                "--gendir" => p.temp_dir = Some(next_arg(&args, &mut i, arg)?),
                "--generated_sources_output" => {
                    p.generated_sources_output_jar = Some(next_arg(&args, &mut i, arg)?)
                }
                "--compress_jar" => p.compress_jar = true,
                "--target_label" => p.target_label = Some(next_arg(&args, &mut i, arg)?),
                "--injecting_rule_kind" => {
                    p.injecting_rule_kind = Some(next_arg(&args, &mut i, arg)?)
                }
                "--post_processor" => {
                    let name = next_arg(&args, &mut i, arg)?;
                    let mut pp_args = Vec::new();
                    collect_until_dash(&args, &mut i, &mut pp_args);
                    p.post_processors.push((name, pp_args));
                }
                other => return Err(format!("unknown option : '{other}'")),
            }
        }
        Ok(p)
    }
}

/// Validated JavaBuilder compilation request (`JavaLibraryBuildRequest.java`).
#[derive(Debug, Clone)]
pub struct JavaLibraryBuildRequest {
    pub work_dir: PathBuf,
    pub javac_opts: Vec<String>,
    pub werror: WerrorCustomOption,
    pub source_files: Vec<PathBuf>,
    pub source_jars: Vec<PathBuf>,
    pub source_path: Vec<PathBuf>,
    pub class_path: Vec<PathBuf>,
    pub boot_class_path: Vec<PathBuf>,
    pub system_module_dir: Option<PathBuf>,
    pub processor_path: Vec<PathBuf>,
    pub processor_names: Vec<String>,
    pub direct_jars: Vec<PathBuf>,
    pub deps_artifacts: Vec<PathBuf>,
    pub strict_java_deps: StrictDepsMode,
    pub fix_deps_tool: String,
    pub reduce_classpath_mode: ReduceClasspathMode,
    pub output_jar: PathBuf,
    pub native_header_output: Option<PathBuf>,
    pub generated_sources_output_jar: Option<PathBuf>,
    pub output_deps_proto: Option<PathBuf>,
    pub output_manifest_proto: Option<PathBuf>,
    pub class_dir: PathBuf,
    pub source_gen_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub native_header_dir: PathBuf,
    pub compress_jar: bool,
    pub target_label: Option<String>,
    pub injecting_rule_kind: Option<String>,
    pub request_id: Option<i32>,
}

impl JavaLibraryBuildRequest {
    pub fn from_options(
        options: OptionsParser,
        work_dir: impl Into<PathBuf>,
        vanilla: bool,
        request_id: Option<i32>,
    ) -> Result<Self, String> {
        let work_dir = work_dir.into();
        for forbidden in &options.javac_opts {
            if forbidden == "-processor" || forbidden == "-processorpath" {
                return Err(format!(
                    "{forbidden} is not supported in javacopts; declare a java_plugin instead"
                ));
            }
        }

        let output_raw = options
            .output_jar
            .as_deref()
            .ok_or_else(|| "missing required --output".to_string())?;
        let output_jar = resolve_in_work_dir(&work_dir, output_raw);

        let normalized_opts = if vanilla {
            JavacOptions::normalize_vanilla(&options.javac_opts)
        } else {
            JavacOptions::normalize_default(&options.javac_opts)
        };
        let mut werror = WerrorCustomOption::new();
        for o in &normalized_opts {
            werror.process(o);
        }

        // Derive `_javac/<target_name>/<jar_stem>_{classes,sources,tmp}` alongside `--output`.
        let label_tail = options
            .target_label
            .as_deref()
            .and_then(|l| l.rsplit_once(':').map(|(_, name)| name))
            .filter(|s| !s.is_empty())
            .unwrap_or("default");
        let jar_stem = output_jar
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "out".to_string());
        let javac_base = output_jar
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("_javac")
            .join(label_tail);

        let class_dir = options
            .class_dir
            .as_deref()
            .map(|p| resolve_in_work_dir(&work_dir, p))
            .unwrap_or_else(|| javac_base.join(format!("{jar_stem}_classes")));
        let source_gen_dir = javac_base.join(format!("{jar_stem}_sources"));
        let temp_dir = options
            .temp_dir
            .as_deref()
            .map(|p| resolve_in_work_dir(&work_dir, p))
            .unwrap_or_else(|| javac_base.join(format!("{jar_stem}_tmp")));
        let native_header_dir = temp_dir.join("native_headers");

        let res_vec = |v: &[String]| -> Vec<PathBuf> {
            v.iter()
                .map(|s| resolve_in_work_dir(&work_dir, s))
                .collect()
        };

        // Deduplicate processor names while preserving first-seen order (`ImmutableSet.copyOf`).
        let mut seen_procs = HashSet::new();
        let mut processor_names = Vec::new();
        for p in options.processor_names {
            if seen_procs.insert(p.clone()) {
                processor_names.push(p);
            }
        }

        Ok(Self {
            javac_opts: normalized_opts,
            werror,
            source_files: res_vec(&options.source_files),
            source_jars: res_vec(&options.source_jars),
            source_path: res_vec(&options.source_path),
            class_path: res_vec(&options.class_path),
            boot_class_path: res_vec(&options.boot_class_path),
            system_module_dir: options
                .system_module_dir
                .as_deref()
                .map(|s| resolve_in_work_dir(&work_dir, s)),
            processor_path: res_vec(&options.processor_path),
            processor_names,
            direct_jars: res_vec(&options.direct_jars),
            deps_artifacts: res_vec(&options.deps_artifacts),
            strict_java_deps: options.strict_java_deps,
            fix_deps_tool: options
                .fix_deps_tool
                .unwrap_or_else(|| "add_dep".to_string()),
            reduce_classpath_mode: options.reduce_classpath_mode,
            output_jar,
            native_header_output: options
                .native_header_output
                .as_deref()
                .map(|s| resolve_in_work_dir(&work_dir, s)),
            generated_sources_output_jar: options
                .generated_sources_output_jar
                .as_deref()
                .map(|s| resolve_in_work_dir(&work_dir, s)),
            output_deps_proto: options
                .output_deps_proto
                .as_deref()
                .map(|s| resolve_in_work_dir(&work_dir, s)),
            output_manifest_proto: options
                .output_manifest_proto
                .as_deref()
                .map(|s| resolve_in_work_dir(&work_dir, s)),
            class_dir,
            source_gen_dir,
            temp_dir,
            native_header_dir,
            compress_jar: options.compress_jar,
            target_label: options.target_label,
            injecting_rule_kind: options.injecting_rule_kind,
            work_dir,
            request_id,
        })
    }

    /// Fail-fast is enabled only for `BAZEL_REDUCED` when no annotation processors are active.
    pub fn fail_fast(&self) -> bool {
        self.reduce_classpath_mode == ReduceClasspathMode::BazelReduced
            && self.processor_names.is_empty()
    }

    /// Synthesizes `-processor <deduped>` when both processors and sources exist, or `-proc:none`
    /// otherwise (`JavaLibraryBuildRequest.addJavacArguments`).
    pub fn processor_javac_args(&self, has_sources: bool) -> Vec<String> {
        let mut out = Vec::new();
        if !self.processor_names.is_empty() && has_sources {
            if !self.processor_path.is_empty() {
                out.push("-processorpath".to_string());
                out.push(join_paths(&self.processor_path));
            }
            out.push("-processor".to_string());
            out.push(self.processor_names.join(","));
            out.push("-s".to_string());
            out.push(self.source_gen_dir.to_string_lossy().into_owned());
        } else {
            out.push("-proc:none".to_string());
        }
        out
    }
}

fn join_paths(paths: &[PathBuf]) -> String {
    let sep = if cfg!(windows) { ";" } else { ":" };
    paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(sep)
}

// ------------------------------------------------ Manifest (`java_compilation.proto`) & GenClass

/// `blaze.buildjar.CompilationUnit` (`java_compilation.proto`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilationUnit {
    pub path: Option<String>,
    pub pkg: Option<String>,
    pub generated_by_annotation_processor: bool,
    pub top_level: Vec<String>,
}

/// `blaze.buildjar.Manifest` (`java_compilation.proto`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilationManifest {
    pub compilation_units: Vec<CompilationUnit>,
}

pub fn parse_compilation_manifest(bytes: &[u8]) -> Result<CompilationManifest, String> {
    let mut out = CompilationManifest::default();
    fields(bytes, |field, wire, _, payload| {
        if field == 1 && wire == 2 {
            let mut cu = CompilationUnit::default();
            fields(payload, |f, w, val, p| {
                match (f, w) {
                    (1, 2) => cu.path = Some(String::from_utf8_lossy(p).into_owned()),
                    (2, 2) => cu.pkg = Some(String::from_utf8_lossy(p).into_owned()),
                    (3, 0) => cu.generated_by_annotation_processor = val != 0,
                    (4, 2) => cu.top_level.push(String::from_utf8_lossy(p).into_owned()),
                    _ => {}
                }
                Ok(())
            })?;
            out.compilation_units.push(cu);
        }
        Ok(())
    })?;
    Ok(out)
}

pub fn encode_compilation_manifest(manifest: &CompilationManifest) -> Vec<u8> {
    let mut out = Vec::new();
    for cu in &manifest.compilation_units {
        let mut inner = Vec::new();
        if let Some(p) = &cu.path {
            push_bytes_field(&mut inner, 1, p.as_bytes());
        }
        if let Some(pkg) = &cu.pkg {
            push_bytes_field(&mut inner, 2, pkg.as_bytes());
        }
        push_varint_field(
            &mut inner,
            3,
            u64::from(cu.generated_by_annotation_processor),
        );
        for tl in &cu.top_level {
            push_bytes_field(&mut inner, 4, tl.as_bytes());
        }
        push_bytes_field(&mut out, 1, &inner);
    }
    out
}

/// Options for `GenClass` (`GenClassOptions.java` / `GenClassOptionsParser.java`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenClassOptions {
    pub manifest: PathBuf,
    pub class_jar: PathBuf,
    pub output_jar: PathBuf,
    pub temp_dir: Option<PathBuf>,
}

impl GenClassOptions {
    pub fn parse<I, S>(work_dir: &Path, raw_args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let args = expand_options_args(work_dir, raw_args).map_err(|e| e.to_string())?;
        let mut manifest = None;
        let mut class_jar = None;
        let mut output_jar = None;
        let mut temp_dir = None;

        let mut i = 0;
        while i < args.len() {
            let flag = args[i].as_str();
            let val = args
                .get(i + 1)
                .ok_or_else(|| format!("Missing value for option {flag}"))?;
            i += 2;
            match flag {
                "--manifest_proto" => manifest = Some(resolve_in_work_dir(work_dir, val)),
                "--class_jar" => class_jar = Some(resolve_in_work_dir(work_dir, val)),
                "--output_jar" => output_jar = Some(resolve_in_work_dir(work_dir, val)),
                "--temp_dir" => temp_dir = Some(resolve_in_work_dir(work_dir, val)),
                other => return Err(format!("Unknown GenClass option: {other}")),
            }
        }
        Ok(Self {
            manifest: manifest.ok_or_else(|| "Missing required --manifest_proto".to_string())?,
            class_jar: class_jar.ok_or_else(|| "Missing required --class_jar".to_string())?,
            output_jar: output_jar.ok_or_else(|| "Missing required --output_jar".to_string())?,
            temp_dir,
        })
    }
}

/// Filters a compiled class JAR down to `.class` files produced by annotation processors
/// (`GenClass.java`).
pub struct GenClass;

impl GenClass {
    /// Builds `(generated_prefixes, handwritten_prefixes)` from `manifest`. Each prefix is
    /// `<pkg with '.' -> '/' + '/'><top_level>` (or bare `<top_level>` in theunnamed package).
    pub fn prefixes_from_manifest(
        manifest: &CompilationManifest,
    ) -> (HashSet<String>, HashSet<String>) {
        let mut generated = HashSet::new();
        let mut handwritten = HashSet::new();
        for unit in &manifest.compilation_units {
            let pkg_prefix = match unit.pkg.as_deref() {
                Some(p) if !p.is_empty() => format!("{}/", p.replace('.', "/")),
                _ => String::new(),
            };
            for tl in &unit.top_level {
                let prefix = format!("{pkg_prefix}{tl}");
                if unit.generated_by_annotation_processor {
                    generated.insert(prefix);
                } else {
                    handwritten.insert(prefix);
                }
            }
        }
        (generated, handwritten)
    }

    /// Returns `true` when `entry_name` is a `.class` entry whose stripped name (or any `$`-bounded
    /// outer-class prefix such as `Foo` / `Foo$Inner` for `Foo$Inner$0.class`) matches a generated
    /// prefix, OR matches no handwritten prefix (`GenClass.prefixesContains`).
    pub fn should_include_entry(
        entry_name: &str,
        generated_prefixes: &HashSet<String>,
        handwritten_prefixes: &HashSet<String>,
    ) -> bool {
        let Some(class_name) = entry_name
            .strip_prefix('/')
            .unwrap_or(entry_name)
            .strip_suffix(".class")
        else {
            return false;
        };
        Self::matches_prefixes(class_name, generated_prefixes)
            || !Self::matches_prefixes(class_name, handwritten_prefixes)
    }

    fn matches_prefixes(class_name: &str, prefixes: &HashSet<String>) -> bool {
        if prefixes.contains(class_name) {
            return true;
        }
        for (idx, ch) in class_name.char_indices() {
            if ch == '$' && prefixes.contains(&class_name[..idx]) {
                return true;
            }
        }
        false
    }

    pub fn execute(options: &GenClassOptions) -> io::Result<()> {
        let manifest_bytes = std::fs::read(&options.manifest)?;
        let manifest = parse_compilation_manifest(&manifest_bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let (generated, handwritten) = Self::prefixes_from_manifest(&manifest);

        let reader = ZipReader::open(&options.class_jar)?;
        let mut creator = JarCreator::new(&options.output_jar);
        creator.set_compression(true);
        creator.set_normalize(true);

        for entry in reader.entries() {
            if Self::should_include_entry(&entry.name, &generated, &handwritten) {
                let data = reader.read_entry_bytes(entry, false)?;
                creator.add_bytes_entry(&entry.name, data);
            }
        }
        creator.execute()
    }
}

// ------------------------------------------------ StrictJavaDeps & ReducedClasspath

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JarOwner {
    pub label: Option<String>,
    pub aspect: Option<String>,
}

/// Reads `Target-Label` and `Injecting-Rule-Kind` from `jar_path`'s `META-INF/MANIFEST.MF`
/// (`DependencyModule.readJarOwnerFromManifest`).
pub fn read_jar_owner(jar_path: &Path) -> io::Result<JarOwner> {
    let reader = ZipReader::open(jar_path)?;
    let Some(mf_entry) = reader.get_entry(MANIFEST_NAME) else {
        return Ok(JarOwner::default());
    };
    let bytes = reader.read_entry_bytes(mf_entry, false)?;
    let attrs = parse_manifest_main_attributes(&bytes);
    let mut owner = JarOwner::default();
    for (k, v) in attrs {
        if k.eq_ignore_ascii_case("Target-Label") {
            // Unescape leading `@@` written by external-repo label escaping if needed.
            owner.label = Some(v);
        } else if k.eq_ignore_ascii_case("Injecting-Rule-Kind") {
            owner.aspect = Some(v);
        }
    }
    Ok(owner)
}

/// Formats Strict Java Deps diagnostics for indirect dependencies used directly during compilation
/// (`StrictJavaDepsPlugin.java`).
pub fn check_strict_java_deps(
    mode: StrictDepsMode,
    target_label: Option<&str>,
    fix_deps_tool: &str,
    direct_jars: &[PathBuf],
    used_jars: &[(PathBuf, DependencyKind)],
) -> Result<Vec<String>, Vec<String>> {
    if mode == StrictDepsMode::Off {
        return Ok(Vec::new());
    }
    let direct_norm: HashSet<String> = direct_jars
        .iter()
        .map(|p| normalize_jdeps_path(&p.to_string_lossy()))
        .collect();

    let mut missing: Vec<(PathBuf, JarOwner)> = Vec::new();
    for (jar, kind) in used_jars {
        // StrictJavaDeps flags EXPLICIT uses of non-direct jars.
        if *kind != DependencyKind::Explicit {
            continue;
        }
        let norm = normalize_jdeps_path(&jar.to_string_lossy());
        if !direct_norm.contains(&norm) {
            let owner = read_jar_owner(jar).unwrap_or_default();
            missing.push((jar.clone(), owner));
        }
    }

    if missing.is_empty() {
        return Ok(Vec::new());
    }

    let level = if mode == StrictDepsMode::Error {
        "error"
    } else {
        "warning"
    };
    let mut messages = Vec::new();
    let mut missing_labels = Vec::new();
    for (jar, owner) in &missing {
        match (&owner.label, &owner.aspect) {
            (Some(lbl), Some(aspect)) => {
                messages.push(format!(
                    "[{level}] [strict] Using type from indirect dependency (tool: {fix_deps_tool}): {} wrapped in {aspect} {lbl}",
                    jar.display()
                ));
                missing_labels.push(lbl.clone());
            }
            (Some(lbl), None) => {
                messages.push(format!(
                    "[{level}] [strict] Using type from {lbl} ({}), which is an indirect dependency",
                    jar.display()
                ));
                missing_labels.push(lbl.clone());
            }
            _ => {
                messages.push(format!(
                    "[{level}] [strict] Using type from indirect dependency {}",
                    jar.display()
                ));
            }
        }
    }
    if !missing_labels.is_empty() {
        if let Some(target) = target_label {
            messages.push(format!(
                " ** Please add the following dependencies:\n  {fix_deps_tool} {} {target}",
                missing_labels.join(" ")
            ));
        }
    }

    if mode == StrictDepsMode::Error {
        Err(messages)
    } else {
        Ok(messages)
    }
}

/// `ReducedClasspathJavaLibraryBuilder.computeStrictClasspath`: filters `full_classpath` to
/// `direct_jars` plus `EXPLICIT` / `IMPLICIT` / `INCOMPLETE` entries from `deps_artifacts`,
/// normalizing paths via [`normalize_jdeps_path`] and requiring `rule_label` on every `.jdeps`.
pub fn compute_strict_classpath(
    full_classpath: &[PathBuf],
    direct_jars: &[PathBuf],
    deps_artifacts: &[PathBuf],
) -> io::Result<Vec<PathBuf>> {
    let mut allowed: HashSet<String> = direct_jars
        .iter()
        .map(|p| normalize_jdeps_path(&p.to_string_lossy()))
        .collect();
    for proto_path in deps_artifacts {
        let bytes = std::fs::read(proto_path)?;
        let jdeps = parse_jdeps(&bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", proto_path.display()),
            )
        })?;
        if jdeps.rule_label.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "jdeps '{}' is missing required rule_label",
                    proto_path.display()
                ),
            ));
        }
        for dep in &jdeps.dependencies {
            if dep.kind.is_used() {
                allowed.insert(normalize_jdeps_path(&dep.path));
            }
        }
    }
    Ok(full_classpath
        .iter()
        .filter(|p| allowed.contains(&normalize_jdeps_path(&p.to_string_lossy())))
        .cloned()
        .collect())
}

// ------------------------------------------------ Builders (Simple, Reduced, Vanilla)

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlazeJavacStatus {
    Ok,
    Error,
    Crash,
    Cancelled,
    RequiresFallback,
}

#[derive(Debug, Clone)]
pub struct JavaBuildResult {
    pub status: BlazeJavacStatus,
    pub output: String,
    pub statistics_classpath_len: usize,
    pub statistics_reduced_classpath_len: usize,
    pub transitive_classpath_fallback: bool,
}

impl JavaBuildResult {
    pub fn is_ok(&self) -> bool {
        self.status == BlazeJavacStatus::Ok
    }
}

fn recreate_clean_dir(dir: &Path) -> io::Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)
}

/// Extracts `.java` sources from `source_jars` into `temp_dir` (nesting collisions under
/// `temp_dir/<counter>/` so case-only clashes survive case-insensitive filesystems) and merges any
/// `protobuf.meta` entries into `class_dir/protobuf.meta` (`SimpleJavaLibraryBuilder.prepareSourceCompilation`).
pub fn prepare_source_compilation(req: &JavaLibraryBuildRequest) -> io::Result<Vec<PathBuf>> {
    recreate_clean_dir(&req.class_dir)?;
    recreate_clean_dir(&req.source_gen_dir)?;
    recreate_clean_dir(&req.temp_dir)?;
    recreate_clean_dir(&req.native_header_dir)?;

    let mut all_sources = req.source_files.clone();
    let mut collision_counter = 0usize;
    let mut protobuf_meta_strategy = ConcatenateStrategy::new(false);
    let mut protobuf_meta_buf = Vec::new();
    let mut saw_protobuf_meta = false;

    for sjar in &req.source_jars {
        let reader = ZipReader::open(sjar)?;
        for entry in reader.entries() {
            if entry.name.ends_with('/') {
                continue;
            }
            let rel = entry.name.strip_prefix('/').unwrap_or(&entry.name);
            if rel.ends_with(".java") {
                let bytes = reader.read_entry_bytes(entry, false)?;
                let mut dest = req.temp_dir.join(rel);
                if dest.exists() {
                    collision_counter += 1;
                    dest = req.temp_dir.join(collision_counter.to_string()).join(rel);
                }
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&dest, bytes)?;
                all_sources.push(dest);
            } else if rel == "protobuf.meta" {
                let bytes = reader.read_entry_bytes(entry, false)?;
                protobuf_meta_strategy.merge(&bytes, &mut protobuf_meta_buf)?;
                saw_protobuf_meta = true;
            }
        }
    }

    let pm_out = req.class_dir.join("protobuf.meta");
    if saw_protobuf_meta {
        protobuf_meta_strategy.finish(&mut protobuf_meta_buf)?;
        std::fs::write(&pm_out, protobuf_meta_buf)?;
    } else if pm_out.exists() {
        let _ = std::fs::remove_file(&pm_out);
    }

    Ok(all_sources)
}

fn collect_files_with_ext(root: &Path, ext: &str, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(root)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let p = entry.path();
        if p.is_dir() {
            collect_files_with_ext(&p, ext, out)?;
        } else if p
            .extension()
            .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(ext))
        {
            out.push(p);
        }
    }
    Ok(())
}

fn collect_contained_packages(class_dir: &Path) -> io::Result<Vec<String>> {
    let mut classes = Vec::new();
    collect_files_with_ext(class_dir, "class", &mut classes)?;
    let mut pkgs = BTreeSet::new();
    for c in classes {
        if let Ok(rel) = c.strip_prefix(class_dir) {
            if rel.file_name().is_some_and(|n| n == "module-info.class") {
                continue;
            }
            let parent = rel.parent().map(|p| {
                p.components()
                    .map(|comp| comp.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(".")
            });
            let pkg = parent.unwrap_or_default();
            pkgs.insert(if pkg.is_empty() { ".".to_string() } else { pkg });
        }
    }
    Ok(pkgs.into_iter().collect())
}

/// Lightweight scanner extracting `package` and top-level type declarations from a `.java` source
/// file for `CompilationManifest` emission.
pub fn scan_java_compilation_unit(
    path: &Path,
    display_path: String,
    generated: bool,
) -> CompilationUnit {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let stripped = strip_java_comments_and_strings(&content);
    let mut pkg = None;
    let mut top_level = Vec::new();

    let mut brace_depth = 0i32;
    let mut tokens = stripped
        .split(|c: char| {
            c.is_whitespace() || matches!(c, ';' | '{' | '}' | '(' | ')' | '<' | '>' | ',')
        })
        .filter(|s| !s.is_empty());

    // Walk character-aware for top-level declarations (`brace_depth == 0`).
    let _ = &mut tokens;
    let mut cur = String::new();
    let mut prev_keywords: Vec<String> = Vec::new();
    let mut expecting_pkg = false;

    for ch in stripped.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '$' || (expecting_pkg && ch == '.') {
            cur.push(ch);
            continue;
        }
        if !cur.is_empty() {
            let word = std::mem::take(&mut cur);
            if expecting_pkg {
                if pkg.is_none() {
                    pkg = Some(word.trim_matches('.').to_string());
                }
                expecting_pkg = false;
                prev_keywords.clear();
            } else if brace_depth == 0 {
                if word == "package" && pkg.is_none() {
                    expecting_pkg = true;
                } else if matches!(
                    prev_keywords.last().map(String::as_str),
                    Some("class" | "interface" | "enum" | "record")
                ) {
                    top_level.push(word);
                    prev_keywords.clear();
                } else {
                    prev_keywords.push(word);
                }
            }
        }
        match ch {
            '{' => {
                brace_depth += 1;
                prev_keywords.clear();
            }
            '}' => {
                brace_depth = (brace_depth - 1).max(0);
                prev_keywords.clear();
            }
            ';' => {
                expecting_pkg = false;
                prev_keywords.clear();
            }
            _ => {}
        }
    }

    CompilationUnit {
        path: Some(display_path),
        pkg,
        generated_by_annotation_processor: generated,
        top_level,
    }
}

fn strip_java_comments_and_strings(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' {
            match chars.peek().copied() {
                Some('/') => {
                    chars.next();
                    for nc in chars.by_ref() {
                        if nc == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    while let Some(nc) = chars.next() {
                        if nc == '*' && chars.peek() == Some(&'/') {
                            chars.next();
                            out.push(' ');
                            break;
                        } else if nc == '\n' {
                            out.push('\n');
                        }
                    }
                    continue;
                }
                _ => {}
            }
        } else if c == '"' || c == '\'' {
            let quote = c;
            out.push(' ');
            while let Some(nc) = chars.next() {
                if nc == '\\' {
                    let _ = chars.next();
                } else if nc == quote {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn invoke_system_javac(
    javac_bin: &str,
    req: &JavaLibraryBuildRequest,
    classpath: &[PathBuf],
    sources: &[PathBuf],
    vanilla: bool,
) -> io::Result<(BlazeJavacStatus, String, bool)> {
    if sources.is_empty() {
        return Ok((BlazeJavacStatus::Ok, String::new(), false));
    }
    let mut cmd = Command::new(javac_bin);
    cmd.arg("-encoding").arg("UTF-8");
    cmd.arg("-d").arg(&req.class_dir);
    if req.native_header_output.is_some() {
        cmd.arg("-h").arg(&req.native_header_dir);
    }
    if let Some(sys) = &req.system_module_dir {
        cmd.arg("--system").arg(sys);
    } else if !req.boot_class_path.is_empty() {
        cmd.arg("-bootclasspath")
            .arg(join_paths(&req.boot_class_path));
    }
    if !classpath.is_empty() {
        cmd.arg("-classpath").arg(join_paths(classpath));
    } else {
        cmd.arg("-classpath").arg("");
    }
    if !req.source_path.is_empty() {
        cmd.arg("-sourcepath").arg(join_paths(&req.source_path));
    } else {
        let inferred_module_src = (!vanilla)
            .then(|| {
                let mut mods = sources
                    .iter()
                    .filter(|f| f.file_name().is_some_and(|n| n == "module-info.java"));
                let only = mods.next()?;
                if mods.next().is_some() {
                    return None;
                }
                let abs = if only.is_absolute() {
                    only.clone()
                } else {
                    std::env::current_dir()
                        .map(|cwd| cwd.join(only))
                        .unwrap_or_else(|_| only.clone())
                };
                abs.parent().map(Path::to_path_buf)
            })
            .flatten();
        if let Some(module_parent) = inferred_module_src {
            cmd.arg("-sourcepath").arg(module_parent);
        } else {
            cmd.arg("-sourcepath").arg("");
        }
    }
    for arg in req.processor_javac_args(true) {
        cmd.arg(arg);
    }
    let clean_opts = JavacOptions::remove_bazel_specific_flags(&req.javac_opts);
    if !vanilla {
        cmd.arg("-Xlint:path");
    }
    for o in clean_opts {
        cmd.arg(o);
    }
    for s in sources {
        cmd.arg(s);
    }

    let output = cmd.output()?;
    let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.stdout.is_empty() {
        stderr.push_str(&String::from_utf8_lossy(&output.stdout));
    }

    if vanilla {
        // VanillaJavaBuilder drops Note: deprecated/unchecked and compiler.warn.sun.proprietary.
        stderr = stderr
            .lines()
            .filter(|l| {
                !l.starts_with("Note: ")
                    && !l.contains(
                        " is internal proprietary API and may be removed in a future release",
                    )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !stderr.is_empty() {
            stderr.push('\n');
        }
    }

    // Strip leading workDir prefix when workDir is non-empty (`FormattedDiagnostic`).
    if !req.work_dir.as_os_str().is_empty() {
        let prefix = format!("{}{}", req.work_dir.display(), std::path::MAIN_SEPARATOR);
        stderr = stderr
            .lines()
            .map(|l| l.strip_prefix(&prefix).unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n");
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
    }

    // Apply `-Werror:` promotion on `[category]` warning lines when compilation exited 0.
    let mut promoted_error = false;
    if !vanilla {
        let mut rewritten = Vec::new();
        for line in stderr.lines() {
            if let Some(cat) = extract_warning_category(line) {
                if req.werror.is_enabled(cat) {
                    promoted_error = true;
                    rewritten.push(line.replacen("warning:", "error:", 1));
                    continue;
                }
            }
            rewritten.push(line.to_string());
        }
        if promoted_error {
            rewritten.push("error: warnings found and -Werror specified".to_string());
            stderr = rewritten.join("\n") + "\n";
        }
    }

    let javac_succeeded = output.status.success();
    let status = if javac_succeeded && !promoted_error {
        BlazeJavacStatus::Ok
    } else if output.status.code() == Some(4) {
        BlazeJavacStatus::Crash
    } else {
        BlazeJavacStatus::Error
    };
    Ok((status, stderr, javac_succeeded))
}

fn extract_warning_category(line: &str) -> Option<&str> {
    if let Some(idx) = line.find("warning: [") {
        let rest = &line[idx + "warning: [".len()..];
        if let Some(end) = rest.find(']') {
            let cat = &rest[..end];
            if !cat.is_empty() {
                return Some(cat);
            }
        }
    }
    // `FormattedDiagnostic`: `compiler.warn.sun.proprietary` omits `[category]` in javac output
    // and maps to lint option `"sunapi"`.
    if line.contains("warning: ")
        && line.contains(" is internal proprietary API and may be removed in a future release")
    {
        return Some("sunapi");
    }
    None
}

fn should_fall_back(status: &BlazeJavacStatus, diagnostics: &str) -> bool {
    match status {
        BlazeJavacStatus::Ok | BlazeJavacStatus::Cancelled | BlazeJavacStatus::RequiresFallback => {
            false
        }
        BlazeJavacStatus::Crash => true,
        BlazeJavacStatus::Error => {
            let trimmed = diagnostics.trim();
            if trimmed.is_empty() {
                return false;
            }
            // If all error lines are JSpecify `[nullness] `, do not fall back.
            let err_lines: Vec<&str> = trimmed.lines().filter(|l| l.contains("error:")).collect();
            if !err_lines.is_empty() && err_lines.iter().all(|l| l.contains("error: [nullness] ")) {
                return false;
            }
            true
        }
    }
}

fn write_output_jars(req: &JavaLibraryBuildRequest) -> io::Result<()> {
    let mut main_jar = JarCreator::new(&req.output_jar);
    main_jar.set_normalize(true);
    main_jar.set_compression(req.compress_jar);
    let label_unescaped = req.target_label.as_deref().map(|l| {
        l.strip_prefix("@@")
            .map(|r| format!("@{r}"))
            .unwrap_or_else(|| l.to_string())
    });
    main_jar.set_jar_owner(label_unescaped, req.injecting_rule_kind.clone());
    main_jar.add_directory(&req.class_dir)?;
    main_jar.execute()?;

    if let Some(nh_jar_path) = &req.native_header_output {
        let mut nh = JarCreator::new(nh_jar_path);
        nh.set_normalize(true);
        nh.set_compression(req.compress_jar);
        nh.add_directory(&req.native_header_dir)?;
        nh.execute()?;
    }
    Ok(())
}

fn write_gensrc_jar(req: &JavaLibraryBuildRequest) -> io::Result<()> {
    if let Some(gs_jar_path) = &req.generated_sources_output_jar {
        let mut gs = JarCreator::new(gs_jar_path);
        gs.set_normalize(true);
        gs.set_compression(req.compress_jar);
        if req.source_gen_dir.exists() {
            gs.add_directory(&req.source_gen_dir)?;
        }
        gs.execute()?;
    }
    Ok(())
}

#[derive(Default)]
struct ClassFileRefs {
    const_classes: HashSet<String>,
    descriptor_classes: HashSet<String>,
    api_surface_classes: HashSet<String>,
}

fn normalize_internal_class_ref(raw: &str) -> Option<String> {
    let trimmed = raw.trim_start_matches('[');
    let internal = if trimmed.len() != raw.len() {
        trimmed.strip_prefix('L')?.strip_suffix(';')?
    } else {
        trimmed
    };
    if internal.is_empty() || internal.starts_with('[') {
        None
    } else {
        Some(internal.to_string())
    }
}

fn collect_descriptor_types(desc: &str, out: &mut HashSet<String>) {
    let bytes = desc.as_bytes();
    let mut i = 0;
    // JVMS §4.7.9.1: a leading `<` in a ClassSignature or MethodSignature introduces formal
    // `TypeParameters` (`< Identifier ClassBound InterfaceBound* ... >`), where `Identifier`
    // is a formal type parameter name rather than a `TypeVariableSignature` (`T...;`) or
    // `ClassTypeSignature` (`L...;`).
    if bytes.first() == Some(&b'<') {
        parse_type_parameters(bytes, &mut i, out);
    }
    while i < bytes.len() {
        parse_signature_component(bytes, &mut i, out);
    }
}

fn parse_type_parameters(bytes: &[u8], i: &mut usize, out: &mut HashSet<String>) {
    if bytes.get(*i) != Some(&b'<') {
        return;
    }
    *i += 1;
    while *i < bytes.len() && bytes[*i] != b'>' {
        while *i < bytes.len() && bytes[*i] != b':' && bytes[*i] != b'>' {
            *i += 1;
        }
        if bytes.get(*i) == Some(&b'>') {
            break;
        }
        while bytes.get(*i) == Some(&b':') {
            *i += 1;
            if matches!(bytes.get(*i), Some(b'L' | b'[' | b'T')) {
                parse_signature_component(bytes, i, out);
            }
        }
    }
    if bytes.get(*i) == Some(&b'>') {
        *i += 1;
    }
}

fn parse_type_arguments(bytes: &[u8], i: &mut usize, out: &mut HashSet<String>) {
    if bytes.get(*i) != Some(&b'<') {
        return;
    }
    *i += 1;
    while *i < bytes.len() && bytes[*i] != b'>' {
        parse_signature_component(bytes, i, out);
    }
    if bytes.get(*i) == Some(&b'>') {
        *i += 1;
    }
}

fn parse_signature_component(bytes: &[u8], i: &mut usize, out: &mut HashSet<String>) {
    let Some(&b) = bytes.get(*i) else {
        return;
    };
    match b {
        b'(' | b')' | b'+' | b'-' | b'*' | b'^' | b':' => *i += 1,
        b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b'V' => *i += 1,
        b'[' => {
            while bytes.get(*i) == Some(&b'[') {
                *i += 1;
            }
            if *i < bytes.len() {
                parse_signature_component(bytes, i, out);
            }
        }
        b'T' => {
            *i += 1;
            while *i < bytes.len() && bytes[*i] != b';' {
                *i += 1;
            }
            if *i < bytes.len() {
                *i += 1;
            }
        }
        b'L' => {
            *i += 1;
            let mut current = String::new();
            while *i < bytes.len() {
                match bytes[*i] {
                    b';' => {
                        if !current.is_empty() {
                            out.insert(current);
                        }
                        *i += 1;
                        break;
                    }
                    b'<' => {
                        if !current.is_empty() {
                            out.insert(current.clone());
                        }
                        parse_type_arguments(bytes, i, out);
                        if bytes.get(*i) == Some(&b'.') {
                            current.push('$');
                            *i += 1;
                        }
                    }
                    b'.' => {
                        if !current.is_empty() {
                            out.insert(current.clone());
                        }
                        current.push('$');
                        *i += 1;
                    }
                    ch => {
                        current.push(ch as char);
                        *i += 1;
                    }
                }
            }
        }
        _ => *i += 1,
    }
}

fn read_u16_be(bytes: &[u8], pos: &mut usize) -> Option<u16> {
    let slice = bytes.get(*pos..*pos + 2)?;
    *pos += 2;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_u32_be(bytes: &[u8], pos: &mut usize) -> Option<u32> {
    let slice = bytes.get(*pos..*pos + 4)?;
    *pos += 4;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn parse_classfile_refs(bytes: &[u8]) -> ClassFileRefs {
    let mut out = ClassFileRefs::default();
    if bytes.len() < 10 || &bytes[0..4] != b"\xCA\xFE\xBA\xBE" {
        return out;
    }
    let mut pos = 8usize;
    let Some(cp_count) = read_u16_be(bytes, &mut pos) else {
        return out;
    };
    let cap = cp_count as usize;
    let mut utf8_pool: Vec<Option<String>> = vec![None; cap];
    let mut class_name_slot: Vec<Option<u16>> = vec![None; cap];
    let mut string_literal_utf8: HashSet<u16> = HashSet::new();
    let mut code_desc_utf8: Vec<u16> = Vec::new();

    let mut idx = 1usize;
    while idx < cap {
        let Some(&tag) = bytes.get(pos) else {
            return out;
        };
        pos += 1;
        match tag {
            1 => {
                let Some(len) = read_u16_be(bytes, &mut pos) else {
                    return out;
                };
                let end = pos + len as usize;
                let Some(raw) = bytes.get(pos..end) else {
                    return out;
                };
                pos = end;
                utf8_pool[idx] = Some(String::from_utf8_lossy(raw).into_owned());
                idx += 1;
            }
            3 | 4 => {
                if bytes.len() < pos + 4 {
                    return out;
                }
                pos += 4;
                idx += 1;
            }
            5 | 6 => {
                if bytes.len() < pos + 8 {
                    return out;
                }
                pos += 8;
                idx += 2;
            }
            7 => {
                let Some(ni) = read_u16_be(bytes, &mut pos) else {
                    return out;
                };
                class_name_slot[idx] = Some(ni);
                idx += 1;
            }
            8 => {
                let Some(si) = read_u16_be(bytes, &mut pos) else {
                    return out;
                };
                string_literal_utf8.insert(si);
                idx += 1;
            }
            9 | 10 | 11 | 17 | 18 => {
                if bytes.len() < pos + 4 {
                    return out;
                }
                pos += 4;
                idx += 1;
            }
            12 => {
                let Some(_name_idx) = read_u16_be(bytes, &mut pos) else {
                    return out;
                };
                let Some(desc_idx) = read_u16_be(bytes, &mut pos) else {
                    return out;
                };
                code_desc_utf8.push(desc_idx);
                idx += 1;
            }
            15 => {
                if bytes.len() < pos + 3 {
                    return out;
                }
                pos += 3;
                idx += 1;
            }
            16 => {
                let Some(desc_idx) = read_u16_be(bytes, &mut pos) else {
                    return out;
                };
                code_desc_utf8.push(desc_idx);
                idx += 1;
            }
            19 | 20 => {
                if bytes.len() < pos + 2 {
                    return out;
                }
                pos += 2;
                idx += 1;
            }
            _ => return out,
        }
    }

    for ni in class_name_slot.iter().flatten() {
        if let Some(Some(s)) = utf8_pool.get(*ni as usize) {
            if let Some(norm) = normalize_internal_class_ref(s) {
                out.const_classes.insert(norm);
            }
        }
    }
    for di in code_desc_utf8 {
        if let Some(Some(s)) = utf8_pool.get(di as usize) {
            collect_descriptor_types(s, &mut out.descriptor_classes);
        }
    }

    // Parse class header, fields, methods, and Signature / Annotation attributes for API surface.
    let scan_attrs = |pos: &mut usize, surface: &mut HashSet<String>| -> Option<()> {
        let acount = read_u16_be(bytes, pos)?;
        for _ in 0..acount {
            let aname_idx = read_u16_be(bytes, pos)?;
            let alen = read_u32_be(bytes, pos)? as usize;
            let body = bytes.get(*pos..*pos + alen)?;
            *pos += alen;
            let aname = utf8_pool
                .get(aname_idx as usize)
                .and_then(|o| o.as_deref())
                .unwrap_or("");
            if aname == "Signature" && body.len() >= 2 {
                let sig_idx = u16::from_be_bytes([body[0], body[1]]) as usize;
                if let Some(Some(sig)) = utf8_pool.get(sig_idx) {
                    collect_descriptor_types(sig, surface);
                }
            } else if aname.contains("Annotations") {
                for (slot, entry) in utf8_pool.iter().enumerate() {
                    if string_literal_utf8.contains(&(slot as u16)) {
                        continue;
                    }
                    if let Some(s) = entry {
                        if s.starts_with('L') && s.ends_with(';') && !s.contains(' ') {
                            collect_descriptor_types(s, surface);
                        }
                    }
                }
            }
        }
        Some(())
    };

    let _ = (|| -> Option<()> {
        let _access = read_u16_be(bytes, &mut pos)?;
        let _this_cls = read_u16_be(bytes, &mut pos)?;
        let super_cls = read_u16_be(bytes, &mut pos)? as usize;
        if let Some(Some(ni)) = class_name_slot.get(super_cls) {
            if let Some(Some(s)) = utf8_pool.get(*ni as usize) {
                if let Some(norm) = normalize_internal_class_ref(s) {
                    out.api_surface_classes.insert(norm);
                }
            }
        }
        let icount = read_u16_be(bytes, &mut pos)?;
        for _ in 0..icount {
            let iface_cls = read_u16_be(bytes, &mut pos)? as usize;
            if let Some(Some(ni)) = class_name_slot.get(iface_cls) {
                if let Some(Some(s)) = utf8_pool.get(*ni as usize) {
                    if let Some(norm) = normalize_internal_class_ref(s) {
                        out.api_surface_classes.insert(norm);
                    }
                }
            }
        }
        let fcount = read_u16_be(bytes, &mut pos)?;
        for _ in 0..fcount {
            let _fa = read_u16_be(bytes, &mut pos)?;
            let _fn_idx = read_u16_be(bytes, &mut pos)?;
            let fd_idx = read_u16_be(bytes, &mut pos)? as usize;
            if let Some(Some(s)) = utf8_pool.get(fd_idx) {
                collect_descriptor_types(s, &mut out.api_surface_classes);
            }
            scan_attrs(&mut pos, &mut out.api_surface_classes)?;
        }
        let mcount = read_u16_be(bytes, &mut pos)?;
        for _ in 0..mcount {
            let _ma = read_u16_be(bytes, &mut pos)?;
            let _mn_idx = read_u16_be(bytes, &mut pos)?;
            let md_idx = read_u16_be(bytes, &mut pos)? as usize;
            if let Some(Some(s)) = utf8_pool.get(md_idx) {
                collect_descriptor_types(s, &mut out.api_surface_classes);
            }
            scan_attrs(&mut pos, &mut out.api_surface_classes)?;
        }
        scan_attrs(&mut pos, &mut out.api_surface_classes)?;
        Some(())
    })();

    out.descriptor_classes
        .extend(out.api_surface_classes.iter().cloned());
    out
}

struct SourceImportSummary {
    explicit_imports: Vec<String>,
    wildcard_packages: HashSet<String>,
    identifiers: HashSet<String>,
    exempt_own_prefixes: HashSet<String>,
}

fn scan_sources_for_strict_deps(
    sources: &[PathBuf],
    source_gen_dir: &Path,
) -> io::Result<SourceImportSummary> {
    let mut summary = SourceImportSummary {
        explicit_imports: Vec::new(),
        wildcard_packages: HashSet::new(),
        identifiers: HashSet::new(),
        exempt_own_prefixes: HashSet::new(),
    };
    let mut all_sources: Vec<(PathBuf, bool)> =
        sources.iter().cloned().map(|p| (p, false)).collect();
    let mut gen_files = Vec::new();
    collect_files_with_ext(source_gen_dir, "java", &mut gen_files)?;
    for g in gen_files {
        all_sources.push((g, true));
    }

    for (src_path, generated) in &all_sources {
        let raw = std::fs::read_to_string(src_path).unwrap_or_default();
        let is_dagger_or_hilt_exempt = raw.contains("dagger.internal.codegen.ComponentProcessor")
            || raw.contains("dagger.hilt.processor.internal.root.RootProcessor");
        if is_dagger_or_hilt_exempt {
            let unit = scan_java_compilation_unit(src_path, String::new(), *generated);
            let pkg_prefix = unit
                .pkg
                .as_deref()
                .filter(|p| !p.is_empty())
                .map(|p| format!("{}/", p.replace('.', "/")))
                .unwrap_or_default();
            for tl in unit.top_level {
                summary
                    .exempt_own_prefixes
                    .insert(format!("{pkg_prefix}{tl}"));
            }
            continue;
        }

        let stripped = strip_java_comments_and_strings(&raw);
        let unit = scan_java_compilation_unit(src_path, String::new(), *generated);
        if let Some(pkg) = &unit.pkg {
            if !pkg.is_empty() {
                summary.wildcard_packages.insert(pkg.clone());
            }
        }

        // Parse `import [static] foo.bar.Baz;` and `import [static] foo.bar.*;`.
        for stmt in stripped.split(';') {
            let trimmed = stmt.trim();
            let Some(rest) = trimmed.strip_prefix("import") else {
                continue;
            };
            if !rest.starts_with(char::is_whitespace) {
                continue;
            }
            let rest = rest.trim_start();
            let (is_static, target) = if let Some(after_static) = rest.strip_prefix("static") {
                if after_static.starts_with(char::is_whitespace) {
                    (true, after_static.trim())
                } else {
                    (false, rest)
                }
            } else {
                (false, rest)
            };
            let clean: String = target.chars().filter(|c| !c.is_whitespace()).collect();
            if let Some(pkg_wild) = clean.strip_suffix(".*") {
                if is_static {
                    summary.explicit_imports.push(pkg_wild.to_string());
                } else {
                    summary.wildcard_packages.insert(pkg_wild.to_string());
                }
            } else if !clean.is_empty() {
                summary.explicit_imports.push(clean.clone());
                if is_static {
                    if let Some((owner_cls, _member)) = clean.rsplit_once('.') {
                        summary.explicit_imports.push(owner_cls.to_string());
                    }
                }
            }
        }

        let mut cur = String::new();
        for ch in stripped.chars() {
            if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                cur.push(ch);
            } else if !cur.is_empty() {
                summary.identifiers.insert(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            summary.identifiers.insert(cur);
        }
    }

    Ok(summary)
}

fn resolve_dotted_to_internal(
    dotted: &str,
    class_owner: &HashMap<String, (usize, String)>,
) -> Option<String> {
    let slash = dotted.replace('.', "/");
    if class_owner.contains_key(&slash) {
        return Some(slash);
    }
    // Nested class imports (`pkg.Outer.Inner` -> `pkg/Outer$Inner`).
    let mut candidate = slash;
    while let Some(pos) = candidate.rfind('/') {
        candidate.replace_range(pos..=pos, "$");
        if class_owner.contains_key(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn collect_used_classpath_jars(
    req: &JavaLibraryBuildRequest,
    active_cp: &[PathBuf],
    sources: &[PathBuf],
) -> io::Result<Vec<(PathBuf, DependencyKind)>> {
    if sources.is_empty() || active_cp.is_empty() {
        return Ok(Vec::new());
    }

    let mut class_files = Vec::new();
    collect_files_with_ext(&req.class_dir, "class", &mut class_files)?;
    let mut own_classes = HashSet::new();
    for cf in &class_files {
        if let Ok(rel) = cf.strip_prefix(&req.class_dir) {
            let s = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if let Some(stem) = s.strip_suffix(".class") {
                own_classes.insert(stem.to_string());
            }
        }
    }

    let mut jar_readers: Vec<Option<ZipReader>> = Vec::with_capacity(active_cp.len());
    // Maps internal class name (`"dep/Greeting"`) -> `(jar_idx_in_active_cp, zip_entry_name)`.
    let mut class_owner: HashMap<String, (usize, String)> = HashMap::new();
    let mut pkg_to_classes: HashMap<String, Vec<(String, usize, String)>> = HashMap::new();

    for (idx, jar_path) in active_cp.iter().enumerate() {
        let Ok(reader) = ZipReader::open(jar_path) else {
            jar_readers.push(None);
            continue;
        };
        for entry in reader.entries() {
            let Some(raw_stem) = entry.name.strip_suffix(".class") else {
                continue;
            };
            if raw_stem == "module-info" {
                continue;
            }
            let stem = if let Some(rest) = raw_stem.strip_prefix("META-INF/versions/") {
                let Some((_, versioned)) = rest.split_once('/') else {
                    continue;
                };
                versioned
            } else if raw_stem.starts_with("META-INF/") {
                continue;
            } else {
                raw_stem
            };
            if own_classes.contains(stem) {
                continue;
            }
            if !class_owner.contains_key(stem) {
                class_owner.insert(stem.to_string(), (idx, entry.name.clone()));
                if !stem.contains('$') {
                    let (pkg, simple) = match stem.rsplit_once('/') {
                        Some((p, s)) => (p.replace('/', "."), s.to_string()),
                        None => (String::new(), stem.to_string()),
                    };
                    pkg_to_classes
                        .entry(pkg)
                        .or_default()
                        .push((simple, idx, stem.to_string()));
                }
            }
        }
        jar_readers.push(Some(reader));
    }

    if class_owner.is_empty() {
        return Ok(Vec::new());
    }

    let src_summary = scan_sources_for_strict_deps(sources, &req.source_gen_dir)?;
    let mut jar_kind: HashMap<usize, DependencyKind> = HashMap::new();
    let mut bfs_classes: VecDeque<String> = VecDeque::new();
    let mut seen_bfs: HashSet<String> = HashSet::new();

    let mut mark_class = |cls: &str, kind: DependencyKind| {
        if let Some(&(jar_idx, _)) = class_owner.get(cls) {
            let entry = jar_kind.entry(jar_idx).or_insert(kind);
            if kind == DependencyKind::Explicit {
                *entry = DependencyKind::Explicit;
            }
            if seen_bfs.insert(cls.to_string()) {
                bfs_classes.push_back(cls.to_string());
            }
        }
    };

    let mut resolved_explicit_imports: HashSet<String> = HashSet::new();
    for imp in &src_summary.explicit_imports {
        if let Some(internal) = resolve_dotted_to_internal(imp, &class_owner) {
            mark_class(&internal, DependencyKind::Explicit);
            resolved_explicit_imports.insert(internal);
        }
    }
    for wpkg in &src_summary.wildcard_packages {
        if let Some(entries) = pkg_to_classes.get(wpkg) {
            for (simple, _idx, internal) in entries {
                if src_summary.identifiers.contains(simple) {
                    mark_class(internal, DependencyKind::Explicit);
                    resolved_explicit_imports.insert(internal.clone());
                }
            }
        }
    }

    for cf in &class_files {
        let own_stem = cf
            .strip_prefix(&req.class_dir)
            .ok()
            .map(|r| {
                r.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .and_then(|s| s.strip_suffix(".class").map(str::to_string))
            .unwrap_or_default();
        let is_exempt = src_summary.exempt_own_prefixes.iter().any(|prefix| {
            own_stem == *prefix
                || own_stem
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('$'))
        });

        let Ok(bytes) = std::fs::read(cf) else {
            continue;
        };
        let refs = parse_classfile_refs(&bytes);
        if is_exempt {
            for c in refs
                .const_classes
                .iter()
                .chain(refs.descriptor_classes.iter())
            {
                mark_class(c, DependencyKind::Implicit);
            }
            continue;
        }
        for c in &refs.const_classes {
            mark_class(c, DependencyKind::Explicit);
        }
        for c in &refs.descriptor_classes {
            let simple = c.rsplit('/').next().unwrap_or(c);
            let outer = simple.split('$').next().unwrap_or(simple);
            let kind = if resolved_explicit_imports.contains(c)
                || src_summary.identifiers.contains(simple)
                || src_summary.identifiers.contains(outer)
            {
                DependencyKind::Explicit
            } else {
                DependencyKind::Implicit
            };
            mark_class(c, kind);
        }
    }

    // Traverse API surface (supertypes, interfaces, member/generic/annotation signatures) of
    // referenced classpath classes to record `IMPLICIT` transitive jars (`ImplicitDependencyExtractor`).
    while let Some(cls) = bfs_classes.pop_front() {
        let Some((jar_idx, entry_name)) = class_owner.get(&cls).cloned() else {
            continue;
        };
        let Some(Some(reader)) = jar_readers.get(jar_idx) else {
            continue;
        };
        let Some(entry) = reader.get_entry(&entry_name) else {
            continue;
        };
        let Ok(bytes) = reader.read_entry_bytes(entry, false) else {
            continue;
        };
        let refs = parse_classfile_refs(&bytes);
        for sup in &refs.api_surface_classes {
            if let Some(&(sup_idx, _)) = class_owner.get(sup) {
                jar_kind.entry(sup_idx).or_insert(DependencyKind::Implicit);
                if seen_bfs.insert(sup.clone()) {
                    bfs_classes.push_back(sup.clone());
                }
            }
        }
    }

    // Emit used jars in `req.class_path` order (`DependencyModule.emitDependencyInformation`).
    let mut by_norm_path: HashMap<String, DependencyKind> = HashMap::new();
    for (idx, kind) in jar_kind {
        let norm = normalize_jdeps_path(&active_cp[idx].to_string_lossy());
        let slot = by_norm_path.entry(norm).or_insert(kind);
        if kind == DependencyKind::Explicit {
            *slot = DependencyKind::Explicit;
        }
    }

    let mut ordered = Vec::new();
    let mut emitted = HashSet::new();
    for cp_jar in &req.class_path {
        let norm = normalize_jdeps_path(&cp_jar.to_string_lossy());
        if let Some(&kind) = by_norm_path.get(&norm) {
            if emitted.insert(norm) {
                ordered.push((cp_jar.clone(), kind));
            }
        }
    }
    Ok(ordered)
}

/// Runs `SimpleJavaLibraryBuilder` or `ReducedClasspathJavaLibraryBuilder` (`BazelJavaBuilder`).
pub fn run_bazel_java_builder(
    javac_bin: &str,
    req: &JavaLibraryBuildRequest,
) -> io::Result<JavaBuildResult> {
    let full_len = req.class_path.len();
    let mut reduced_len = full_len;
    let mut fallback_used = false;

    let mut active_sources = prepare_source_compilation(req)?;
    let mut active_cp: Vec<PathBuf> = req.class_path.clone();
    let (mut status, mut diag_output, javac_succeeded) = match req.reduce_classpath_mode {
        ReduceClasspathMode::JavaBuilderReduced if !active_sources.is_empty() => {
            let reduced_cp =
                compute_strict_classpath(&req.class_path, &req.direct_jars, &req.deps_artifacts)?;
            reduced_len = reduced_cp.len();
            let (st1, out1, ok1) =
                invoke_system_javac(javac_bin, req, &reduced_cp, &active_sources, false)?;
            if should_fall_back(&st1, &out1) {
                fallback_used = true;
                active_sources = prepare_source_compilation(req)?;
                active_cp = req.class_path.clone();
                invoke_system_javac(javac_bin, req, &active_cp, &active_sources, false)?
            } else {
                active_cp = reduced_cp;
                (st1, out1, ok1)
            }
        }
        ReduceClasspathMode::BazelReduced => {
            let (st1, out1, ok1) =
                invoke_system_javac(javac_bin, req, &req.class_path, &active_sources, false)?;
            if should_fall_back(&st1, &out1) {
                (BlazeJavacStatus::RequiresFallback, String::new(), false)
            } else {
                (st1, out1, ok1)
            }
        }
        ReduceClasspathMode::None
        | ReduceClasspathMode::BazelFallback
        | ReduceClasspathMode::JavaBuilderReduced => {
            invoke_system_javac(javac_bin, req, &req.class_path, &active_sources, false)?
        }
    };

    let mut used_jars: Vec<(PathBuf, DependencyKind)> = Vec::new();
    if javac_succeeded && status != BlazeJavacStatus::RequiresFallback && !active_sources.is_empty()
    {
        used_jars = collect_used_classpath_jars(req, &active_cp, &active_sources)?;
        let unescaped_label = req.target_label.as_deref().map(|l| {
            l.strip_prefix("@@")
                .map(|r| format!("@{r}"))
                .unwrap_or_else(|| l.to_string())
        });
        match check_strict_java_deps(
            req.strict_java_deps,
            unescaped_label.as_deref(),
            &req.fix_deps_tool,
            &req.direct_jars,
            &used_jars,
        ) {
            Ok(warnings) => {
                for w in warnings {
                    diag_output.push_str(&w);
                    diag_output.push('\n');
                }
            }
            Err(errors) => {
                status = BlazeJavacStatus::Error;
                for e in errors {
                    diag_output.push_str(&e);
                    diag_output.push('\n');
                }
            }
        }
    }

    if status == BlazeJavacStatus::Ok {
        write_output_jars(req)?;
    }

    // Even when compilation fails, write the generated-sources jar if processors were active
    // (`SimpleJavaLibraryBuilder.run`: `if (!build.getProcessors().isEmpty())`).
    if status == BlazeJavacStatus::Ok
        || (!req.processor_names.is_empty() && status != BlazeJavacStatus::RequiresFallback)
    {
        write_gensrc_jar(req)?;
    }

    // Always emit `.jdeps` and `Manifest` in the `finally` path (`DependencyModule.emitDependencyInformation`).
    if let Some(deps_out) = &req.output_deps_proto {
        let contained = if javac_succeeded && status != BlazeJavacStatus::RequiresFallback {
            collect_contained_packages(&req.class_dir)?
        } else {
            Vec::new()
        };
        let deps: Vec<Dependency> = if status != BlazeJavacStatus::RequiresFallback {
            used_jars
                .iter()
                .map(|(p, kind)| Dependency {
                    path: p.to_string_lossy().into_owned(),
                    kind: *kind,
                    locations: vec![],
                })
                .collect()
        } else {
            Vec::new()
        };
        let jdeps = Jdeps {
            paths: deps.iter().map(|d| d.path.clone()).collect(),
            dependencies: deps,
            rule_label: req.target_label.clone(),
            success: Some(status == BlazeJavacStatus::Ok),
            contained_packages: contained,
            requires_reduced_classpath_fallback: status == BlazeJavacStatus::RequiresFallback,
        };
        if let Some(parent) = deps_out.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(deps_out, encode_jdeps(&jdeps))?;
    }

    if let Some(mf_out) = &req.output_manifest_proto {
        let mut units = Vec::new();
        for src in &active_sources {
            let rel = src
                .strip_prefix(&req.work_dir)
                .unwrap_or(src)
                .to_string_lossy()
                .into_owned();
            units.push(scan_java_compilation_unit(src, rel, false));
        }
        let mut gen_sources = Vec::new();
        collect_files_with_ext(&req.source_gen_dir, "java", &mut gen_sources)?;
        for gs in &gen_sources {
            let rel = gs
                .strip_prefix(&req.source_gen_dir)
                .unwrap_or(gs)
                .to_string_lossy()
                .into_owned();
            units.push(scan_java_compilation_unit(gs, rel, true));
        }
        let mf = CompilationManifest {
            compilation_units: units,
        };
        if let Some(parent) = mf_out.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(mf_out, encode_compilation_manifest(&mf))?;
    }

    if status == BlazeJavacStatus::RequiresFallback {
        // BAZEL_REDUCED exits 0 with `requires_reduced_classpath_fallback = true` on `.jdeps`.
        diag_output.clear();
    } else if status != BlazeJavacStatus::Ok && status != BlazeJavacStatus::Cancelled {
        status = BlazeJavacStatus::Error;
    }

    Ok(JavaBuildResult {
        status,
        output: diag_output,
        statistics_classpath_len: full_len,
        statistics_reduced_classpath_len: reduced_len,
        transitive_classpath_fallback: fallback_used,
    })
}

/// Runs `VanillaJavaBuilder`: uses standard JDK javac options (`normalize_vanilla`), ignores
/// strict-deps / reduced-classpath flags, writes stub `.jdeps` (`rule_label` + `success` only,
/// never `requires_reduced_classpath_fallback`), and writes an empty `CompilationManifest`.
pub fn run_vanilla_java_builder(
    javac_bin: &str,
    req: &JavaLibraryBuildRequest,
) -> io::Result<JavaBuildResult> {
    let sources = prepare_source_compilation(req)?;
    let (status, diag_output, _javac_succeeded) =
        invoke_system_javac(javac_bin, req, &req.class_path, &sources, true)?;

    if status == BlazeJavacStatus::Ok {
        write_output_jars(req)?;
    }
    write_gensrc_jar(req)?;

    if let Some(deps_out) = &req.output_deps_proto {
        let jdeps = Jdeps {
            dependencies: Vec::new(),
            paths: Vec::new(),
            rule_label: req.target_label.clone(),
            success: Some(status == BlazeJavacStatus::Ok),
            contained_packages: Vec::new(),
            requires_reduced_classpath_fallback: false,
        };
        if let Some(parent) = deps_out.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(deps_out, encode_jdeps(&jdeps))?;
    }

    if let Some(mf_out) = &req.output_manifest_proto {
        if let Some(parent) = mf_out.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(
            mf_out,
            encode_compilation_manifest(&CompilationManifest::default()),
        )?;
    }

    let _ = BTreeMap::<String, String>::new();
    Ok(JavaBuildResult {
        status,
        output: diag_output,
        statistics_classpath_len: req.class_path.len(),
        statistics_reduced_classpath_len: req.class_path.len(),
        transitive_classpath_fallback: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_util::zip::{CompressionMethod, DosDateTime, ZipFileEntry, ZipWriter};

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zel-buildjar-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn options_parser_expands_recursive_param_files_unescapes_double_at_and_extracts_sourcepath() {
        let d = tmp_dir("optparse");
        std::fs::write(
            d.join("inner.params"),
            "--target_label\r@@@repo//pkg:lib\r\n\r\n--compress_jar\n",
        )
        .unwrap();
        std::fs::write(
            d.join("outer.params"),
            "--output\nout/lib.jar\n@inner.params\n--javacopts\n-g\n-sourcepath\nsrc/a:src/b\n--release=17\n--\n",
        )
        .unwrap();

        let parsed = OptionsParser::parse_with_path_separator(&d, ["@outer.params"], ':').unwrap();
        assert_eq!(parsed.output_jar.as_deref(), Some("out/lib.jar"));
        assert_eq!(parsed.target_label.as_deref(), Some("@@repo//pkg:lib"));
        assert!(parsed.compress_jar);
        assert_eq!(parsed.source_path, vec!["src/a", "src/b"]);
        assert_eq!(parsed.javac_opts, vec!["-g", "--release=17"]);

        // Rejects commas in --processors and missing `--` after --javacopts.
        assert!(OptionsParser::parse(&d, ["--processors", "a.Proc,b.Proc"]).is_err());
        assert!(OptionsParser::parse(&d, ["--javacopts", "-g"]).is_err());
        assert!(OptionsParser::parse(&d, ["--bogus_flag"]).is_err());

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn build_request_dedupes_processors_forces_proc_none_and_rejects_processor_in_javacopts() {
        let d = tmp_dir("req");
        let opts = OptionsParser::parse(
            &d,
            [
                "--output",
                "out.jar",
                "--processors",
                "com.ProcA",
                "com.ProcB",
                "com.ProcA",
                "--processorpath",
                "p.jar",
                "--reduce_classpath_mode",
                "BAZEL_REDUCED",
            ],
        )
        .unwrap();
        let req = JavaLibraryBuildRequest::from_options(opts, &d, false, Some(7)).unwrap();
        assert!(!req.fail_fast(), "processors disable fail_fast");
        assert_eq!(req.processor_javac_args(false), vec!["-proc:none"]);
        assert_eq!(
            req.processor_javac_args(true)[..4],
            [
                "-processorpath".to_string(),
                d.join("p.jar").to_string_lossy().into_owned(),
                "-processor".to_string(),
                "com.ProcA,com.ProcB".to_string(),
            ]
        );

        let bad = OptionsParser::parse(
            &d,
            [
                "--output",
                "out.jar",
                "--javacopts",
                "-processor",
                "x.P",
                "--",
            ],
        )
        .unwrap();
        assert!(JavaLibraryBuildRequest::from_options(bad, &d, false, None).is_err());

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn genclass_filters_handwritten_vs_generated_and_inner_classes() {
        let d = tmp_dir("genclass");
        let mf = CompilationManifest {
            compilation_units: vec![
                CompilationUnit {
                    path: Some("pkg/Hand.java".into()),
                    pkg: Some("com.example".into()),
                    generated_by_annotation_processor: false,
                    top_level: vec!["Hand".into()],
                },
                CompilationUnit {
                    path: Some("pkg/Auto_Hand.java".into()),
                    pkg: Some("com.example".into()),
                    generated_by_annotation_processor: true,
                    top_level: vec!["Auto_Hand".into()],
                },
            ],
        };
        let mf_bytes = encode_compilation_manifest(&mf);
        assert_eq!(parse_compilation_manifest(&mf_bytes).unwrap(), mf);
        let mf_path = d.join("lib.manifest_proto");
        std::fs::write(&mf_path, &mf_bytes).unwrap();

        let mut zw = ZipWriter::new(false);
        for (name, body) in [
            ("com/example/Hand.class", b"hand".as_slice()),
            ("com/example/Hand$Inner$1.class", b"hand_inner"),
            ("com/example/Auto_Hand.class", b"gen"),
            ("com/example/Auto_Hand$Builder.class", b"gen_inner"),
            ("com/example/UnknownGenerated.class", b"unknown"),
            ("com/example/resource.txt", b"res"),
        ] {
            let mut e = ZipFileEntry::new(name);
            e.method = CompressionMethod::Stored;
            e.time = Some(DosDateTime::DOS_EPOCH);
            zw.write_entry(e, body).unwrap();
        }
        let class_jar = d.join("class.jar");
        std::fs::write(&class_jar, zw.finish().unwrap()).unwrap();

        let out_jar = d.join("gen.jar");
        let gopts = GenClassOptions::parse(
            &d,
            [
                "--manifest_proto",
                "lib.manifest_proto",
                "--class_jar",
                "class.jar",
                "--output_jar",
                "gen.jar",
            ],
        )
        .unwrap();
        GenClass::execute(&gopts).unwrap();

        let r = ZipReader::open(&out_jar).unwrap();
        let names: Vec<&str> = r.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "META-INF/",
                "META-INF/MANIFEST.MF",
                "com/example/Auto_Hand$Builder.class",
                "com/example/Auto_Hand.class",
                "com/example/UnknownGenerated.class",
            ]
        );
        let cls = r.get_entry("com/example/Auto_Hand.class").unwrap();
        assert_eq!(cls.method, CompressionMethod::Deflated);
        assert_eq!(cls.time, Some(DosDateTime::JAR_DEFAULT_CLASS));

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn compute_strict_classpath_requires_rule_label_and_skips_unused() {
        let d = tmp_dir("strictcp");
        let bad_jdeps = d.join("no_label.jdeps");
        std::fs::write(
            &bad_jdeps,
            encode_jdeps(&Jdeps {
                dependencies: vec![Dependency {
                    path: "b.jar".into(),
                    kind: DependencyKind::Explicit,
                    locations: vec![],
                }],
                paths: vec!["b.jar".into()],
                rule_label: None,
                ..Default::default()
            }),
        )
        .unwrap();
        let full = vec![
            PathBuf::from("a.jar"),
            PathBuf::from("b.jar"),
            PathBuf::from("c.jar"),
        ];
        let direct = vec![PathBuf::from("a.jar")];
        assert!(compute_strict_classpath(&full, &direct, &[bad_jdeps]).is_err());

        let ok_jdeps = d.join("ok.jdeps");
        std::fs::write(
            &ok_jdeps,
            encode_jdeps(&Jdeps {
                dependencies: vec![
                    Dependency {
                        path: "./b.jar".into(),
                        kind: DependencyKind::Unused,
                        locations: vec![],
                    },
                    Dependency {
                        path: "./c.jar".into(),
                        kind: DependencyKind::Implicit,
                        locations: vec![],
                    },
                ],
                paths: vec!["./c.jar".into()],
                rule_label: Some("//pkg:a".into()),
                success: Some(true),
                ..Default::default()
            }),
        )
        .unwrap();
        let shrunk = compute_strict_classpath(&full, &direct, &[ok_jdeps]).unwrap();
        assert_eq!(shrunk, vec![PathBuf::from("a.jar"), PathBuf::from("c.jar")]);

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn strict_java_deps_reads_jar_owner_and_formats_fix_command() {
        let d = tmp_dir("sjd");
        let indirect_jar = d.join("libindirect.jar");
        let mut jc = JarCreator::new(&indirect_jar);
        jc.set_jar_owner(Some("//dep:indirect".into()), None);
        jc.add_bytes_entry("dep/Indirect.class", b"cls");
        jc.execute().unwrap();

        let owner = read_jar_owner(&indirect_jar).unwrap();
        assert_eq!(owner.label.as_deref(), Some("//dep:indirect"));

        let err = check_strict_java_deps(
            StrictDepsMode::Error,
            Some("//app:main"),
            "add_dep",
            &[],
            &[(indirect_jar, DependencyKind::Explicit)],
        )
        .unwrap_err();
        assert!(err.iter().any(|m| m.contains("//dep:indirect")), "{err:?}");
        assert!(
            err.iter()
                .any(|m| m.contains("add_dep //dep:indirect //app:main")),
            "{err:?}"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn java_builder_compiles_with_system_javac_and_handles_javabuilder_reduced_fallback() {
        if Command::new("javac").arg("-version").output().is_err() {
            return;
        }
        let d = tmp_dir("javac");
        // 1. Compile indirect library `dep/Greeting.java` -> `libdep.jar`.
        std::fs::create_dir_all(d.join("dep")).unwrap();
        std::fs::write(
            d.join("dep/Greeting.java"),
            "package dep; public class Greeting { public static String hi() { return \"hi\"; } }\n",
        )
        .unwrap();
        let dep_opts = OptionsParser::parse(
            &d,
            [
                "--output",
                "libdep.jar",
                "--output_deps_proto",
                "libdep.jdeps",
                "--output_manifest_proto",
                "libdep.manifest",
                "--target_label",
                "//dep:greeting",
                "--compress_jar",
                "--sources",
                "dep/Greeting.java",
            ],
        )
        .unwrap();
        let dep_req = JavaLibraryBuildRequest::from_options(dep_opts, &d, false, None).unwrap();
        let dep_res = run_bazel_java_builder("javac", &dep_req).unwrap();
        assert!(dep_res.is_ok(), "{}", dep_res.output);

        // 2. Write a direct jar whose `.jdeps` does NOT mention `libdep.jar`, then compile `app/Use.java`
        // under `JAVABUILDER_REDUCED` with full classpath `[libdir.jar, libdep.jar]`: first attempt fails
        // on shrunk classpath `[libdir.jar]` and automatically falls back in-process to full classpath!
        let mut dir_jc = JarCreator::new(d.join("libdir.jar"));
        dir_jc.set_jar_owner(Some("//dir:dir".into()), None);
        dir_jc.execute().unwrap();
        std::fs::write(
            d.join("libdir.jdeps"),
            encode_jdeps(&Jdeps {
                rule_label: Some("//dir:dir".into()),
                success: Some(true),
                ..Default::default()
            }),
        )
        .unwrap();

        std::fs::create_dir_all(d.join("app")).unwrap();
        std::fs::write(
            d.join("app/Use.java"),
            "package app; public class Use { public static String call() { return dep.Greeting.hi(); } }\n",
        )
        .unwrap();
        let app_opts = OptionsParser::parse(
            &d,
            [
                "--output",
                "libapp.jar",
                "--output_deps_proto",
                "libapp.jdeps",
                "--output_manifest_proto",
                "libapp.manifest",
                "--target_label",
                "//app:use",
                "--compress_jar",
                "--sources",
                "app/Use.java",
                "--classpath",
                "libdir.jar",
                "libdep.jar",
                "--direct_dependencies",
                "libdir.jar",
                "--deps_artifacts",
                "libdir.jdeps",
                "--reduce_classpath_mode",
                "JAVABUILDER_REDUCED",
            ],
        )
        .unwrap();
        let app_req = JavaLibraryBuildRequest::from_options(app_opts, &d, false, None).unwrap();
        let app_res = run_bazel_java_builder("javac", &app_req).unwrap();
        assert!(app_res.is_ok(), "{}", app_res.output);
        assert!(app_res.transitive_classpath_fallback);
        assert_eq!(app_res.statistics_reduced_classpath_len, 1);
        assert_eq!(app_res.statistics_classpath_len, 2);

        let app_jar = ZipReader::open(&d.join("libapp.jar")).unwrap();
        assert!(app_jar.get_entry("app/Use.class").is_some());
        let app_jdeps = parse_jdeps(&std::fs::read(d.join("libapp.jdeps")).unwrap()).unwrap();
        assert_eq!(app_jdeps.contained_packages, vec!["app"]);
        assert_eq!(app_jdeps.success, Some(true));

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn werror_promotes_sunapi_and_respects_category_exclusions() {
        if Command::new("javac").arg("-version").output().is_err() {
            return;
        }
        let d = tmp_dir("werror_sunapi");
        std::fs::create_dir_all(d.join("p")).unwrap();
        std::fs::write(
            d.join("p/UnsafeUser.java"),
            "package p; public class UnsafeUser { sun.misc.Unsafe u; }\n",
        )
        .unwrap();

        // 1. Bare `-Werror` normalizes to `-Werror:all` and promotes `compiler.warn.sun.proprietary` (`sunapi`).
        let opts_err = OptionsParser::parse(
            &d,
            [
                "--output",
                "u1.jar",
                "--sources",
                "p/UnsafeUser.java",
                "--javacopts",
                "-Werror",
                "--",
            ],
        )
        .unwrap();
        let req_err = JavaLibraryBuildRequest::from_options(opts_err, &d, false, None).unwrap();
        let res_err = run_bazel_java_builder("javac", &req_err).unwrap();
        assert!(
            !res_err.is_ok(),
            "expected -Werror failure: {}",
            res_err.output
        );
        assert!(
            res_err
                .output
                .contains("error: warnings found and -Werror specified"),
            "{}",
            res_err.output
        );
        assert!(!d.join("u1.jar").exists());

        // 2. `-Werror` followed by `-Werror:-sunapi` disables `sunapi` promotion and succeeds!
        let opts_ok = OptionsParser::parse(
            &d,
            [
                "--output",
                "u2.jar",
                "--sources",
                "p/UnsafeUser.java",
                "--javacopts",
                "-Werror",
                "-Werror:-sunapi",
                "--",
            ],
        )
        .unwrap();
        let req_ok = JavaLibraryBuildRequest::from_options(opts_ok, &d, false, None).unwrap();
        let res_ok = run_bazel_java_builder("javac", &req_ok).unwrap();
        assert!(res_ok.is_ok(), "{}", res_ok.output);
        assert!(d.join("u2.jar").exists());

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn strict_deps_enforces_indirect_errors_and_jdeps_tracks_explicit_implicit_and_unused() {
        if Command::new("javac").arg("-version").output().is_err() {
            return;
        }
        let d = tmp_dir("strict_e2e");
        std::fs::create_dir_all(d.join("ind")).unwrap();
        std::fs::create_dir_all(d.join("dir")).unwrap();
        std::fs::create_dir_all(d.join("unused")).unwrap();
        std::fs::create_dir_all(d.join("app")).unwrap();

        // 1. Build `libind.jar` (`ind.Base`).
        std::fs::write(
            d.join("ind/Base.java"),
            "package ind; public class Base {}\n",
        )
        .unwrap();
        let ind_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libind.jar",
                    "--target_label",
                    "//ind:base",
                    "--sources",
                    "ind/Base.java",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        assert!(run_bazel_java_builder("javac", &ind_req).unwrap().is_ok());

        // 2. Build `libdir.jar` (`dir.Sub extends ind.Base`) and `libunused.jar` (`unused.Extra`).
        std::fs::write(
            d.join("dir/Sub.java"),
            "package dir; public class Sub extends ind.Base {}\n",
        )
        .unwrap();
        let dir_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libdir.jar",
                    "--output_deps_proto",
                    "libdir.jdeps",
                    "--target_label",
                    "//dir:sub",
                    "--sources",
                    "dir/Sub.java",
                    "--classpath",
                    "libind.jar",
                    "--direct_dependencies",
                    "libind.jar",
                    "--strict_java_deps",
                    "ERROR",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        assert!(run_bazel_java_builder("javac", &dir_req).unwrap().is_ok());

        std::fs::write(
            d.join("unused/Extra.java"),
            "package unused; public class Extra {}\n",
        )
        .unwrap();
        let unused_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libunused.jar",
                    "--target_label",
                    "//unused:extra",
                    "--sources",
                    "unused/Extra.java",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        assert!(run_bazel_java_builder("javac", &unused_req)
            .unwrap()
            .is_ok());

        // 3. Direct reference to `ind.Base` without `libind.jar` in `--direct_dependencies` fails
        // under `--strict_java_deps ERROR` and warns under `--strict_java_deps WARN`.
        std::fs::write(
            d.join("app/Bad.java"),
            "package app; public class Bad { public ind.Base b = new ind.Base(); }\n",
        )
        .unwrap();
        let bad_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libbad.jar",
                    "--output_deps_proto",
                    "libbad.jdeps",
                    "--target_label",
                    "//app:bad",
                    "--sources",
                    "app/Bad.java",
                    "--classpath",
                    "libdir.jar",
                    "libind.jar",
                    "--direct_dependencies",
                    "libdir.jar",
                    "--strict_java_deps",
                    "ERROR",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        let bad_res = run_bazel_java_builder("javac", &bad_req).unwrap();
        assert!(!bad_res.is_ok(), "expected strict deps failure");
        assert!(
            bad_res.output.contains("[error] [strict]")
                && bad_res.output.contains("add_dep //ind:base //app:bad"),
            "{}",
            bad_res.output
        );
        assert!(!d.join("libbad.jar").exists());
        let bad_jdeps = parse_jdeps(&std::fs::read(d.join("libbad.jdeps")).unwrap()).unwrap();
        assert_eq!(bad_jdeps.success, Some(false));
        assert_eq!(bad_jdeps.contained_packages, vec!["app"]);
        assert_eq!(
            bad_jdeps.dependencies.len(),
            1,
            "{:?}",
            bad_jdeps.dependencies
        );
        assert!(bad_jdeps.dependencies[0].path.ends_with("libind.jar"));
        assert_eq!(bad_jdeps.dependencies[0].kind, DependencyKind::Explicit);

        let warn_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libwarn.jar",
                    "--target_label",
                    "//app:bad",
                    "--sources",
                    "app/Bad.java",
                    "--classpath",
                    "libdir.jar",
                    "libind.jar",
                    "--direct_dependencies",
                    "libdir.jar",
                    "--strict_java_deps",
                    "WARN",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        let warn_res = run_bazel_java_builder("javac", &warn_req).unwrap();
        assert!(warn_res.is_ok(), "{}", warn_res.output);
        assert!(
            warn_res.output.contains("[warning] [strict]"),
            "{}",
            warn_res.output
        );
        assert!(d.join("libwarn.jar").exists());

        // 4. Referencing only `dir.Sub` (`Sub extends ind.Base`) with unused `libunused.jar` in
        // `--direct_dependencies` succeeds under `ERROR`, marks `libdir.jar` EXPLICIT + `libind.jar`
        // IMPLICIT, omits `libunused.jar`, and downstream `compute_strict_classpath` drops `libunused.jar`.
        std::fs::write(
            d.join("app/Good.java"),
            "package app; public class Good { public dir.Sub s = new dir.Sub(); }\n",
        )
        .unwrap();
        let good_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libgood.jar",
                    "--output_deps_proto",
                    "libgood.jdeps",
                    "--target_label",
                    "//app:good",
                    "--sources",
                    "app/Good.java",
                    "--classpath",
                    "libdir.jar",
                    "libunused.jar",
                    "libind.jar",
                    "--direct_dependencies",
                    "libdir.jar",
                    "libunused.jar",
                    "--strict_java_deps",
                    "ERROR",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        let good_res = run_bazel_java_builder("javac", &good_req).unwrap();
        assert!(good_res.is_ok(), "{}", good_res.output);
        let good_jdeps = parse_jdeps(&std::fs::read(d.join("libgood.jdeps")).unwrap()).unwrap();
        assert_eq!(
            good_jdeps.dependencies.len(),
            2,
            "{:?}",
            good_jdeps.dependencies
        );
        assert!(good_jdeps.dependencies[0].path.ends_with("libdir.jar"));
        assert_eq!(good_jdeps.dependencies[0].kind, DependencyKind::Explicit);
        assert!(good_jdeps.dependencies[1].path.ends_with("libind.jar"));
        assert_eq!(good_jdeps.dependencies[1].kind, DependencyKind::Implicit);

        let full_downstream = vec![
            d.join("libgood.jar"),
            d.join("libdir.jar"),
            d.join("libunused.jar"),
            d.join("libind.jar"),
        ];
        let shrunk = compute_strict_classpath(
            &full_downstream,
            &[d.join("libgood.jar")],
            &[d.join("libgood.jdeps")],
        )
        .unwrap();
        assert_eq!(
            shrunk,
            vec![
                d.join("libgood.jar"),
                d.join("libdir.jar"),
                d.join("libind.jar"),
            ]
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn strict_deps_error_jdeps_retains_discovered_dependencies_and_packages() {
        if Command::new("javac").arg("-version").output().is_err() {
            return;
        }
        let d = tmp_dir("strict_jdeps_retained");
        std::fs::create_dir_all(d.join("trans")).unwrap();
        std::fs::create_dir_all(d.join("viol")).unwrap();
        std::fs::create_dir_all(d.join("dir")).unwrap();
        std::fs::create_dir_all(d.join("app")).unwrap();

        // `libtrans.jar`: transitive superclass of `dir.DirectSub`.
        std::fs::write(
            d.join("trans/TransBase.java"),
            "package trans; public class TransBase {}\n",
        )
        .unwrap();
        let trans_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libtrans.jar",
                    "--target_label",
                    "//trans:base",
                    "--sources",
                    "trans/TransBase.java",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        assert!(run_bazel_java_builder("javac", &trans_req).unwrap().is_ok());

        // `libviol.jar`: indirect dependency directly referenced by `app.Caller` (strict-deps violation).
        std::fs::write(
            d.join("viol/ViolType.java"),
            "package viol; public class ViolType {}\n",
        )
        .unwrap();
        let viol_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libviol.jar",
                    "--target_label",
                    "//viol:type",
                    "--sources",
                    "viol/ViolType.java",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        assert!(run_bazel_java_builder("javac", &viol_req).unwrap().is_ok());

        // `libdir.jar`: direct dependency extending `trans.TransBase`.
        std::fs::write(
            d.join("dir/DirectSub.java"),
            "package dir; public class DirectSub extends trans.TransBase {}\n",
        )
        .unwrap();
        let dir_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libdir.jar",
                    "--target_label",
                    "//dir:sub",
                    "--sources",
                    "dir/DirectSub.java",
                    "--classpath",
                    "libtrans.jar",
                    "--direct_dependencies",
                    "libtrans.jar",
                    "--strict_java_deps",
                    "ERROR",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        assert!(run_bazel_java_builder("javac", &dir_req).unwrap().is_ok());

        // `app.Caller` uses `dir.DirectSub` (direct -> EXPLICIT, pulling `libtrans.jar` -> IMPLICIT)
        // AND `viol.ViolType` (indirect -> EXPLICIT + strict-deps ERROR).
        std::fs::write(
            d.join("app/Caller.java"),
            "package app; public class Caller { public dir.DirectSub s = new dir.DirectSub(); public viol.ViolType v = new viol.ViolType(); }\n",
        )
        .unwrap();
        let app_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libcaller.jar",
                    "--output_deps_proto",
                    "libcaller.jdeps",
                    "--target_label",
                    "//app:caller",
                    "--sources",
                    "app/Caller.java",
                    "--classpath",
                    "libdir.jar",
                    "libviol.jar",
                    "libtrans.jar",
                    "--direct_dependencies",
                    "libdir.jar",
                    "--strict_java_deps",
                    "ERROR",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        let res = run_bazel_java_builder("javac", &app_req).unwrap();
        assert!(!res.is_ok(), "expected strict-deps ERROR");
        assert!(!d.join("libcaller.jar").exists());

        let jdeps = parse_jdeps(&std::fs::read(d.join("libcaller.jdeps")).unwrap()).unwrap();
        assert_eq!(jdeps.success, Some(false));
        assert_eq!(jdeps.rule_label.as_deref(), Some("//app:caller"));
        assert_eq!(jdeps.contained_packages, vec!["app"]);
        assert_eq!(jdeps.dependencies.len(), 3, "{:?}", jdeps.dependencies);
        assert!(jdeps.dependencies[0].path.ends_with("libdir.jar"));
        assert_eq!(jdeps.dependencies[0].kind, DependencyKind::Explicit);
        assert!(jdeps.dependencies[1].path.ends_with("libviol.jar"));
        assert_eq!(jdeps.dependencies[1].kind, DependencyKind::Explicit);
        assert!(jdeps.dependencies[2].path.ends_with("libtrans.jar"));
        assert_eq!(jdeps.dependencies[2].kind, DependencyKind::Implicit);

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn generic_signature_type_parameter_bounds_extract_classes_and_enforce_strict_deps() {
        // 1. Direct JVMS §4.7.9.1 `Signature` grammar checks on formal `TypeParameters` vs `TypeVariableSignature`.
        let mut s1 = HashSet::new();
        collect_descriptor_types("<T:Lcom/lib/Bar;>Ljava/lang/Object;", &mut s1);
        assert_eq!(
            s1,
            HashSet::from(["com/lib/Bar".to_string(), "java/lang/Object".to_string()])
        );

        let mut s2 = HashSet::new();
        collect_descriptor_types(
            "<ListType::Ljava/io/Serializable;:Ljava/lang/Comparable<TListType;>;>Ljava/lang/Object;",
            &mut s2,
        );
        assert_eq!(
            s2,
            HashSet::from([
                "java/io/Serializable".to_string(),
                "java/lang/Comparable".to_string(),
                "java/lang/Object".to_string(),
            ])
        );

        let mut s3 = HashSet::new();
        collect_descriptor_types(
            "<T:[Lcom/lib/Elem;U:Lcom/lib/Second;>([TT;Lcom/lib/Outer<Lcom/lib/Arg;>.Middle.Inner<+Lcom/lib/Wild;>;)TT;^Lcom/lib/CheckedEx;^TErr;",
            &mut s3,
        );
        assert_eq!(
            s3,
            HashSet::from([
                "com/lib/Elem".to_string(),
                "com/lib/Second".to_string(),
                "com/lib/Outer".to_string(),
                "com/lib/Arg".to_string(),
                "com/lib/Outer$Middle".to_string(),
                "com/lib/Outer$Middle$Inner".to_string(),
                "com/lib/Wild".to_string(),
                "com/lib/CheckedEx".to_string(),
            ])
        );

        // 2. End-to-end `javac` + `run_bazel_java_builder` check: `class Box<T extends com.lib.Bar> {}`
        // (with no `import` and no fields/methods mentioning `T` or `Bar`, so `com/lib/Bar` appears
        // ONLY inside the `ClassSignature` `<T:Lcom/lib/Bar;>Ljava/lang/Object;`!) must fail
        // `--strict_java_deps ERROR` when `libbar.jar` is indirect, and must propagate `libbar.jar`
        // as `IMPLICIT` when referenced through a direct generic holder class.
        if Command::new("javac").arg("-version").output().is_err() {
            return;
        }
        let d = tmp_dir("generic_bound_strict");
        std::fs::create_dir_all(d.join("com/lib")).unwrap();
        std::fs::create_dir_all(d.join("dir")).unwrap();
        std::fs::create_dir_all(d.join("app")).unwrap();

        std::fs::write(
            d.join("com/lib/Bar.java"),
            "package com.lib; public class Bar {}\n",
        )
        .unwrap();
        let bar_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libbar.jar",
                    "--target_label",
                    "//com/lib:bar",
                    "--sources",
                    "com/lib/Bar.java",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        assert!(run_bazel_java_builder("javac", &bar_req).unwrap().is_ok());

        // Indirect type used exclusively in formal type parameter bound `<T extends com.lib.Bar>`.
        std::fs::write(
            d.join("app/Box.java"),
            "package app; public class Box<T extends com.lib.Bar> {}\n",
        )
        .unwrap();
        let box_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libbox.jar",
                    "--output_deps_proto",
                    "libbox.jdeps",
                    "--target_label",
                    "//app:box",
                    "--sources",
                    "app/Box.java",
                    "--classpath",
                    "libbar.jar",
                    "--strict_java_deps",
                    "ERROR",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        let box_res = run_bazel_java_builder("javac", &box_req).unwrap();
        assert!(
            !box_res.is_ok(),
            "expected strict-deps failure on type-parameter bound: {}",
            box_res.output
        );
        assert!(
            box_res.output.contains("[error] [strict]") && box_res.output.contains("//com/lib:bar"),
            "{}",
            box_res.output
        );
        let box_jdeps = parse_jdeps(&std::fs::read(d.join("libbox.jdeps")).unwrap()).unwrap();
        assert_eq!(box_jdeps.success, Some(false));
        assert_eq!(box_jdeps.dependencies.len(), 1);
        assert!(box_jdeps.dependencies[0].path.ends_with("libbar.jar"));
        assert_eq!(box_jdeps.dependencies[0].kind, DependencyKind::Explicit);

        // Direct generic holder `Holder<T extends com.lib.Bar>`: downstream consumer referencing
        // `Holder<?>` records `libholder.jar` as EXPLICIT and `libbar.jar` as IMPLICIT.
        std::fs::write(
            d.join("dir/Holder.java"),
            "package dir; public class Holder<T extends com.lib.Bar> {}\n",
        )
        .unwrap();
        let holder_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libholder.jar",
                    "--target_label",
                    "//dir:holder",
                    "--sources",
                    "dir/Holder.java",
                    "--classpath",
                    "libbar.jar",
                    "--direct_dependencies",
                    "libbar.jar",
                    "--strict_java_deps",
                    "ERROR",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        assert!(run_bazel_java_builder("javac", &holder_req)
            .unwrap()
            .is_ok());

        std::fs::write(
            d.join("app/UseHolder.java"),
            "package app; public class UseHolder { public dir.Holder<?> h; }\n",
        )
        .unwrap();
        let use_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libuse.jar",
                    "--output_deps_proto",
                    "libuse.jdeps",
                    "--target_label",
                    "//app:use",
                    "--sources",
                    "app/UseHolder.java",
                    "--classpath",
                    "libholder.jar",
                    "libbar.jar",
                    "--direct_dependencies",
                    "libholder.jar",
                    "--strict_java_deps",
                    "ERROR",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        let use_res = run_bazel_java_builder("javac", &use_req).unwrap();
        assert!(use_res.is_ok(), "{}", use_res.output);
        let use_jdeps = parse_jdeps(&std::fs::read(d.join("libuse.jdeps")).unwrap()).unwrap();
        assert_eq!(use_jdeps.success, Some(true));
        assert_eq!(
            use_jdeps.dependencies.len(),
            2,
            "{:?}",
            use_jdeps.dependencies
        );
        assert!(use_jdeps.dependencies[0].path.ends_with("libholder.jar"));
        assert_eq!(use_jdeps.dependencies[0].kind, DependencyKind::Explicit);
        assert!(use_jdeps.dependencies[1].path.ends_with("libbar.jar"));
        assert_eq!(use_jdeps.dependencies[1].kind, DependencyKind::Implicit);

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn empty_sourcepath_prevents_compiling_sources_bundled_in_classpath_jars() {
        if Command::new("javac").arg("-version").output().is_err() {
            return;
        }
        let d = tmp_dir("empty-sourcepath");
        std::fs::create_dir_all(d.join("dep")).unwrap();
        std::fs::create_dir_all(d.join("app")).unwrap();
        std::fs::create_dir_all(d.join("modsrc/pkg")).unwrap();

        // Build a dependency jar that bundles an uncompiled `dep/BundledSource.java` alongside
        // a compiled `dep/CompiledApi.class`.
        std::fs::write(
            d.join("dep/CompiledApi.java"),
            "package dep; public class CompiledApi {}\n",
        )
        .unwrap();
        let api_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libapi.jar",
                    "--target_label",
                    "//dep:api",
                    "--sources",
                    "dep/CompiledApi.java",
                    "--strict_java_deps",
                    "OFF",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        assert!(run_bazel_java_builder("javac", &api_req).unwrap().is_ok());

        let uncompiled_src = d.join("dep/BundledSource.java");
        std::fs::write(
            &uncompiled_src,
            "package dep; public class BundledSource {}\n",
        )
        .unwrap();
        let mut dep_jc = JarCreator::new(d.join("dep_with_sources.jar"));
        dep_jc.set_jar_owner(Some("//dep:with_sources".into()), None);
        let api_bytes = std::fs::read(d.join("libapi.jar")).unwrap();
        let api_zip = zel_util::zip::ZipReader::from_bytes(api_bytes).unwrap();
        for entry in api_zip.entries() {
            if entry.name.ends_with(".class") {
                let class_path = d.join(&entry.name);
                if let Some(par) = class_path.parent() {
                    std::fs::create_dir_all(par).unwrap();
                }
                std::fs::write(&class_path, api_zip.read_entry_bytes(entry, true).unwrap())
                    .unwrap();
                assert!(dep_jc.add_entry(&entry.name, class_path));
            }
        }
        assert!(dep_jc.add_entry("dep/BundledSource.java", uncompiled_src));
        dep_jc.execute().unwrap();

        // Referencing the uncompiled `dep.BundledSource` with empty `--sourcepath` must fail in
        // both `BazelJavaBuilder` and `VanillaJavaBuilder` instead of compiling the `.java` entry
        // out of `dep_with_sources.jar` into the output jar.
        std::fs::write(
            d.join("app/UseBundledSource.java"),
            "package app; public class UseBundledSource { public dep.BundledSource s; }\n",
        )
        .unwrap();
        let bad_opts = OptionsParser::parse(
            &d,
            [
                "--output",
                "libbad.jar",
                "--target_label",
                "//app:bad",
                "--sources",
                "app/UseBundledSource.java",
                "--classpath",
                "dep_with_sources.jar",
                "--direct_dependencies",
                "dep_with_sources.jar",
                "--strict_java_deps",
                "ERROR",
            ],
        )
        .unwrap();
        let bad_req =
            JavaLibraryBuildRequest::from_options(bad_opts.clone(), &d, false, None).unwrap();
        let bad_res = run_bazel_java_builder("javac", &bad_req).unwrap();
        assert!(
            !bad_res.is_ok(),
            "expected javac to reject uncompiled source in classpath jar, got ok"
        );
        assert!(
            bad_res.output.contains("BundledSource"),
            "{}",
            bad_res.output
        );
        assert!(!d.join("libbad.jar").exists());

        let vanilla_req = JavaLibraryBuildRequest::from_options(bad_opts, &d, true, None).unwrap();
        let vanilla_res = run_vanilla_java_builder("javac", &vanilla_req).unwrap();
        assert!(
            !vanilla_res.is_ok(),
            "expected vanilla javac failure: {}",
            vanilla_res.output
        );
        assert!(!d.join("libbad.jar").exists());

        // Referencing the compiled `dep.CompiledApi` with `-Werror:all` succeeds cleanly with no
        // `-Xlint:path` warning on `-sourcepath ""`, and never emits `dep/BundledSource.class`.
        std::fs::write(
            d.join("app/UseCompiledApi.java"),
            "package app; public class UseCompiledApi { public dep.CompiledApi a; }\n",
        )
        .unwrap();
        let ok_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libok.jar",
                    "--target_label",
                    "//app:ok",
                    "--sources",
                    "app/UseCompiledApi.java",
                    "--classpath",
                    "dep_with_sources.jar",
                    "--direct_dependencies",
                    "dep_with_sources.jar",
                    "--strict_java_deps",
                    "ERROR",
                    "--javacopts",
                    "-Werror:all",
                    "--",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        let ok_res = run_bazel_java_builder("javac", &ok_req).unwrap();
        assert!(ok_res.is_ok(), "{}", ok_res.output);
        let ok_bytes = std::fs::read(d.join("libok.jar")).unwrap();
        let ok_zip = zel_util::zip::ZipReader::from_bytes(ok_bytes).unwrap();
        let names: Vec<&str> = ok_zip.entries().iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"app/UseCompiledApi.class"), "{names:?}");
        assert!(!names.contains(&"dep/BundledSource.class"), "{names:?}");

        // Single `module-info.java` with empty `--sourcepath` infers its parent directory as
        // `SOURCE_PATH` (`BlazeJavacMain.setLocations`).
        std::fs::write(
            d.join("modsrc/module-info.java"),
            "module demo.mod { exports pkg; }\n",
        )
        .unwrap();
        std::fs::write(
            d.join("modsrc/pkg/ModApi.java"),
            "package pkg; public class ModApi {}\n",
        )
        .unwrap();
        let mod_req = JavaLibraryBuildRequest::from_options(
            OptionsParser::parse(
                &d,
                [
                    "--output",
                    "libmod.jar",
                    "--target_label",
                    "//modsrc:mod",
                    "--sources",
                    "modsrc/module-info.java",
                    "modsrc/pkg/ModApi.java",
                    "--strict_java_deps",
                    "OFF",
                ],
            )
            .unwrap(),
            &d,
            false,
            None,
        )
        .unwrap();
        let mod_res = run_bazel_java_builder("javac", &mod_req).unwrap();
        assert!(mod_res.is_ok(), "{}", mod_res.output);

        let _ = std::fs::remove_dir_all(&d);
    }
}
