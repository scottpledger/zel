//! Port of `ModExecutor`: executes the graph queries of `mod` and prints the result with the
//! configured output formatter.

use super::inspector::{
    ext_id_string, sorted_unique_ext_ids, AugmentedModule, ExtOrd, Inspector, MaybeCompleteExtSet,
    ResolutionReason,
};
use super::{graphviz, json, text, ModError, ModErrorKind, ModOptions, OutputFormat};
use crate::model::{AttrVal, ModuleKey};
use crate::resolve::ExtensionId;
use crate::version::Version;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IsExpanded {
    False,
    True,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IsIndirect {
    False,
    True,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IsCycle {
    False,
    True,
}

/// Detailed edge type for the [`ResultNode`] graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeMetadata {
    pub expanded: IsExpanded,
    pub indirect: IsIndirect,
    pub cycle: IsCycle,
}

/// A node representing a module in the result graph. Children behave like an
/// `ImmutableSetMultimap<ModuleKey, NodeMetadata>`: grouped by key in first-insertion order, with
/// duplicate (key, metadata) pairs dropped.
#[derive(Debug, Clone, Default)]
pub struct ResultNode {
    /// Whether the module is one of the targets in a paths query.
    pub target: bool,
    children: Vec<(ModuleKey, Vec<NodeMetadata>)>,
}

impl PartialEq for ResultNode {
    /// Multimap equality: insertion order is irrelevant.
    fn eq(&self, other: &Self) -> bool {
        let sorted = |n: &ResultNode| {
            let mut v = n.entries();
            v.sort();
            v
        };
        self.target == other.target && sorted(self) == sorted(other)
    }
}

impl ResultNode {
    pub fn new() -> ResultNode {
        ResultNode::default()
    }

    pub fn with_target(mut self, t: bool) -> ResultNode {
        self.target = t;
        self
    }

    fn add_meta(&mut self, key: &ModuleKey, meta: NodeMetadata) {
        if let Some((_, metas)) = self.children.iter_mut().find(|(k, _)| k == key) {
            if !metas.contains(&meta) {
                metas.push(meta);
            }
        } else {
            self.children.push((key.clone(), vec![meta]));
        }
    }

    pub fn add_child(&mut self, key: &ModuleKey, expanded: IsExpanded, indirect: IsIndirect) {
        self.add_meta(key, NodeMetadata { expanded, indirect, cycle: IsCycle::False });
    }

    pub fn with_child(mut self, key: &ModuleKey, expanded: IsExpanded, indirect: IsIndirect) -> ResultNode {
        self.add_child(key, expanded, indirect);
        self
    }

    pub fn add_cycle(&mut self, key: &ModuleKey) {
        self.add_meta(key, NodeMetadata { expanded: IsExpanded::False, indirect: IsIndirect::False, cycle: IsCycle::True });
    }

    fn add_children_of(&mut self, other: &ResultNode) {
        for (k, metas) in &other.children {
            for m in metas {
                self.add_meta(k, *m);
            }
        }
    }

    /// Total number of (key, metadata) entries.
    pub fn size(&self) -> usize {
        self.children.iter().map(|(_, m)| m.len()).sum()
    }

    fn entries(&self) -> Vec<(ModuleKey, NodeMetadata)> {
        self.children.iter().flat_map(|(k, ms)| ms.iter().map(move |m| (k.clone(), *m))).collect()
    }

    fn keys(&self) -> Vec<&ModuleKey> {
        self.children.iter().map(|(k, _)| k).collect()
    }

    /// `getChildrenSortedByKey`: a sorted set by key only (the first metadata of a key wins).
    pub fn children_sorted_by_key(&self) -> Vec<(ModuleKey, NodeMetadata)> {
        let mut v: Vec<(ModuleKey, NodeMetadata)> =
            self.children.iter().filter_map(|(k, ms)| ms.first().map(|m| (k.clone(), *m))).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// `getChildrenSortedByEdgeType`: cycles first, then unexpanded, then direct, then by key.
    pub fn children_sorted_by_edge_type(&self) -> Vec<(ModuleKey, NodeMetadata)> {
        let mut v = self.entries();
        v.sort_by(|a, b| {
            b.1.cycle
                .cmp(&a.1.cycle)
                .then(a.1.expanded.cmp(&b.1.expanded))
                .then(a.1.indirect.cmp(&b.1.indirect))
                .then_with(|| a.0.cmp(&b.0))
        });
        v
    }
}

pub type ResultGraph = BTreeMap<ModuleKey, ResultNode>;

/// Extension -> (imported repo name -> modules importing it).
#[derive(Debug, Clone, Default)]
pub struct ImportsTable {
    pub entries: Vec<(ExtensionId, BTreeMap<String, BTreeSet<ModuleKey>>)>,
}

impl ImportsTable {
    pub fn get(&self, id: &ExtensionId) -> Option<&BTreeMap<String, BTreeSet<ModuleKey>>> {
        self.entries.iter().find(|(i, _)| i == id).map(|(_, m)| m)
    }

    /// Repos of `id` imported by `key` (`get(id).inverse().get(key)`), sorted.
    pub fn repos_imported_by(&self, id: &ExtensionId, key: &ModuleKey) -> BTreeSet<String> {
        self.get(id)
            .map(|m| m.iter().filter(|(_, mods)| mods.contains(key)).map(|(r, _)| r.clone()).collect())
            .unwrap_or_default()
    }

    /// Extensions from which `key` imports at least one repo, in lexicographic order.
    pub fn extensions_used_by(&self, key: &ModuleKey) -> Vec<ExtensionId> {
        let set: BTreeSet<ExtOrd> = self
            .entries
            .iter()
            .filter(|(_, m)| m.values().any(|mods| mods.contains(key)))
            .map(|(i, _)| ExtOrd(i.clone()))
            .collect();
        set.into_iter().map(|e| e.0).collect()
    }
}

/// Everything the output formatters need.
pub struct FormatCtx<'a> {
    pub result: &'a ResultGraph,
    pub insp: &'a Inspector,
    pub ext_imports: &'a ImportsTable,
    pub opts: &'a ModOptions,
}

/// Compact representation of the data provided by the `--verbose` flag.
#[derive(Debug, Clone, PartialEq)]
pub struct Explanation {
    pub changed_version: Version,
    pub resolution_reason: ResolutionReason,
    pub requested_by_modules: Option<Vec<ModuleKey>>,
}

impl Explanation {
    /// The exact label printed next to the module with `--verbose`.
    pub fn to_explanation_string(&self, unused: bool) -> String {
        let changed = if self.changed_version.is_empty() { "_".to_string() } else { self.changed_version.to_string() };
        let to_or_was = if unused { "to" } else { "was" };
        let reason = match &self.requested_by_modules {
            Some(mods) => mods.iter().map(|m| m.to_string()).collect::<Vec<_>>().join(", "),
            None => self.resolution_reason.lower_name().to_string(),
        };
        format!("({to_or_was} {changed}, cause {reason})")
    }
}

impl<'a> FormatCtx<'a> {
    /// `None` if the module version has not changed during resolution or if the module is `<root>`.
    pub fn extra_resolution_explanation(&self, key: &ModuleKey, parent: &ModuleKey) -> Option<Explanation> {
        if key.is_root() {
            return None;
        }
        let module = self.insp.dep_graph.get(key)?;
        let parent_module = self.insp.dep_graph.get(parent)?;
        let repo_name = parent_module.get_all_deps(self.opts.include_unused).get(key)?.clone();
        let reason = parent_module.reason(&repo_name)?;
        let replacement: &AugmentedModule = if module.is_used() {
            module
        } else {
            self.insp.dep_graph.get(parent_module.dep(&repo_name)?)?
        };
        if reason != ResolutionReason::Original {
            let changed_version = if !module.is_used() {
                replacement.version.clone()
            } else {
                self.insp.dep_graph.get(parent_module.unused_dep(&repo_name)?)?.version.clone()
            };
            let changed_by = if reason == ResolutionReason::MinimalVersionSelection {
                Some(replacement.original_dependants.clone())
            } else {
                None
            };
            return Some(Explanation { changed_version, resolution_reason: reason, requested_by_modules: changed_by });
        }
        None
    }
}

/// Targets of the result graph: either all modules or a specific set.
enum Targets {
    Complete,
    Set(HashSet<ModuleKey>),
}

impl Targets {
    fn is_complete(&self) -> bool {
        matches!(self, Targets::Complete)
    }
    fn contains(&self, k: &ModuleKey) -> bool {
        match self {
            Targets::Complete => true,
            Targets::Set(s) => s.contains(k),
        }
    }
}

pub struct ModExecutor<'a> {
    insp: &'a Inspector,
    extension_filter: Option<MaybeCompleteExtSet>,
    opts: &'a ModOptions,
    ext_imports: ImportsTable,
    out: String,
}

impl<'a> ModExecutor<'a> {
    pub fn new(insp: &'a Inspector, extension_filter: Option<MaybeCompleteExtSet>, opts: &'a ModOptions) -> Self {
        let mut ex = ModExecutor { insp, extension_filter, opts, ext_imports: ImportsTable::default(), out: String::new() };
        let all: BTreeSet<ModuleKey> = insp.dep_graph.keys().cloned().collect();
        ex.ext_imports = ex.compute_repo_imports_table(&all);
        ex
    }

    /// Takes the text printed so far.
    pub fn take_output(&mut self) -> String {
        std::mem::take(&mut self.out)
    }

    fn println(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn output_graph(&mut self, result: &ResultGraph) {
        let ctx = FormatCtx { result, insp: self.insp, ext_imports: &self.ext_imports, opts: self.opts };
        let s = match self.opts.output {
            OutputFormat::Json => json::format(&ctx),
            OutputFormat::Graph => graphviz::format(&ctx),
            _ => text::format(&ctx),
        };
        self.out.push_str(&s);
    }

    pub fn graph(&mut self, from: &[ModuleKey]) {
        let targets = self.compute_extension_filter_targets();
        let result = if targets.is_empty() {
            self.expand_and_prune(from)
        } else {
            self.expand_paths_to_targets(from, &targets, false)
        };
        self.output_graph(&result);
    }

    pub fn path(&mut self, from: &[ModuleKey], to: &[ModuleKey]) {
        self.paths(from, to, true);
    }

    pub fn all_paths(&mut self, from: &[ModuleKey], to: &[ModuleKey]) {
        self.paths(from, to, false);
    }

    fn paths(&mut self, from: &[ModuleKey], to: &[ModuleKey], single: bool) {
        let mut targets = self.compute_extension_filter_targets();
        for t in to {
            if !targets.contains(t) {
                targets.push(t.clone());
            }
        }
        if targets.is_empty() {
            self.println("No target modules specified.");
            return;
        }
        let result = self.expand_paths_to_targets(from, &targets, single);
        if result.is_empty() {
            self.println("No path found to the specified target modules.");
            return;
        }
        self.output_graph(&result);
    }

    fn root_direct_children(&self) -> BTreeSet<ModuleKey> {
        self.insp
            .dep_graph
            .get(&ModuleKey::root())
            .map(|m| m.get_all_deps(self.opts.include_unused).keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Reconstructs a path backwards from a child to the root and adds it to the result graph.
    fn add_path_to_result_graph(
        &self,
        result_graph: &mut HashMap<ModuleKey, ResultNode>,
        bfs_parent: &HashMap<ModuleKey, Option<ModuleKey>>,
        path_parent: Option<&ModuleKey>,
        path_child: &ModuleKey,
    ) {
        let mut child_node = result_graph.get(path_child).cloned().unwrap_or_default();
        child_node.target = true;
        result_graph.insert(path_child.clone(), child_node);

        let root_direct_children = self.root_direct_children();
        let mut current_child = path_child.clone();
        let mut current_parent = path_parent.cloned();
        while let Some(parent) = current_parent {
            let mut parent_node = ResultNode::new();
            if let Some(existing) = result_graph.get(&parent) {
                parent_node.add_children_of(existing);
                parent_node.target = existing.target;
            }
            let is_indirect = parent.is_root() && !root_direct_children.contains(&current_child);
            parent_node.add_child(
                &current_child,
                IsExpanded::True,
                if is_indirect { IsIndirect::True } else { IsIndirect::False },
            );
            result_graph.insert(parent.clone(), parent_node);
            current_child = parent;
            current_parent = bfs_parent.get(&current_child).cloned().flatten();
        }
    }

    fn sorted_deps(&self, key: &ModuleKey) -> Vec<ModuleKey> {
        self.insp
            .dep_graph
            .get(key)
            .map(|m| m.get_all_deps(self.opts.include_unused).into_keys().filter(|k| self.filter_builtin(k)).collect())
            .unwrap_or_default()
    }

    /// Finds paths from `from` to `targets` and returns a (depth-pruned) graph containing them.
    pub fn expand_paths_to_targets(
        &mut self,
        from: &[ModuleKey],
        targets: &[ModuleKey],
        find_single_path: bool,
    ) -> ResultGraph {
        let mut bfs_parent: HashMap<ModuleKey, Option<ModuleKey>> = HashMap::new();
        let mut sorted_from: Vec<&ModuleKey> = from.iter().filter(|k| self.filter_builtin(k)).collect();
        sorted_from.sort();
        for k in sorted_from {
            bfs_parent.insert(k.clone(), Some(ModuleKey::root()));
        }
        bfs_parent.insert(ModuleKey::root(), None);

        let mut result_graph: HashMap<ModuleKey, ResultNode> = HashMap::new();
        let mut queue: VecDeque<ModuleKey> = from.iter().cloned().collect();
        let mut found: HashSet<ModuleKey> = HashSet::new();
        let target_set: HashSet<ModuleKey> = targets.iter().cloned().collect();

        while !queue.is_empty() {
            if find_single_path && target_set.iter().all(|t| found.contains(t)) {
                break;
            }
            let current = queue.pop_front().unwrap();
            if target_set.contains(&current) && !(find_single_path && found.contains(&current)) {
                let parent = bfs_parent.get(&current).cloned().flatten();
                self.add_path_to_result_graph(&mut result_graph, &bfs_parent, parent.as_ref(), &current);
                found.insert(current.clone());
                if find_single_path && target_set.iter().all(|t| found.contains(t)) {
                    break;
                }
            }
            if !self.insp.dep_graph.contains_key(&current) {
                continue;
            }
            for dep in self.sorted_deps(&current) {
                if target_set.contains(&dep) && !(find_single_path && found.contains(&dep)) {
                    self.add_path_to_result_graph(&mut result_graph, &bfs_parent, Some(&current), &dep);
                    found.insert(dep.clone());
                }
                if !bfs_parent.contains_key(&dep) {
                    bfs_parent.insert(dep.clone(), Some(current.clone()));
                    queue.push_back(dep);
                }
            }
        }

        let mut pruner = Pruner::new(Targets::Set(target_set), result_graph, self.opts);
        let result = pruner.prune_by_depth();
        let present: BTreeSet<ModuleKey> = result.keys().cloned().collect();
        self.ext_imports = self.compute_repo_imports_table(&present);
        result
    }

    /// Expands the full dependency graph from `from` and prunes it to the requested depth.
    pub fn expand_and_prune(&mut self, from: &[ModuleKey]) -> ResultGraph {
        let mut full: HashMap<ModuleKey, ResultNode> = HashMap::new();
        let root_direct = self.root_direct_children();
        let pinned = self.pinned_children_of_root(&root_direct, from);

        let mut root_node = ResultNode::new();
        for p in &pinned {
            let is_direct = root_direct.contains(p);
            root_node.add_child(p, IsExpanded::True, if is_direct { IsIndirect::False } else { IsIndirect::True });
        }
        full.insert(ModuleKey::root(), root_node);

        let mut visited: HashSet<ModuleKey> = pinned.iter().cloned().collect();
        let mut queue: VecDeque<ModuleKey> = pinned.iter().cloned().collect();
        visited.insert(ModuleKey::root());
        while let Some(current) = queue.pop_front() {
            let mut node = ResultNode::new();
            for dep in self.sorted_deps(&current) {
                if visited.contains(&dep) {
                    node.add_child(&dep, IsExpanded::False, IsIndirect::False);
                } else {
                    node.add_child(&dep, IsExpanded::True, IsIndirect::False);
                    visited.insert(dep.clone());
                    queue.push_back(dep);
                }
            }
            full.insert(current, node);
        }
        let mut pruner = Pruner::new(Targets::Complete, full, self.opts);
        let result = pruner.prune_by_depth();
        let present: BTreeSet<ModuleKey> = result.keys().cloned().collect();
        self.ext_imports = self.compute_repo_imports_table(&present);
        result
    }

    /// Sorted direct children of the root in the result graph (root's direct deps replace `<root>`).
    fn pinned_children_of_root(&self, root_direct: &BTreeSet<ModuleKey>, from: &[ModuleKey]) -> Vec<ModuleKey> {
        let mut keys: BTreeSet<ModuleKey> = from.iter().cloned().collect();
        if keys.contains(&ModuleKey::root()) {
            keys.remove(&ModuleKey::root());
            keys.extend(root_direct.iter().cloned());
        }
        keys.into_iter().filter(|k| self.filter_builtin(k)).collect()
    }

    /// Modules using the filtered extension(s), if `--extension_filter` is set.
    fn compute_extension_filter_targets(&self) -> Vec<ModuleKey> {
        let Some(filter) = &self.extension_filter else { return vec![] };
        self.insp
            .dep_graph
            .keys()
            .filter(|k| self.filter_unused(k))
            .filter(|k| self.filter_builtin(k))
            .filter(|k| {
                let used: Vec<&ExtensionId> =
                    self.insp.extension_usages.iter().filter(|(_, m, _)| m == *k).map(|(i, _, _)| i).collect();
                match filter {
                    MaybeCompleteExtSet::Complete => !used.is_empty(),
                    MaybeCompleteExtSet::Set(s) => used.iter().any(|i| s.contains(i)),
                }
            })
            .cloned()
            .collect()
    }

    /// Computes, for each extension, the multimap of imported repo -> importing modules.
    fn compute_repo_imports_table(&self, present: &BTreeSet<ModuleKey>) -> ImportsTable {
        let mut table = ImportsTable::default();
        for id in self.insp.extension_ids() {
            if let Some(f) = &self.extension_filter {
                if !f.contains(&id) {
                    continue;
                }
            }
            let mut repos: BTreeMap<String, BTreeSet<ModuleKey>> = BTreeMap::new();
            for (i, k, u) in &self.insp.extension_usages {
                if *i != id || !present.contains(k) {
                    continue;
                }
                for proxy in &u.proxies {
                    for (_, exported) in &proxy.imports {
                        repos.entry(exported.clone()).or_default().insert(k.clone());
                    }
                }
            }
            table.entries.push((id, repos));
        }
        table
    }

    fn filter_unused(&self, key: &ModuleKey) -> bool {
        self.opts.include_unused || self.insp.dep_graph.get(key).map(|m| m.is_used()).unwrap_or(false)
    }

    fn filter_builtin(&self, key: &ModuleKey) -> bool {
        self.opts.include_builtin || !(key.name == "bazel_tools" && key.version.is_empty())
    }

    /// `show_extension`.
    pub fn show_extension(&mut self, extensions: &[ExtensionId], from_usages: &[ModuleKey]) -> Result<(), ModError> {
        for ext in sorted_unique_ext_ids(extensions.to_vec()) {
            self.display_extension(&ext, from_usages)?;
        }
        Ok(())
    }

    fn tag_to_function_args(attrs: &[(String, AttrVal)]) -> String {
        let mut v: Vec<&(String, AttrVal)> = attrs.iter().collect();
        // show 'name' first for readability, similar to buildifier
        v.sort_by(|a, b| {
            let ka = if a.0 == "name" { "" } else { a.0.as_str() };
            let kb = if b.0 == "name" { "" } else { b.0.as_str() };
            ka.cmp(kb)
        });
        v.iter().map(|(k, val)| format!("{}={}", k, val.repr())).collect::<Vec<_>>().join(", ")
    }

    fn display_extension(&mut self, extension: &ExtensionId, from_usages: &[ModuleKey]) -> Result<(), ModError> {
        self.out.push_str(&format!("## {}:\n", ext_id_string(extension)));
        self.println("");
        self.println("Fetched repositories:");
        let Some(imports) = self.ext_imports.get(extension).cloned() else {
            return Err(ModError::raw(
                ModErrorKind::InvalidArguments,
                format!("No extension {} exists in the dependency graph", ext_id_string(extension)),
            ));
        };
        let used_repos: BTreeSet<String> = imports.keys().cloned().collect();
        let all_repos = self.insp.repos_of(extension);
        let unused_repos: BTreeSet<String> = all_repos.difference(&used_repos).cloned().collect();
        for repo in &used_repos {
            let mods: BTreeSet<&ModuleKey> = imports[repo].iter().collect();
            let line = format!(
                "  - {} (imported by {})\n",
                repo,
                mods.iter().map(|m| m.to_string()).collect::<Vec<_>>().join(", ")
            );
            self.out.push_str(&line);
        }
        for repo in &unused_repos {
            self.out.push_str(&format!("  - {repo}\n"));
        }
        self.println("");

        let usage_modules: Vec<ModuleKey> = if from_usages.is_empty() {
            self.insp
                .extension_usages
                .iter()
                .filter(|(i, _, _)| i == extension)
                .map(|(_, k, _)| k.clone())
                .collect()
        } else {
            from_usages.to_vec()
        };
        for module in &usage_modules {
            let Some((_, _, usage)) =
                self.insp.extension_usages.iter().find(|(i, k, _)| i == extension && k == module)
            else {
                continue;
            };
            let first = usage.proxies.first();
            let loc = first.map(|p| short_location(&p.location)).unwrap_or_default();
            self.out.push_str(&format!("## Usage in {module} from {loc}\n"));

            if extension.is_innate() {
                // Fake module extension created by use_repo_rule(); name is "<bzl_file_label> <rule_name>".
                let last_space = extension.name.rfind(' ').unwrap_or(0);
                let raw_label = &extension.name[..last_space];
                let rule_name = &extension.name[last_space + 1..];
                self.out.push_str(&format!("{rule_name} = use_repo_rule(\"{raw_label}\", \"{rule_name}\")\n"));
                for tag in &usage.tags {
                    let callee = if tag.tag_name != "repo" {
                        format!("{}.{}", rule_name, tag.tag_name)
                    } else {
                        rule_name.to_string()
                    };
                    self.out.push_str(&format!("{}({})\n", callee, Self::tag_to_function_args(&tag.attrs)));
                }
                self.println("");
            } else {
                for tag in &usage.tags {
                    self.out.push_str(&format!(
                        "{}.{}({})\n",
                        extension.name,
                        tag.tag_name,
                        Self::tag_to_function_args(&tag.attrs)
                    ));
                }
                self.out.push_str("use_repo(\n");
                self.out.push_str(&format!("  {},\n", extension.name));
                for proxy in &usage.proxies {
                    for (local, exported) in &proxy.imports {
                        if local == exported {
                            self.out.push_str(&format!("  \"{local}\",\n"));
                        } else {
                            self.out.push_str(&format!("  {local}=\"{exported}\",\n"));
                        }
                    }
                }
                self.out.push_str(")\n\n");
            }
        }
        Ok(())
    }
}

/// `file:line` from a `file:line:column` location string.
fn short_location(loc: &str) -> String {
    let parts: Vec<&str> = loc.rsplitn(3, ':').collect();
    if parts.len() == 3 && parts[0].chars().all(|c| c.is_ascii_digit()) && parts[1].chars().all(|c| c.is_ascii_digit()) {
        format!("{}:{}", parts[2], parts[1])
    } else {
        loc.to_string()
    }
}

/// Prunes the result graph after the specified depth.
struct Pruner<'o> {
    old_result: HashMap<ModuleKey, ResultNode>,
    result_builder: HashMap<ModuleKey, ResultNode>,
    parent_stack: HashSet<ModuleKey>,
    targets: Targets,
    opts: &'o ModOptions,
}

impl<'o> Pruner<'o> {
    fn new(targets: Targets, old_result: HashMap<ModuleKey, ResultNode>, opts: &'o ModOptions) -> Self {
        Pruner { old_result, result_builder: HashMap::new(), parent_stack: HashSet::new(), targets, opts }
    }

    /// Prunes the tree after the specified depth using DFS (because some nodes may still appear
    /// after the max depth).
    fn prune_by_depth(&mut self) -> ResultGraph {
        if self.old_result.is_empty() {
            return ResultGraph::new();
        }
        let root = ModuleKey::root();
        self.result_builder.insert(root.clone(), ResultNode::new());
        self.parent_stack.insert(root.clone());
        let root_children = self.old_result.get(&root).cloned().unwrap_or_default().children_sorted_by_key();
        for (key, meta) in root_children {
            self.result_builder.get_mut(&root).unwrap().add_child(&key, IsExpanded::True, meta.indirect);
            self.visit_visible(&key, 1, &root, IsExpanded::True);
        }
        self.result_builder.drain().collect()
    }

    fn visit_visible(&mut self, module_key: &ModuleKey, depth: i32, parent_key: &ModuleKey, expanded: IsExpanded) {
        self.parent_stack.insert(module_key.clone());
        let old_node = self.old_result.get(module_key).cloned().unwrap_or_default();
        self.result_builder.entry(module_key.clone()).or_default().target = old_node.target;
        if depth > 1 {
            if let Some(p) = self.result_builder.get_mut(parent_key) {
                p.add_child(module_key, expanded, IsIndirect::False);
            }
        }
        if expanded == IsExpanded::False {
            self.parent_stack.remove(module_key);
            return;
        }
        for (child_key, meta) in old_node.children_sorted_by_key() {
            if !self.parent_stack.contains(&child_key) {
                if depth < self.opts.depth {
                    self.visit_visible(&child_key, depth + 1, module_key, meta.expanded);
                } else if !self.targets.is_complete() {
                    self.visit_detached(&child_key, module_key, module_key.clone(), meta.expanded);
                }
            } else if self.opts.cycles {
                self.result_builder.get_mut(module_key).unwrap().add_cycle(&child_key);
            }
        }
        self.parent_stack.remove(module_key);
    }

    /// Detached mode is only present with targets and handles adding targets and target parents
    /// living below the specified depth to the graph.
    fn visit_detached(
        &mut self,
        module_key: &ModuleKey,
        parent_key: &ModuleKey,
        mut last_visible_parent: ModuleKey,
        expanded: IsExpanded,
    ) {
        self.parent_stack.insert(module_key.clone());
        let old_node = self.old_result.get(module_key).cloned().unwrap_or_default();
        let node = ResultNode::new().with_target(old_node.target);
        let mut registered = false;
        if old_node.target || self.is_target_parent(&old_node) {
            let indirect = if &last_visible_parent == parent_key { IsIndirect::False } else { IsIndirect::True };
            if let Some(p) = self.result_builder.get_mut(&last_visible_parent) {
                p.add_child(module_key, expanded, indirect);
            }
            self.result_builder.insert(module_key.clone(), node);
            registered = true;
            last_visible_parent = module_key.clone();
        }
        if expanded == IsExpanded::False {
            self.parent_stack.remove(module_key);
            return;
        }
        for (child_key, meta) in old_node.children_sorted_by_key() {
            if !self.parent_stack.contains(&child_key) {
                self.visit_detached(&child_key, module_key, last_visible_parent.clone(), meta.expanded);
            } else if self.opts.cycles && registered {
                if let Some(n) = self.result_builder.get_mut(module_key) {
                    n.add_cycle(&child_key);
                }
            }
        }
        self.parent_stack.remove(module_key);
    }

    fn is_target_parent(&self, node: &ResultNode) -> bool {
        node.keys().into_iter().filter(|k| !self.parent_stack.contains(*k)).any(|k| self.targets.contains(k))
    }
}
