//! Port of `GraphvizOutputFormatter`: Graphviz `dot` output of the result graph.

use super::executor::{FormatCtx, IsIndirect};
use super::inspector::ext_id_string;
use super::ExtensionShow;
use crate::model::ModuleKey;
use crate::resolve::ExtensionId;
use std::collections::{BTreeSet, HashSet, VecDeque};

fn to_id(key: &ModuleKey) -> String {
    if key.is_root() {
        return "\"<root>\"".to_string();
    }
    format!("\"{}@{}\"", key.name, if key.version.is_empty() { "_".to_string() } else { key.version.to_string() })
}

fn ext_to_id(id: &ExtensionId) -> String {
    ext_id_string(id)
}

fn ext_repo_to_id(id: &ExtensionId, repo: &str) -> String {
    format!("\"{}%{}\"", ext_to_id(id), repo)
}

fn print_extension(ctx: &FormatCtx<'_>, str: &mut String, id: &ExtensionId) {
    str.push_str(&format!("  subgraph \"cluster_{}\" {{\n", ext_to_id(id)));
    str.push_str(&format!("    label=\"{}\"\n", ext_to_id(id)));
    let info = ctx.opts.extension_info;
    if info == ExtensionShow::Usages {
        return;
    }
    let used: BTreeSet<String> = ctx.ext_imports.get(id).map(|m| m.keys().cloned().collect()).unwrap_or_default();
    for repo in &used {
        str.push_str(&format!("    {} [ label=\"{}\" ]\n", ext_repo_to_id(id, repo), repo));
    }
    if info == ExtensionShow::Repos {
        return;
    }
    for repo in ctx.insp.repos_of(id).difference(&used) {
        str.push_str(&format!("    {} [ label=\"{}\" style=dotted ]\n", ext_repo_to_id(id, repo), repo));
    }
    str.push_str("  }\n");
}

fn reason_label(ctx: &FormatCtx<'_>, key: &ModuleKey, parent: &ModuleKey) -> String {
    if !ctx.opts.verbose {
        return String::new();
    }
    let Some(e) = ctx.extra_resolution_explanation(key, parent) else { return String::new() };
    let label = e.resolution_reason.label();
    if label.is_empty() {
        String::new()
    } else {
        format!("label={label}")
    }
}

/// Formats the result graph as a Graphviz `digraph` (including the trailing `println`).
pub fn format(ctx: &FormatCtx<'_>) -> String {
    let opts = ctx.opts;
    let mut str = String::new();
    str.push_str("digraph mygraph {\n  node [ shape=box ]\n  edge [ fontsize=8 ]\n");
    let mut seen: HashSet<ModuleKey> = HashSet::new();
    let mut seen_extensions: HashSet<ExtensionId> = HashSet::new();
    let mut to_visit: VecDeque<ModuleKey> = VecDeque::new();
    seen.insert(ModuleKey::root());
    to_visit.push_back(ModuleKey::root());

    while let Some(key) = to_visit.pop_front() {
        let Some(module) = ctx.insp.dep_graph.get(&key) else { continue };
        let Some(node) = ctx.result.get(&key) else { continue };
        let source_id = to_id(&key);

        if key.is_root() {
            let root_label = format!("<root> ({}@{})", module.name, module.version);
            str.push_str(&format!("  \"<root>\" [ label=\"{root_label}\" ]\n"));
        } else if node.target || !module.is_used() {
            let shape = if node.target { "diamond" } else { "box" };
            let style = if module.is_used() { "solid" } else { "dotted" };
            str.push_str(&format!("  {} [ shape={shape} style={style} ]\n", to_id(&key)));
        }

        if opts.extension_info != ExtensionShow::Hidden {
            for id in ctx.ext_imports.extensions_used_by(&key) {
                if opts.extension_info == ExtensionShow::Usages {
                    str.push_str(&format!("  {} -> \"{}\"\n", to_id(&key), ext_to_id(&id)));
                    continue;
                }
                if seen_extensions.insert(id.clone()) {
                    print_extension(ctx, &mut str, &id);
                }
                for repo in ctx.ext_imports.repos_imported_by(&id, &key) {
                    str.push_str(&format!("  {} -> {}\n", to_id(&key), ext_repo_to_id(&id, &repo)));
                }
            }
        }
        for (child, meta) in node.children_sorted_by_key() {
            let child_id = to_id(&child);
            if meta.indirect == IsIndirect::False {
                let label = reason_label(ctx, &child, &key);
                str.push_str(&format!("  {source_id} -> {child_id} [ {label} ]\n"));
            } else {
                str.push_str(&format!("  {source_id} -> {child_id} [ style=dashed ]\n"));
            }
            if seen.insert(child.clone()) {
                to_visit.push_back(child);
            }
        }
    }
    str.push('}');
    str.push('\n');
    str
}
