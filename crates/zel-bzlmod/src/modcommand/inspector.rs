//! Port of `BazelModuleInspectorFunction` / `BazelModuleInspectorValue`: an augmented,
//! bidirectional view of the module dependency graph used by `mod`.

use crate::model::{ExtensionUsage, ModuleKey, ModuleOverride};
use crate::resolve::{ExtensionId, Resolution};
use crate::version::Version;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The reason why a final dependency of a module was resolved the way it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionReason {
    Original,
    MinimalVersionSelection,
    SingleVersionOverride,
    MultipleVersionOverride,
    NonRegistryOverride,
}

impl ResolutionReason {
    /// Short label used by the graphviz output.
    pub fn label(&self) -> &'static str {
        match self {
            ResolutionReason::Original => "",
            ResolutionReason::MinimalVersionSelection => "MVS",
            ResolutionReason::SingleVersionOverride => "SVO",
            ResolutionReason::MultipleVersionOverride => "MVO",
            ResolutionReason::NonRegistryOverride => "NRO",
        }
    }

    /// Lower-cased enum name (`Ascii.toLowerCase(reason.toString())`).
    pub fn lower_name(&self) -> &'static str {
        match self {
            ResolutionReason::Original => "original",
            ResolutionReason::MinimalVersionSelection => "minimal_version_selection",
            ResolutionReason::SingleVersionOverride => "single_version_override",
            ResolutionReason::MultipleVersionOverride => "multiple_version_override",
            ResolutionReason::NonRegistryOverride => "non_registry_override",
        }
    }
}

/// `AugmentedModule`: a module wrapper with references to dependants (including unused ones).
#[derive(Debug, Clone)]
pub struct AugmentedModule {
    pub name: String,
    pub version: Version,
    pub key: ModuleKey,
    pub repo_name: String,
    /// Modules in the resolved graph depending on this module after resolution.
    pub dependants: Vec<ModuleKey>,
    /// Modules in the complete graph that originally depended on this module.
    pub original_dependants: Vec<ModuleKey>,
    /// Repo name -> resolved dependency key.
    pub deps: Vec<(String, ModuleKey)>,
    /// Repo name -> original (now unused) dependency key.
    pub unused_deps: Vec<(String, ModuleKey)>,
    pub dep_reasons: Vec<(String, ResolutionReason)>,
    pub loaded: bool,
}

impl AugmentedModule {
    /// Sorted union of used (and optionally unused) deps: key -> repo name.
    pub fn get_all_deps(&self, unused: bool) -> BTreeMap<ModuleKey, String> {
        let mut map = BTreeMap::new();
        for (n, k) in &self.deps {
            map.insert(k.clone(), n.clone());
        }
        if unused {
            for (n, k) in &self.unused_deps {
                map.insert(k.clone(), n.clone());
            }
        }
        map
    }

    pub fn is_used(&self) -> bool {
        self.key.is_root() || !self.dependants.is_empty()
    }

    pub fn dep(&self, repo_name: &str) -> Option<&ModuleKey> {
        self.deps.iter().find(|(n, _)| n == repo_name).map(|(_, k)| k)
    }

    pub fn unused_dep(&self, repo_name: &str) -> Option<&ModuleKey> {
        self.unused_deps.iter().find(|(n, _)| n == repo_name).map(|(_, k)| k)
    }

    /// The repo name under which this module refers to `key` (`deps().inverse().get(key)`).
    pub fn repo_name_of_dep(&self, key: &ModuleKey) -> Option<&str> {
        self.deps.iter().find(|(_, k)| k == key).map(|(n, _)| n.as_str())
    }

    pub fn reason(&self, repo_name: &str) -> Option<ResolutionReason> {
        self.dep_reasons.iter().find(|(n, _)| n == repo_name).map(|(_, r)| *r)
    }
}

/// Compares labels of the form `@@repo//pkg:name` like Java's `Label.compareTo`.
fn cmp_label(a: &str, b: &str) -> Ordering {
    fn split(s: &str) -> (&str, Vec<&str>, &str) {
        let s = s.trim_start_matches('@');
        let (repo, rest) = s.split_once("//").unwrap_or((s, ""));
        let (pkg, name) = rest.split_once(':').unwrap_or((rest, ""));
        (repo, pkg.split('/').collect(), name)
    }
    let (ra, pa, na) = split(a);
    let (rb, pb, nb) = split(b);
    ra.cmp(rb).then_with(|| pa.cmp(&pb)).then_with(|| na.cmp(nb))
}

/// `ModuleExtensionId.LEXICOGRAPHIC_COMPARATOR`.
pub fn cmp_ext_id(a: &ExtensionId, b: &ExtensionId) -> Ordering {
    cmp_label(&a.bzl_file, &b.bzl_file)
        .then_with(|| a.name.cmp(&b.name))
        .then_with(|| match (&a.isolation_key, &b.isolation_key) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some((m1, u1)), Some((m2, u2))) => m1.cmp(m2).then_with(|| u1.cmp(u2)),
        })
}

/// `ModuleExtensionId.toString()`: `<label>%<name>[%<module>+<usage>]`.
pub fn ext_id_string(id: &ExtensionId) -> String {
    let iso = match &id.isolation_key {
        Some((m, u)) => format!("%{m}+{u}"),
        None => String::new(),
    };
    format!("{}%{}{}", id.bzl_file, id.name, iso)
}

/// Sorts and dedups extension ids by the lexicographic comparator.
pub fn sorted_unique_ext_ids(mut ids: Vec<ExtensionId>) -> Vec<ExtensionId> {
    ids.sort_by(cmp_ext_id);
    ids.dedup_by(|a, b| cmp_ext_id(a, b) == Ordering::Equal);
    ids
}

/// A set of extensions that may be "complete" (contain everything).
#[derive(Debug, Clone)]
pub enum MaybeCompleteExtSet {
    Complete,
    Set(Vec<ExtensionId>),
}

impl MaybeCompleteExtSet {
    pub fn contains(&self, id: &ExtensionId) -> bool {
        match self {
            MaybeCompleteExtSet::Complete => true,
            MaybeCompleteExtSet::Set(v) => v.contains(id),
        }
    }
}

/// `BazelModuleInspectorValue` plus the extension usage table of the dep graph value.
#[derive(Debug, Clone, Default)]
pub struct Inspector {
    pub dep_graph: BTreeMap<ModuleKey, AugmentedModule>,
    /// Module name -> all keys mentioned in the un-pruned graph (sorted).
    pub modules_index: BTreeMap<String, Vec<ModuleKey>>,
    /// Module key -> canonical repo name (only for modules in the resolved graph), in resolution order.
    pub key_to_canonical: Vec<(ModuleKey, String)>,
    /// (extension, using module, usage) in dep graph order.
    pub extension_usages: Vec<(ExtensionId, ModuleKey, ExtensionUsage)>,
    /// Extension -> internal names of generated repos (in the order reported).
    pub extension_repos: Vec<(ExtensionId, Vec<String>)>,
    /// Extension -> unique name (used for canonical repo names of generated repos).
    pub extension_unique_names: BTreeMap<ExtensionId, String>,
}

struct Builder {
    name: String,
    version: Version,
    key: ModuleKey,
    repo_name: String,
    dependants: Vec<ModuleKey>,
    original_dependants: Vec<ModuleKey>,
    deps: Vec<(String, ModuleKey)>,
    unused_deps: Vec<(String, ModuleKey)>,
    dep_reasons: Vec<(String, ResolutionReason)>,
    loaded: bool,
}

impl Builder {
    fn new(key: &ModuleKey) -> Builder {
        Builder {
            name: key.name.clone(),
            version: key.version.clone(),
            key: key.clone(),
            repo_name: key.name.clone(),
            dependants: vec![],
            original_dependants: vec![],
            deps: vec![],
            unused_deps: vec![],
            dep_reasons: vec![],
            loaded: false,
        }
    }
    fn build(self) -> AugmentedModule {
        AugmentedModule {
            name: self.name,
            version: self.version,
            key: self.key,
            repo_name: self.repo_name,
            dependants: self.dependants,
            original_dependants: self.original_dependants,
            deps: self.deps,
            unused_deps: self.unused_deps,
            dep_reasons: self.dep_reasons,
            loaded: self.loaded,
        }
    }
}

fn push_unique<T: PartialEq>(v: &mut Vec<T>, t: T) {
    if !v.contains(&t) {
        v.push(t);
    }
}

fn put<V>(v: &mut Vec<(String, V)>, k: &str, val: V) {
    if let Some(e) = v.iter_mut().find(|(n, _)| n == k) {
        e.1 = val;
    } else {
        v.push((k.to_string(), val));
    }
}

impl Inspector {
    /// Builds the inspector from a resolution (`computeAugmentedGraph` + extension tables).
    pub fn from_resolution(res: &Resolution, extension_repos: Vec<(ExtensionId, Vec<String>)>) -> Inspector {
        let used: Vec<&ModuleKey> = res.dep_graph.keys.iter().collect();
        let mut builders: HashMap<ModuleKey, Builder> = HashMap::new();
        for (parent_key, parent) in res.unpruned_dep_graph.iter() {
            {
                let pb = builders.entry(parent_key.clone()).or_insert_with(|| {
                    let mut b = Builder::new(parent_key);
                    b.name = parent.name.clone();
                    b.repo_name = parent.repo_name.clone();
                    b
                });
                pb.version = parent.version.clone();
                pb.loaded = true;
            }
            for (child_dep, key) in &parent.deps {
                let original_key = parent
                    .original_deps
                    .iter()
                    .find(|(n, _)| n == child_dep)
                    .map(|(_, k)| k.clone())
                    .unwrap_or_else(|| key.clone());
                let original_module = res.unpruned_dep_graph.get(&original_key);
                let module = res.unpruned_dep_graph.get(key);

                let ob = builders.entry(original_key.clone()).or_insert_with(|| Builder::new(&original_key));
                if let Some(om) = original_module {
                    ob.name = om.name.clone();
                    ob.version = om.version.clone();
                    ob.repo_name = om.repo_name.clone();
                    ob.loaded = true;
                }
                push_unique(&mut ob.original_dependants, parent_key.clone());

                let nb = builders.entry(key.clone()).or_insert_with(|| {
                    let mut b = Builder::new(key);
                    if let Some(m) = module {
                        b.name = m.name.clone();
                        b.version = m.version.clone();
                        b.repo_name = m.repo_name.clone();
                    }
                    b.loaded = true;
                    b
                });
                if used.contains(&parent_key) {
                    push_unique(&mut nb.dependants, parent_key.clone());
                }

                let reason = if key.version == original_key.version {
                    ResolutionReason::Original
                } else {
                    match res.overrides.get(&key.name) {
                        Some(ModuleOverride::SingleVersion { .. }) => ResolutionReason::SingleVersionOverride,
                        Some(ModuleOverride::MultipleVersion { .. }) => ResolutionReason::MultipleVersionOverride,
                        Some(ModuleOverride::NonRegistry(_)) => ResolutionReason::NonRegistryOverride,
                        None => ResolutionReason::MinimalVersionSelection,
                    }
                };
                let pb = builders.get_mut(parent_key).unwrap();
                if reason != ResolutionReason::Original {
                    put(&mut pb.unused_deps, child_dep, original_key.clone());
                }
                put(&mut pb.deps, child_dep, key.clone());
                put(&mut pb.dep_reasons, child_dep, reason);
            }
        }
        let dep_graph: BTreeMap<ModuleKey, AugmentedModule> =
            builders.into_iter().map(|(k, b)| (k, b.build())).collect();
        Inspector::assemble(
            dep_graph,
            res.canonical_repo_names.iter().map(|(c, k)| (k.clone(), c.clone())).collect(),
            res.extension_usages.clone(),
            extension_repos,
            res.extension_unique_names.clone(),
        )
    }

    /// Assembles an inspector from already-augmented modules (computes the modules index).
    pub fn assemble(
        dep_graph: BTreeMap<ModuleKey, AugmentedModule>,
        key_to_canonical: Vec<(ModuleKey, String)>,
        extension_usages: Vec<(ExtensionId, ModuleKey, ExtensionUsage)>,
        extension_repos: Vec<(ExtensionId, Vec<String>)>,
        extension_unique_names: BTreeMap<ExtensionId, String>,
    ) -> Inspector {
        let mut modules_index: BTreeMap<String, Vec<ModuleKey>> = BTreeMap::new();
        for m in dep_graph.values() {
            modules_index.entry(m.name.clone()).or_default().push(m.key.clone());
        }
        for v in modules_index.values_mut() {
            v.sort();
        }
        Inspector { dep_graph, modules_index, key_to_canonical, extension_usages, extension_repos, extension_unique_names }
    }

    pub fn canonical_name(&self, key: &ModuleKey) -> Option<&str> {
        self.key_to_canonical.iter().find(|(k, _)| k == key).map(|(_, c)| c.as_str())
    }

    pub fn module(&self, key: &ModuleKey) -> Option<&AugmentedModule> {
        self.dep_graph.get(key)
    }

    /// All extension ids in the usage table (insertion order, deduped).
    pub fn extension_ids(&self) -> Vec<ExtensionId> {
        let mut out: Vec<ExtensionId> = vec![];
        for (id, _, _) in &self.extension_usages {
            if !out.contains(id) {
                out.push(id.clone());
            }
        }
        out
    }

    /// Extension -> set of repo names generated by it.
    pub fn repos_of(&self, id: &ExtensionId) -> BTreeSet<String> {
        self.extension_repos
            .iter()
            .filter(|(i, _)| i == id)
            .flat_map(|(_, r)| r.iter().cloned())
            .collect()
    }

    /// Extensions used by module `key`.
    pub fn extensions_used_by(&self, key: &ModuleKey) -> BTreeSet<ExtOrd> {
        self.extension_usages
            .iter()
            .filter(|(_, k, _)| k == key)
            .map(|(id, _, _)| ExtOrd(id.clone()))
            .collect()
    }
}

/// Wrapper giving `ExtensionId` the lexicographic ordering of Java.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtOrd(pub ExtensionId);

impl Ord for ExtOrd {
    fn cmp(&self, o: &Self) -> Ordering {
        cmp_ext_id(&self.0, &o.0)
    }
}
impl PartialOrd for ExtOrd {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
