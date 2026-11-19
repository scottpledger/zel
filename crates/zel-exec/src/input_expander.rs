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

//! Port of `SpawnInputExpander` and `AbstractSpawnStrategy.SpawnExecutionContextImpl`:
//! hierarchical `TreeMap` input mapping with memoization when `will_access_repeatedly` is true.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use zel_sandbox::{ExpandedInput, HierarchicalPath, Spawn};
use zel_vfs::PathFragment;

/// `SpawnInputExpander`: builds the `HIERARCHICAL_COMPARATOR`-ordered input mapping for a `Spawn`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SpawnInputExpander;

impl SpawnInputExpander {
    pub fn get_input_mapping(
        &self,
        spawn: &Spawn,
        base_directory: &PathFragment,
    ) -> Result<BTreeMap<HierarchicalPath, ExpandedInput>, String> {
        spawn.build_input_mapping(base_directory)
    }
}

/// `AbstractSpawnStrategy.SpawnExecutionContextImpl` input mapping cache:
/// memoizes the expanded input map ONLY when `will_access_repeatedly` is true, returning the
/// exact same `Arc` instance when `base_directory` matches.
#[allow(clippy::type_complexity)]
pub struct SpawnInputMappingCache {
    expander: SpawnInputExpander,
    memo: Mutex<Option<(PathFragment, Arc<BTreeMap<HierarchicalPath, ExpandedInput>>)>>,
}

impl Default for SpawnInputMappingCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SpawnInputMappingCache {
    pub fn new() -> Self {
        Self {
            expander: SpawnInputExpander,
            memo: Mutex::new(None),
        }
    }

    pub fn get_input_mapping(
        &self,
        spawn: &Spawn,
        base_directory: &PathFragment,
        will_access_repeatedly: bool,
    ) -> Result<Arc<BTreeMap<HierarchicalPath, ExpandedInput>>, String> {
        {
            let guard = self.memo.lock().unwrap();
            if let Some((cached_base, cached_map)) = &*guard {
                if cached_base == base_directory {
                    return Ok(cached_map.clone());
                }
            }
        }
        let map = Arc::new(self.expander.get_input_mapping(spawn, base_directory)?);
        if will_access_repeatedly {
            let mut guard = self.memo.lock().unwrap();
            *guard = Some((base_directory.clone(), map.clone()));
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_sandbox::{PathMapper, RunfilesTarget, RunfilesTreeData};

    #[test]
    fn hierarchical_order_expansion_and_memoization() {
        let mut s = Spawn {
            inputs: vec![
                PathFragment::create("a-b"),
                PathFragment::create("a/b"),
                PathFragment::create("tree"),
                PathFragment::create("empty_tree"),
            ],
            ..Default::default()
        };
        s.tree_inputs.insert(
            PathFragment::create("tree"),
            vec![PathFragment::create("child.txt")],
        );
        s.tree_inputs
            .insert(PathFragment::create("empty_tree"), vec![]);
        let mut rf_map = BTreeMap::new();
        rf_map.insert(PathFragment::create("_main/empty"), None);
        rf_map.insert(
            PathFragment::create("_main/in_rf"),
            Some(RunfilesTarget::File {
                exec_path: PathFragment::create("pkg/real.txt"),
                is_symlink: false,
            }),
        );
        s.runfiles_trees.push(RunfilesTreeData {
            exec_path: PathFragment::create("app.runfiles"),
            workspace_name: "_main".into(),
            mapping: rf_map,
            ..Default::default()
        });
        s.path_mapper = PathMapper::Prefix {
            from: PathFragment::create("tree"),
            to: PathFragment::create("mapped_tree"),
        };

        let cache = SpawnInputMappingCache::new();
        let base = PathFragment::create("base");
        // will_access_repeatedly=false does not memoize.
        let m1 = cache.get_input_mapping(&s, &base, false).unwrap();
        let m2 = cache.get_input_mapping(&s, &base, false).unwrap();
        assert!(!Arc::ptr_eq(&m1, &m2));

        // will_access_repeatedly=true memoizes and returns the same instance.
        let m3 = cache.get_input_mapping(&s, &base, true).unwrap();
        let m4 = cache.get_input_mapping(&s, &base, true).unwrap();
        assert!(Arc::ptr_eq(&m3, &m4));

        // Hierarchical comparator puts base/a/b before base/a-b.
        let keys: Vec<String> = m3.keys().map(|k| k.0.path_string().to_string()).collect();
        let pos_slash = keys.iter().position(|k| k == "base/a/b").unwrap();
        let pos_dash = keys.iter().position(|k| k == "base/a-b").unwrap();
        assert!(pos_slash < pos_dash, "{keys:?}");

        // Non-empty tree replaced by mapped child; empty tree kept; runfiles expanded without MANIFEST.
        assert!(!keys.contains(&"base/tree".to_string()));
        assert!(keys.contains(&"base/mapped_tree/child.txt".to_string()));
        assert!(keys.contains(&"base/empty_tree".to_string()));
        assert_eq!(
            m3.get(&HierarchicalPath::from("base/app.runfiles/_main/empty")),
            Some(&ExpandedInput::Empty)
        );
        assert!(!keys.iter().any(|k| k.ends_with("MANIFEST")));
    }
}
