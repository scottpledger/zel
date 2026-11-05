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

//! Process-wide table of analysis objects that Starlark sees as opaque `Host(kind, id)` values:
//! `File` (`Artifact`), `runfiles`, `Target` (a configured target as seen by a dependent rule),
//! and `Args`. Files are interned by (exec path, kind) so equal files compare/hash equal and
//! can live in depsets and dict keys; ids stay valid across configured targets.

use crate::{Artifact, ConfiguredTarget, Runfiles};
use crate::args::ArgsBuilder;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use zel_starlark::{StructValue, Value};

pub(crate) enum Obj {
    File(Artifact),
    Runfiles(Arc<Runfiles>),
    Target(Arc<ConfiguredTarget>),
    Args(Arc<Mutex<ArgsBuilder>>),
    TemplateDict(Arc<Mutex<Vec<(String, String)>>>),
}

#[derive(Default)]
struct Table {
    items: Vec<Obj>,
    files: HashMap<(String, u8), usize>,
}

fn table() -> &'static Mutex<Table> {
    static T: OnceLock<Mutex<Table>> = OnceLock::new();
    T.get_or_init(|| {
        let _ = zel_starlark::HOST_REPR.set(host_repr);
        Mutex::new(Table::default())
    })
}

fn host_repr(kind: &str, id: &str) -> Option<String> {
    let idx: usize = id.parse().ok()?;
    let t = table().lock().unwrap();
    match (kind, t.items.get(idx)?) {
        ("file", Obj::File(a)) => Some(if a.is_source {
            format!("<source file {}>", a.exec_path)
        } else {
            format!("<generated file {}>", a.root_relative_path_for_repr())
        }),
        ("target", Obj::Target(ct)) => {
            if ct.rule_class == "source file" {
                Some(format!("<input file target {}>", ct.label.canonical_form()))
            } else if ct.rule_class == "generated file" {
                Some(format!("<output file target {}>", ct.label.canonical_form()))
            } else {
                Some(format!("<target {}>", ct.label.canonical_form()))
            }
        }
        ("runfiles", Obj::Runfiles(_)) => Some("<runfiles>".to_string()),
        ("args", Obj::Args(_)) => Some("<args>".to_string()),
        _ => None,
    }
}

impl Artifact {
    fn root_relative_path_for_repr(&self) -> String {
        // `<generated file pkg/x>` — Bazel prints the root-relative path.
        self.root_relative_path.path_string().to_string()
    }
}

fn file_key(a: &Artifact) -> (String, u8) {
    (
        a.exec_path.path_string().to_string(),
        (a.is_tree_artifact as u8) | ((a.is_unresolved_symlink as u8) << 1),
    )
}

/// Interns an artifact as a Starlark `File` value.
pub fn file_value(a: &Artifact) -> Value {
    let mut t = table().lock().unwrap();
    let key = file_key(a);
    if let Some(&i) = t.files.get(&key) {
        return Value::Host("file".into(), i.to_string());
    }
    let i = t.items.len();
    t.items.push(Obj::File(a.clone()));
    t.files.insert(key, i);
    Value::Host("file".into(), i.to_string())
}

/// The artifact behind a `File` value.
pub fn artifact_of(v: &Value) -> Option<Artifact> {
    let Value::Host(k, id) = v else { return None };
    if k != "file" {
        return None;
    }
    let t = table().lock().unwrap();
    match t.items.get(id.parse::<usize>().ok()?)? {
        Obj::File(a) => Some(a.clone()),
        _ => None,
    }
}

pub fn runfiles_value(r: Runfiles) -> Value {
    let mut t = table().lock().unwrap();
    t.items.push(Obj::Runfiles(Arc::new(r)));
    Value::Host("runfiles".into(), (t.items.len() - 1).to_string())
}

pub fn runfiles_of(v: &Value) -> Option<Arc<Runfiles>> {
    let Value::Host(k, id) = v else { return None };
    if k != "runfiles" {
        return None;
    }
    let t = table().lock().unwrap();
    match t.items.get(id.parse::<usize>().ok()?)? {
        Obj::Runfiles(r) => Some(r.clone()),
        _ => None,
    }
}

pub fn target_value(ct: Arc<ConfiguredTarget>) -> Value {
    let mut t = table().lock().unwrap();
    t.items.push(Obj::Target(ct));
    Value::Host("target".into(), (t.items.len() - 1).to_string())
}

pub fn target_of(v: &Value) -> Option<Arc<ConfiguredTarget>> {
    let Value::Host(k, id) = v else { return None };
    if k != "target" {
        return None;
    }
    let t = table().lock().unwrap();
    match t.items.get(id.parse::<usize>().ok()?)? {
        Obj::Target(c) => Some(c.clone()),
        _ => None,
    }
}

pub(crate) fn args_value(a: ArgsBuilder) -> Value {
    let mut t = table().lock().unwrap();
    t.items.push(Obj::Args(Arc::new(Mutex::new(a))));
    Value::Host("args".into(), (t.items.len() - 1).to_string())
}

pub(crate) fn args_of(v: &Value) -> Option<Arc<Mutex<ArgsBuilder>>> {
    let Value::Host(k, id) = v else { return None };
    if k != "args" {
        return None;
    }
    let t = table().lock().unwrap();
    match t.items.get(id.parse::<usize>().ok()?)? {
        Obj::Args(a) => Some(a.clone()),
        _ => None,
    }
}

/// `File.<attr>` (`FileApi`).
pub(crate) fn file_attr(a: &Artifact, attr: &str) -> Result<Value, String> {
    use std::collections::BTreeMap;
    Ok(match attr {
        "path" => Value::String(a.exec_path.path_string().to_string()),
        "short_path" => Value::String(a.short_path()),
        "basename" => Value::String(a.basename().to_string()),
        "dirname" => Value::String(a.dirname()),
        "extension" => Value::String(a.extension().to_string()),
        "is_source" => Value::Bool(a.is_source),
        "is_directory" => Value::Bool(a.is_tree_artifact),
        "is_symlink" => Value::Bool(a.is_unresolved_symlink),
        "owner" => Value::Label(a.owner_label.clone()),
        "root" => {
            let mut m = BTreeMap::new();
            m.insert("path".to_string(), Value::String(a.root_path()));
            Value::Struct(Arc::new(StructValue::new(m)))
        }
        "tree_relative_path" => {
            return Err(format!(
                "'tree_relative_path' is only available on files inside a tree artifact; '{}' is not",
                a.exec_path
            ))
        }
        other => return Err(format!("'File' value has no field or method '{other}'")),
    })
}

/// `ctx.actions.template_dict()`.
pub(crate) fn new_template_dict() -> Value {
    let mut t = table().lock().unwrap();
    t.items.push(Obj::TemplateDict(Arc::new(Mutex::new(Vec::new()))));
    Value::Host("template_dict".into(), (t.items.len() - 1).to_string())
}

pub(crate) fn template_dict_of(v: &Value) -> Option<Arc<Mutex<Vec<(String, String)>>>> {
    let Value::Host(k, id) = v else { return None };
    if k != "template_dict" {
        return None;
    }
    let t = table().lock().unwrap();
    match t.items.get(id.parse::<usize>().ok()?)? {
        Obj::TemplateDict(d) => Some(d.clone()),
        _ => None,
    }
}

pub(crate) fn template_dict_entries(id: &str) -> Option<Vec<(String, String)>> {
    template_dict_of(&Value::Host("template_dict".into(), id.to_string()))
        .map(|d| d.lock().unwrap().clone())
}
