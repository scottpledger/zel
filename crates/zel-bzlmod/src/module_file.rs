//! `MODULE.bazel` evaluation (`ModuleFileGlobals` + `ModuleThreadContext`) on top of `zel-starlark`.

use crate::model::*;
use crate::version::Version;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use zel_starlark::{EvalContext, StarlarkError, Value};

pub type IncludeReader = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

pub struct ModuleFileOptions {
    pub key: ModuleKey,
    pub ignore_dev_deps: bool,
    /// `--experimental_isolated_extension_usages`.
    pub isolated_extension_usages: bool,
    pub registry_url: Option<String>,
    /// Display name for the file in error locations.
    pub file_name: String,
    /// Reads an included file given its normalized label (`//:foo.MODULE.bazel`); `None` for
    /// modules that may not use `include()`.
    pub include_reader: Option<IncludeReader>,
    /// Built-in modules implicitly depended upon by every module (`bazel_tools`, ...).
    pub builtin_modules: Vec<String>,
}

#[derive(Debug)]
pub struct EvalResult {
    pub module: InterimModule,
    pub overrides: Vec<(String, ModuleOverride)>,
    pub warnings: Vec<String>,
}

struct UsageBuilder {
    bzl_file: String,
    extension_name: String,
    isolate: bool,
    proxies: Vec<Proxy>,
    imports: Vec<(String, String)>, // local -> exported
    repo_overrides: Vec<(String, RepoOverrideSpec)>,
    tags: Vec<Tag>,
    /// A `dev_dependency` usage of a non-root module: parsed but never recorded.
    detached: bool,
}

struct State {
    opts_key: ModuleKey,
    ignore_dev_deps: bool,
    isolated_extension_usages: bool,
    module_called: bool,
    had_non_module_call: bool,
    name: String,
    version: Version,
    repo_name: Option<String>,
    bazel_compat: Vec<String>,
    toolchains: Vec<String>,
    platforms: Vec<String>,
    deps: Vec<(String, ModuleKey)>,
    nodeps: Vec<ModuleKey>,
    flag_aliases: Vec<(String, String)>,
    usages: Vec<UsageBuilder>,
    overrides: Vec<(String, ModuleOverride)>,
    repo_name_usages: HashMap<String, (String, String)>, // name -> (how, location)
    warnings: Vec<String>,
    include_stack: Vec<String>,
    current_file: String,
    include_reader: Option<IncludeReader>,
    // Per-file bookkeeping for exports.
    proxy_refs: Vec<(usize, usize)>,
}

fn err<T>(m: impl Into<String>) -> Result<T, StarlarkError> {
    Err(StarlarkError(m.into()))
}

/// Splits `file:line:col` (the file may itself contain colons) into its parts.
fn split_loc(loc: &str) -> (&str, &str, &str) {
    let mut it = loc.rsplitn(3, ':');
    let col = it.next().unwrap_or("1");
    let line = it.next().unwrap_or("1");
    let file = it.next().unwrap_or(loc);
    (file, line, col)
}

/// One `File "...", line N, column M, in <toplevel>` frame plus its source line.
fn tb_frame(loc: &str) -> String {
    let (file, line, col) = split_loc(loc);
    let mut out = format!("\tFile \"{file}\", line {line}, column {col}, in <toplevel>\n");
    if let Some(src) = line.parse::<usize>().ok().and_then(|l| zel_starlark::source_line(file, l)) {
        out.push_str(&format!("\t\t{src}\n"));
    }
    out
}

/// Builds a Starlark-style `Traceback` error for a failing MODULE.bazel builtin.
fn tb(loc: &str, func: &str, msg: impl std::fmt::Display) -> StarlarkError {
    StarlarkError(format!("{loc}: Traceback (most recent call last):\n{}Error in {func}: {msg}", tb_frame(loc)))
}

/// Inserts the frames of an `include()` call site (the including file's toplevel frame and the
/// `<builtin>` include frame) right below the traceback header of an error raised in the
/// included file.
fn add_include_frame(e: String, loc: &str) -> String {
    // The trailer is added once, by the outermost file.
    let e = match e.rsplit_once('\n') {
        Some((body, last)) if last.starts_with("ERROR: error executing MODULE.bazel file for") => body.to_string(),
        _ => e,
    };
    let Some((head, rest)) = e.split_once('\n') else { return e };
    if !head.contains("Traceback (most recent call last):") {
        return e;
    }
    format!("{head}\n{}\tFile \"<builtin>\", in include\n{rest}", tb_frame(loc))
}

/// Location of the start of an `include(...)` statement given the location of its call paren.
fn include_stmt_loc(loc: &str) -> String {
    let (file, line, col) = split_loc(loc);
    let c: usize = col.parse().unwrap_or(1);
    let start = c.saturating_sub("include".len()).max(1);
    format!("{file}:{line}:{start}")
}

fn tb_err<T>(loc: &str, func: &str, msg: impl std::fmt::Display) -> Result<T, StarlarkError> {
    Err(tb(loc, func, msg))
}

pub fn validate_module_name(name: &str) -> Result<(), StarlarkError> {
    let b = name.as_bytes();
    let ok = !b.is_empty()
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-'))
        && (b.len() == 1 || b[b.len() - 1].is_ascii_lowercase() || b[b.len() - 1].is_ascii_digit());
    if ok {
        Ok(())
    } else {
        err(format!(
            "invalid module name '{name}': valid names must 1) only contain lowercase letters (a-z), digits (0-9), dots (.), hyphens (-), and underscores (_); 2) begin with a lowercase letter; 3) end with a lowercase letter or digit."
        ))
    }
}

pub fn validate_user_repo_name(name: &str) -> Result<(), StarlarkError> {
    let b = name.as_bytes();
    let ok = !b.is_empty()
        && b[0].is_ascii_alphanumeric()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        err(format!(
            "invalid user-provided repo name '{name}': valid names may contain only A-Z, a-z, 0-9, '-', '_', '.', and must start with a letter or a number"
        ))
    }
}

/// Argument binder emulating `@StarlarkMethod` parameter handling.
struct Args<'a> {
    fname: &'a str,
    pos: Vec<Value>,
    kw: BTreeMap<String, Value>,
    next: usize,
    loc: &'a str,
}

impl<'a> Args<'a> {
    fn new(fname: &'a str, pos: Vec<Value>, kw: BTreeMap<String, Value>, loc: &'a str) -> Self {
        Self { fname, pos, kw, next: 0, loc }
    }
    fn err<T>(&self, m: impl std::fmt::Display) -> Result<T, StarlarkError> {
        tb_err(self.loc, self.fname, m)
    }
    /// Positional-or-named parameter.
    fn param(&mut self, name: &str) -> Option<Value> {
        if self.next < self.pos.len() {
            let v = self.pos[self.next].clone();
            self.next += 1;
            return Some(v);
        }
        self.kw.remove(name)
    }
    /// Named-only parameter.
    fn named(&mut self, name: &str) -> Option<Value> {
        self.kw.remove(name)
    }
    fn req_str(&mut self, name: &str, positional: bool) -> Result<String, StarlarkError> {
        let v = if positional { self.param(name) } else { self.named(name) };
        match v {
            Some(Value::String(s)) => Ok(s),
            Some(o) => self.err(format!(
                "for parameter '{name}', got {}, want 'string'",
                o.type_name()
            )),
            None => self.err(format!(
                "missing 1 required {}argument: {name}",
                if positional { "" } else { "named " }
            )),
        }
    }
    fn opt_str(&mut self, name: &str, positional: bool, default: &str) -> Result<String, StarlarkError> {
        let v = if positional { self.param(name) } else { self.named(name) };
        match v {
            Some(Value::String(s)) => Ok(s),
            Some(o) => self.err(format!(
                "for parameter '{name}', got {}, want 'string'",
                o.type_name()
            )),
            None => Ok(default.to_string()),
        }
    }
    fn opt_bool(&mut self, name: &str) -> bool {
        self.named(name).map(|v| v.is_truthy()).unwrap_or(false)
    }
    fn opt_int(&mut self, name: &str, default: i64) -> i64 {
        match self.named(name) {
            Some(Value::Int(i)) => i.to_i64().unwrap_or(default),
            _ => default,
        }
    }
    fn str_list(&mut self, name: &str, v: Option<Value>) -> Result<Vec<String>, StarlarkError> {
        match v {
            None => Ok(vec![]),
            Some(Value::List(_)) | Some(Value::Tuple(_)) => {
                let a = AttrVal::from_value(v.as_ref().unwrap());
                match a {
                    AttrVal::List(l) => l
                        .into_iter()
                        .enumerate()
                        .map(|(i, x)| match x {
                            AttrVal::Str(s) | AttrVal::Label(s) => Ok(s),
                            other => self.err(format!(
                                "at index {i} of {name}, got element of type {}, want string",
                                other.type_name()
                            )),
                        })
                        .collect(),
                    _ => Ok(vec![]),
                }
            }
            Some(o) => self.err(format!("for parameter '{name}', got {}, want 'sequence'", o.type_name())),
        }
    }
    fn finish(self) -> Result<(), StarlarkError> {
        if let Some(k) = self.kw.keys().next() {
            return self.err(format!("got unexpected keyword argument '{k}'"));
        }
        if self.next < self.pos.len() {
            return self.err(format!("accepts no more than {} positional arguments but got {}", self.next, self.pos.len()));
        }
        Ok(())
    }
}

fn host_names() -> &'static [&'static str] {
    &[
        "module",
        "bazel_dep",
        "register_toolchains",
        "register_execution_platforms",
        "use_extension",
        "use_repo",
        "override_repo",
        "inject_repo",
        "use_repo_rule",
        "include",
        "single_version_override",
        "multiple_version_override",
        "archive_override",
        "git_override",
        "local_path_override",
        "flag_alias",
    ]
}

fn check_absolute_patterns(what: &str, items: &[String]) -> Result<(), StarlarkError> {
    for i in items {
        if !i.starts_with("//") && !i.starts_with('@') {
            return err(format!(
                "{what}: expected absolute target patterns (that is, starting with either '@' or '//'), but got '{i}'"
            ));
        }
    }
    Ok(())
}

/// Normalizes an extension bzl label as `normalizeLabelString`: keeps the *apparent* repo name.
fn normalize_label(own_repo: &str, is_root: bool, raw: &str) -> Result<String, StarlarkError> {
    let bad = |m: &str| err(format!("invalid label \"{raw}\": {m}"));
    let (repo, rest) = if let Some(r) = raw.strip_prefix("@@") {
        match r.split_once("//") {
            Some((repo, rest)) => (Some(repo.to_string()), rest.to_string()),
            None => return bad("labels with a repository must have a package"),
        }
    } else if let Some(r) = raw.strip_prefix('@') {
        match r.split_once("//") {
            Some((repo, rest)) => (Some(repo.to_string()), rest.to_string()),
            None => (Some(r.to_string()), format!(":{r}")),
        }
    } else if let Some(r) = raw.strip_prefix("//") {
        (None, r.to_string())
    } else if raw.starts_with(':') {
        (None, raw.to_string())
    } else {
        // A bare name is a target in the module's root package.
        (None, format!(":{raw}"))
    };
    let (pkg, name) = match rest.split_once(':') {
        Some((p, n)) => (p.to_string(), n.to_string()),
        None => {
            let n = rest.rsplit('/').next().unwrap_or("").to_string();
            (rest.clone(), n)
        }
    };
    let repo_name = match repo {
        None | Some(_) if raw.starts_with("@@") => repo.unwrap_or_default(),
        None => {
            let _ = is_root;
            // Own repo: root module refers to itself as "" but the apparent form is its own name.
            if is_root { String::new() } else { own_repo.to_string() }
        }
        Some(r) if r.is_empty() && is_root => String::new(),
        Some(r) => r,
    };
    if repo_name.is_empty() {
        Ok(format!("//{pkg}:{name}"))
    } else if raw.starts_with("@@") {
        Ok(format!("@@{repo_name}//{pkg}:{name}"))
    } else {
        Ok(format!("@{repo_name}//{pkg}:{name}"))
    }
}

pub fn eval_module_file(src: &str, opts: ModuleFileOptions) -> Result<EvalResult, String> {
    let is_root = opts.key.is_root();
    let st = Arc::new(Mutex::new(State {
        opts_key: opts.key.clone(),
        ignore_dev_deps: opts.ignore_dev_deps,
        isolated_extension_usages: opts.isolated_extension_usages,
        module_called: false,
        had_non_module_call: false,
        name: String::new(),
        version: opts.key.version.clone(),
        repo_name: None,
        bazel_compat: vec![],
        toolchains: vec![],
        platforms: vec![],
        deps: vec![],
        nodeps: vec![],
        flag_aliases: vec![],
        usages: vec![],
        overrides: vec![],
        repo_name_usages: HashMap::new(),
        warnings: vec![],
        include_stack: vec![],
        current_file: opts.file_name.clone(),
        include_reader: opts.include_reader.clone(),
        proxy_refs: vec![],
    }));
    run_file(&st, src, &opts.file_name, opts.registry_url.is_none())?;

    let mut s = st.lock().unwrap();
    // buildModule
    let mut deps = std::mem::take(&mut s.deps);
    let mut dep_map: Vec<(String, ModuleKey)> = Vec::new();
    for (n, k) in deps.drain(..) {
        if let Some(e) = dep_map.iter_mut().find(|(x, _)| *x == n) {
            e.1 = k;
        } else {
            dep_map.push((n, k));
        }
    }
    for b in &opts.builtin_modules {
        if s.name == *b {
            continue;
        }
        if let Some(prev) = s.repo_name_usages.get(b).cloned() {
            return Err(format!(
                "The repo name '{b}' cannot be defined as a built-in dependency at <builtin> as it is already defined {} at {}, '{b}' is a built-in dependency and cannot be used by any 'bazel_dep' or 'use_repo' directive\nERROR: error executing MODULE.bazel file for the root module",
                prev.0, prev.1
            ));
        }
        dep_map.retain(|(n, _)| n != b);
        dep_map.push((b.clone(), ModuleKey::new(b.clone(), Version::empty())));
        s.repo_name_usages.insert(b.clone(), ("as a built-in dependency".into(), "<builtin>".into()));
    }
    let mut usages = Vec::new();
    let mut overridden = HashMap::new();
    let mut overriding = HashMap::new();
    let usage_names = s.repo_name_usages.clone();
    let own_repo = s.repo_name.clone().unwrap_or_else(|| s.name.clone());
    for ub in &s.usages {
        if ub.proxies.is_empty() || ub.detached {
            continue;
        }
        let mut isolation_key = None;
        if ub.isolate {
            let only = &ub.proxies[0];
            if only.proxy_name.is_empty() {
                return Err(format!(
                    "Isolated extension usage at {} must be assigned to a top-level variable",
                    only.location
                ));
            }
            isolation_key = Some((s.opts_key.clone(), only.proxy_name.clone()));
        }
        for (ov_name, ov) in &ub.repo_overrides {
            let ov_func = if ov.must_exist { "override_repo" } else { "inject_repo" };
            if !usage_names.contains_key(&ov.overriding_repo_name) {
                return Err(build_module_error(is_root, &s.opts_key, &ov.location, ov_func, format!(
                    "The repo exported as '{ov_name}' by module extension '{}' is overridden with '{}', but no repo is visible under this name",
                    ub.extension_name, ov.overriding_repo_name
                )));
            }
            let imported_as = ub.imports.iter().find(|(_, e)| e == ov_name).map(|(l, _)| l.clone());
            if let Some(imp) = imported_as {
                if !ov.must_exist {
                    return Err(build_module_error(is_root, &s.opts_key, &ov.location, ov_func, format!(
                        "Cannot import repo '{ov_name}' that has been injected into module extension '{}' at {}. Please refer to @{} directly.",
                        ub.extension_name, ov.location, ov.overriding_repo_name
                    )));
                }
                if ov.overriding_repo_name == own_repo {
                    return Err(build_module_error(is_root, &s.opts_key, &ov.location, ov_func, format!(
                        "Cannot import repo '{ov_name}' from module extension '{}' because it is overridden with the root module at {}. Please refer to @{} directly.",
                        ub.extension_name, ov.location, ov.overriding_repo_name
                    )));
                }
                overridden.insert(imp, (ov_name.clone(), ov.overriding_repo_name.clone(), ub.extension_name.clone()));
            }
            overriding.insert(ov.overriding_repo_name.clone(), (ov_name.clone(), ov.overriding_repo_name.clone(), ub.extension_name.clone()));
        }
        usages.push(ExtensionUsage {
            bzl_file: ub.bzl_file.clone(),
            extension_name: ub.extension_name.clone(),
            isolation_key,
            proxies: ub.proxies.clone(),
            tags: ub.tags.clone(),
            repo_overrides: ub.repo_overrides.iter().cloned().collect(),
        });
    }
    if let Some(k) = overriding.keys().find(|k| overridden.contains_key(*k)) {
        let (ov_name, _, ext) = overriding.get(k).unwrap();
        return Err(format!(
            "The repo '{k}' used as an override for '{ov_name}' in module extension '{ext}' is itself overridden, which is not supported."
        ));
    }
    let mut overrides = s.overrides.clone();
    if is_root {
        // Built-in module overrides are applied by the caller.
    }
    let _ = &mut overrides;
    let module = InterimModule {
        key: s.opts_key.clone(),
        name: s.name.clone(),
        version: s.version.clone(),
        repo_name: s.repo_name.clone().unwrap_or_else(|| s.name.clone()),
        bazel_compatibility: s.bazel_compat.clone(),
        toolchains_to_register: s.toolchains.clone(),
        execution_platforms_to_register: s.platforms.clone(),
        original_deps: dep_map.clone(),
        deps: dep_map,
        nodep_deps: s.nodeps.clone(),
        registry_url: opts.registry_url.clone(),
        extension_usages: usages,
        flag_aliases: s.flag_aliases.clone(),
    };
    Ok(EvalResult { module, overrides, warnings: s.warnings.clone() })
}

fn run_file(st: &Arc<Mutex<State>>, src: &str, file_name: &str, show_source: bool) -> Result<(), String> {
    // Bazel only shows source lines for module files it can read from disk (not registry files).
    if show_source {
        zel_starlark::register_source(file_name, src);
    }
    let mut ctx = EvalContext::default();
    ctx.file_name = file_name.to_string();
    ctx.strict_names = true;
    let st2 = st.clone();
    ctx.host = Some(Arc::new(move |_ctx, name, args, kw| dispatch(&st2, _ctx, name, args, kw)));
    for n in host_names() {
        ctx.globals.insert((*n).to_string(), Value::BuiltinFunction(format!("host:{n}")));
    }
    let key = st.lock().unwrap().opts_key.clone();
    if let Err(errors) = ctx.eval_file_checked(src, Some(("MODULE.bazel files", false))) {
        let is_include_err = errors.iter().any(|e| e.contains("bad include label") || e.contains("error reading '") || e.contains("unable to load package for '"));
        if is_include_err {
            let mut msg = String::new();
            for e in &errors {
                let clean = if let Some(idx) = e.find("bad include label") {
                    &e[idx..]
                } else if let Some(idx) = e.find("error reading '") {
                    &e[idx..]
                } else if let Some(idx) = e.find("unable to load package for '") {
                    &e[idx..]
                } else {
                    e.as_str()
                };
                if clean.starts_with("ERROR: ") {
                    msg.push_str(&format!("{clean}\n"));
                } else {
                    msg.push_str(&format!("ERROR: {clean}\n"));
                }
            }
            return Err(msg.trim_end().to_string());
        }
        let is_syntax = errors.iter().any(|e| {
            e.contains("syntax error at")
                || e.contains("statements are not allowed")
                || e.contains("functions may not be defined")
                || e.contains("statements may not be used")
                || e.contains("is not defined")
                || e.contains("duplicate keyword argument")
                || e.contains("positional argument may not follow")
                || e.contains("*args arguments are not allowed")
                || e.contains("**kwargs arguments are not allowed")
                || e.contains("the `include` directive MUST")
                || e.contains("return statements must be inside a function")
        });
        // A runtime error of the top-level code without builtin attribution (e.g. a dict literal with an
        // unhashable or duplicate key) is reported as a traceback at its location.
        let errors: Vec<String> = if !is_syntax && !ctx.static_failure {
            errors
                .into_iter()
                .map(|e| {
                    if e.contains("Traceback (most recent call last)") || e.starts_with("ERROR: ") {
                        return e;
                    }
                    let mut it = e.splitn(4, ':');
                    let (f, l, c, m) = (it.next(), it.next(), it.next(), it.next());
                    match (f, l, c, m) {
                        (Some(f), Some(l), Some(c), Some(m)) if l.parse::<usize>().is_ok() && c.parse::<usize>().is_ok() => {
                            let loc = format!("{f}:{l}:{c}");
                            format!("{loc}: Traceback (most recent call last):\n{}Error: {}", tb_frame(&loc), m.trim_start())
                        }
                        _ => e,
                    }
                })
                .collect()
        } else {
            errors
        };
        let mut msg = String::new();
        for e in &errors {
            let line = e.trim_matches(|c| c == '\n' || c == '\r');
            if is_syntax {
                if line.starts_with("ERROR: ") {
                    msg.push_str(&format!("{line}\n"));
                } else {
                    msg.push_str(&format!("ERROR: {line}\n"));
                }
            } else if line.starts_with("ERROR: ") {
                msg.push_str(&format!("{line}\n"));
            } else {
                msg.push_str(&format!("ERROR: {line}\n"));
            }
        }
        if !is_syntax && errors.iter().any(|e| e.contains("Traceback (most recent call last)")) {
            msg.push_str(&format!("ERROR: error executing MODULE.bazel file for {key}"));
        }
        if is_syntax {
            if errors.iter().any(|e| e.contains("syntax error at")) {
                msg.push_str(&format!("ERROR: error parsing MODULE.bazel file for {key}"));
            } else {
                msg.push_str(&format!("ERROR: syntax error in MODULE.bazel file for {key}"));
            }
        }
        return Err(msg.trim_end().to_string());
    }
    // Export proxy names (`ext = use_extension(...)`).
    let mut s = st.lock().unwrap();
    for (name, v) in &ctx.globals {
        if let Value::Host(kind, id) = v {
            if kind == "ext_proxy" {
                if let Some((u, p)) = parse_proxy_id(id) {
                    if let Some(proxy) = s.usages.get_mut(u).and_then(|ub| ub.proxies.get_mut(p)) {
                        proxy.proxy_name = name.clone();
                    }
                }
            }
        }
    }
    Ok(())
}

fn parse_proxy_id(id: &str) -> Option<(usize, usize)> {
    let (a, b) = id.split_once(':')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

fn host_str(s: &str) -> Value {
    Value::String(s.to_string())
}


// ---------------------------------------------------------------------------------------------
// `BuiltinFunction` argument binding (`@StarlarkMethod` signatures of `ModuleFileGlobals`).
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Ty {
    Str,
    Int,
    Bool,
    Iterable,
    StrOrNone,
    Proxy,
}

impl Ty {
    fn want(self) -> &'static str {
        match self {
            Ty::Str => "string",
            Ty::Int => "int",
            Ty::Bool => "bool",
            Ty::Iterable => "Iterable",
            Ty::StrOrNone => "string or NoneType",
            Ty::Proxy => "module_extension_proxy",
        }
    }
    fn accepts(self, v: &Value) -> bool {
        match self {
            Ty::Str => matches!(v, Value::String(_)),
            Ty::Int => matches!(v, Value::Int(_)),
            Ty::Bool => matches!(v, Value::Bool(_)),
            Ty::Iterable => matches!(v, Value::List(_) | Value::Tuple(_) | Value::Dict(_) | Value::Set(_) | Value::Range(_)),
            Ty::StrOrNone => matches!(v, Value::String(_) | Value::None),
            Ty::Proxy => matches!(v, Value::Host(k, _) if k == "ext_proxy"),
        }
    }
}

/// (name, positional, required, type)
type P = (&'static str, bool, bool, Ty);

struct Sig {
    params: &'static [P],
    extra_pos: bool,
    extra_kw: bool,
}

fn signature(name: &str) -> Option<Sig> {
    use Ty::*;
    const MODULE: &[P] = &[
        ("name", false, false, Str),
        ("version", false, false, Str),
        ("compatibility_level", false, false, Int),
        ("repo_name", false, false, Str),
        ("bazel_compatibility", false, false, Iterable),
    ];
    const BAZEL_DEP: &[P] = &[
        ("name", false, true, Str),
        ("version", false, false, Str),
        ("max_compatibility_level", false, false, Int),
        ("repo_name", false, false, StrOrNone),
        ("dev_dependency", false, false, Bool),
    ];
    const REGISTER: &[P] = &[("dev_dependency", false, false, Bool)];
    const USE_EXTENSION: &[P] = &[
        ("extension_bzl_file", true, true, Str),
        ("extension_name", true, true, Str),
        ("dev_dependency", false, false, Bool),
        ("isolate", false, false, Bool),
    ];
    const USE_REPO: &[P] = &[("extension_proxy", true, true, Proxy)];
    const USE_REPO_RULE: &[P] = &[("repo_rule_bzl_file", true, true, Str), ("repo_rule_name", true, true, Str)];
    const INCLUDE: &[P] = &[("label", true, true, Str), ("dev_dependency", false, false, Bool)];
    const SVO: &[P] = &[
        ("module_name", false, true, Str),
        ("version", false, false, Str),
        ("registry", false, false, Str),
        ("patches", false, false, Iterable),
        ("patch_cmds", false, false, Iterable),
        ("patch_strip", false, false, Int),
    ];
    const MVO: &[P] = &[("module_name", false, true, Str), ("versions", false, true, Iterable), ("registry", false, false, Str)];
    const MODNAME: &[P] = &[("module_name", false, true, Str)];
    const LPO: &[P] = &[("module_name", false, true, Str), ("path", false, true, Str)];
    const FLAG_ALIAS: &[P] = &[("name", false, true, Str), ("starlark_flag", false, true, Str)];
    const REPO_CALL: &[P] = &[("name", false, true, Str), ("dev_dependency", false, false, Bool)];
    Some(match name {
        "module" => Sig { params: MODULE, extra_pos: false, extra_kw: false },
        "bazel_dep" => Sig { params: BAZEL_DEP, extra_pos: false, extra_kw: false },
        "register_toolchains" | "register_execution_platforms" => Sig { params: REGISTER, extra_pos: true, extra_kw: false },
        "use_extension" => Sig { params: USE_EXTENSION, extra_pos: false, extra_kw: false },
        "use_repo" | "override_repo" | "inject_repo" => Sig { params: USE_REPO, extra_pos: true, extra_kw: true },
        "use_repo_rule" => Sig { params: USE_REPO_RULE, extra_pos: false, extra_kw: false },
        "include" => Sig { params: INCLUDE, extra_pos: false, extra_kw: false },
        "single_version_override" => Sig { params: SVO, extra_pos: false, extra_kw: false },
        "multiple_version_override" => Sig { params: MVO, extra_pos: false, extra_kw: false },
        "archive_override" | "git_override" => Sig { params: MODNAME, extra_pos: false, extra_kw: true },
        "local_path_override" => Sig { params: LPO, extra_pos: false, extra_kw: false },
        "flag_alias" => Sig { params: FLAG_ALIAS, extra_pos: false, extra_kw: false },
        "call:repo_rule_proxy" => Sig { params: REPO_CALL, extra_pos: false, extra_kw: true },
        _ => return None,
    })
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Port of `BuiltinFunction`'s argument processor: reports Bazel's exact binding errors.
/// `pos` excludes any implicit receiver argument.
fn check_binding(fname: &str, sig: &Sig, pos: &[Value], kw: &BTreeMap<String, Value>) -> Result<(), String> {
    let mut filled: Vec<bool> = vec![false; sig.params.len()];
    let mut arg_index = 0usize;
    for (i, (pname, positional, _, ty)) in sig.params.iter().enumerate() {
        if arg_index >= pos.len() || !*positional {
            break;
        }
        let v = &pos[arg_index];
        if !ty.accepts(v) {
            return Err(format!(
                "in call to {fname}(), parameter '{pname}' got value of type '{}', want '{}'",
                v.type_name(),
                ty.want()
            ));
        }
        filled[i] = true;
        arg_index += 1;
    }
    if arg_index < pos.len() && !sig.extra_pos {
        return Err(if arg_index == 0 {
            format!("{fname}() got unexpected positional argument")
        } else {
            format!("{fname}() accepts no more than {arg_index} positional argument{} but got {}", plural(arg_index), pos.len())
        });
    }
    for (k, v) in kw {
        match sig.params.iter().position(|p| p.0 == k) {
            None => {
                if !sig.extra_kw {
                    let names: Vec<&str> = sig.params.iter().map(|p| p.0).collect();
                    return Err(format!(
                        "{fname}() got unexpected keyword argument '{k}'{}",
                        zel_starlark::did_you_mean(k, names)
                    ));
                }
            }
            Some(i) => {
                let (pname, _, _, ty) = sig.params[i];
                if !ty.accepts(v) {
                    return Err(format!(
                        "in call to {fname}(), parameter '{pname}' got value of type '{}', want '{}'",
                        v.type_name(),
                        ty.want()
                    ));
                }
                if filled[i] {
                    return Err(format!("{fname}() got multiple values for argument '{pname}'"));
                }
                filled[i] = true;
            }
        }
    }
    let missing_pos: Vec<&str> = sig.params.iter().enumerate().filter(|(i, p)| !filled[*i] && p.2 && p.1).map(|(_, p)| p.0).collect();
    let missing_named: Vec<&str> = sig.params.iter().enumerate().filter(|(i, p)| !filled[*i] && p.2 && !p.1).map(|(_, p)| p.0).collect();
    if !missing_pos.is_empty() {
        return Err(format!(
            "{fname}() missing {} required positional argument{}: {}",
            missing_pos.len(),
            plural(missing_pos.len()),
            missing_pos.join(", ")
        ));
    }
    if !missing_named.is_empty() {
        return Err(format!(
            "{fname}() missing {} required named argument{}: {}",
            missing_named.len(),
            plural(missing_named.len()),
            missing_named.join(", ")
        ));
    }
    Ok(())
}

/// Drains `kw` in call (source) order.
fn drain_kw_ordered(kw: &mut BTreeMap<String, Value>, order: &[String]) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for k in order {
        if let Some(v) = kw.remove(k) {
            out.push((k.clone(), v));
        }
    }
    out.extend(std::mem::take(kw));
    out
}

/// Tag attributes in call (source) order, like the `Dict` Bazel builds from the call's kwargs.
fn ordered_attrs(kw: &BTreeMap<String, Value>, order: &[String]) -> Vec<(String, AttrVal)> {
    let mut out: Vec<(String, AttrVal)> = Vec::new();
    for k in order {
        if let Some(v) = kw.get(k) {
            out.push((k.clone(), AttrVal::from_value(v)));
        }
    }
    for (k, v) in kw {
        if !out.iter().any(|(n, _)| n == k) {
            out.push((k.clone(), AttrVal::from_value(v)));
        }
    }
    out
}

/// An error thrown by `ModuleThreadContext.buildModule` after the file ran: reported with a
/// traceback at `loc` and the `error executing MODULE.bazel file` trailer.
fn build_module_error(is_root: bool, key: &ModuleKey, loc: &str, func: &str, msg: String) -> String {
    let who = if is_root { "the root module".to_string() } else { key.to_string() };
    format!("ERROR: {}\nERROR: error executing MODULE.bazel file for {who}", tb(loc, func, msg).0)
}

/// Wraps bare builtin errors into Starlark tracebacks (`Error in <fn>: msg`) like Bazel.
fn dispatch(
    st: &Arc<Mutex<State>>,
    ctx: &mut EvalContext,
    name: &str,
    pos: Vec<Value>,
    kw: BTreeMap<String, Value>,
) -> Result<Value, StarlarkError> {
    let loc = ctx.call_location.clone();
    match dispatch_inner(st, ctx, name, pos, kw) {
        Err(e)
            if !e.0.contains("Traceback (most recent call last)")
                && !e.0.starts_with("bad include label")
                && !e.0.starts_with("error reading '")
                && !e.0.starts_with("unable to load package for '") =>
        {
            let fname = if name == "call:repo_rule_proxy" { "call" } else { name.rsplit([':', '.']).next().unwrap_or(name) };
            Err(tb(&loc, fname, e.0))
        }
        r => r,
    }
}

fn dispatch_inner(
    st: &Arc<Mutex<State>>,
    ctx: &mut EvalContext,
    name: &str,
    pos: Vec<Value>,
    kw: BTreeMap<String, Value>,
) -> Result<Value, StarlarkError> {
    let loc = ctx.call_location.clone();
    if let Some(dup) = ctx.pending_dup_kw.take() {
        // `f(name = "a", **{"name": "b"})`
        let fname = if name == "call:repo_rule_proxy" { "call" } else { name.rsplit([':', '.']).next().unwrap_or(name) };
        return tb_err(&loc, fname, format!("{fname}() got multiple values for argument '{dup}'"));
    }
    if let Some(sg) = signature(name) {
        let skip = usize::from(name == "call:repo_rule_proxy");
        let fname = if skip == 1 { "call" } else { name };
        if let Err(m) = check_binding(fname, &sg, &pos[skip.min(pos.len())..], &kw) {
            // Missing-argument errors are reported at the call; the other binding errors (unexpected keyword,
            // wrong type) at the enclosing assignment's `=` when the call is part of an assignment.
            let kw_type_error = m
                .split("parameter '")
                .nth(1)
                .and_then(|r| r.split('\'').next())
                .is_some_and(|pname| m.starts_with("in call to") && kw.contains_key(pname));
            let at_assignment = m.contains("got unexpected keyword argument") || kw_type_error;
            let at = if at_assignment { ctx.assign_stmt_location.as_ref().map(|l| l.to_string()).unwrap_or_else(|| loc.clone()) } else { loc.clone() };
            return tb_err(&at, fname, m);
        }
    }
    // Proxy method / attr / call dispatch.
    if let Some(m) = name.strip_prefix("method:ext_proxy.") {
        let Some(Value::Host(_, id)) = pos.first().cloned() else { return err("bad proxy") };
        if pos.len() > 1 {
            return tb_err(&loc, "call", "call() got unexpected positional argument");
        }
        let (u, _) = parse_proxy_id(&id).ok_or_else(|| StarlarkError("bad proxy".into()))?;
        let mut s = st.lock().unwrap();
        let dev = s.usages[u].proxies[parse_proxy_id(&id).unwrap().1].dev_dependency;
        let attrs = ordered_attrs(&kw, &ctx.call_kw_order);
        s.usages[u].tags.push(Tag { tag_name: m.to_string(), attrs, dev_dependency: dev, location: loc });
        return Ok(Value::None);
    }
    if let Some(m) = name.strip_prefix("attr:ext_proxy.") {
        let Some(Value::Host(_, id)) = pos.first().cloned() else { return err("bad proxy") };
        return Ok(Value::Host("tag_callable".into(), format!("{id}:{m}")));
    }
    if name == "call:tag_callable" {
        let Some(Value::Host(_, id)) = pos.first().cloned() else { return err("bad tag") };
        if pos.len() > 1 {
            return tb_err(&loc, "call", "call() got unexpected positional argument");
        }
        let mut parts = id.splitn(3, ':');
        let u: usize = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let p: usize = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let tag = parts.next().unwrap_or("").to_string();
        let mut s = st.lock().unwrap();
        let dev = s.usages[u].proxies[p].dev_dependency;
        let attrs = ordered_attrs(&kw, &ctx.call_kw_order);
        s.usages[u].tags.push(Tag { tag_name: tag, attrs, dev_dependency: dev, location: loc });
        return Ok(Value::None);
    }
    if name == "call:repo_rule_proxy" {
        let Some(Value::Host(_, id)) = pos.first().cloned() else { return err("bad proxy") };
        let u: usize = id.parse().unwrap_or(0);
        let mut a = Args::new("repo rule", pos[1..].to_vec(), kw, &loc);
        let rname = a.req_str("name", false)?;
        let dev = a.opt_bool("dev_dependency");
        validate_user_repo_name(&rname)?;
        let mut s = st.lock().unwrap();
        if s.ignore_dev_deps && dev {
            return Ok(Value::None);
        }
        let mut attrs: Vec<(String, AttrVal)> = vec![("name".into(), AttrVal::Str(rname.clone()))];
        attrs.extend(ordered_attrs(&a.kw, &ctx.call_kw_order));
        let file = s.current_file.clone();
        let p = s.usages[u].proxies.len();
        s.usages[u].proxies.push(Proxy {
            proxy_name: String::new(),
            dev_dependency: dev,
            location: loc.clone(),
            imports: vec![],
            containing_module_file: file,
        });
        s.usages[u].tags.push(Tag { tag_name: "repo".into(), attrs, dev_dependency: dev, location: loc.clone() });
        add_import(&mut s, u, p, &rname, &rname, "by a repo rule", &loc)?;
        return Ok(Value::None);
    }

    let fname = name;
    let mut a = Args::new(fname, pos, kw, &loc);
    let mut s = st.lock().unwrap();
    let nonmodule = |s: &mut State| s.had_non_module_call = true;
    match name {
        "module" => {
            if s.module_called {
                return tb_err(&loc, fname, "the module() directive can only be called once");
            }
            let mname = a.opt_str("name", false, "")?;
            let version = a.opt_str("version", false, "")?;
            let compat_level = a.opt_int("compatibility_level", -1);
            let repo_name = a.opt_str("repo_name", false, "")?;
            let bc = a.named("bazel_compatibility");
            let bazel_compat = a.str_list("bazel_compatibility", bc)?;
            a.finish()?;
            if compat_level != -1 && s.opts_key.is_root() {
                s.warnings.push(format!("{loc}: The attribute 'compatibility_level' in module() is a no-op and will be removed in a future Bazel release. Please remove it from your MODULE.bazel file."));
            }
            if s.had_non_module_call {
                return tb_err(&loc, fname, "if module() is called, it must be called before any other functions");
            }
            s.module_called = true;
            if !mname.is_empty() {
                validate_module_name(&mname).map_err(|e| tb(&loc, fname, e.0))?;
            }
            let rn = if repo_name.is_empty() {
                add_repo_name_usage(&mut s, &mname, "as the current module name", &loc).map_err(|e| tb(&loc, fname, e.0))?;
                mname.clone()
            } else {
                validate_user_repo_name(&repo_name).map_err(|e| tb(&loc, fname, e.0))?;
                add_repo_name_usage(&mut s, &repo_name, "as the module's own repo name", &loc).map_err(|e| tb(&loc, fname, e.0))?;
                repo_name
            };
            for c in &bazel_compat {
                // `VALID_BAZEL_COMPATIBILITY_VERSION`: `(>|<|-|<=|>=)(\d+\.){2}\d+`, fully matched.
                let ok = [">=", "<=", ">", "<", "-"].iter().any(|p| {
                    c.strip_prefix(p).is_some_and(|rest| {
                        let parts: Vec<&str> = rest.split('.').collect();
                        parts.len() == 3 && parts.iter().all(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()))
                    })
                });
                if !ok {
                    return tb_err(&loc, fname, format!("invalid version argument '{c}': valid argument must 1) start with (<,<=,>,>=,-); 2) contain a version number in form of X.X.X where X is a number"));
                }
            }
            if !version.is_empty() {
                s.version = Version::parse(&version).map_err(|_| tb(&loc, fname, "Invalid version in module()"))?;
            }
            s.name = mname;
            s.repo_name = Some(rn);
            s.bazel_compat = bazel_compat;
            Ok(Value::None)
        }
        "bazel_dep" => {
            nonmodule(&mut s);
            let dname = a.req_str("name", false)?;
            let version = a.opt_str("version", false, "")?;
            let max_compat = a.opt_int("max_compatibility_level", -1);
            let repo_arg = a.named("repo_name");
            let dev = a.opt_bool("dev_dependency");
            a.finish()?;
            validate_module_name(&dname).map_err(|e| tb(&loc, fname, e.0))?;
            let parsed = Version::parse(&version).map_err(|_| tb(&loc, fname, "Invalid version in bazel_dep()"))?;
            if max_compat != -1 && s.opts_key.is_root() {
                s.warnings.push(format!("{loc}: The attribute 'max_compatibility_level' in bazel_dep() is a no-op and will be removed in a future Bazel release. Please remove it from your MODULE.bazel file."));
            }
            let repo_name: Option<String> = match repo_arg {
                Some(Value::None) => None,
                Some(Value::String(r)) if r.is_empty() => Some(dname.clone()),
                Some(Value::String(r)) => {
                    validate_user_repo_name(&r).map_err(|e| tb(&loc, fname, e.0))?;
                    Some(r)
                }
                None => Some(dname.clone()),
                Some(_) => return tb_err(&loc, fname, "internal error: unexpected repoName type"),
            };
            if let Some(ref r) = repo_name {
                add_repo_name_usage(&mut s, r, "by a bazel_dep", &loc).map_err(|e| tb(&loc, fname, e.0))?;
            }
            if dname == s.name {
                return tb_err(&loc, fname, format!("module '{dname}' cannot depend on itself"));
            }
            if !(s.ignore_dev_deps && dev) {
                match &repo_name {
                    Some(r) => {
                        let key = ModuleKey::new(dname.clone(), parsed);
                        if let Some(e) = s.deps.iter_mut().find(|(n, _)| n == r) {
                            e.1 = key;
                        } else {
                            s.deps.push((r.clone(), key));
                        }
                    }
                    None => s.nodeps.push(ModuleKey::new(dname.clone(), parsed)),
                }
            }
            Ok(Value::None)
        }
        "register_toolchains" | "register_execution_platforms" => {
            nonmodule(&mut s);
            let dev = a.opt_bool("dev_dependency");
            let rest: Vec<Value> = a.pos[a.next..].to_vec();
            a.next = a.pos.len();
            a.finish()?;
            if s.ignore_dev_deps && dev {
                return Ok(Value::None);
            }
            let mut labels = Vec::new();
            for (i, v) in rest.into_iter().enumerate() {
                match AttrVal::from_value(&v) {
                    AttrVal::Str(x) => labels.push(x),
                    other => {
                        return tb_err(&loc, fname, format!("at index {i} of {name}, got element of type {}, want string", other.type_name()))
                    }
                }
            }
            for i in &labels {
                if !i.starts_with("//") && !i.starts_with('@') {
                    return tb_err(&loc, fname, format!(
                        "Expected absolute target patterns (must begin with '//' or '@') for '{name}' argument, but got '{i}' as an argument"
                    ));
                }
            }
            if name == "register_toolchains" {
                s.toolchains.extend(labels);
            } else {
                s.platforms.extend(labels);
            }
            Ok(Value::None)
        }
        "use_extension" => {
            nonmodule(&mut s);
            let bzl = a.req_str("extension_bzl_file", true)?;
            let ext_name = a.req_str("extension_name", true)?;
            let dev = a.opt_bool("dev_dependency");
            let isolate_arg = a.named("isolate");
            let isolate = match isolate_arg {
                Some(Value::Bool(b)) => {
                    if !s.isolated_extension_usages {
                        let at = ctx.assign_stmt_location.as_ref().map(|l| l.to_string()).unwrap_or_else(|| loc.clone());
                return tb_err(&at, fname, "in call to use_extension(), parameter 'isolate' is experimental and thus unavailable with the current flags. It may be enabled by setting --experimental_isolated_extension_usages");
                    }
                    b
                }
                _ => false,
            };
            a.finish()?;
            if ext_name.is_empty() || !ext_name.chars().all(|c| c.is_alphanumeric() || c == '_') || ext_name.chars().next().unwrap().is_ascii_digit() {
                return tb_err(&loc, fname, format!("extension name is not a valid identifier: {ext_name}"));
            }
            let own = s.repo_name.clone().unwrap_or_else(|| s.name.clone());
            let nb = normalize_label(&own, s.opts_key.is_root(), &bzl).map_err(|e| tb(&loc, fname, e.0))?;
            let file = s.current_file.clone();
            let proxy = Proxy {
                proxy_name: String::new(),
                dev_dependency: dev,
                location: loc.clone(),
                imports: vec![],
                containing_module_file: file,
            };
            let idx = if s.ignore_dev_deps && dev {
                // Not recorded: build a detached usage builder.
                s.usages.push(UsageBuilder { bzl_file: nb, extension_name: ext_name, isolate, proxies: vec![], imports: vec![], repo_overrides: vec![], tags: vec![], detached: true });
                s.usages.len() - 1
            } else {
                get_or_create_usage(&mut s, &nb, &ext_name, isolate)
            };
            let p = s.usages[idx].proxies.len();
            s.usages[idx].proxies.push(proxy);
            s.proxy_refs.push((idx, p));
            Ok(Value::Host("ext_proxy".into(), format!("{idx}:{p}")))
        }
        "use_repo" => {
            nonmodule(&mut s);
            let Some(Value::Host(k, id)) = a.param("extension_proxy") else {
                return tb_err(&loc, fname, "use_repo(): parameter 'extension_proxy' must be a module extension proxy");
            };
            if k != "ext_proxy" {
                return tb_err(&loc, fname, "use_repo(): parameter 'extension_proxy' must be a module extension proxy");
            }
            let (u, p) = parse_proxy_id(&id).unwrap();
            let rest: Vec<Value> = a.pos[a.next..].to_vec();
            for (i, v) in rest.into_iter().enumerate() {
                let Value::String(arg) = v else {
                    return tb_err(&loc, fname, format!("at index {i} of args, got element of type {}, want string", v.type_name()));
                };
                add_import(&mut s, u, p, &arg, &arg, "by a use_repo() call", &loc).map_err(|e| tb(&loc, fname, e.0))?;
            }
            let mname = s.name.clone();
            let mver = s.version.normalized().to_string();
            for (k, v) in a.kw {
                let Value::String(v) = v else {
                    return tb_err(&loc, fname, format!("got dict<string, {}> for 'kwargs', want dict<string, string>", v.type_name()));
                };
                add_import(&mut s, u, p, &k, &v.replace("{name}", &mname).replace("{version}", &mver), "by a use_repo() call", &loc).map_err(|e| tb(&loc, fname, e.0))?;
            }
            Ok(Value::None)
        }
        "override_repo" | "inject_repo" => {
            nonmodule(&mut s);
            let Some(Value::Host(k, id)) = a.param("extension_proxy") else {
                return tb_err(&loc, fname, format!("{name}(): parameter 'extension_proxy' must be a module extension proxy"));
            };
            if k != "ext_proxy" {
                return tb_err(&loc, fname, format!("{name}(): parameter 'extension_proxy' must be a module extension proxy"));
            }
            let (u, _) = parse_proxy_id(&id).unwrap();
            if !s.opts_key.is_root() || s.ignore_dev_deps {
                return Ok(Value::None);
            }
            let must_exist = name == "override_repo";
            let rest: Vec<Value> = a.pos[a.next..].to_vec();
            let mut pairs = Vec::new();
            for v in rest {
                let Value::String(arg) = v else { return tb_err(&loc, fname, format!("{name}(): args must be strings")) };
                pairs.push((arg.clone(), arg));
            }
            for (k, v) in a.kw {
                let Value::String(v) = v else { return tb_err(&loc, fname, format!("{name}(): kwargs must be strings")) };
                pairs.push((k, v));
            }
            for (overridden, overriding) in pairs {
                // override_repo(ext, extension_repo = "root_repo"); inject_repo(ext, local = "ext_name")
                let (exported, local) = if must_exist { (overridden.clone(), overriding.clone()) } else { (overriding.clone(), overridden.clone()) };
                validate_user_repo_name(&exported).map_err(|e| tb(&loc, fname, e.0))?;
                validate_user_repo_name(&local).map_err(|e| tb(&loc, fname, e.0))?;
                let ub = &mut s.usages[u];
                if let Some((_, c)) = ub.repo_overrides.iter().find(|(n, _)| *n == exported) {
                    return tb_err(&loc, fname, format!(
                        "The repo exported as '{exported}' by module extension '{}' is already overridden with '{}' at {}",
                        ub.extension_name, c.overriding_repo_name, c.location
                    ));
                }
                ub.repo_overrides.push((exported, RepoOverrideSpec { overriding_repo_name: local, must_exist, location: loc.clone() }));
            }
            Ok(Value::None)
        }
        "use_repo_rule" => {
            nonmodule(&mut s);
            let bzl = a.req_str("repo_rule_bzl_file", true)?;
            let rname = a.req_str("repo_rule_name", true)?;
            a.finish()?;
            let ext_name = format!("{bzl} {rname}");
            let idx = get_or_create_usage(&mut s, "//:MODULE.bazel", &ext_name, false);
            Ok(Value::Host("repo_rule_proxy".into(), idx.to_string()))
        }
        "include" => {
            nonmodule(&mut s);
            let label = a.req_str("label", true)?;
            let dev = a.opt_bool("dev_dependency");
            a.finish()?;
            if s.ignore_dev_deps && dev {
                return Ok(Value::None);
            }
            let sloc = include_stmt_loc(&loc);
            let Some(reader) = s.include_reader.clone() else {
                return Err(StarlarkError(format!("bad include label '{label}' at {sloc}: trying to call `include()` from a non-root module")));
            };
            if !label.starts_with("//") {
                return Err(StarlarkError(format!("bad include label '{label}' at {sloc}: include() must be called with repo-relative labels (starting with double slashes)")));
            }
            let stripped = label.strip_prefix("//").unwrap();
            let (_pkg, target) = match stripped.split_once(':') {
                Some((p, t)) => (p, t),
                None => stripped.rsplit_once('/').unwrap_or(("", stripped)),
            };
            if !target.ends_with(".MODULE.bazel") {
                return Err(StarlarkError(format!("bad include label '{label}' at {sloc}: the file to be included must have a name ending in '.MODULE.bazel'")));
            }
            if target.starts_with('.') {
                return Err(StarlarkError(format!("bad include label '{label}' at {sloc}: the name of the file to be included must not start with '.'")));
            }
            if s.include_stack.contains(&label) {
                let i = s.include_stack.iter().position(|x| *x == label).unwrap();
                let mut chain: Vec<String> = s.include_stack[i..].to_vec();
                chain.push(label.clone());
                return tb_err(&loc, fname, format!("include() cycle detected: {}", chain.join(" -> ")));
            }
            let content = match reader(&label) {
                Ok(c) => c,
                Err(e) => {
                    if let Some(pkg_err) = e.strip_prefix("\u{1}PKG:") {
                        return Err(StarlarkError(format!("unable to load package for '{label}' included at {sloc}: {pkg_err}")));
                    }
                    return Err(StarlarkError(format!("error reading '{label}' included at {sloc}: {e}")));
                }
            };
            s.include_stack.push(label.clone());
            let inc_path = if let Some((dir, _)) = s.current_file.rsplit_once('/') {
                let rel = label.trim_start_matches("//").replace(':', "/");
                let rel = rel.trim_start_matches('/');
                format!("{dir}/{rel}")
            } else {
                target.to_string()
            };
            let prev = std::mem::replace(&mut s.current_file, inc_path.clone());
            drop(s);
            let r = run_file(st, &content, &inc_path, true);
            let mut s = st.lock().unwrap();
            s.current_file = prev;
            s.include_stack.pop();
            r.map_err(|e| StarlarkError(add_include_frame(e, &loc)))?;
            Ok(Value::None)
        }
        "single_version_override" => {
            nonmodule(&mut s);
            let module_name = a.req_str("module_name", false)?;
            let version = a.opt_str("version", false, "")?;
            let registry = a.opt_str("registry", false, "")?;
            let pv = a.named("patches");
            let patches = a.str_list("patches", pv)?;
            let pc = a.named("patch_cmds");
            let patch_cmds = a.str_list("patch_cmds", pc)?;
            let patch_strip = a.opt_int("patch_strip", 0);
            a.finish()?;
            validate_module_name(&module_name)?;
            let parsed = Version::parse(&version).map_err(|_| StarlarkError(format!("Invalid version in single_version_override(): {version}")))?;
            // Overrides of non-root modules are ignored, so their patch labels are not checked.
            if s.opts_key.is_root() {
                for p in &patches {
                    validate_patch_label(p)?;
                }
            }
            add_override(&mut s, module_name, ModuleOverride::SingleVersion { version: parsed, registry, patches, patch_cmds, patch_strip })
        }
        "multiple_version_override" => {
            nonmodule(&mut s);
            let module_name = a.req_str("module_name", false)?;
            let vv = a.named("versions");
            let versions = a.str_list("versions", vv)?;
            let registry = a.opt_str("registry", false, "")?;
            a.finish()?;
            validate_module_name(&module_name)?;
            let mut parsed = Vec::new();
            for v in &versions {
                parsed.push(Version::parse(v).map_err(|_| StarlarkError("Invalid version in multiple_version_override()".to_string()))?);
            }
            if parsed.len() < 2 {
                return tb_err(&loc, fname, "multiple_version_override() must specify at least 2 versions");
            }
            add_override(&mut s, module_name, ModuleOverride::MultipleVersion { versions: parsed, registry })
        }
        "archive_override" => {
            nonmodule(&mut s);
            let module_name = a.req_str("module_name", false)?;
            validate_module_name(&module_name)?;
            let mut attrs: Vec<(String, AttrVal)> = Vec::new();
            for (k, v) in drain_kw_ordered(&mut a.kw, &ctx.call_kw_order) {
                attrs.push((k, AttrVal::from_value(&v)));
            }
            a.pos.drain(..a.next);
            if !a.pos.is_empty() {
                return err("archive_override() takes only module_name positionally");
            }
            let mut spec_attrs = vec![];
            let mut has_urls = false;
            for (k, v) in attrs {
                if k == "url" || k == "urls" {
                    has_urls = true;
                }
                if k == "patches" {
                    let l: Vec<AttrVal> = v.str_list().into_iter().map(|p| AttrVal::Str(patch_label_to_canonical(&p))).collect();
                    spec_attrs.push((k, AttrVal::List(l)));
                } else {
                    spec_attrs.push((k, v));
                }
            }
            let _ = has_urls;
            add_override(&mut s, module_name, ModuleOverride::NonRegistry(RepoSpec::new(HTTP_BZL, "http_archive", spec_attrs)))
        }
        "git_override" => {
            nonmodule(&mut s);
            let module_name = a.req_str("module_name", false)?;
            validate_module_name(&module_name)?;
            let mut spec_attrs = vec![];
            let mut has_remote = false;
            for (k, v) in drain_kw_ordered(&mut a.kw, &ctx.call_kw_order) {
                if k == "name" {
                    return err("git_override(): 'name' attribute shouldn't be specified; use 'module_name' instead");
                }
                if k == "remote" {
                    has_remote = true;
                }
                let v = AttrVal::from_value(&v);
                if k == "patches" {
                    let l: Vec<AttrVal> = v.str_list().into_iter().map(|p| AttrVal::Str(patch_label_to_canonical(&p))).collect();
                    spec_attrs.push((k, AttrVal::List(l)));
                } else {
                    spec_attrs.push((k, v));
                }
            }
            let _ = has_remote;
            add_override(&mut s, module_name, ModuleOverride::NonRegistry(RepoSpec::new(GIT_BZL, "git_repository", spec_attrs)))
        }
        "local_path_override" => {
            nonmodule(&mut s);
            let module_name = a.req_str("module_name", false)?;
            let path = a.req_str("path", false)?;
            a.finish()?;
            validate_module_name(&module_name)?;
            add_override(
                &mut s,
                module_name,
                ModuleOverride::NonRegistry(RepoSpec::new(LOCAL_BZL, "local_repository", vec![("path".into(), AttrVal::Str(path))])),
            )
        }
        "flag_alias" => {
            nonmodule(&mut s);
            let n = a.req_str("name", false)?;
            let label = a.req_str("starlark_flag", false)?;
            a.finish()?;
            s.flag_aliases.push((n, label));
            Ok(Value::None)
        }
        other => Ok(host_str(&format!("<unknown host function {other}>"))),
    }
}

fn validate_patch_label(raw: &str) -> Result<(), StarlarkError> {
    if raw.starts_with("//") || raw.starts_with("@//") || raw.starts_with("@@//") || raw.starts_with(':') {
        Ok(())
    } else if raw.starts_with('@') {
        let repo = raw.trim_start_matches('@').split("//").next().unwrap_or("");
        err(format!("no repo visible as '@{repo}' here (patch label '{raw}' must refer to the root module)"))
    } else {
        err(format!("invalid label \"{raw}\" in 'patches': absolute label must begin with '@' or '//'"))
    }
}

fn patch_label_to_canonical(raw: &str) -> String {
    let r = raw.trim_start_matches('@');
    let r = if let Some(x) = r.strip_prefix("//") { format!("//{x}") } else { r.to_string() };
    if r.starts_with("//") {
        let (pkg, name) = match r.trim_start_matches("//").split_once(':') {
            Some((p, n)) => (p.to_string(), n.to_string()),
            None => (r.trim_start_matches("//").to_string(), r.rsplit('/').next().unwrap_or("").to_string()),
        };
        format!("@@//{pkg}:{name}")
    } else {
        format!("@@//{}", r.trim_start_matches(':').replace("//", ""))
    }
}

fn add_override(s: &mut State, module_name: String, o: ModuleOverride) -> Result<Value, StarlarkError> {
    if s.overrides.iter().any(|(n, _)| *n == module_name) {
        return err(format!("multiple overrides for dep {module_name} found"));
    }
    s.overrides.push((module_name, o));
    Ok(Value::None)
}

fn add_repo_name_usage(s: &mut State, repo: &str, how: &str, loc: &str) -> Result<(), StarlarkError> {
    if let Some((chow, cloc)) = s.repo_name_usages.insert(repo.to_string(), (how.to_string(), loc.to_string())) {
        return err(format!(
            "The repo name '{repo}' cannot be defined {how} at {loc} as it is already defined {chow} at {cloc}"
        ));
    }
    Ok(())
}

fn get_or_create_usage(s: &mut State, bzl: &str, name: &str, isolate: bool) -> usize {
    if !isolate {
        if let Some(i) = s.usages.iter().position(|u| u.bzl_file == bzl && u.extension_name == name && !u.isolate) {
            return i;
        }
    }
    s.usages.push(UsageBuilder {
        bzl_file: bzl.to_string(),
        extension_name: name.to_string(),
        isolate,
        proxies: vec![],
        imports: vec![],
        repo_overrides: vec![],
        tags: vec![],
        detached: false,
    });
    s.usages.len() - 1
}

fn add_import(s: &mut State, u: usize, p: usize, local: &str, exported: &str, how: &str, loc: &str) -> Result<(), StarlarkError> {
    validate_user_repo_name(local)?;
    validate_user_repo_name(exported)?;
    add_repo_name_usage(s, local, how, loc)?;
    let ub = &mut s.usages[u];
    if let Some((coll, _)) = ub.imports.iter().find(|(_, e)| e == exported) {
        let coll = coll.clone();
        let cloc = s.repo_name_usages.get(&coll).map(|x| x.1.clone()).unwrap_or_default();
        return err(format!(
            "The repo exported as '{exported}' by module extension '{}' is already imported at {cloc}",
            s.usages[u].extension_name
        ));
    }
    ub.imports.push((local.to_string(), exported.to_string()));
    if let Some(proxy) = ub.proxies.get_mut(p) {
        proxy.imports.push((local.to_string(), exported.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(root: bool) -> ModuleFileOptions {
        ModuleFileOptions {
            key: if root { ModuleKey::root() } else { ModuleKey::new("foo", Version::parse("1.0").unwrap()) },
            ignore_dev_deps: !root,
            isolated_extension_usages: true,
            registry_url: None,
            file_name: "MODULE.bazel".into(),
            include_reader: None,
            builtin_modules: vec![],
        }
    }

    #[test]
    fn full_module_file() {
        let src = r#"
module(name = "root", version = "1.0", repo_name = "my_root")
bazel_dep(name = "a", version = "1.2", repo_name = "a_repo")
bazel_dep(name = "devonly", version = "1", dev_dependency = True)
bazel_dep(name = "nodep", version = "1", repo_name = None)
register_toolchains("//:tc", "@a//:all")
register_execution_platforms("//:plat")
ext = use_extension("@a//:ext.bzl", "ext")
ext.tag(x = 1, y = ["a"])
use_repo(ext, "r1", renamed = "r2")
http_archive = use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl", "http_archive")
http_archive(name = "arch", urls = ["http://x"])
single_version_override(module_name = "a", version = "1.3", patches = ["//:p.patch"], patch_strip = 1)
local_path_override(module_name = "devonly", path = "../x")
"#;
        let r = eval_module_file(src, opts(true)).unwrap();
        let m = r.module;
        assert_eq!(m.name, "root");
        assert_eq!(m.repo_name, "my_root");
        assert_eq!(m.deps.len(), 2);
        assert_eq!(m.deps[0].0, "a_repo");
        assert_eq!(m.nodep_deps.len(), 1);
        assert_eq!(m.toolchains_to_register, vec!["//:tc", "@a//:all"]);
        assert_eq!(m.extension_usages.len(), 2);
        let u = &m.extension_usages[0];
        assert_eq!(u.bzl_file, "@a//:ext.bzl");
        assert_eq!(u.proxies[0].proxy_name, "ext");
        assert_eq!(u.tags[0].tag_name, "tag");
        assert_eq!(u.proxies[0].imports, vec![("r1".into(), "r1".into()), ("renamed".into(), "r2".into())]);
        assert!(m.extension_usages[1].is_innate());
        assert_eq!(r.overrides.len(), 2);
    }

    #[test]
    fn non_root_ignores_dev_deps_and_errors() {
        let src = "module(name='foo')\nbazel_dep(name='x', version='1', dev_dependency=True)\n";
        let r = eval_module_file(src, opts(false)).unwrap();
        assert!(r.module.deps.is_empty());
        let e = eval_module_file("bazel_dep(name='x')\nmodule(name='y')", opts(true)).unwrap_err();
        assert!(e.contains("if module() is called, it must be called before any other functions"), "{e}");
        let e = eval_module_file("module(name='a')\nbazel_dep(name='a')", opts(true)).unwrap_err();
        assert!(e.contains("The repo name 'a' cannot be defined by a bazel_dep"), "{e}");
        let e = eval_module_file("bazel_dep(name='x', repo_name='y')\nbazel_dep(name='z', repo_name='y')", opts(true)).unwrap_err();
        assert!(e.contains("The repo name 'y' cannot be defined"), "{e}");
        let e = eval_module_file("bazel_dep(name='Bad')", opts(true)).unwrap_err();
        assert!(e.contains("invalid module name 'Bad'"), "{e}");
    }

    #[test]
    fn include_works() {
        let mut o = opts(true);
        o.include_reader = Some(Arc::new(|l| {
            if l == "//:more.MODULE.bazel" { Ok("bazel_dep(name='inc', version='1')".into()) } else { Err("nope".into()) }
        }));
        let r = eval_module_file("module(name='r')\ninclude('//:more.MODULE.bazel')", o).unwrap();
        assert_eq!(r.module.deps[0].0, "inc");
    }

    fn err_of(src: &str) -> String {
        eval_module_file(src, opts(true)).unwrap_err()
    }

    #[test]
    fn builtin_argument_binding_errors_match_bazel() {
        assert!(err_of("module(\"a\")").contains("Error in module: module() got unexpected positional argument\n"), "{}", err_of("module(\"a\")"));
        assert!(err_of("bazel_dep(\"md\", \"1.0\")").contains("Error in bazel_dep: bazel_dep() got unexpected positional argument"));
        assert!(err_of("bazel_dep()").contains("bazel_dep() missing 1 required named argument: name"));
        assert!(err_of("module(name = \"a\", nme = \"x\")")
            .contains("module() got unexpected keyword argument 'nme' (did you mean 'name'?)"));
        assert!(err_of("module(name = 1)").contains("in call to module(), parameter 'name' got value of type 'int', want 'string'"));
        assert!(err_of("module(name = \"a\", compatibility_level = \"x\")")
            .contains("in call to module(), parameter 'compatibility_level' got value of type 'string', want 'int'"));
        assert!(err_of("bazel_dep(name = \"md\", dev_dependency = \"yes\")")
            .contains("parameter 'dev_dependency' got value of type 'string', want 'bool'"));
        assert!(err_of("use_extension(\"//:x.bzl\")").contains("use_extension() missing 1 required positional argument: extension_name"));
        assert!(err_of("use_extension(\"//:x.bzl\", \"e\", \"f\")").contains("use_extension() accepts no more than 2 positional arguments but got 3"));
        assert!(err_of("e = use_extension(\"//:x.bzl\", \"e\")\nuse_repo(\"int\")")
            .contains("parameter 'extension_proxy' got value of type 'string', want 'module_extension_proxy'"));
        assert!(err_of("e = use_extension(\"//:x.bzl\", \"e\")\nuse_repo(e, 1)").contains("at index 0 of args, got element of type int, want string"));
        assert!(err_of("e = use_extension(\"//:x.bzl\", \"e\")\nuse_repo(e, a = 1)")
            .contains("got dict<string, int> for 'kwargs', want dict<string, string>"));
        assert!(err_of("register_toolchains(1)").contains("at index 0 of register_toolchains, got element of type int, want string"));
        assert!(err_of("register_toolchains(\"not a label\")").contains(
            "Expected absolute target patterns (must begin with '//' or '@') for 'register_toolchains' argument, but got 'not a label' as an argument"
        ));
        assert!(err_of("r = use_repo_rule(\"//:x.bzl\", \"r\")\nr()").contains("Error in call: call() missing 1 required named argument: name"));
        assert!(err_of("r = use_repo_rule(\"//:x.bzl\", \"r\")\nr(\"n\")").contains("call() got unexpected positional argument"));
    }

    #[test]
    fn version_and_compatibility_messages_follow_head() {
        let e = err_of("module(name = \"a\", version = \"1.0!\")");
        assert!(e.contains("Error in module: Invalid version in module()\n"), "{e}");
        let e = err_of("bazel_dep(name = \"md\", version = \"1.0!\")");
        assert!(e.contains("Error in bazel_dep: Invalid version in bazel_dep()\n"), "{e}");
        let e = err_of("module(name = \"a\", bazel_compatibility = [\"99\"])");
        assert!(e.contains("contain a version number in form of X.X.X where X is a number"), "{e}");
    }

    #[test]
    fn round8_compat_literal_kwargs_and_tag_positional() {
        // `VALID_BAZEL_COMPATIBILITY_VERSION` needs the full X.X.X.
        for bad in ["<100", ">=1.2", ">1.2.3.4", "1.2.3", ">=a.b.c", "<1.2."] {
            let e = err_of(&format!("module(name = \"a\", bazel_compatibility = [\"{bad}\"])"));
            assert!(e.contains("invalid version argument"), "{bad}: {e}");
        }
        for good in ["<100.0.0", ">=1.2.3", "-9.0.1", "<=1.0.0", ">0.0.1"] {
            assert!(eval_module_file(&format!("module(name = 'a', bazel_compatibility = ['{good}'])"), opts(true)).is_ok(), "{good}");
        }
        // HEAD's MODULE.bazel checker allows a literal `**{...}` (`allowLiteralStarStarArgs = true`)...
        let e = err_of("bazel_dep(name = \"ma\", **{\"name\": \"mb\"})");
        assert!(e.contains("bazel_dep() got multiple values for argument 'name'"), "{e}");
        assert!(!e.contains("are not allowed"), "{e}");
        // ...but not a non-literal one.
        let e = err_of("d = {}\nbazel_dep(name = \"ma\", **d)");
        assert!(e.contains("**kwargs arguments must be a literal dict in MODULE.bazel files."), "{e}");
        // Tag proxies take keyword arguments only.
        let e = err_of("e = use_extension(\"//:e.bzl\", \"ex\")\ne.tag(1)");
        assert!(e.contains("call() got unexpected positional argument"), "{e}");
        let e = err_of("e = use_extension(\"//:e.bzl\", \"ex\")\nt = e.tag\nt(1)");
        assert!(e.contains("call() got unexpected positional argument"), "{e}");
    }

    #[test]
    fn traceback_has_trailer_and_no_extra_prefix_lines() {
        let e = err_of("module(name = \"a\")\nmodule(name = \"b\")");
        let lines: Vec<&str> = e.lines().collect();
        assert!(lines[0].contains("Traceback (most recent call last):"), "{e}");
        assert_eq!(lines[1], "\tFile \"MODULE.bazel\", line 2, column 7, in <toplevel>");
        assert_eq!(lines.last().unwrap(), &"ERROR: error executing MODULE.bazel file for <root>");
        assert_eq!(e.matches("error executing MODULE.bazel file").count(), 1, "{e}");
    }

    #[test]
    fn include_errors_carry_include_frames_and_one_trailer() {
        let mut o = opts(true);
        o.include_reader = Some(Arc::new(|_| Ok("x = 1\nfail(\"boom\")".into())));
        let e = eval_module_file("module(name = 'r')\ninclude('//:a.MODULE.bazel')", o).unwrap_err();
        let want_prefix = "\tFile \"MODULE.bazel\", line 2, column 8, in <toplevel>\n\t\tinclude('//:a.MODULE.bazel')\n\tFile \"<builtin>\", in include\n";
        assert!(e.contains(want_prefix), "{e}");
        assert!(e.contains("Error in fail: boom"), "{e}");
        assert_eq!(e.matches("error executing MODULE.bazel file").count(), 1, "{e}");
        // `include` statement errors are reported at the start of the statement (column 1).
        let mut o = opts(true);
        o.include_reader = Some(Arc::new(|_| Err("not a regular file".into())));
        let e = eval_module_file("module(name = 'r')\ninclude('//:a.MODULE.bazel')", o).unwrap_err();
        assert!(e.contains("included at MODULE.bazel:2:1: not a regular file"), "{e}");
    }

    #[test]
    fn include_directive_static_checks() {
        let e = err_of("include()");
        assert!(e.contains("MODULE.bazel:1:1: the `include` directive MUST be called with one positional string literal argument"), "{e}");
        assert!(e.ends_with("ERROR: syntax error in MODULE.bazel file for <root>"), "{e}");
        let e = err_of("x = include");
        assert!(e.contains("MODULE.bazel:1:5: the `include` directive MUST be called directly at the top-level"), "{e}");
        let e = err_of("return");
        assert!(e.contains("return statements must be inside a function"), "{e}");
    }

    #[test]
    fn bare_extension_label_is_a_root_package_target() {
        let r = eval_module_file("e = use_extension(\"foo\", \"bar\")", opts(true)).unwrap();
        assert_eq!(r.module.extension_usages[0].bzl_file, "//:foo");
    }

    #[test]
    fn build_module_errors_report_traceback_and_root_trailer() {
        let e = err_of("e = use_extension(\"//:x.bzl\", \"e\")\noverride_repo(e, \"x\")");
        assert!(e.contains("Error in override_repo: The repo exported as 'x' by module extension 'e' is overridden with 'x', but no repo is visible under this name"), "{e}");
        assert!(e.ends_with("ERROR: error executing MODULE.bazel file for the root module"), "{e}");
    }

    #[test]
    fn override_display_is_java_record_tostring() {
        let o = ModuleOverride::SingleVersion {
            version: Version::parse("1.0").unwrap(),
            registry: String::new(),
            patches: vec!["@@//:a.patch".into()],
            patch_cmds: vec![],
            patch_strip: 2,
        };
        assert_eq!(o.to_string(), "SingleVersionOverride[version=1.0, registry=, patches=[//:a.patch], patchCmds=[], patchStrip=2]");
        let m = ModuleOverride::MultipleVersion { versions: vec![Version::parse("1.0").unwrap(), Version::parse("1.1").unwrap()], registry: "http://x".into() };
        assert_eq!(m.to_string(), "MultipleVersionOverride[versions=[1.0, 1.1], registry=http://x]");
    }
}
