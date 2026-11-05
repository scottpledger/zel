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

//! `ResourceConverter` (`--jobs`, `--local_test_jobs`, `--local_resources`), host capacity
//! (`LocalHostCapacity`) and `ResourceManager` (acquisition of CPU / RAM / extra resources and
//! local-test slots with Bazel's "allow one action when resources are unavailable" rule).

use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex};

pub use zel_util::resource_converter::{HostResources, FLAG_SYNTAX};
use zel_util::resource_converter::{self as rc, IntegerSpec};

/// `BuildRequestOptions.MAX_JOBS`.
pub const MAX_JOBS: i64 = rc::MAX_JOBS as i64;

/// `BuildRequestOptions.JobsConverter` (`--jobs`): `auto`/`HOST_CPUS`/`HOST_RAM` keywords with an
/// optional `[-|*]<float>` operation, or an integer; must be at least 1 and is capped at
/// `MAX_JOBS` (Bazel logs a warning). Errors are Bazel's exact converter messages. This is the
/// same converter `zel-options` runs at flag-parse time (the raw flag string is preserved in
/// `OptionValueDescription::raw_values`, and evaluated again here against `host`).
pub fn convert_jobs(input: &str, host: &HostResources) -> Result<usize, String> {
    IntegerSpec::JOBS.convert_with_host(input, host).map(|v| v as usize)
}

/// `ExecutionOptions.LocalTestJobsConverter`: `0..=i32::MAX`; `auto` is 0 (= limited by local
/// resources rather than by a slot count).
pub fn convert_local_test_jobs(input: &str, host: &HostResources) -> Result<usize, String> {
    IntegerSpec::LOCAL_TEST_JOBS.convert_with_host(input, host).map(|v| v as usize)
}

/// `--local_resources=name=value` assignment (`ResourceConverter.AssignmentConverter`).
pub fn convert_assignment(input: &str, host: &HostResources) -> Result<(String, f64), String> {
    rc::convert_resource_assignment(input, &|| *host)
}

/// `ResourceSet`: what an action asks for.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResourceSet {
    pub memory_mb: f64,
    pub cpu: f64,
    pub local_test_count: i32,
    pub extra: BTreeMap<String, f64>,
}

impl ResourceSet {
    /// `AbstractAction.DEFAULT_RESOURCE_SET`: 250 MB, 1 CPU.
    pub fn default_action() -> Self {
        Self { memory_mb: 250.0, cpu: 1.0, ..Default::default() }
    }

    /// `TestTargetProperties`: resources by test size, adjusted by `cpu:N` and
    /// `resources:<name>:<amount>` tags.
    pub fn for_test(size: &str, tags: &[String]) -> Self {
        let (ram, cpu) = match size {
            "small" => (20.0, 1.0),
            "medium" => (100.0, 1.0),
            "large" => (300.0, 1.0),
            "enormous" => (800.0, 1.0),
            _ => (100.0, 1.0),
        };
        let mut rs = ResourceSet { memory_mb: ram, cpu, local_test_count: 1, extra: BTreeMap::new() };
        for t in tags {
            if let Some(n) = t.strip_prefix("cpu:") {
                if let Ok(v) = n.parse::<f64>() {
                    rs.cpu = v;
                }
            } else if let Some(r) = t.strip_prefix("resources:") {
                if let Some((name, amount)) = r.split_once(':') {
                    if let Ok(v) = amount.parse::<f64>() {
                        match name {
                            "cpu" => rs.cpu = v,
                            "memory" => rs.memory_mb = v,
                            other => {
                                rs.extra.insert(other.to_string(), v);
                            }
                        }
                    }
                }
            }
        }
        rs
    }
}

struct Used {
    cpu: f64,
    mem: f64,
    tests: i32,
    extra: BTreeMap<String, f64>,
    active: usize,
}

/// `ResourceManager`.
pub struct ResourceManager {
    pub available_cpu: f64,
    pub available_mem_mb: f64,
    pub available_extra: BTreeMap<String, f64>,
    /// `--local_test_jobs`; 0 = unlimited by count (resources limit instead).
    pub local_test_jobs: usize,
    pub allow_one_when_unavailable: bool,
    pub cpu_load_scheduling: bool,
    pub memory_load_scheduling: bool,
    used: Mutex<Used>,
    cv: Condvar,
}

pub struct ResourceHandle<'a> {
    mgr: &'a ResourceManager,
    rs: ResourceSet,
}

impl Drop for ResourceHandle<'_> {
    fn drop(&mut self) {
        self.mgr.release(&self.rs);
    }
}

impl ResourceManager {
    /// Builds the manager like `ResourceManager.setAvailableResources(getLocalResources())`:
    /// cpu = host CPUs, memory = 0.67 × host RAM, overridden by `--local_resources`.
    pub fn from_options(local_resources: &[String], local_test_jobs: usize, host: &HostResources) -> Result<Self, String> {
        let mut avail: BTreeMap<String, f64> = BTreeMap::new();
        avail.insert("cpu".into(), host.cpus);
        avail.insert("memory".into(), 0.67 * host.memory_mb);
        for a in local_resources {
            let (n, v) = convert_assignment(a, host)?;
            avail.insert(n, v);
        }
        let cpu = avail.remove("cpu").unwrap();
        let mem = avail.remove("memory").unwrap();
        Ok(Self::new(cpu, mem, avail, local_test_jobs))
    }

    pub fn new(cpu: f64, mem: f64, extra: BTreeMap<String, f64>, local_test_jobs: usize) -> Self {
        Self {
            available_cpu: cpu,
            available_mem_mb: mem,
            available_extra: extra,
            local_test_jobs,
            allow_one_when_unavailable: true,
            cpu_load_scheduling: false,
            memory_load_scheduling: false,
            used: Mutex::new(Used { cpu: 0.0, mem: 0.0, tests: 0, extra: BTreeMap::new(), active: 0 }),
            cv: Condvar::new(),
        }
    }

    fn available(&self, u: &Used, rs: &ResourceSet) -> bool {
        // If nothing is running, one action is always allowed (so over-sized actions still run).
        if u.active == 0 && self.allow_one_when_unavailable {
            return true;
        }
        if (self.cpu_load_scheduling || self.memory_load_scheduling) && u.active > 0 {
            if self.cpu_load_scheduling && u.cpu >= self.available_cpu {
                return false;
            }
            if self.memory_load_scheduling && u.mem >= self.available_mem_mb {
                return false;
            }
        }
        let test_ok = rs.local_test_count == 0
            || self.local_test_jobs == 0
            || u.tests + rs.local_test_count <= self.local_test_jobs as i32;
        let extra_ok = rs.extra.iter().all(|(k, v)| {
            let cap = self.available_extra.get(k).copied().unwrap_or(0.0);
            u.extra.get(k).copied().unwrap_or(0.0) + v <= cap
        });
        test_ok && extra_ok && u.cpu + rs.cpu <= self.available_cpu && u.mem + rs.memory_mb <= self.available_mem_mb
    }

    /// Blocks until the resources can be granted.
    pub fn acquire(&self, rs: &ResourceSet) -> ResourceHandle<'_> {
        let mut u = self.used.lock().unwrap();
        while !self.available(&u, rs) {
            u = self.cv.wait(u).unwrap();
        }
        u.cpu += rs.cpu;
        u.mem += rs.memory_mb;
        u.tests += rs.local_test_count;
        for (k, v) in &rs.extra {
            *u.extra.entry(k.clone()).or_insert(0.0) += v;
        }
        u.active += 1;
        ResourceHandle { mgr: self, rs: rs.clone() }
    }

    fn release(&self, rs: &ResourceSet) {
        let mut u = self.used.lock().unwrap();
        u.cpu -= rs.cpu;
        u.mem -= rs.memory_mb;
        u.tests -= rs.local_test_count;
        for (k, v) in &rs.extra {
            if let Some(e) = u.extra.get_mut(k) {
                *e -= v;
            }
        }
        u.active -= 1;
        self.cv.notify_all();
    }

    pub fn in_use(&self) -> (f64, f64, i32) {
        let u = self.used.lock().unwrap();
        (u.cpu, u.mem, u.tests)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn host() -> HostResources {
        HostResources { cpus: 8.0, memory_mb: 16384.0 }
    }

    #[test]
    fn resource_expressions() {
        let h = host();
        assert_eq!(convert_jobs("auto", &h).unwrap(), 8);
        assert_eq!(convert_jobs("HOST_CPUS*.5", &h).unwrap(), 4);
        assert_eq!(convert_jobs("HOST_CPUS-2", &h).unwrap(), 6);
        assert_eq!(convert_jobs("12", &h).unwrap(), 12);
        assert_eq!(convert_jobs("99999", &h).unwrap(), 5000);
        assert_eq!(convert_jobs("HOST_RAM*.5", &h).unwrap(), 5000); // 8192 capped
        assert_eq!(convert_jobs("0", &h).unwrap_err(), "Value '(0)' must be at least 1.");
        assert!(convert_jobs("lots", &h).unwrap_err().starts_with("Parameter 'lots' does not follow correct syntax. This flag takes an integer, or a keyword"));
        assert!(convert_jobs("HOST_CPUS/2", &h).is_err());
        assert_eq!(convert_local_test_jobs("0", &h).unwrap(), 0);
        // `auto` is 0 for --local_test_jobs (not HOST_CPUS).
        assert_eq!(convert_local_test_jobs("auto", &h).unwrap(), 0);
        assert_eq!(convert_local_test_jobs("HOST_CPUS*.5", &h).unwrap(), 4);
        assert_eq!(
            convert_local_test_jobs("-1", &h).unwrap_err(),
            "Value '(-1.000000)' must be at least 0.000000."
        );
    }

    #[test]
    fn local_resources_assignments_override_defaults() {
        let h = host();
        let m = ResourceManager::from_options(&["cpu=2".into(), "memory=HOST_RAM*.5".into(), "gpu=1".into()], 0, &h).unwrap();
        assert_eq!(m.available_cpu, 2.0);
        assert_eq!(m.available_mem_mb, 8192.0);
        assert_eq!(m.available_extra["gpu"], 1.0);
        let d = ResourceManager::from_options(&[], 0, &h).unwrap();
        assert_eq!(d.available_cpu, 8.0);
        assert!((d.available_mem_mb - 0.67 * 16384.0).abs() < 1e-6);
        assert!(convert_assignment("cpu", &h).is_err());
        assert!(convert_assignment("=3", &h).is_err());
    }

    #[test]
    fn manager_limits_concurrency_by_cpu_and_always_admits_one() {
        let m = Arc::new(ResourceManager::new(2.0, 10_000.0, BTreeMap::new(), 0));
        let peak = Arc::new(AtomicUsize::new(0));
        let cur = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|s| {
            for _ in 0..8 {
                let (m, peak, cur) = (m.clone(), peak.clone(), cur.clone());
                s.spawn(move || {
                    let _h = m.acquire(&ResourceSet { cpu: 1.0, memory_mb: 10.0, ..Default::default() });
                    let n = cur.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    cur.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        // An over-sized action still runs when idle.
        let _h = m.acquire(&ResourceSet { cpu: 100.0, memory_mb: 1e9, ..Default::default() });
        assert_eq!(m.in_use().0, 100.0);
    }

    #[test]
    fn extra_resources_and_test_slots() {
        let mut extra = BTreeMap::new();
        extra.insert("gpu".to_string(), 1.0);
        let m = Arc::new(ResourceManager::new(100.0, 1e6, extra, 2));
        let cur = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|s| {
            for _ in 0..5 {
                let (m, cur, peak) = (m.clone(), cur.clone(), peak.clone());
                s.spawn(move || {
                    let rs = ResourceSet::for_test("small", &["resources:gpu:1".to_string()]);
                    let mut rs2 = rs.clone();
                    rs2.extra.clear(); // slots only
                    let _h = m.acquire(&rs2);
                    let n = cur.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    cur.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        assert_eq!(peak.load(Ordering::SeqCst), 2, "local_test_jobs=2 caps tests");
        let gpu_peak = Arc::new(AtomicUsize::new(0));
        let gcur = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|s| {
            for _ in 0..4 {
                let (m, gcur, gpu_peak) = (m.clone(), gcur.clone(), gpu_peak.clone());
                s.spawn(move || {
                    let _h = m.acquire(&ResourceSet::for_test("small", &["resources:gpu:1".to_string()]));
                    let n = gcur.fetch_add(1, Ordering::SeqCst) + 1;
                    gpu_peak.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    gcur.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        assert_eq!(gpu_peak.load(Ordering::SeqCst), 1, "one gpu");
    }

    #[test]
    fn test_resource_tags() {
        let r = ResourceSet::for_test("large", &["cpu:4".into(), "resources:memory:1000".into()]);
        assert_eq!((r.cpu, r.memory_mb, r.local_test_count), (4.0, 1000.0, 1));
        assert_eq!(ResourceSet::for_test("small", &[]).memory_mb, 20.0);
    }
}
