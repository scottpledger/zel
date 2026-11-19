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

//! Starlark CPU profiler (`StarlarkProfiler` / `--starlark_cpu_profile`) writing
//! gzip-compressed `perftools.profiles.Profile` (`pprof`) data with zero external dependencies.

use crate::eval::thread::StarlarkThread;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct ProfilerState {
    output_path: PathBuf,
    period: Duration,
    last_sample: Instant,
    samples: HashMap<Vec<(String, String, u32)>, u64>,
}

fn global_profiler() -> &'static Mutex<Option<ProfilerState>> {
    static STATE: OnceLock<Mutex<Option<ProfilerState>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

/// Starts global Starlark CPU profiling writing to `output_path` with sampling `period`.
/// Returns `false` if a profile is already active.
pub fn start_cpu_profile(output_path: impl AsRef<Path>, period: Duration) -> bool {
    let mut guard = global_profiler().lock().unwrap();
    if guard.is_some() {
        return false;
    }
    let period = if period.is_zero() {
        Duration::ZERO
    } else {
        period
    };
    let now = Instant::now();
    *guard = Some(ProfilerState {
        output_path: output_path.as_ref().to_path_buf(),
        period,
        last_sample: now.checked_sub(period).unwrap_or(now),
        samples: HashMap::new(),
    });
    true
}

/// Records a sample for `thread` if profiling is active and at least `period` has elapsed.
pub fn record_sample(thread: &StarlarkThread) {
    let mut guard = global_profiler().lock().unwrap();
    let Some(ref mut state) = *guard else {
        return;
    };
    let now = Instant::now();
    if !state.period.is_zero() && now.duration_since(state.last_sample) < state.period {
        return;
    }
    state.last_sample = now;

    // pprof expects leaf-first (innermost frame first, outermost frame last).
    let mut stack = Vec::with_capacity(thread.frames.len() + 1);
    for frame in thread.frames.iter().rev() {
        stack.push((
            frame.function_name.clone(),
            frame.location.file.clone(),
            frame.location.line as u32,
        ));
    }
    if stack.is_empty() {
        stack.push(("<toplevel>".to_string(), String::new(), 0));
    }
    *state.samples.entry(stack).or_insert(0) += 1;
}

/// Stops CPU profiling, writing a gzip-compressed `pprof` protobuf to the configured path.
pub fn stop_cpu_profile() -> std::io::Result<()> {
    let state = {
        let mut guard = global_profiler().lock().unwrap();
        guard.take()
    };
    let Some(state) = state else {
        return Ok(());
    };
    let pprof_bytes = encode_pprof(&state);
    let gz_bytes = gzip_stored(&pprof_bytes);
    if let Some(parent) = state.output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&state.output_path, gz_bytes)
}

fn push_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push(((v as u8) & 0x7f) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

fn push_tag(buf: &mut Vec<u8>, field: u32, wire_type: u8) {
    push_varint(buf, ((field as u64) << 3) | (wire_type as u64));
}

fn push_bytes_field(buf: &mut Vec<u8>, field: u32, data: &[u8]) {
    push_tag(buf, field, 2);
    push_varint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

fn push_varint_field(buf: &mut Vec<u8>, field: u32, val: u64) {
    if val != 0 {
        push_tag(buf, field, 0);
        push_varint(buf, val);
    }
}

fn encode_pprof(state: &ProfilerState) -> Vec<u8> {
    let mut strings: Vec<String> = vec![String::new()];
    let mut str_idx: HashMap<String, u64> = HashMap::from([(String::new(), 0)]);
    let mut intern = |s: &str| -> u64 {
        if let Some(&id) = str_idx.get(s) {
            return id;
        }
        let id = strings.len() as u64;
        strings.push(s.to_string());
        str_idx.insert(s.to_string(), id);
        id
    };

    let cpu_id = intern("cpu");
    let us_id = intern("microseconds");
    let period_us = state.period.as_micros().max(1) as u64;

    let mut functions: Vec<(u64, u64, u64)> = Vec::new();
    let mut fn_map: HashMap<(String, String), u64> = HashMap::new();
    let mut locations: Vec<(u64, u64, u32)> = Vec::new();
    let mut loc_map: HashMap<(u64, u32), u64> = HashMap::new();

    let mut encoded_samples: Vec<Vec<u8>> = Vec::new();
    for (stack, &ticks) in &state.samples {
        let mut loc_ids = Vec::with_capacity(stack.len());
        for (fn_name, file, line) in stack {
            let fn_id = if let Some(&id) = fn_map.get(&(fn_name.clone(), file.clone())) {
                id
            } else {
                let id = (functions.len() + 1) as u64;
                let name_sid = intern(fn_name);
                let file_sid = intern(file);
                functions.push((id, name_sid, file_sid));
                fn_map.insert((fn_name.clone(), file.clone()), id);
                id
            };
            let loc_id = if let Some(&id) = loc_map.get(&(fn_id, *line)) {
                id
            } else {
                let id = (locations.len() + 1) as u64;
                locations.push((id, fn_id, *line));
                loc_map.insert((fn_id, *line), id);
                id
            };
            loc_ids.push(loc_id);
        }

        let mut sample = Vec::new();
        for lid in loc_ids {
            push_tag(&mut sample, 1, 0);
            push_varint(&mut sample, lid);
        }
        push_tag(&mut sample, 2, 0);
        push_varint(&mut sample, ticks.saturating_mul(period_us));
        encoded_samples.push(sample);
    }

    let mut out = Vec::new();
    // sample_type (field 1): ValueType { type = cpu, unit = microseconds }
    let mut vt = Vec::new();
    push_varint_field(&mut vt, 1, cpu_id);
    push_varint_field(&mut vt, 2, us_id);
    push_bytes_field(&mut out, 1, &vt);

    // sample (field 2)
    for s in &encoded_samples {
        push_bytes_field(&mut out, 2, s);
    }

    // location (field 4)
    for &(loc_id, fn_id, line) in &locations {
        let mut line_msg = Vec::new();
        push_varint_field(&mut line_msg, 1, fn_id);
        push_varint_field(&mut line_msg, 2, line as u64);
        let mut loc_msg = Vec::new();
        push_varint_field(&mut loc_msg, 1, loc_id);
        push_bytes_field(&mut loc_msg, 4, &line_msg);
        push_bytes_field(&mut out, 4, &loc_msg);
    }

    // function (field 5)
    for &(fn_id, name_sid, file_sid) in &functions {
        let mut fn_msg = Vec::new();
        push_varint_field(&mut fn_msg, 1, fn_id);
        push_varint_field(&mut fn_msg, 2, name_sid);
        push_varint_field(&mut fn_msg, 3, name_sid);
        push_varint_field(&mut fn_msg, 4, file_sid);
        push_bytes_field(&mut out, 5, &fn_msg);
    }

    // string_table (field 6)
    for s in &strings {
        push_bytes_field(&mut out, 6, s.as_bytes());
    }

    // period_type (field 11) & period (field 12)
    push_bytes_field(&mut out, 11, &vt);
    push_varint_field(&mut out, 12, period_us);
    out
}

fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xedb8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Encodes `data` into a standard RFC 1952 gzip stream using stored DEFLATE blocks.
fn gzip_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 32);
    // Gzip 10-byte header: ID1=0x1f, ID2=0x8b, CM=8 (deflate), FLG=0, MTIME=0, XFL=0, OS=255
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff]);

    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    } else {
        let chunks: Vec<&[u8]> = data.chunks(0xffff).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            let bfinal = u8::from(i + 1 == chunks.len());
            out.push(bfinal);
            let len = chunk.len() as u16;
            let nlen = !len;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&nlen.to_le_bytes());
            out.extend_from_slice(chunk);
        }
    }

    let crc = crc32_ieee(data);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}
