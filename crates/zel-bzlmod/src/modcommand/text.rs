//! Port of `TextOutputFormatter`: human-readable tree output.

use super::executor::{FormatCtx, IsCycle, IsExpanded, IsIndirect};
use super::inspector::ext_id_string;
use super::{Charset, ExtensionShow};
use crate::model::ModuleKey;
use crate::resolve::ExtensionId;
use std::collections::HashSet;

struct DrawCharset {
    empty_indent: &'static str,
    prev_child_indent: &'static str,
    child_indent: &'static str,
    indirect_child_indent: &'static str,
    last_child_indent: &'static str,
    last_indirect_child_indent: &'static str,
}

const ASCII: DrawCharset = DrawCharset {
    empty_indent: "    ",
    prev_child_indent: "|   ",
    child_indent: "|___",
    indirect_child_indent: "|...",
    last_child_indent: "|___",
    last_indirect_child_indent: "|...",
};

const UTF8: DrawCharset = DrawCharset {
    empty_indent: "    ",
    prev_child_indent: "│   ",
    child_indent: "├───",
    indirect_child_indent: "├╌╌╌",
    last_child_indent: "└───",
    last_indirect_child_indent: "└╌╌╌",
};

struct TextFormatter<'a, 'b> {
    ctx: &'b FormatCtx<'a>,
    charset: &'static DrawCharset,
    /// Bottom of the stack first; the last element is the top.
    is_last_child_stack: Vec<bool>,
    seen_extensions: HashSet<ExtensionId>,
    visited: HashSet<ModuleKey>,
    out: String,
}

/// Formats the result graph as text (including the trailing `println`).
pub fn format(ctx: &FormatCtx<'_>) -> String {
    let mut f = TextFormatter {
        ctx,
        charset: if ctx.opts.charset == Charset::Ascii { &ASCII } else { &UTF8 },
        is_last_child_stack: vec![],
        seen_extensions: HashSet::new(),
        visited: HashSet::new(),
        out: String::new(),
    };
    f.print_module(&ModuleKey::root(), None, IsExpanded::True, IsIndirect::False, IsCycle::False, 0);
    f.out.push('\n');
    f.out
}

impl TextFormatter<'_, '_> {
    /// Prints the indents and the tree drawing characters.
    fn print_tree_drawing(&mut self, indirect: IsIndirect, depth: usize) {
        if depth == 0 {
            return;
        }
        let indents = self.is_last_child_stack.len().saturating_sub(1);
        for i in 0..indents {
            if self.is_last_child_stack[i] {
                self.out.push_str(self.charset.empty_indent);
            } else {
                self.out.push_str(self.charset.prev_child_indent);
            }
        }
        let last = *self.is_last_child_stack.last().unwrap_or(&false);
        let s = match (indirect, last) {
            (IsIndirect::True, true) => self.charset.last_indirect_child_indent,
            (IsIndirect::True, false) => self.charset.indirect_child_indent,
            (IsIndirect::False, true) => self.charset.last_child_indent,
            (IsIndirect::False, false) => self.charset.child_indent,
        };
        self.out.push_str(s);
    }

    fn print_extension(&mut self, key: &ModuleKey, id: &ExtensionId, unexpanded: bool, depth: usize) {
        let info = self.ctx.opts.extension_info;
        self.print_tree_drawing(IsIndirect::False, depth);
        self.out.push('$');
        self.out.push_str(&ext_id_string(id));
        self.out.push(' ');
        if unexpanded && info == ExtensionShow::All {
            self.out.push_str("... ");
        }
        self.out.push('\n');
        if info == ExtensionShow::Usages {
            return;
        }
        let repo_imports = self.ctx.ext_imports.repos_imported_by(id, key);
        let mut unused_repos = vec![];
        if !unexpanded && info == ExtensionShow::All {
            let imported: std::collections::BTreeSet<String> =
                self.ctx.ext_imports.get(id).map(|m| m.keys().cloned().collect()).unwrap_or_default();
            unused_repos = self.ctx.insp.repos_of(id).difference(&imported).cloned().collect::<Vec<_>>();
        }
        let total = repo_imports.len() + unused_repos.len();
        let mut curr = 1;
        for used in &repo_imports {
            self.is_last_child_stack.push(curr == total);
            curr += 1;
            self.print_extension_repo(used, IsIndirect::False, depth + 1);
            self.is_last_child_stack.pop();
        }
        if unexpanded || info == ExtensionShow::Repos {
            return;
        }
        for unused in &unused_repos {
            self.is_last_child_stack.push(curr == total);
            curr += 1;
            self.print_extension_repo(unused, IsIndirect::True, depth + 1);
            self.is_last_child_stack.pop();
        }
    }

    fn print_extension_repo(&mut self, repo: &str, indirect: IsIndirect, depth: usize) {
        self.print_tree_drawing(indirect, depth);
        self.out.push_str(repo);
        self.out.push('\n');
    }

    /// Depth-first traversal to print the actual output.
    fn print_module(
        &mut self,
        key: &ModuleKey,
        parent: Option<&ModuleKey>,
        expanded: IsExpanded,
        indirect: IsIndirect,
        cycle: IsCycle,
        depth: usize,
    ) {
        self.print_tree_drawing(indirect, depth);
        let added = self.visited.insert(key.clone());
        self.print_module_body(key, parent, expanded, indirect, cycle, depth, added);
        if added {
            self.visited.remove(key);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn print_module_body(
        &mut self,
        key: &ModuleKey,
        parent: Option<&ModuleKey>,
        expanded: IsExpanded,
        indirect: IsIndirect,
        cycle: IsCycle,
        depth: usize,
        added: bool,
    ) {
        let ctx = self.ctx;
        let opts = ctx.opts;
        let Some(node) = ctx.result.get(key) else {
            self.out.push_str(&format!("{key} \n"));
            return;
        };
        if key.is_root() {
            if let Some(root) = ctx.insp.dep_graph.get(&ModuleKey::root()) {
                let v = if root.version.is_empty() { "_".to_string() } else { root.version.to_string() };
                self.out.push_str(&format!("<root> ({}@{})", root.name, v));
            } else {
                self.out.push_str("<root> (@_)");
            }
        } else {
            self.out.push_str(&format!("{key} "));
        }

        let mut total = node.size();
        let extensions_used = ctx.ext_imports.extensions_used_by(key);
        if opts.extension_info != ExtensionShow::Hidden {
            total += extensions_used.len();
        }

        // If we've already seen this node in the current traversal path, treat it as a cycle even
        // if the graph structure says otherwise (which can happen due to merged paths).
        let is_cycle = cycle == IsCycle::True || !added;
        if is_cycle {
            self.out.push_str("(cycle) ");
        } else if expanded == IsExpanded::False {
            self.out.push_str("(*) ");
        } else if node.target {
            self.out.push_str("# ");
        }
        let module = ctx.insp.dep_graph.get(key);
        let used = module.map(|m| m.is_used()).unwrap_or(false);
        if !opts.verbose && !used {
            self.out.push_str("(unused) ");
        }
        // If the edge is indirect, the parent is not only unknown, but the node could have come
        // from multiple paths merged in the process, so we skip the resolution explanation.
        if indirect == IsIndirect::False && opts.verbose {
            if let Some(parent) = parent {
                if let Some(e) = ctx.extra_resolution_explanation(key, parent) {
                    self.out.push_str(&e.to_explanation_string(!used));
                }
            }
        }
        self.out.push('\n');

        if expanded == IsExpanded::False || is_cycle {
            return;
        }

        let mut curr = 1;
        if opts.extension_info != ExtensionShow::Hidden {
            for id in &extensions_used {
                let unexpanded = !self.seen_extensions.insert(id.clone());
                self.is_last_child_stack.push(curr == total);
                curr += 1;
                self.print_extension(key, id, unexpanded, depth + 1);
                self.is_last_child_stack.pop();
            }
        }
        for (child_key, meta) in node.children_sorted_by_edge_type() {
            self.is_last_child_stack.push(curr == total);
            curr += 1;
            self.print_module(&child_key, Some(key), meta.expanded, meta.indirect, meta.cycle, depth + 1);
            self.is_last_child_stack.pop();
        }
    }
}
