//! `repository_ctx` / `module_ctx` / `path` implementations (host objects for Starlark).
//! Mirrors `StarlarkBaseExternalContext`, `StarlarkRepositoryContext`, `ModuleExtensionContext`
//! and `StarlarkPath`.

use crate::archive::{extract, ExtractOptions};
use crate::download::{DownloadRequest, Downloader, RepositoryCache};
use crate::model::AttrVal;
use crate::rt::*;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use zel_starlark::{EvalContext, StarlarkError, StructValue, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CtxKind {
    Repository,
    Module,
}

#[derive(Clone)]
pub struct CtxData {
    pub kind: CtxKind,
    pub name: String,
    /// Canonical repo name this context runs for (the generated repo, or the extension's repo).
    pub canonical: String,
    /// Directory in which relative paths resolve and writes are allowed.
    pub dir: PathBuf,
    pub attr: Value,
    pub modules: Value,
    pub root_has_non_dev: bool,
    pub env_seen: Arc<Mutex<BTreeMap<String, Option<String>>>>,
    pub file_inputs: Arc<Mutex<BTreeMap<String, String>>>,
    /// Recorded inputs (`RepoRecordedInput` + value) in recording order: `("ENV:NAME", Some(value))`.
    pub rec: Arc<Mutex<Vec<(String, Option<String>)>>>,
    pub label_repo: String,
    pub label_pkg: String,
    pub reproducible: Arc<Mutex<bool>>,
    pub facts: Value,
    pub is_isolated: bool,
    pub extension_id: Option<String>,
}

pub struct PendingDownload {
    pub receiver: Option<std::sync::mpsc::Receiver<(Result<crate::download::DownloadResult, String>, Vec<String>)>>,
    /// Directory the archive is extracted into (`download_and_extract`) or the download target.
    pub output: PathBuf,
    pub target: PathBuf,
    pub allow_fail: bool,
    pub extract_after: bool,
    pub tmp_dir: Option<PathBuf>,
    pub extract_opts: Option<ExtractOptions>,
    pub deferred: Option<String>,
    pub canonical: String,
}

static PENDING_DOWNLOADS: Mutex<Vec<Option<PendingDownload>>> = Mutex::new(Vec::new());

fn register_pending_download(pd: PendingDownload) -> usize {
    let mut list = PENDING_DOWNLOADS.lock().unwrap();
    list.push(Some(pd));
    list.len() - 1
}

fn take_pending_download(id_str: &str) -> Result<PendingDownload, StarlarkError> {
    let tok = id_str.split('|').next().unwrap_or(id_str);
    let idx: usize = tok.parse().map_err(|_| StarlarkError("invalid pending_download id".into()))?;
    let mut list = PENDING_DOWNLOADS.lock().unwrap();
    if idx >= list.len() || list[idx].is_none() {
        return Err(StarlarkError("pending_download has already completed or is invalid".into()));
    }
    Ok(list[idx].take().unwrap())
}

fn wait_pending_download(rt: &Arc<Rt>, id_str: &str) -> Result<Value, StarlarkError> {
    let mut pd = take_pending_download(id_str)?;
    let Some(rx) = pd.receiver.take() else { return err("pending_download has no receiver") };
    let (r, warnings) = match rx.recv() {
        Ok(x) => x,
        Err(_) => return err("Download thread terminated unexpectedly"),
    };
    finish_download(rt, pd, r, warnings)
}

/// Completes a download like `StarlarkBaseExternalContext.completeDownload` (plus extraction for
/// `download_and_extract`).
fn finish_download(rt: &Arc<Rt>, pd: PendingDownload, r: Result<crate::download::DownloadResult, String>, warnings: Vec<String>) -> Result<Value, StarlarkError> {
    for w in warnings {
        rt.info(w);
    }
    let cleanup = |tmp: &Option<PathBuf>| {
        if let Some(t) = tmp {
            let _ = std::fs::remove_dir_all(t);
        }
    };
    let res = match r {
        Ok(res) => res,
        Err(e) => {
            let e = if e.starts_with("java.io.") { e } else { format!("java.io.IOException: {e}") };
            if pd.allow_fail {
                if !pd.extract_after {
                    cleanup(&pd.tmp_dir);
                }
                return Ok(struct_value(vec![("success", Value::Bool(false))]));
            }
            return err(e);
        }
    };
    if res.from_cache {
        let url = res.url.clone();
        let hit_url = if url.is_empty() { String::new() } else { url };
        CTX_CACHE_HITS.with(|h| h.borrow_mut().push((res.sha256.clone(), hit_url)));
    }
    if let Some(m) = pd.deferred {
        return err(m);
    }
    let size = std::fs::metadata(&res.path).map(|m| m.len() as i64).unwrap_or(0);
    if pd.extract_after {
        if let Some(opts) = &pd.extract_opts {
            let r = std::fs::create_dir_all(&pd.output).map_err(|e| e.to_string()).and_then(|_| extract(&pd.target, &pd.output, opts));
            if let Err(e) = r {
                return err(java_extract_error(&pd.target, &pd.output, e));
            }
        }
        cleanup(&pd.tmp_dir);
    }
    Ok(struct_value(vec![
        ("success", Value::Bool(true)),
        ("sha256", str_value(res.sha256)),
        ("integrity", str_value(res.integrity)),
        ("size_bytes", int_value(size)),
    ]))
}

thread_local! {
    /// Repository-cache hits of downloads done by repository rules of the repo currently being fetched
    /// (`CacheHitReportingModule`): `(hash, url)`.
    pub(crate) static CTX_CACHE_HITS: std::cell::RefCell<Vec<(String, String)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Frames an archive extraction failure the way Bazel's `DecompressorValue` reports it.
fn java_extract_error(archive: &Path, dest: &Path, e: String) -> String {
    if e.starts_with("Expected a file with a") || e.starts_with("No decompressor found") || e.starts_with("java.io.") {
        e
    } else {
        // `DecompressorValue.decompress` reports the archive's parent directory, not the destination.
        let shown = archive.parent().unwrap_or(dest);
        let prefix = format!("{}: ", archive.display());
        let msg = e.strip_prefix(&prefix).unwrap_or(&e);
        format!("java.io.IOException: Error extracting {} to {}: {}", archive.display(), shown.display(), crate::archive::java_message(msg))
    }
}

fn int_arg(kw: &BTreeMap<String, Value>, name: &str) -> i64 {
    match kw.get(name) {
        Some(Value::Int(i)) => i.to_i64().unwrap_or(0),
        _ => 0,
    }
}

fn renamed_strip_prefix(method: &str, strip_prefix: &str, old: &str) -> Result<String, StarlarkError> {
    if old.is_empty() {
        return Ok(strip_prefix.to_string());
    }
    if strip_prefix.is_empty() {
        return Ok(old.to_string());
    }
    err(format!("{method}() got multiple values for parameter 'strip_prefix' (via compatibility alias 'stripPrefix')"))
}

fn validate_stripping(method: &str, strip_prefix: &str, strip_components: i64) -> Result<(), StarlarkError> {
    if strip_components < 0 {
        return err(format!("{method}() has an invalid argument for 'strip_components': {strip_components}. Must be non-negative."));
    }
    if !strip_prefix.is_empty() && strip_components > 0 {
        return err(format!("{method}() got multiple strip values. Only one of 'strip_prefix' or 'strip_components' can be set"));
    }
    Ok(())
}

/// `Dict.cast` failure text: `got dict<K, V> for 'what', want dict<string, W>`.
fn dict_cast_error(d: &[(Value, Value)], what: &str, want_val: &str, key_ok: impl Fn(&Value) -> bool, val_ok: impl Fn(&Value) -> bool) -> Option<StarlarkError> {
    let bad = d.iter().find(|(k, v)| !key_ok(k) || !val_ok(v))?;
    let kt = if key_ok(&bad.0) { "string" } else { bad.0.type_name() };
    let vt = if val_ok(&bad.1) { want_val } else { bad.1.type_name() };
    Some(StarlarkError(format!("got dict<{kt}, {vt}> for '{what}', want dict<string, {want_val}>")))
}

/// `getAuthHeaders(getAuthContents(auth))`: the validated `auth` dict as url -> entries.
fn parse_auth(v: Option<&Value>) -> Result<BTreeMap<String, BTreeMap<String, String>>, StarlarkError> {
    let mut out = BTreeMap::new();
    let Some(Value::Dict(d)) = v else { return Ok(out) };
    let entries: Vec<(Value, Value)> = d.lock().unwrap().entries.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    if let Some(e) = dict_cast_error(&entries, "auth", "dict", |k| matches!(k, Value::String(_)), |v| matches!(v, Value::Dict(_))) {
        return Err(e);
    }
    for (k, v) in entries {
        let (Value::String(url), Value::Dict(m)) = (k, v) else { continue };
        let map: BTreeMap<String, Value> = m.lock().unwrap().entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let sget = |n: &str| map.get(n).map(|v| v.to_string());
        match map.get("type").map(|v| v.to_string()).as_deref() {
            Some("basic") => {
                if !map.contains_key("login") || !map.contains_key("password") {
                    return err(format!("Found request to do basic auth for {url} without 'login' and 'password' being provided."));
                }
            }
            Some("pattern") => {
                let Some(pattern) = sget("pattern") else {
                    return err(format!("Found request to do pattern auth for {url} without a pattern being provided"));
                };
                for comp in ["password", "login"] {
                    if pattern.contains(&format!("<{comp}>")) && !map.contains_key(comp) {
                        return err(format!("Auth pattern contains <{comp}> but it was not provided in auth dict."));
                    }
                }
            }
            _ => {}
        }
        out.insert(url, map.iter().map(|(k, v)| (k.clone(), v.to_string())).collect());
    }
    Ok(out)
}

/// `getHeaderContents`: header name -> value(s) joined into one string.
fn parse_headers(v: Option<&Value>) -> Result<BTreeMap<String, String>, StarlarkError> {
    let mut out = BTreeMap::new();
    let Some(Value::Dict(d)) = v else { return Ok(out) };
    let entries: Vec<(Value, Value)> = d.lock().unwrap().entries.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    if let Some(bad) = entries.iter().find(|(k, _)| !matches!(k, Value::String(_))) {
        return err(format!("got dict<{}, {}> for 'headers', want dict<string, unknown>", bad.0.type_name(), bad.1.type_name()));
    }
    for (k, val) in entries {
        let value = match &val {
            Value::String(s) => s.clone(),
            Value::List(_) | Value::Tuple(_) => {
                let items = zel_starlark::eval::builtins::value_to_sequence(&val)?;
                let mut parts = vec![];
                for (i, it) in items.iter().enumerate() {
                    match it {
                        Value::String(s) => parts.push(s.clone()),
                        other => return err(format!("at index {i}, got {} for 'header values', want string", other.type_name())),
                    }
                }
                parts.join(",")
            }
            _ => return err("headers argument must be a dict whose keys are string and whose values are either string or sequence of string"),
        };
        out.insert(k.to_string(), value);
    }
    Ok(out)
}

fn is_localhost_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1" || host == "[::1]"
}

/// `getUrls`: validates the `url` argument and drops plain-http URLs lacking a checksum.
fn get_urls(v: &Value, ensure_non_empty: bool, checksum_given: bool) -> Result<Vec<String>, StarlarkError> {
    let list: Vec<String> = match v {
        Value::String(s) => vec![s.clone()],
        other => {
            let mut out = vec![];
            for it in zel_starlark::eval::builtins::value_to_sequence(other)? {
                match it {
                    Value::String(s) => out.push(s),
                    o => return err(format!("Expected a string or sequence of strings for 'url' argument, but got '{}' item in the sequence", o.type_name())),
                }
            }
            out
        }
    };
    if ensure_non_empty && list.is_empty() {
        return err("java.io.IOException: urls not set");
    }
    let mut out = vec![];
    for u in list {
        let parsed = crate::download::Url::parse(&u).map_err(|_| StarlarkError(format!("java.io.IOException: Bad URL: {u}")))?;
        if !matches!(parsed.scheme.to_ascii_lowercase().as_str(), "http" | "https" | "file") {
            return err(format!("java.io.IOException: Unsupported protocol: {}", parsed.scheme));
        }
        if !checksum_given {
            if !parsed.scheme.eq_ignore_ascii_case("http") {
                out.push(u);
            }
        } else {
            out.push(u);
        }
    }
    if ensure_non_empty && out.is_empty() {
        return err("java.io.IOException: No URLs left after removing plain http URLs due to missing checksum. Please provide either a checksum or an https download location.");
    }
    Ok(out)
}

/// `Path.createTempDirectory("temp")`: a fresh `temp<digits>` directory under `parent`.
fn make_temp_dir(parent: &Path) -> Result<PathBuf, StarlarkError> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) ^ ((std::process::id() as u64) << 32);
    loop {
        let p = parent.join(format!("temp{n}"));
        match std::fs::create_dir(&p) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700))
                        .map_err(|e| StarlarkError(format!("java.io.IOException: {} ({})", p.display(), errno_text(&e))))?;
                }
                return Ok(p);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n = n.wrapping_add(7919),
            Err(e) => return err(format!("java.io.IOException: {} ({})", p.display(), errno_text(&e))),
        }
    }
}

fn err<T>(m: impl Into<String>) -> Result<T, StarlarkError> {
    Err(StarlarkError(m.into()))
}

fn s_err<T>(r: Result<T, String>) -> Result<T, StarlarkError> {
    r.map_err(StarlarkError)
}

/// Coerces and validates `kw` against attribute definitions, returning typed values.
pub fn validate_attrs(rule: &str, defs: &[(String, crate::rt::AttrDef)], kw: &BTreeMap<String, Value>) -> Result<(), StarlarkError> {
    for k in kw.keys() {
        if k != "name" && !defs.iter().any(|(n, _)| n == k) {
            let rname = kw.get("name").and_then(|v| match v {
                Value::String(s) => Some(s.as_str()),
                _ => None,
            }).unwrap_or("");
            let mut names: Vec<&str> = defs.iter().map(|(n, _)| n.as_str()).collect();
            names.push("name");
            let hint = crate::ctxargs::did_you_mean(k, &names);
            return err(format!("in call to '{rule}' repo rule with name '{rname}', unknown attribute '{k}' provided{hint}"));
        }
    }
    for (n, d) in defs {
        match kw.get(n) {
            None | Some(Value::None) => {
                if d.mandatory {
                    let rname = kw.get("name").and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
                    return err(format!("in call to '{rule}' repo rule with name '{rname}', mandatory attribute '{n}' isn't being specified"));
                }
            }
            Some(v) => check_type(rule, n, d, v)?,
        }
    }
    Ok(())
}

/// Argument validation for a repository rule call (`RepositoryRuleFunction` / attribute coercion): unknown attribute,
/// type mismatch with Bazel's wording, then the remaining checks (mandatory, allowed values).
pub fn validate_repo_rule_call(rule: &str, defs: &[(String, crate::rt::AttrDef)], kw: &BTreeMap<String, Value>, order: &[String]) -> Result<(), StarlarkError> {
    let rname = kw.get("name").and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
    // Implicit attributes of every rule class; `repo_mapping` is not available to bzlmod repo rule calls.
    let implicit = ["tags", "visibility"];
    let mut kw2 = kw.clone();
    for i in implicit {
        kw2.remove(i);
    }
    let defs2: Vec<(String, crate::rt::AttrDef)> = defs.iter().filter(|(n, _)| n != "repo_mapping").cloned().collect();
    // unknown attributes first (via the generic check), then types
    let keys: Vec<&String> = if !order.is_empty() && order.iter().all(|k| kw2.contains_key(k) || !kw.contains_key(k)) {
        let mut v: Vec<&String> = order.iter().filter(|k| kw2.contains_key(*k)).collect();
        for k in kw2.keys() {
            if !v.contains(&k) {
                v.push(k);
            }
        }
        v
    } else {
        kw2.keys().collect()
    };
    for k in keys {
        if k != "name" && !defs2.iter().any(|(n, _)| n == k) {
            // report the first unknown attribute in call order
            let mut only = BTreeMap::new();
            only.insert("name".to_string(), kw2.get("name").cloned().unwrap_or(Value::None));
            only.insert(k.clone(), kw2[k].clone());
            return validate_attrs(rule, &defs2, &only);
        }
    }
    for (k, v) in &kw2 {
        if matches!(v, Value::None) || k == "name" {
            continue;
        }
        let Some((_, d)) = defs2.iter().find(|(n, _)| n == k) else { continue };
        if let Some((ty, what, bad)) = repo_attr_type_error(&d.kind, v) {
            let what = if what == "attribute" { format!("attribute '{k}'") } else { what };
            let (r, t) = match bad {
                Some((r, t)) => (r, t),
                None => (v.repr(), v.type_name().to_string()),
            };
            return err(format!(
                "in call to '{rule}' repo rule with name '{rname}', expected value of type '{ty}' for {what}, but got {r} ({t})"
            ));
        }
    }
    validate_attrs(rule, &defs2, &kw2)
}

/// Label-typed attributes of a repo rule call: target-name syntax and (for calls made from a `MODULE.bazel` file of
/// `owner`) visibility of the repo a label refers to.
pub fn check_repo_rule_labels(
    rt: &Arc<Rt>,
    rule: &str,
    defs: &[(String, crate::rt::AttrDef)],
    kw: &BTreeMap<String, Value>,
    owner: Option<&str>,
) -> Result<(), StarlarkError> {
    let rname = kw.get("name").and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
    for (k, v) in kw {
        let Some((_, d)) = defs.iter().find(|(n, _)| n == k) else { continue };
        let a = AttrVal::from_value(v);
        let mut raws: Vec<String> = vec![];
        match (d.kind.as_str(), &a) {
            ("label", AttrVal::Str(s) | AttrVal::Label(s)) => raws.push(s.clone()),
            ("label_list", AttrVal::List(l)) => raws.extend(l.iter().filter_map(|e| e.as_str().map(String::from))),
            ("label_keyed_string_dict", AttrVal::Dict(m)) => raws.extend(m.iter().map(|(k, _)| k.clone())),
            ("string_keyed_label_dict", AttrVal::Dict(m)) => raws.extend(m.iter().filter_map(|(_, e)| e.as_str().map(String::from))),
            ("label_list_dict", AttrVal::Dict(m)) => {
                for (_, e) in m {
                    if let AttrVal::List(l) = e {
                        raws.extend(l.iter().filter_map(|e| e.as_str().map(String::from)));
                    }
                }
            }
            _ => {}
        }
        for raw in raws {
            let after_repo = match raw.find("//") {
                Some(i) if raw.starts_with('@') || raw.starts_with("//") => &raw[i + 2..],
                _ => raw.as_str(),
            };
            if let Some((_, name)) = after_repo.split_once(':') {
                if name.contains(':') {
                    return err(format!(
                        "in call to '{rule}' repo rule with name '{rname}', invalid label '{raw}' in attribute '{k}': invalid target name '{name}': target names may not contain ':'"
                    ));
                }
            }
            if let (Some(o), true) = (owner, raw.starts_with('@') && !raw.starts_with("@@")) {
                if let Ok((repo, _, _)) = rt.resolve_label(&raw, o, "") {
                    if repo.starts_with("[unknown repo") {
                        let apparent = raw[1..].split("//").next().unwrap_or("");
                        let who = if o.is_empty() { "the root module".to_string() } else { format!("module '{o}'") };
                        return err(format!(
                            "in call to '{rule}' repo rule with name '{rname}', no repository visible as '@{apparent}' to {who}, but referenced by label '{raw}' in attribute '{k}'"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// `(expected type name, context)` when `v` cannot be coerced to an attribute of `kind`.
fn repo_attr_type_error(kind: &str, v: &Value) -> Option<(String, String, Option<(String, String)>)> {
    let a = AttrVal::from_value(v);
    let (ty, elem): (&str, Option<&str>) = match kind {
        "string" | "label" | "output" => ("string", None),
        "int" => ("int", None),
        "bool" => ("boolean", None),
        "string_list" | "output_list" => ("list(string)", Some("string")),
        "label_list" => ("list(label)", Some("string")),
        "int_list" => ("list(int)", Some("int")),
        "string_dict" => ("dict(string, string)", Some("dict")),
        "string_list_dict" => ("dict(string, list(string))", Some("dict")),
        "label_keyed_string_dict" => ("dict(label, string)", Some("dict")),
        "string_keyed_label_dict" => ("dict(string, label)", Some("dict")),
        "label_list_dict" => ("dict(string, list(label))", Some("dict")),
        _ => return None,
    };
    let ok_scalar = |a: &AttrVal, want: &str| match want {
        "string" => matches!(a, AttrVal::Str(_) | AttrVal::Label(_)),
        "int" => matches!(a, AttrVal::Int(_)),
        _ => true,
    };
    match elem {
        None => {
            let good = match kind {
                "int" => matches!(a, AttrVal::Int(_)),
                "bool" => matches!(a, AttrVal::Bool(_) | AttrVal::Int(0) | AttrVal::Int(1)),
                _ => matches!(a, AttrVal::Str(_) | AttrVal::Label(_)),
            };
            if good {
                None
            } else {
                Some((ty.to_string(), "attribute".to_string(), None))
            }
        }
        Some("dict") => match &a {
            AttrVal::Dict(entries) => {
                for (_, val) in entries {
                    let vok = match kind {
                        "string_list_dict" | "label_list_dict" => matches!(val, AttrVal::List(l) if l.iter().all(|e| matches!(e, AttrVal::Str(_) | AttrVal::Label(_)))),
                        _ => matches!(val, AttrVal::Str(_) | AttrVal::Label(_)),
                    };
                    if !vok {
                        return Some(("string".to_string(), "dict value element".to_string(), Some((val.repr(), val.type_name().to_string()))));
                    }
                }
                None
            }
            _ => Some((ty.to_string(), "attribute".to_string(), None)),
        },
        Some(el) => match &a {
            AttrVal::List(l) => {
                for e in l {
                    if !ok_scalar(e, el) {
                        return Some((el.to_string(), "list element".to_string(), Some((e.repr(), e.type_name().to_string()))));
                    }
                }
                None
            }
            _ => Some((ty.to_string(), "attribute".to_string(), None)),
        },
    }
}

fn check_type(rule: &str, n: &str, d: &AttrDef, v: &Value) -> Result<(), StarlarkError> {
    let a = AttrVal::from_value(v);
    let bad = |want: &str| err(format!("in attribute '{n}' of '{rule}' rule: got value of type '{}', want '{want}'", v.type_name()));
    match d.kind.as_str() {
        "string" => {
            if !matches!(a, AttrVal::Str(_) | AttrVal::Label(_)) {
                return bad("string");
            }
            if !d.values.is_empty() && !d.values.contains(&a) {
                return err(format!(
                    "{rule}: expected value of type '{}' for attribute '{n}' to be one of [{}], but got {}",
                    "string",
                    d.values.iter().map(|x| x.repr()).collect::<Vec<_>>().join(", "),
                    a.repr()
                ));
            }
        }
        "int" => {
            if !matches!(a, AttrVal::Int(_)) {
                return bad("int");
            }
        }
        "bool" => {
            if !matches!(a, AttrVal::Bool(_) | AttrVal::Int(0) | AttrVal::Int(1)) {
                return bad("bool");
            }
        }
        "string_list" | "label_list" | "int_list" | "output_list" => match &a {
            AttrVal::List(l) => {
                if !d.allow_empty && l.is_empty() {
                    return err(format!("attribute '{n}' in '{rule}' rule may not be empty"));
                }
                for e in l {
                    let ok = if d.kind == "int_list" { matches!(e, AttrVal::Int(_)) } else { matches!(e, AttrVal::Str(_) | AttrVal::Label(_)) };
                    if !ok {
                        return bad(&d.kind);
                    }
                }
            }
            _ => return bad(&d.kind),
        },
        "string_dict" | "string_list_dict" | "label_keyed_string_dict" => {
            if !matches!(a, AttrVal::Dict(_)) {
                return bad(&d.kind);
            }
        }
        "label" | "output" => {
            if !matches!(a, AttrVal::Str(_) | AttrVal::Label(_)) {
                return bad("string");
            }
        }
        _ => {}
    }
    Ok(())
}

/// Builds the `attr` struct (typed values, defaults applied, label attrs resolved).
pub fn build_attr_struct(
    rt: &Arc<Rt>,
    defs: &[(String, AttrDef)],
    given: &[(String, AttrVal)],
    label_repo: &str,
    label_pkg: &str,
    // Repository and package of the `.bzl` that defines the rule / tag class: label-typed
    // *defaults* are relative to it, given values to the user's.
    def_ctx: (&str, &str),
) -> Result<Value, String> {
    let mut out: Vec<(String, Value)> = vec![];
    let get = |n: &str| given.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
    let mut seen: Vec<&str> = vec![];
    let conv = |kind: &str, v: &AttrVal, is_default: bool| -> Result<Value, String> {
        let (cr, cp) = if is_default { def_ctx } else { (label_repo, label_pkg) };
        let lbl = |s: &str| -> Result<Value, String> {
            // Label attributes accept labels relative to the package (`foo.bzl` is `:foo.bzl`).
            let rel;
            let s = if !s.starts_with('@') && !s.starts_with('/') && !s.starts_with(':') {
                rel = format!(":{s}");
                rel.as_str()
            } else {
                s
            };
            let (r, p, n) = rt.resolve_label(s, cr, cp)?;
            if !is_default {
                if let Some(rest) = s.strip_prefix('@').filter(|r| !r.starts_with('@')) {
                    let apparent = rest.split_once("//").map(|x| x.0).unwrap_or(rest).to_string();
                    crate::rt::RECORD_TAG_MAPPINGS.with(|g| {
                        if let Some(v) = g.borrow_mut().as_mut() {
                            let e = (cr.to_string(), apparent, r.clone());
                            if !v.contains(&e) {
                                v.push(e);
                            }
                        }
                    });
                }
            }
            Ok(rt.label_value(&r, &p, &n))
        };
        Ok(match (kind, v) {
            ("label", AttrVal::Str(s) | AttrVal::Label(s)) => lbl(s)?,
            ("label_list", AttrVal::List(l)) => list_value(l.iter().map(|x| lbl(x.as_str().unwrap_or(""))).collect::<Result<_, _>>()?),
            ("string_keyed_label_dict", AttrVal::Dict(d)) => {
                let mut items = vec![];
                for (k, val) in d {
                    items.push((str_value(k), lbl(val.as_str().unwrap_or(""))?));
                }
                dict_value(items)
            }
            ("label_keyed_string_dict", AttrVal::Dict(d)) => {
                let mut items = vec![];
                for (k, val) in d {
                    items.push((lbl(k)?, val.to_value()));
                }
                dict_value(items)
            }
            ("bool", AttrVal::Int(i)) => Value::Bool(*i != 0),
            _ => v.to_value(),
        })
    };
    for (n, d) in defs {
        seen.push(n);
        let v = match get(n) {
            Some(AttrVal::None) | None => match &d.default {
                Some(def) => conv(&d.kind, def, true)?,
                None => match d.kind.as_str() {
                    "string" => str_value(""),
                    "int" => int_value(0),
                    "bool" => Value::Bool(false),
                    "string_list" | "label_list" | "int_list" | "output_list" => list_value(vec![]),
                    "string_dict" | "string_list_dict" | "label_keyed_string_dict" | "string_keyed_label_dict" | "label_list_dict" => dict_value(vec![]),
                    _ => Value::None,
                },
            },
            Some(v) => conv(&d.kind, &v, false)?,
        };
        out.push((n.clone(), v));
    }
    let m: BTreeMap<String, Value> = out.into_iter().collect();
    Ok(Value::Struct(Arc::new(zel_starlark::StructValue::new_attr(m))))
}

pub fn host_os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac os x"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

pub fn host_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_os = "macos") {
        "x86_64"
    } else {
        "amd64"
    }
}

fn os_value(rt: &Rt) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("name".to_string(), str_value(host_os_name()));
    fields.insert("arch".to_string(), str_value(host_arch()));
    fields.insert(
        "environ".to_string(),
        dict_value(rt.opts.client_env.iter().map(|(k, v)| (str_value(k), str_value(v))).collect()),
    );
    // `StarlarkOS` has no `repr`: Bazel prints it as an opaque object (never the environment).
    Value::Struct(Arc::new(StructValue::new_opaque(
        fields,
        "com.google.devtools.build.lib.bazel.repository.starlark.StarlarkOS",
    )))
}

pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            o => out.push(o.as_os_str()),
        }
    }
    out
}

impl CtxData {
    fn resolve(&self, rt: &Arc<Rt>, v: &Value) -> Result<PathBuf, StarlarkError> {
        match v {
            Value::String(s) => {
                if let Some(rest) = s.strip_prefix("@@[unknown repo '") {
                    let apparent = rest.split('\'').next().unwrap_or("");
                    let (repo_part, tail) = s.strip_prefix("@@").unwrap().split_once("//").unwrap_or((rest, ""));
                    let from = repo_part.rsplit("requested from ").next().unwrap_or("@@]").trim_end_matches(']');
                    let origin = if from == "@@" { "main repository".to_string() } else { format!("repository '{from}'") };
                    return err(format!(
                        "Unable to load package for @@{repo_part}//{tail}: The repository '@@{repo_part}' could not be resolved: No repository visible as '@{apparent}' from {origin}"
                    ));
                }
                let p = Path::new(s);
                Ok(normalize(&if p.is_absolute() { p.to_path_buf() } else { self.dir.join(p) }))
            }
            Value::Host(k, id) if k == "path" => Ok(PathBuf::from(id)),
            Value::Label(l) => {
                let repo = l.repository().name().to_string();
                if let Some(rest) = repo.strip_prefix("[unknown repo '") {
                    let apparent = rest.split('\'').next().unwrap_or("");
                    let from = repo.rsplit("requested from ").next().unwrap_or("@@]").trim_end_matches(']');
                    let origin = if from == "@@" { "main repository".to_string() } else { format!("repository '{from}'") };
                    let pkg = l.package_name();
                    return err(format!(
                        "Unable to load package for @@{repo}//{pkg}:{}: The repository '@@{repo}' could not be resolved: No repository visible as '@{apparent}' from {origin}",
                        l.name()
                    ));
                }
                let dir = s_err(rt.fetch_repo(&repo))?;
                let pkg = l.package_name();
                let base = if pkg.is_empty() { dir } else { dir.join(pkg) };
                Ok(base.join(l.name()))
            }
            other => err(format!("path() got argument of type '{}', want 'string', 'Label' or 'path'", other.type_name())),
        }
    }

    fn check_writable(&self, p: &Path) -> Result<(), StarlarkError> {
        if normalize(p).starts_with(&self.dir) {
            Ok(())
        } else {
            err(format!("Cannot write outside of the repository directory for path {}", p.display()))
        }
    }
}

fn arg<'a>(pos: &'a [Value], kw: &'a BTreeMap<String, Value>, i: usize, name: &str) -> Option<&'a Value> {
    pos.get(i).or_else(|| kw.get(name)).filter(|v| !matches!(v, Value::None))
}

fn str_arg(pos: &[Value], kw: &BTreeMap<String, Value>, i: usize, name: &str, default: &str) -> Result<String, StarlarkError> {
    match arg(pos, kw, i, name) {
        None => Ok(default.to_string()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(o) => err(format!("for parameter '{name}', got {}, want 'string'", o.type_name())),
    }
}

fn bool_arg(pos: &[Value], kw: &BTreeMap<String, Value>, i: usize, name: &str, default: bool) -> bool {
    arg(pos, kw, i, name).map(|v| v.is_truthy()).unwrap_or(default)
}

fn str_map(v: Option<&Value>) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if let Some(Value::Dict(d)) = v {
        for (k, v) in &d.lock().unwrap().entries {
            m.insert(k.to_string(), v.to_string());
        }
    }
    m
}

fn url_list(v: Option<&Value>) -> Result<Vec<String>, StarlarkError> {
    match v {
        Some(Value::String(s)) => {
            if s.is_empty() {
                err("urls not set")
            } else {
                Ok(vec![s.clone()])
            }
        }
        Some(Value::List(_)) | Some(Value::Tuple(_)) => {
            let l = v.unwrap().to_string_list();
            if l.is_empty() {
                err("urls not set")
            } else {
                Ok(l)
            }
        }
        _ => err("urls not set"),
    }
}

fn check_ctx_kwargs(fname: &str, kw: &BTreeMap<String, Value>, allowed: &[&str]) -> Result<(), StarlarkError> {
    for k in kw.keys() {
        if !allowed.contains(&k.as_str()) {
            if fname == "template" && k == "watch" {
                return err(format!("template() got unexpected keyword argument 'watch' (did you mean 'path'?)"));
            }
            return err(format!("{fname}() got unexpected keyword argument '{k}'"));
        }
    }
    Ok(())
}

fn path_attr(p: &Path, attr: &str) -> Result<Value, StarlarkError> {
    match attr {
        "basename" => Ok(str_value(p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())),
        "dirname" => Ok(match p.parent() {
            Some(d) if !d.as_os_str().is_empty() => path_value(d),
            _ => Value::None,
        }),
        "exists" => Ok(Value::Bool(p.exists())),
        "is_dir" => Ok(Value::Bool(p.is_dir())),
        "get_child" | "readdir" => Ok(Value::BuiltinFunction(format!("host:method:path.{attr}"))),
        "realpath" => match std::fs::canonicalize(p) {
            Ok(c) => Ok(path_value(&c)),
            Err(e) => {
                // Reports the first path component that does not exist.
                let mut cur = PathBuf::new();
                let mut bad = p.to_path_buf();
                for c in p.components() {
                    cur.push(c.as_os_str());
                    if cur.symlink_metadata().is_err() {
                        bad = cur.clone();
                        break;
                    }
                }
                err(format!("{NOFN}[unix_jni.cc:382] {} ({})", bad.display(), errno_text(&e)))
            }
        },
        other => err(format!("'path' value has no field or method '{other}'")),
    }
}

/// Marks an error that Bazel reports as plain `Error: msg` (no `in <function>` part).
const NOFN: &str = "\u{1}NOFN\u{1}";

/// The `Facts` object of `module_ctx.facts`: `Host("Facts", <json>)` with dict-like access.
pub fn facts_value(j: &crate::json::Json) -> Value {
    Value::Host("Facts".into(), j.to_compact())
}

pub(crate) fn facts_op(name: &str, pos: &[Value]) -> Result<Value, StarlarkError> {
    let Some(Value::Host(_, id)) = pos.first() else { return err("missing receiver") };
    let j = crate::json::Json::parse(id).unwrap_or(crate::json::Json::Null);
    let dict = crate::json::json_to_starlark(&j);
    if name.starts_with("debugprint:") {
        return Ok(str_value(format!("Facts({})", dict.repr())));
    }
    let Some(Value::String(k)) = pos.get(1) else {
        return if name.starts_with("contains:") { Ok(Value::Bool(false)) } else { err("Facts keys must be strings") };
    };
    let Value::Dict(d) = &dict else { return err("invalid facts") };
    let found = d.lock().unwrap().entries.iter().find(|(kk, _)| matches!(kk, Value::String(s) if s == k)).map(|(_, v)| v.clone());
    if name.starts_with("contains:") {
        return Ok(Value::Bool(found.is_some()));
    }
    found.ok_or_else(|| StarlarkError(format!("key {} not found in Facts", Value::String(k.clone()).repr())))
}

pub(crate) fn dispatch_object(
    rt: &Arc<Rt>,
    ctx: &mut EvalContext,
    name: &str,
    pos: Vec<Value>,
    kw: BTreeMap<String, Value>,
) -> Result<Value, StarlarkError> {
    let kw_names: Vec<String> = kw.keys().cloned().collect();
    let r = dispatch_object_inner(rt, ctx, name, pos, kw);
    // Argument-binding failures caused by a keyword argument (wrong type, unexpected, positional-only) are
    // reported at the enclosing assignment's `=`; everything else at the call.
    let binding_at = |ctx: &EvalContext, msg: &str| -> Option<zel_starlark::Location> {
        let pname = msg.split("parameter '").nth(1).and_then(|r| r.split('\'').next());
        let kw_type_error = msg.starts_with("in call to ") && msg.contains("got value of type") && pname.is_some_and(|p| kw_names.iter().any(|k| k == p));
        let other = msg.contains("got unexpected keyword argument") || msg.contains("got named argument for positional-only parameter");
        if kw_type_error || other { ctx.assign_stmt_location.clone() } else { None }
    };
    match r {
        Err(e) if e.0.starts_with(crate::ctxargs::STALE) && name.starts_with("method:") && !ctx.last_call_loc.file.is_empty() => {
            let fname = name.rsplit('.').next().unwrap_or("");
            let at = binding_at(ctx, &e.0[crate::ctxargs::STALE.len()..]).unwrap_or_else(|| if ctx.arg_eval_loc.file.is_empty() { ctx.last_call_loc.clone() } else { ctx.arg_eval_loc.clone() });
            let trace = ctx.thread.format_stack_trace_at(&at);
            Err(StarlarkError(format!("{trace}Error in {fname}: {}", &e.0[crate::ctxargs::STALE.len()..])))
        }
        Err(e) if e.0.starts_with(NOFN) && name.starts_with("attr:") => Err(StarlarkError(e.0[NOFN.len()..].to_string())),
        Err(e) if e.0.starts_with(NOFN) && !ctx.last_call_loc.file.is_empty() => {
            let msg = &e.0[NOFN.len()..];
            let at = if msg.ends_with("object is not callable") {
                if let Some(frame) = ctx.thread.frames.last() {
                    frame.location.clone()
                } else {
                    ctx.last_call_loc.clone()
                }
            } else if name.starts_with("method:") && !ctx.attr_loc.file.is_empty() {
                ctx.attr_loc.clone()
            } else {
                ctx.last_call_loc.clone()
            };
            let trace = ctx.thread.format_stack_trace_at(&at);
            Err(StarlarkError(format!("{trace}Error: {msg}")))
        }
        Err(e) if name.starts_with("method:") && !e.0.contains("Traceback (most recent call last)") && !e.0.starts_with("ERROR: ") && !ctx.last_call_loc.file.is_empty() => {
            let fname = name.rsplit('.').next().unwrap_or("");
            let at = binding_at(ctx, &e.0).unwrap_or_else(|| ctx.last_call_loc.clone());
            let trace = ctx.thread.format_stack_trace_at(&at);
            Err(StarlarkError(format!("{trace}Error in {fname}: {}", e.0)))
        }
        r => r,
    }
}

fn dispatch_object_inner(
    rt: &Arc<Rt>,
    _ctx: &mut EvalContext,
    name: &str,
    pos: Vec<Value>,
    kw: BTreeMap<String, Value>,
) -> Result<Value, StarlarkError> {
    let (is_method, rest) = match (name.strip_prefix("method:"), name.strip_prefix("attr:")) {
        (Some(r), _) => (true, r),
        (_, Some(r)) => (false, r),
        _ => return err("bad object dispatch"),
    };
    let (kind, member) = rest.split_once('.').unwrap_or((rest, ""));
    let Some(Value::Host(_, id)) = pos.first().cloned() else { return err("missing receiver") };
    let args = &pos[1..];
    if kind == "Facts" {
        if member == "get" {
            if !is_method {
                return Ok(Value::BuiltinFunction("host:method:Facts.get".into()));
            }
            let key = pos.get(1).or_else(|| kw.get("key")).cloned().unwrap_or(Value::None);
            let default = pos.get(2).or_else(|| kw.get("default")).cloned().unwrap_or(Value::None);
            let Value::String(_) = key else { return err("Facts.get: key must be a string") };
            return Ok(facts_op("index:Facts", &[pos[0].clone(), key]).unwrap_or(default));
        }
        return err(format!("'Facts' value has no field or method '{member}'"));
    }
    if kind == "path" {
        let p = PathBuf::from(&id);
        if !is_method {
            return path_attr(&p, member);
        }
        return match member {
            "get_child" => {
                if let Some(k) = kw.keys().next() {
                    return err(format!("get_child() got unexpected keyword argument '{k}'"));
                }
                let mut parts = vec![];
                for a in args {
                    match a {
                        Value::String(s) => parts.push(s.clone()),
                        other => return err(format!("at index {}, got {} for 'relative_paths', want string", parts.len(), other.type_name())),
                    }
                }
                Ok(path_value(&normalize(&p.join(parts.join("/")))))
            }
            "readdir" => {
                let bound = crate::ctxargs::bind_raw("readdir", "*watch:s", args, &kw)?;
                if !p.is_dir() {
                    return err(format!("can't readdir(), not a directory: {}", p.display()));
                }
                let watch = bound.get("watch").and_then(|v| v.as_str()).unwrap_or("auto").to_string();
                match watch.as_str() {
                    "yes" | "no" | "auto" => {}
                    other => return err(format!("bad value for 'watch' parameter; want 'yes', 'no', or 'auto', got {other}")),
                }
                let active: Vec<CtxData> = rt.state.lock().unwrap().ctxs.iter().flatten().cloned().collect();
                for c in &active {
                    maybe_watch_dirents(rt, c, &p, &watch)?;
                }
                let mut names: Vec<String> = std::fs::read_dir(&p)
                    .map_err(|e| StarlarkError(format!("java.io.IOException: {} ({})", p.display(), errno_text(&e))))?
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                // Bazel's Unix native directory stream exposes these entries in reverse lexical
                // order for its in-memory repository filesystem.
                names.sort_by(|a, b| b.cmp(a));
                Ok(list_value(names.into_iter().map(|n| path_value(&p.join(n))).collect()))
            }
            "exists" | "is_dir" | "realpath" | "basename" | "dirname" => path_attr(&p, member),
            other => err(format!("'path' value has no field or method '{other}'")),
        };
    }
    if kind == "pending_download" {
        if member == "wait" {
            if !is_method {
                return Ok(Value::BuiltinFunction("host:method:pending_download.wait".into()));
            }
            return wait_pending_download(rt, &id);
        }
        return err(format!("'pending_download' object has no member '{member}'"));
    }
    if kind == "exec_result" {
        let idx: usize = id.parse().unwrap_or(usize::MAX);
        let Some((code, stdout, stderr)) = rt.get_exec_result(idx) else {
            return err("invalid exec_result");
        };
        return match member {
            "return_code" => Ok(int_value(code as i64)),
            "stdout" => Ok(str_value(String::from_utf8_lossy(&stdout).into_owned())),
            "stderr" => Ok(str_value(String::from_utf8_lossy(&stderr).into_owned())),
            other => err(format!("'exec_result' object has no attribute '{other}'")),
        };
    }
    if kind != "repository_ctx" && kind != "module_ctx" {
        return err(format!("'{kind}' object has no members"));
    }
    let idx: usize = id.parse().unwrap_or(usize::MAX);
    let data = rt.state.lock().unwrap().ctxs.get(idx).and_then(|c| c.clone());
    let Some(data) = data else { return err("the context object is no longer valid") };
    if !is_method {
        return match member {
            "name" if kind != "module_ctx" => Ok(str_value(&data.name)),
            "attr" if kind != "module_ctx" => Ok(data.attr.clone()),
            "os" => Ok(os_value(rt)),
            "workspace_root" if kind != "module_ctx" => Ok(path_value(&rt.opts.workspace_root)),
            "original_name" if kind != "module_ctx" => Ok(str_value(&data.name)),
            "modules" if kind == "module_ctx" => Ok(data.modules.clone()),
            "root_module_has_non_dev_dependency" if kind == "module_ctx" => Ok(Value::Bool(data.root_has_non_dev)),
            "facts" if kind == "module_ctx" => Ok(data.facts.clone()),
            "is_isolated" if kind == "module_ctx" => Ok(Value::Bool(data.is_isolated)),
            "path" | "file" | "read" | "execute" | "download" | "download_and_extract" | "extract" | "which" | "getenv" | "report_progress" | "load_wasm" | "execute_wasm" => Ok(Value::BuiltinFunction(format!("host:method:{kind}.{member}"))),
            "watch" => Ok(Value::BuiltinFunction(format!("host:method:{kind}.{member}"))),
            "delete" | "rename" | "symlink" | "template" | "patch" | "watch_tree" | "repo_metadata" if kind == "repository_ctx" => Ok(Value::BuiltinFunction(format!("host:method:{kind}.{member}"))),
            "is_dev_dependency" | "extension_metadata" | "tag_sort_key" if kind == "module_ctx" => Ok(Value::BuiltinFunction(format!("host:method:{kind}.{member}"))),
            other => err(format!("{NOFN}'{kind}' value has no field or method '{other}'")),
        };
    }
    let bound = crate::ctxargs::bind(kind, member, args, &kw)?;
    let (args, kw): (&[Value], BTreeMap<String, Value>) = match &bound {
        Some(b) => (&[], b.clone()),
        None => (args, kw),
    };
    // `stripPrefix` is the legacy spelling of `strip_prefix` (used by rules_python).
    let kw_norm: BTreeMap<String, Value> = {
        let mut m = kw.clone();
        if let Some(v) = m.remove("stripPrefix") {
            m.entry("strip_prefix".to_string()).or_insert(v);
        }
        m
    };
    let kwr = &kw_norm;
    if kind == "module_ctx" && matches!(member, "delete" | "template" | "symlink" | "rename" | "patch" | "watch_tree" | "repo_metadata") {
        return err(format!("{NOFN}'{kind}' value has no field or method '{member}'"));
    }
    match member {
        "name" | "original_name" => err(format!("{NOFN}'string' object is not callable")),
        "workspace_root" => err(format!("{NOFN}'path' object is not callable")),
        "attr" => err(format!("{NOFN}'struct' object is not callable")),
        "os" => err(format!("{NOFN}'repository_os' object is not callable")),
        "path" => {
            check_ctx_kwargs(member, &kw, &["path"])?;
            let v = args.first().or_else(|| kwr.get("path")).ok_or_else(|| StarlarkError("path() missing argument".into()))?;
            let p = data.resolve(rt, v)?;
            Ok(path_value(&p))
        }
        "getenv" => {
            let n = str_arg(args, kwr, 0, "name", "")?;
            let v = rt.opts.client_env.get(&n).cloned();
            data.env_seen.lock().unwrap().insert(n.clone(), v.clone());
            data.record(format!("ENV:{}", escape_input(&n)), v.clone());
            Ok(match (v, kwr.get("default")) {
                (Some(v), _) => str_value(v),
                (None, Some(d)) => d.clone(),
                (None, None) => Value::None,
            })
        }
        "report_progress" => {
            let _m = str_arg(args, kwr, 0, "status", "")?;
            Ok(Value::None)
        }
        "watch" => {
            let v = kwr.get("path").unwrap();
            let p = data.resolve(rt, v)?;
            maybe_watch(rt, &data, &p, "yes")?;
            Ok(Value::None)
        }
        "watch_tree" => {
            let v = kwr.get("path").unwrap();
            let p = data.resolve(rt, v)?;
            if !p.is_dir() {
                return err(format!("can't call watch_tree() on non-directory {}", p.display()));
            }
            maybe_watch(rt, &data, &p, "yes")?;
            Ok(Value::None)
        }
        "which" => {
            let prog = str_arg(args, kwr, 0, "program", "")?;
            if prog.is_empty() {
                return err("Program argument of which() may not be empty");
            }
            if prog.contains('/') || prog.contains('\\') {
                return err(format!("Program argument of which() may not contain a / or a \\ ('{prog}' given)"));
            }
            let path_env = rt.opts.client_env.get("PATH").cloned().unwrap_or_default();
            for d in path_env.split(':') {
                let c = Path::new(d).join(&prog);
                if c.is_file() && is_executable(&c) {
                    return Ok(path_value(&c));
                }
            }
            Ok(Value::None)
        }
        "read" => {
            let v = kwr.get("path").unwrap();
            let p = data.resolve(rt, v)?;
            let watch = str_arg(args, kwr, 1, "watch", "auto")?;
            maybe_watch(rt, &data, &p, &watch)?;
            if p.is_dir() {
                return err(format!("attempting to read() a directory: {}", p.display()));
            }
            let b = std::fs::read(&p).map_err(|e| StarlarkError(format!("java.io.FileNotFoundException: {} ({})", p.display(), errno_text(&e))))?;
            Ok(str_value(latin1_bytes_to_string(&b)))
        }
        "file" => {
            let v = kwr.get("path").unwrap();
            let p = data.resolve(rt, v)?;
            data.check_writable(&p)?;
            let content = str_arg(args, kwr, 1, "content", "")?;
            let exec = bool_arg(args, kwr, 2, "executable", true);
            write_file(&p, content.as_bytes(), exec)?;
            Ok(Value::None)
        }
        "template" => {
            let v = kwr.get("path").unwrap();
            let p = data.resolve(rt, v)?;
            let tv = kwr.get("template").unwrap();
            let tp = data.resolve(rt, tv)?;
            let subs = match kwr.get("substitutions") {
                Some(d) => dict_str_str(d, "substitutions")?,
                None => vec![],
            };
            if tp.is_dir() {
                return err(format!("attempting to use a directory as template: {}", tp.display()));
            }
            let watch = str_arg(args, kwr, 4, "watch_template", "auto")?;
            maybe_watch(rt, &data, &tp, &watch)?;
            data.check_writable(&p)?;
            let mut content = std::fs::read(&tp).map_err(|e| StarlarkError(format!("java.io.FileNotFoundException: {} ({})", tp.display(), errno_text(&e))))?;
            for (k, val) in subs {
                content = replace_bytes(&content, k.as_bytes(), val.as_bytes());
            }
            let exec = bool_arg(args, kwr, 3, "executable", true);
            write_file(&p, &content, exec)?;
            Ok(Value::None)
        }
        "delete" => {
            let v = kwr.get("path").unwrap();
            let p = data.resolve(rt, v)?;
            if !p.starts_with(&data.dir) {
                return Ok(Value::Bool(false));
            }
            let existed = p.symlink_metadata().is_ok();
            let r = if p.is_dir() && !p.is_symlink() { std::fs::remove_dir_all(&p).is_ok() } else { std::fs::remove_file(&p).is_ok() };
            Ok(Value::Bool(existed && r))
        }
        "rename" => {
            let a = data.resolve(rt, kwr.get("src").unwrap())?;
            let b = data.resolve(rt, kwr.get("dst").unwrap())?;
            data.check_writable(&a)?;
            data.check_writable(&b)?;
            if b.symlink_metadata().is_ok() {
                return err(format!("java.io.IOException: Could not rename {} to {}: already exists", a.display(), b.display()));
            }
            if let Some(par) = b.parent() {
                make_dirs(par)?;
            }
            std::fs::rename(&a, &b).map_err(|e| {
                StarlarkError(format!(
                    "java.io.IOException: Could not rename {} to {}: [unix_jni.cc:638] {} -> {} ({})",
                    a.display(),
                    b.display(),
                    a.display(),
                    b.display(),
                    errno_text(&e)
                ))
            })?;
            Ok(Value::None)
        }
        "symlink" => {
            let from = data.resolve(rt, kwr.get("target").unwrap())?;
            let to = data.resolve(rt, kwr.get("link_name").unwrap())?;
            data.check_writable(&to)?;
            if let Some(par) = to.parent() {
                make_dirs(par)?;
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&from, &to).map_err(|e| {
                let line = if e.kind() == std::io::ErrorKind::AlreadyExists || e.raw_os_error() == Some(17) { 297 } else { 297 };
                StarlarkError(format!(
                    "java.io.IOException: Could not create symlink from {} to {}: [unix_jni.cc:{line}] {} ({})",
                    from.display(),
                    to.display(),
                    to.display(),
                    errno_text(&e)
                ))
            })?;
            #[cfg(not(unix))]
            return err("symlink is not supported on this platform");
            Ok(Value::None)
        }
        "execute" => {
            check_ctx_kwargs(member, &kw, &["arguments", "timeout", "environment", "quiet", "working_directory"])?;
            let argv: Vec<String> = {
                let v = kwr.get("arguments").unwrap();
                let items = zel_starlark::eval::builtins::value_to_sequence(v)?;
                let mut out = vec![];
                for (i, item) in items.into_iter().enumerate() {
                    out.push(match item {
                        Value::String(s) => s,
                        Value::Host(k, p) if k == "path" => p,
                        Value::Label(l) => data.resolve(rt, &Value::Label(l))?.display().to_string(),
                        _ => return err(format!("Argument {i} of execute is neither a path, label, nor string.")),
                    });
                }
                out
            };
            let timeout = match arg(args, kwr, 1, "timeout") {
                Some(Value::Int(i)) => i.to_i64().unwrap_or(600),
                _ => 600,
            };
            let envm = arg(args, kwr, 2, "environment");
            let quiet = bool_arg(args, kwr, 3, "quiet", true);
            let wd = match arg(args, kwr, 4, "working_directory") {
                Some(Value::String(s)) if !s.is_empty() => data.resolve(rt, &Value::String(s.clone()))?,
                _ => data.dir.clone(),
            };
            // `createDirectory(workingDirectory)`: both the context's and the requested directory.
            if data.kind == CtxKind::Module {
                let _ = std::fs::create_dir_all(&data.dir);
            }
            let _ = std::fs::create_dir_all(&wd);
            let mut env = rt.opts.client_env.clone();
            if let Some(Value::Dict(d)) = envm {
                for (k, v) in &d.lock().unwrap().entries {
                    let Value::String(ks) = k else {
                        return err(format!(
                            "got dict<{}, {}> for 'environment', want dict<string, unknown>",
                            k.type_name(),
                            v.type_name()
                        ));
                    };
                    match v {
                        Value::None => {
                            env.remove(ks);
                        }
                        Value::String(vs) => {
                            env.insert(ks.clone(), vs.clone());
                        }
                        other => return err(format!("environment values must be strings or None, got {}", other.repr())),
                    }
                }
            }
            let (code, out, errs) = if argv.is_empty() {
                (0, vec![], vec![])
            } else {
                run_process(&argv, &wd, &env, timeout)?
            };
            if !quiet {
                if !out.is_empty() {
                    rt.info(String::from_utf8_lossy(&out).trim_end_matches('\n').to_string());
                }
                if !errs.is_empty() {
                    rt.info(String::from_utf8_lossy(&errs).trim_end_matches('\n').to_string());
                }
            }
            let idx = rt.register_exec_result(code, out, errs);
            Ok(Value::Host("exec_result".into(), idx.to_string()))
        }
        "download" | "download_and_extract" => {
            let extract_after = member == "download_and_extract";
            let ident = format!("repository @@{}", data.canonical);
            let mut strip_prefix = String::new();
            let mut strip_components = 0i64;
            if extract_after {
                strip_prefix = renamed_strip_prefix(member, &str_arg(&[], kwr, 99, "strip_prefix", "")?, &str_arg(&[], kwr, 99, "stripPrefix", "")?)?;
                strip_components = int_arg(kwr, "strip_components");
                validate_stripping(member, &strip_prefix, strip_components)?;
            }
            let auth = parse_auth(kwr.get("auth"))?;
            let headers = parse_headers(kwr.get("headers"))?;
            let allow_fail = bool_arg(&[], kwr, 99, "allow_fail", false);
            let sha256 = str_arg(&[], kwr, 99, "sha256", "")?;
            let integrity = str_arg(&[], kwr, 99, "integrity", "")?;
            let urls = get_urls(kwr.get("url").unwrap(), !allow_fail, !sha256.is_empty() || !integrity.is_empty())?;
            if !sha256.is_empty() && !integrity.is_empty() {
                return err("Expected either 'sha256' or 'integrity', but not both");
            }
            let (sha256, integrity, deferred) = match crate::download::request_checksum(&sha256, &integrity) {
                Ok(_) => (sha256, integrity, None),
                Err(e) => (String::new(), String::new(), Some(format!("Checksum error in {ident}: {e}"))),
            };
            let rename_files: Vec<(String, String)> = match kwr.get("rename_files") {
                Some(d) if extract_after => dict_str_str(d, "rename_files")?,
                _ => vec![],
            };
            let canonical_id = str_arg(&[], kwr, 99, "canonical_id", "")?;
            let output = match kwr.get("output") {
                Some(v) => data.resolve(rt, v)?,
                None => data.dir.clone(),
            };
            data.check_writable(&output)?;
            let executable = bool_arg(&[], kwr, 99, "executable", false);
            let block = bool_arg(&[], kwr, 99, "block", true);
            let type_suffix = kwr.get("type").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()).map(String::from);
            let mut pre_err: Option<String> = None;
            let (target, tmp_dir) = if extract_after {
                make_dirs(&output)?;
                let tmp = make_temp_dir(&output)?;
                let mut fname = urls.first().and_then(|u| u.rsplit('/').next()).unwrap_or("archive").split('?').next().unwrap_or("archive").to_string();
                if fname.is_empty() {
                    fname = "archive".into();
                }
                if let Some(t) = &type_suffix {
                    fname = format!("{fname}.{t}");
                }
                (tmp.join(fname), Some(tmp))
            } else {
                if let Some(par) = output.parent() {
                    if let Err(e) = make_dirs(par) {
                        pre_err = Some(e.0);
                    }
                }
                (output.clone(), None)
            };
            let req = DownloadRequest {
                urls: urls.clone(),
                output: target.clone(),
                sha256,
                integrity,
                headers,
                auth,
                executable: if extract_after { false } else { executable },
                canonical_id,
                context: urls.first().cloned().unwrap_or_default(),
            };
            let extract_opts = if extract_after {
                Some(ExtractOptions {
                    strip_prefix,
                    strip_components: strip_components.max(0) as usize,
                    add_prefix: String::new(),
                    rename_files: rename_files.into_iter().collect(),
                    archive_type: None,
                    allow_absolute: true,
                })
            } else {
                None
            };
            let downloader = rt.downloader();
            let pd = PendingDownload {
                receiver: None,
                output: output.clone(),
                target: target.clone(),
                allow_fail,
                extract_after,
                tmp_dir,
                extract_opts,
                deferred,
                canonical: data.canonical.clone(),
            };
            if !block {
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let r = match pre_err {
                        Some(e) => (Err(e), vec![]),
                        None => {
                            let _ = crate::download::take_warnings();
                            let r = downloader.download(&req);
                            (r, crate::download::take_warnings())
                        }
                    };
                    let _ = tx.send(r);
                });
                let token = register_pending_download(PendingDownload { receiver: Some(rx), ..pd });
                return Ok(Value::Host("pending_download".into(), format!("{}|{}", token, target.display())));
            }
            let _ = crate::download::take_warnings();
            let r = match pre_err {
                Some(e) => Err(e),
                None => downloader.download(&req),
            };
            let warnings = crate::download::take_warnings();
            finish_download(rt, pd, r, warnings)
        }
        "extract" => {
            let strip_prefix = renamed_strip_prefix("extract", &str_arg(&[], kwr, 99, "strip_prefix", "")?, &str_arg(&[], kwr, 99, "stripPrefix", "")?)?;
            let strip_components = int_arg(kwr, "strip_components");
            validate_stripping("extract", &strip_prefix, strip_components)?;
            let a = data.resolve(rt, kwr.get("archive").unwrap())?;
            if a.symlink_metadata().is_err() && !a.exists() {
                return err(format!("Archive path '{}' does not exist.", a.display()));
            }
            if a.is_dir() {
                return err(format!("attempting to extract a directory: {}", a.display()));
            }
            let watch = str_arg(&[], kwr, 99, "watch_archive", "auto")?;
            maybe_watch(rt, &data, &a, &watch)?;
            let output = match kwr.get("output") {
                Some(v) => data.resolve(rt, v)?,
                None => data.dir.clone(),
            };
            data.check_writable(&output)?;
            let rename_files = match kwr.get("rename_files") {
                Some(d) => dict_str_str(d, "rename_files")?,
                None => vec![],
            };
            let archive_type = kwr.get("type").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()).map(String::from);
            let opts = ExtractOptions {
                strip_prefix,
                strip_components: strip_components.max(0) as usize,
                add_prefix: String::new(),
                rename_files: rename_files.into_iter().collect(),
                archive_type,
                allow_absolute: true,
            };
            let _ = std::fs::create_dir_all(&output);
            extract(&a, &output, &opts).map_err(|e| StarlarkError(java_extract_error(&a, &output, e)))?;
            Ok(Value::None)
        }
        "patch" => {
            let pf = data.resolve(rt, args.first().or_else(|| kwr.get("patch_file")).ok_or_else(|| StarlarkError("patch() missing patch_file".into()))?)?;
            let strip = match arg(args, kwr, 1, "strip") {
                Some(Value::Int(i)) => i.to_i64().unwrap_or(0),
                _ => 0,
            };
            let base = match arg(args, kwr, usize::MAX, "directory") {
                Some(Value::String(d)) if !d.is_empty() => data.resolve(rt, &Value::String(d.clone()))?,
                _ => data.dir.clone(),
            };
            s_err(apply_patch(&base, &pf, strip, &rt.opts.client_env))?;
            Ok(Value::None)
        }
        "is_dev_dependency" => {
            let tag = args.first().or_else(|| kwr.get("tag"));
            match tag {
                Some(Value::Struct(s)) => Ok(Value::Bool(s.fields.get("_dev_dependency").map(|v| v.is_truthy()).unwrap_or(false))),
                _ => err("is_dev_dependency() expects a tag"),
            }
        }
        "copy" => err(format!("{NOFN}'{kind}' value has no field or method 'copy'")),
        "_unused_copy" => {
            let src_path = data.resolve(rt, kwr.get("src").unwrap())?;
            let dst_path = data.resolve(rt, kwr.get("dst").unwrap())?;
            let exec = bool_arg(&[], kwr, 99, "executable", true);
            let watch = str_arg(&[], kwr, 99, "watch_src", "auto")?;
            maybe_watch(rt, &data, &src_path, &watch)?;
            data.check_writable(&dst_path)?;
            let wrap_io = |e: String| StarlarkError(format!("java.io.IOException: Could not copy {} to {}: {e}", src_path.display(), dst_path.display()));
            let wrap_eval = |e: String| StarlarkError(format!("Could not copy {} to {}: {e}", src_path.display(), dst_path.display()));
            if let Some(par) = dst_path.parent() {
                make_dirs(par).map_err(|e| wrap_io(e.0.trim_start_matches("java.io.IOException: ").to_string()))?;
            }
            fn copy_file(src: &Path, dst: &Path, exec: bool) -> Result<(), (bool, String)> {
                if dst.symlink_metadata().map(|m| m.is_dir()).unwrap_or(false) {
                    return Err((false, format!("{} is a directory", dst.display())));
                }
                let _ = std::fs::remove_file(dst);
                std::fs::copy(src, dst).map_err(|e| (true, format!("{} ({})", src.display(), errno_text(&e))))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(dst, std::fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 }));
                }
                let _ = exec;
                Ok(())
            }
            fn create_copy_dir(dir: &Path) -> Result<(), (bool, String)> {
                match dir.symlink_metadata() {
                    Err(_) => std::fs::create_dir(dir).map_err(|e| (true, format!("{} ({})", dir.display(), errno_text(&e)))),
                    Ok(m) if m.file_type().is_symlink() => Err((false, format!("{} is a symlink", dir.display()))),
                    Ok(m) if !m.is_dir() => Err((false, format!("{} is not a directory", dir.display()))),
                    Ok(_) => Ok(()),
                }
            }
            fn copy_tree(src: &Path, dst: &Path, dst_root: &Path, exec: bool, anc: &mut Vec<PathBuf>) -> Result<(), (bool, String)> {
                if src.starts_with(dst_root) || dst_root.starts_with(src) {
                    return Err((false, format!("{} and {} overlap", src.display(), dst_root.display())));
                }
                if anc.contains(&src.to_path_buf()) {
                    return Err((false, format!("{} is part of a symlink cycle", src.display())));
                }
                anc.push(src.to_path_buf());
                let mut names: Vec<String> = std::fs::read_dir(src)
                    .map_err(|e| (true, format!("{} ({})", src.display(), errno_text(&e))))?
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                for n in names {
                    let mut sc = src.join(&n);
                    let dc = dst.join(&n);
                    let md = sc.symlink_metadata().map_err(|e| (true, format!("{} ({})", sc.display(), errno_text(&e))))?;
                    let is_dir;
                    if md.file_type().is_symlink() {
                        let real = std::fs::canonicalize(&sc).map_err(|_| (false, format!("{} is a dangling symlink", sc.display())))?;
                        is_dir = real.is_dir();
                        sc = real;
                    } else {
                        is_dir = md.is_dir();
                    }
                    if is_dir {
                        create_copy_dir(&dc)?;
                        copy_tree(&sc, &dc, dst_root, exec, anc)?;
                    } else {
                        copy_file(&sc, &dc, exec)?;
                    }
                }
                anc.pop();
                Ok(())
            }
            let r = if src_path.is_dir() {
                create_copy_dir(&dst_path).and_then(|_| {
                    let real_src = std::fs::canonicalize(&src_path).unwrap_or(src_path.clone());
                    let real_dst = std::fs::canonicalize(&dst_path).unwrap_or(dst_path.clone());
                    copy_tree(&real_src, &dst_path, &real_dst, exec, &mut vec![])
                })
            } else {
                copy_file(&src_path, &dst_path, exec)
            };
            r.map_err(|(io, m)| if io { wrap_io(m) } else { wrap_eval(m) })?;
            Ok(Value::None)
        }
        "tag_sort_key" if kind == "module_ctx" => {
            let tag = arg(args, kwr, 0, "tag").ok_or_else(|| StarlarkError("tag_sort_key() missing 'tag'".into()))?;
            match tag {
                Value::Struct(s) => {
                    if let Some(sk) = s.fields.get("_sort_key") {
                        Ok(sk.clone())
                    } else {
                        err("tag_sort_key() expects a tag object")
                    }
                }
                _ => err("tag_sort_key() expects a tag object"),
            }
        }
        "load_wasm" => {
            if !rt.opts.experimental_repository_ctx_execute_wasm {
                return err("'repository_ctx' value has no field or method 'load_wasm' (use --experimental_repository_ctx_execute_wasm to enable it)");
            }
            let path_val = arg(args, kwr, 0, "path").ok_or_else(|| StarlarkError("load_wasm() missing 'path'".into()))?;
            let alloc_fn = str_arg(args, kwr, 2, "allocate_fn", "allocate")?;
            let p = data.resolve(rt, path_val)?;
            if !p.exists() {
                return err(format!("load_wasm(): file not found: {}", p.display()));
            }
            let content = std::fs::read(&p).map_err(|e| StarlarkError(format!("load_wasm(): error reading {}: {e}", p.display())))?;
            let mut parser = crate::wasm::Parser::new(&content);
            let module = parser.parse_module().map_err(StarlarkError)?;
            if !module.exports.iter().any(|e| e.name == alloc_fn && e.kind == crate::wasm::ExportKind::Func) {
                return err(format!("WebAssembly module doesn't contain an export named \"{alloc_fn}\""));
            }
            Ok(Value::Host("wasm_module".into(), format!("{}|{}", p.to_string_lossy(), alloc_fn)))
        }
        "execute_wasm" => {
            if !rt.opts.experimental_repository_ctx_execute_wasm {
                return err("'repository_ctx' value has no field or method 'execute_wasm' (use --experimental_repository_ctx_execute_wasm to enable it)");
            }
            let module_arg = arg(args, kwr, 0, "module").ok_or_else(|| StarlarkError("execute_wasm() missing 'module'".into()))?;
            let func_name = str_arg(args, kwr, 1, "function", "")?;
            let input_str = str_arg(args, kwr, 2, "input", "")?;
            let mem_limit = match kwr.get("memory_limit").or_else(|| args.get(4)) {
                Some(Value::Int(i)) => i.to_i64().unwrap_or(64 * 1024 * 1024) as usize,
                _ => 64 * 1024 * 1024,
            };

            let (path_str, alloc_fn) = match module_arg {
                Value::Host(k, s) if k == "wasm_module" => {
                    let parts: Vec<&str> = s.splitn(2, '|').collect();
                    (parts[0].to_string(), parts.get(1).copied().unwrap_or("allocate").to_string())
                }
                other => {
                    let p = data.resolve(rt, other)?;
                    (p.to_string_lossy().to_string(), "allocate".to_string())
                }
            };

            let p = std::path::Path::new(&path_str);
            if !p.exists() {
                return err(format!("execute_wasm(): file not found: {path_str}"));
            }
            let content = std::fs::read(p).map_err(|e| StarlarkError(format!("execute_wasm(): error reading {path_str}: {e}")))?;

            let (return_code, output_str, error_msg) = crate::wasm::run_wasm_execution(
                &content,
                &func_name,
                &input_str,
                &alloc_fn,
                mem_limit,
            );

            let mut fields = BTreeMap::new();
            fields.insert("return_code".into(), int_value(return_code));
            fields.insert("output".into(), Value::String(output_str));
            fields.insert("error_message".into(), Value::String(error_msg));
            Ok(Value::Struct(Arc::new(zel_starlark::StructValue::new(fields))))
        }
        "repo_metadata" if kind == "repository_ctx" => {
            let reproducible = bool_arg(args, kwr, 0, "reproducible", false);
            let attrs = arg(args, kwr, 1, "attrs_for_reproducibility").unwrap_or(&Value::None);
            let has_attrs = match attrs {
                Value::Dict(d) => !d.lock().unwrap().entries.is_empty(),
                _ => false,
            };
            if reproducible && has_attrs {
                return err("attrs_for_reproducibility can only be specified if reproducible is False");
            }
            if reproducible {
                *data.reproducible.lock().unwrap() = true;
            }
            let mut fields = BTreeMap::new();
            fields.insert("reproducible".into(), Value::Bool(reproducible));
            fields.insert("attrs_for_reproducibility".into(), attrs.clone());
            Ok(Value::Struct(Arc::new(zel_starlark::StructValue::new(fields))))
        }
        "extension_metadata" if kind == "module_ctx" => rt.host_dispatch(_ctx, "extension_metadata", args.to_vec(), kw.clone()),
        other => err(format!("{NOFN}'{kind}' value has no field or method '{other}'")),
    }
}


/// `strerror`-style text of an I/O error (without Rust's " (os error N)" suffix).
fn errno_text(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// Decodes raw file bytes as Bazel does (ISO-8859-1 chars holding the raw bytes), then re-exposes the
/// original bytes as the string's UTF-8 content when they are valid UTF-8.
fn latin1_bytes_to_string(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        true
    }
}

fn replace_bytes(hay: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    if from.is_empty() {
        return hay.to_vec();
    }
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while i < hay.len() {
        if hay[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

/// Casts a dict argument to `dict<string, string>` entries (`Dict.cast` semantics).
fn dict_str_str(v: &Value, what: &str) -> Result<Vec<(String, String)>, StarlarkError> {
    let Value::Dict(d) = v else { return err(format!("got {} for '{what}', want dict", v.type_name())) };
    let mut out = vec![];
    for (k, val) in &d.lock().unwrap().entries {
        let Value::String(ks) = k else { return err(format!("got dict<{}, ?> for '{what}', want dict<string, string>", k.type_name())) };
        let Value::String(vs) = val else { return err(format!("got dict<string, {}> for '{what}', want dict<string, string>", val.type_name())) };
        out.push((ks.clone(), vs.clone()));
    }
    Ok(out)
}

/// `mkdir -p` with Bazel's IOException wording.
fn make_dirs(dir: &Path) -> Result<(), StarlarkError> {
    if dir.is_dir() {
        return Ok(());
    }
    // An existing non-directory component yields "<path> (File exists)".
    let mut cur = PathBuf::new();
    for c in dir.components() {
        cur.push(c.as_os_str());
        if let Ok(m) = cur.symlink_metadata() {
            if !m.is_dir() && !(m.file_type().is_symlink() && cur.is_dir()) {
                return err(format!("java.io.IOException: {} (File exists)", cur.display()));
            }
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| StarlarkError(format!("java.io.IOException: {} ({})", dir.display(), errno_text(&e))))
}

/// `RepoRecordedInput.escape`.
pub fn escape_input(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace(' ', "\\s")
}

/// A recorded input in marker/lockfile syntax: `<input> <escaped value>` (`\0` = null).
pub fn format_recorded(input: &str, value: Option<&str>) -> String {
    format!("{input} {}", value.map(escape_input).unwrap_or_else(|| "\\0".to_string()))
}

impl CtxData {
    /// `recordInputWithValue`: the first value recorded for an input wins.
    pub fn record(&self, input: String, value: Option<String>) {
        let mut r = self.rec.lock().unwrap();
        if !r.iter().any(|(i, _)| *i == input) {
            r.push((input, value));
        }
    }
}

/// `toRepoCacheFriendlyPath`: `Ok(None)` when the path is not to be watched.
fn repo_cache_friendly_path(rt: &Rt, data: &CtxData, p: &Path, mode: &str) -> Result<Option<String>, StarlarkError> {
    if mode == "no" {
        return Ok(None);
    }
    if p.starts_with(&data.dir) {
        if mode == "auto" {
            return Ok(None);
        }
        return err("attempted to watch path under working directory");
    }
    let real = |x: &Path| std::fs::canonicalize(x).unwrap_or_else(|_| x.to_path_buf());
    let ws = &rt.opts.workspace_root;
    for w in [ws.clone(), real(ws)] {
        if let Ok(rel) = p.strip_prefix(&w) {
            return Ok(Some(format!("@@//{}", rel.display())));
        }
    }
    let ext = rt.external_dir();
    for e in [ext.clone(), real(&ext)] {
        if let Ok(rel) = p.strip_prefix(&e) {
            let mut comps = rel.components();
            if let Some(first) = comps.next() {
                let rest: PathBuf = comps.collect();
                return Ok(Some(format!("@@{}//{}", first.as_os_str().to_string_lossy(), rest.display())));
            }
        }
    }
    if data.kind == CtxKind::Module {
        if mode == "auto" {
            return Ok(None);
        }
        return err("attempted to watch path outside workspace, but it's prohibited in the current context");
    }
    Ok(Some(p.display().to_string()))
}

/// `maybeWatch`: validates `watch`, rejects explicit watches under the working directory and
/// records a `FILE:` input (`DIR`, `ENOENT` or the content digest).
fn maybe_watch(rt: &Rt, data: &CtxData, p: &Path, mode: &str) -> Result<(), StarlarkError> {
    match mode {
        "yes" | "no" | "auto" => {}
        other => return err(format!("bad value for 'watch' parameter; want 'yes', 'no', or 'auto', got {other}")),
    }
    let Some(rel) = repo_cache_friendly_path(rt, data, p, mode)? else { return Ok(()) };
    let value = if p.is_dir() {
        "DIR".to_string()
    } else if !p.exists() {
        "ENOENT".to_string()
    } else {
        match std::fs::read(p) {
            Ok(b) => crate::hash::sha256_hex(&b),
            Err(e) => return err(format!("java.io.IOException: {} ({})", p.display(), errno_text(&e))),
        }
    };
    data.record(format!("FILE:{}", escape_input(&rel)), Some(value));
    Ok(())
}

/// `maybeWatchDirents`: records `FILE:` (type) and `DIRENTS:` (digest of the sorted entry names).
fn maybe_watch_dirents(rt: &Rt, data: &CtxData, p: &Path, mode: &str) -> Result<(), StarlarkError> {
    let Some(rel) = repo_cache_friendly_path(rt, data, p, mode)? else { return Ok(()) };
    maybe_watch(rt, data, p, mode)?;
    let mut names: Vec<String> = std::fs::read_dir(p)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    data.record(format!("DIRENTS:{}", escape_input(&rel)), Some(crate::hash::Fingerprint::new().add_strings(&names).hex_digest()));
    Ok(())
}

fn write_file(p: &Path, content: &[u8], exec: bool) -> Result<(), StarlarkError> {
    if let Some(par) = p.parent() {
        make_dirs(par)?;
    }
    // `Path.delete()`: removes an existing file or empty directory; a non-empty directory fails.
    if let Ok(m) = p.symlink_metadata() {
        if m.is_dir() {
            if let Err(e) = std::fs::remove_dir(p) {
                return err(format!("java.io.IOException: [unix_jni.cc:660] {} ({})", p.display(), errno_text(&e)));
            }
        } else {
            let _ = std::fs::remove_file(p);
        }
    }
    std::fs::write(p, content).map_err(|e| StarlarkError(format!("java.io.IOException: {} ({})", p.display(), errno_text(&e))))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 }));
    }
    let _ = exec;
    Ok(())
}

pub fn run_process(argv: &[String], wd: &Path, env: &BTreeMap<String, String>, timeout_secs: i64) -> Result<(i32, Vec<u8>, Vec<u8>), StarlarkError> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    if argv.is_empty() {
        return Ok((0, vec![], vec![]));
    }
    let _ = std::fs::create_dir_all(wd);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(wd).env_clear().envs(env).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Like Bazel's `process-wrapper`, run the command in its own session so it has no controlling terminal
    // (tools such as `patch` would otherwise prompt on /dev/tty).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        extern "C" {
            fn setsid() -> i32;
        }
        // SAFETY: `setsid` is async-signal-safe and takes no arguments.
        unsafe {
            cmd.pre_exec(|| {
                setsid();
                Ok(())
            });
        }
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ok((1, vec![], format!("src/main/tools/process-wrapper-legacy.cc:80: \"execvp({}, ...)\": {}\n", argv[0], errno_text(&e)).into_bytes()));
        }
    };
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t1 = std::thread::spawn(move || {
        let mut b = vec![];
        let _ = so.read_to_end(&mut b);
        b
    });
    let t2 = std::thread::spawn(move || {
        let mut b = vec![];
        let _ = se.read_to_end(&mut b);
        b
    });
    let start = std::time::Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                break st.code().unwrap_or_else(|| {
                    use std::os::unix::process::ExitStatusExt;
                    // Java reports a signal death as 128 + signal.
                    st.signal().map(|sg| 128 + sg).unwrap_or(-1)
                })
            }
            Ok(None) => {
                if start.elapsed().as_secs() as i64 >= timeout_secs {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = t1.join();
                    let _ = t2.join();
                    return Ok((256, vec![], b"Timed out".to_vec()));
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(e) => return err(format!("Failed to wait for process: {e}")),
        }
    };
    let out = t1.join().unwrap_or_default();
    let errs = t2.join().unwrap_or_default();
    Ok((code, out, errs))
}

/// Applies a patch natively (port of Bazel's `PatchUtil`); no external `patch` binary.
pub fn apply_patch(dir: &Path, patch_file: &Path, strip: i64, _env: &BTreeMap<String, String>) -> Result<(), String> {
    crate::patch::apply(patch_file, strip, dir).map_err(|e| match e {
        crate::patch::PatchError::Failed(m) => format!("Error applying patch {}: {}", patch_file.display(), m),
        crate::patch::PatchError::Io(m) => m,
    })
}

impl Rt {
    pub fn downloader(&self) -> Downloader {
        let mut d = Downloader::new();
        d.distdirs = self.opts.distdirs.clone();
        d.disable_download = self.opts.disable_download;
        if let Some(c) = &self.opts.repository_cache {
            d.repo_cache = Some(RepositoryCache::new(c.clone()));
        }
        if self.opts.download_retries > 0 {
            d.retries = self.opts.download_retries;
        }
        d.rewriter = self.opts.downloader_rewriter.clone();
        d
    }

    pub(crate) fn register_ctx(&self, data: CtxData) -> usize {
        let mut st = self.state.lock().unwrap();
        st.ctxs.push(Some(data));
        st.ctxs.len() - 1
    }

    pub(crate) fn drop_ctx(&self, idx: usize) {
        if let Some(c) = self.state.lock().unwrap().ctxs.get_mut(idx) {
            *c = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repo_metadata_rules() {
        let rt = Rt::new(ExternalOptions::default(), Arc::new(crate::resolve::Resolution::empty()));
        let dir = std::env::temp_dir().join("test_repo_metadata");
        let _ = std::fs::create_dir_all(&dir);
        let data = CtxData {
            kind: CtxKind::Repository,
            name: "test_repo".into(),
            canonical: "test_repo+".into(),
            dir,
            attr: Value::None,
            modules: Value::None,
            root_has_non_dev: false,
            env_seen: Arc::new(Mutex::new(BTreeMap::new())),
            file_inputs: Arc::new(Mutex::new(BTreeMap::new())),
            rec: Arc::new(Mutex::new(Vec::new())),
            label_repo: "test_repo".into(),
            label_pkg: "".into(),
            reproducible: Arc::new(Mutex::new(false)),
            facts: Value::None,
            is_isolated: false,
            extension_id: None,
        };
        let idx = rt.register_ctx(data);

        // reproducible = True with non-empty attrs must error
        let mut kw = BTreeMap::new();
        kw.insert("reproducible".into(), Value::Bool(true));
        kw.insert(
            "attrs_for_reproducibility".into(),
            dict_value(vec![(str_value("sha256"), str_value("abc"))]),
        );

        let mut eval_ctx = EvalContext::default();
        let pos = vec![Value::Host("repository_ctx".into(), idx.to_string())];
        let r = dispatch_object(&rt, &mut eval_ctx, "method:repository_ctx.repo_metadata", pos, kw);
        assert!(r.is_err());
        assert!(r.unwrap_err().0.contains("attrs_for_reproducibility can only be specified if reproducible is False"));
    }

    #[test]
    fn test_ctx_copy() {
        let rt = Rt::new(ExternalOptions::default(), Arc::new(crate::resolve::Resolution::empty()));
        let dir = std::env::temp_dir().join(format!("test_ctx_copy_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let data = CtxData {
            kind: CtxKind::Repository,
            name: "test_copy".into(),
            canonical: "test_copy+".into(),
            dir: dir.clone(),
            attr: Value::None,
            modules: Value::None,
            root_has_non_dev: false,
            env_seen: Arc::new(Mutex::new(BTreeMap::new())),
            file_inputs: Arc::new(Mutex::new(BTreeMap::new())),
            rec: Arc::new(Mutex::new(Vec::new())),
            label_repo: "test_copy".into(),
            label_pkg: "".into(),
            reproducible: Arc::new(Mutex::new(false)),
            facts: Value::None,
            is_isolated: false,
            extension_id: None,
        };
        let idx = rt.register_ctx(data);

        let src = dir.join("src.txt");
        std::fs::write(&src, b"copied content").unwrap();

        // `copy(src, dst)` takes its paths positionally (they are not named parameters).
        let kw = BTreeMap::new();

        let mut eval_ctx = EvalContext::default();
        let pos = vec![
            Value::Host("repository_ctx".into(), idx.to_string()),
            Value::String(src.to_string_lossy().to_string()),
            Value::String(dir.join("dst.txt").to_string_lossy().to_string()),
        ];
        let err = dispatch_object(&rt, &mut eval_ctx, "method:repository_ctx.copy", pos, kw).unwrap_err();
        assert!(err.0.contains("'repository_ctx' value has no field or method 'copy'"), "{}", err.0);
    }

    #[test]
    fn test_ctx_tag_sort_key() {
        let rt = Rt::new(ExternalOptions::default(), Arc::new(crate::resolve::Resolution::empty()));
        let dir = std::env::temp_dir().join("test_ctx_tag_sort");
        let data = CtxData {
            kind: CtxKind::Module,
            name: "ext".into(),
            canonical: "ext+".into(),
            dir,
            attr: Value::None,
            modules: Value::None,
            root_has_non_dev: false,
            env_seen: Arc::new(Mutex::new(BTreeMap::new())),
            file_inputs: Arc::new(Mutex::new(BTreeMap::new())),
            rec: Arc::new(Mutex::new(Vec::new())),
            label_repo: "ext".into(),
            label_pkg: "".into(),
            reproducible: Arc::new(Mutex::new(false)),
            facts: dict_value(vec![(str_value("persisted_key"), str_value("persisted_val"))]),
            is_isolated: true,
            extension_id: Some("@@//:e.bzl%ext".into()),
        };
        let idx = rt.register_ctx(data);

        // Verify module_ctx.facts and is_isolated
        let mut eval_ctx = EvalContext::default();
        let pos = vec![Value::Host("module_ctx".into(), idx.to_string())];
        let facts_val = dispatch_object(&rt, &mut eval_ctx, "attr:module_ctx.facts", pos.clone(), BTreeMap::new()).unwrap();
        match facts_val {
            Value::Dict(d) => assert!(d.lock().unwrap().entries.iter().any(|(k, _)| matches!(k, Value::String(s) if s == "persisted_key"))),
            _ => panic!("Expected facts dict"),
        }
        let iso_val = dispatch_object(&rt, &mut eval_ctx, "attr:module_ctx.is_isolated", pos.clone(), BTreeMap::new()).unwrap();
        assert_eq!(iso_val, Value::Bool(true));

        // Test tag_sort_key
        let mut f1 = BTreeMap::new();
        f1.insert("_sort_key".into(), Value::Tuple(vec![int_value(0), int_value(1)]));
        let tag1 = Value::Struct(Arc::new(zel_starlark::StructValue::new(f1)));

        let mut f2 = BTreeMap::new();
        f2.insert("_sort_key".into(), Value::Tuple(vec![int_value(1), int_value(0)]));
        let tag2 = Value::Struct(Arc::new(zel_starlark::StructValue::new(f2)));

        // `tag` is positional-only.
        let mut pos1 = pos.clone();
        pos1.push(tag1);
        let k1 = dispatch_object(&rt, &mut eval_ctx, "method:module_ctx.tag_sort_key", pos1, BTreeMap::new()).unwrap();

        let mut pos2 = pos;
        pos2.push(tag2);
        let k2 = dispatch_object(&rt, &mut eval_ctx, "method:module_ctx.tag_sort_key", pos2, BTreeMap::new()).unwrap();

        assert_eq!(k1, Value::Tuple(vec![int_value(0), int_value(1)]));
        assert_eq!(k2, Value::Tuple(vec![int_value(1), int_value(0)]));
    }
}
