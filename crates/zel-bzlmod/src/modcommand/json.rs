//! Port of `JsonOutputFormatter`: JSON output of the result graph (Gson pretty-printing style).

use super::executor::{FormatCtx, IsCycle, IsExpanded, IsIndirect};
use super::inspector::ext_id_string;
use super::ExtensionShow;
use crate::json::Json;
use crate::model::ModuleKey;
use crate::resolve::ExtensionId;
use std::collections::HashSet;

struct JsonFormatter<'a, 'b> {
    ctx: &'b FormatCtx<'a>,
    seen_extensions: HashSet<ExtensionId>,
}

/// Formats the result graph as pretty JSON followed by a newline.
pub fn format(ctx: &FormatCtx<'_>) -> String {
    let mut f = JsonFormatter { ctx, seen_extensions: HashSet::new() };
    let mut root = f.print_module(&ModuleKey::root(), None, IsExpanded::True, IsIndirect::False);
    if let Json::Obj(members) = &mut root {
        members.push(("root".into(), Json::Bool(true)));
    }
    let mut s = root.to_pretty(2, false);
    s.push('\n');
    s
}

fn print_key(key: &ModuleKey) -> String {
    if key.is_root() {
        "<root>".to_string()
    } else {
        key.to_string()
    }
}

impl JsonFormatter<'_, '_> {
    fn print_extension(&self, key: &ModuleKey, id: &ExtensionId, unexpanded: bool) -> Json {
        let ctx = self.ctx;
        let mut obj = vec![
            ("key".to_string(), Json::Str(ext_id_string(id))),
            ("unexpanded".to_string(), Json::Bool(unexpanded)),
        ];
        let info = ctx.opts.extension_info;
        if info == ExtensionShow::Usages {
            return Json::Obj(obj);
        }
        let repo_imports = ctx.ext_imports.repos_imported_by(id, key);
        obj.push(("used_repos".into(), Json::Arr(repo_imports.into_iter().map(Json::Str).collect())));
        if unexpanded || info == ExtensionShow::Repos {
            return Json::Obj(obj);
        }
        let imported: std::collections::BTreeSet<String> =
            ctx.ext_imports.get(id).map(|m| m.keys().cloned().collect()).unwrap_or_default();
        let unused: Vec<Json> = ctx.insp.repos_of(id).difference(&imported).cloned().map(Json::Str).collect();
        obj.push(("unused_repos".into(), Json::Arr(unused)));
        Json::Obj(obj)
    }

    /// Depth-first traversal to display modules (while explicitly detecting cycles).
    fn print_module(
        &mut self,
        key: &ModuleKey,
        parent: Option<&ModuleKey>,
        expanded: IsExpanded,
        indirect: IsIndirect,
    ) -> Json {
        let ctx = self.ctx;
        let opts = ctx.opts;
        let node = ctx.result.get(key).cloned().unwrap_or_default();
        let module = ctx.insp.dep_graph.get(key);
        let mut obj: Vec<(String, Json)> = vec![("key".into(), Json::Str(print_key(key)))];
        obj.push(("name".into(), Json::Str(module.map(|m| m.name.clone()).unwrap_or_default())));
        obj.push((
            "version".into(),
            Json::Str(module.map(|m| m.version.to_string()).unwrap_or_default()),
        ));
        // A null apparent name is omitted by Gson (serializeNulls is off).
        let apparent_name: Option<String> = match parent {
            // The apparent repository name under which parent refers to key.
            Some(p) => ctx
                .insp
                .dep_graph
                .get(p)
                .and_then(|pm| pm.repo_name_of_dep(key))
                .map(|s| s.to_string()),
            // The apparent repository name under which key refers to itself.
            None => module.map(|m| m.repo_name.clone()),
        };
        if let Some(a) = apparent_name {
            obj.push(("apparentName".into(), Json::Str(a)));
        }

        if indirect == IsIndirect::False && opts.verbose {
            if let Some(parent) = parent {
                if let Some(e) = ctx.extra_resolution_explanation(key, parent) {
                    if !module.map(|m| m.is_used()).unwrap_or(false) {
                        obj.push(("unused".into(), Json::Bool(true)));
                        obj.push(("resolvedVersion".into(), Json::Str(e.changed_version.to_string())));
                    } else {
                        obj.push(("originalVersion".into(), Json::Str(e.changed_version.to_string())));
                    }
                    // Faithful to the Java implementation, which stores the changed version here.
                    obj.push(("resolutionReason".into(), Json::Str(e.changed_version.to_string())));
                    if let Some(by) = &e.requested_by_modules {
                        obj.push((
                            "resolvedRequestedBy".into(),
                            Json::Arr(by.iter().map(|k| Json::Str(print_key(k))).collect()),
                        ));
                    }
                }
            }
        }

        if expanded == IsExpanded::False {
            obj.push(("unexpanded".into(), Json::Bool(true)));
            return Json::Obj(obj);
        }

        let mut deps = vec![];
        let mut indirect_deps = vec![];
        let mut cycles = vec![];
        for (child, meta) in node.children_sorted_by_edge_type() {
            if meta.cycle == IsCycle::True {
                cycles.push(self.print_module(&child, Some(key), IsExpanded::False, IsIndirect::False));
            } else if meta.indirect == IsIndirect::True {
                indirect_deps.push(self.print_module(&child, Some(key), meta.expanded, IsIndirect::True));
            } else {
                deps.push(self.print_module(&child, Some(key), meta.expanded, IsIndirect::False));
            }
        }
        obj.push(("dependencies".into(), Json::Arr(deps)));
        obj.push(("indirectDependencies".into(), Json::Arr(indirect_deps)));
        obj.push(("cycles".into(), Json::Arr(cycles)));

        if opts.extension_info == ExtensionShow::Hidden {
            return Json::Obj(obj);
        }
        let mut usages = vec![];
        for id in ctx.ext_imports.extensions_used_by(key) {
            let unexpanded = !self.seen_extensions.insert(id.clone());
            usages.push(self.print_extension(key, &id, unexpanded));
        }
        obj.push(("extensionUsages".into(), Json::Arr(usages)));
        Json::Obj(obj)
    }
}
