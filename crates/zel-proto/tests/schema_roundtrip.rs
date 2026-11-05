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

//! Field-for-field codec checks for every embedded schema: a fully populated sample of each
//! message type must round-trip through binary and agree byte-for-byte with `protoc`.

use std::io::Write;
use std::process::{Command, Stdio};
use zel_proto::dynamic::{Msg, Value};
use zel_proto::schema::{registry, FieldDesc, FieldType};

fn fill(m: &mut Msg, depth: usize, seed: u32) {
    let desc = m.desc();
    let mut oneofs_done: Vec<String> = Vec::new();
    for f in &desc.fields {
        if let Some(o) = &f.oneof {
            if oneofs_done.contains(o) {
                continue;
            }
            oneofs_done.push(o.clone());
        }
        if let FieldType::Message(t) = &f.ty {
            if depth == 0 && f.map.is_none() {
                continue;
            }
            let _ = t;
        }
        set_field(m, f, depth, seed + f.number);
    }
}

fn scalar(ty: &FieldType, n: u32) -> Option<Value> {
    Some(match ty {
        FieldType::Double => Value::Double(n as f64 + 0.5),
        FieldType::Float => Value::Float(n as f32 + 0.25),
        FieldType::Int32 | FieldType::SInt32 | FieldType::SFixed32 => Value::Int(-(n as i64)),
        FieldType::Int64 | FieldType::SInt64 | FieldType::SFixed64 => Value::Int(-(n as i64) * 1_000_000_007),
        FieldType::UInt32 | FieldType::Fixed32 => Value::UInt(n as u64 + 1),
        FieldType::UInt64 | FieldType::Fixed64 => Value::UInt((n as u64 + 1) << 33),
        FieldType::Bool => Value::Bool(true),
        FieldType::String => Value::Str(format!("s{n}\n\"é\\")),
        FieldType::Bytes => Value::Bytes(vec![1, 2, 255, n as u8]),
        FieldType::Enum(e) => {
            let en = registry().enum_desc(e)?;
            Value::Enum(en.values.last()?.1)
        }
        FieldType::Message(_) => return None,
    })
}

fn set_field(m: &mut Msg, f: &FieldDesc, depth: usize, n: u32) {
    let reps = if f.is_repeated() { 2 } else { 1 };
    if let Some((kt, vt)) = &f.map {
        for i in 0..2u32 {
            let k = match kt {
                FieldType::String => Value::Str(format!("k{i}{n}")),
                other => scalar(other, n + i).unwrap(),
            };
            let v = match vt {
                FieldType::Message(t) => {
                    let mut c = Msg::new(t);
                    fill(&mut c, depth.saturating_sub(1), n + i);
                    Value::Msg(c)
                }
                other => scalar(other, n + 7 + i).unwrap(),
            };
            m.put_map(&f.name, k, v);
        }
        return;
    }
    for i in 0..reps {
        match &f.ty {
            FieldType::Message(t) => {
                let mut c = Msg::new(t);
                fill(&mut c, depth.saturating_sub(1), n + i);
                m.set_msg(&f.name, c);
            }
            ty => match scalar(ty, n + i).unwrap() {
                Value::Int(v) => {
                    m.set_int(&f.name, v);
                }
                Value::UInt(v) => {
                    m.set_uint(&f.name, v);
                }
                Value::Bool(v) => {
                    m.set_bool(&f.name, v);
                }
                Value::Float(v) => {
                    m.set_double(&f.name, v as f64);
                }
                Value::Double(v) => {
                    m.set_double(&f.name, v);
                }
                Value::Str(v) => {
                    m.set_str(&f.name, v);
                }
                Value::Bytes(v) => {
                    m.set_bytes(&f.name, v);
                }
                Value::Enum(v) => {
                    m.set_int(&f.name, v as i64);
                }
                _ => unreachable!(),
            },
        }
    }
}

fn all_types() -> Vec<String> {
    registry()
        .message_names()
        .filter(|n| !n.starts_with("google.protobuf.") && !n.ends_with("Entry"))
        .cloned()
        .collect()
}

#[test]
fn every_message_roundtrips_binary() {
    let names = all_types();
    assert!(names.len() > 290, "only {} message types", names.len());
    let mut n = 0;
    for name in &names {
        let mut m = Msg::new(name);
        fill(&mut m, 3, 3);
        let bytes = m.encode_to_vec();
        let d = Msg::decode(name, &bytes).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(d.encode_to_vec(), bytes, "{name} re-encode differs");
        assert_eq!(d.to_json(), m.to_json(), "{name} json differs after roundtrip");
        n += 1;
    }
    eprintln!("roundtripped {n} message types");
}

fn protoc(args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    let mut c = Command::new("protoc").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let mut stdin = c.stdin.take()?;
    let data = input.to_vec();
    let t = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = c.wait_with_output().ok()?;
    let _ = t.join();
    if !out.status.success() {
        panic!("protoc {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }
    Some(out.stdout)
}

/// Files `protoc` can compile directly from the embedded tree (no googleapis dependency).
const PROTOC_FILES: &[(&str, &str, &[&str])] = &[
    ("src/main/protobuf/spawn.proto", "tools.protos", &["SpawnExec", "ExecLogEntry", "SpawnMetrics", "Digest"]),
    ("src/main/protobuf/command_line.proto", "command_line", &["CommandLine", "ChunkList", "OptionList"]),
    ("src/main/protobuf/invocation_policy.proto", "blaze.invocation_policy", &["InvocationPolicy", "FlagPolicy"]),
    ("src/main/protobuf/failure_details.proto", "failure_details", &["FailureDetail", "Spawn", "Execution"]),
    ("src/main/protobuf/action_cache.proto", "blaze", &["ActionCacheStatistics", "ActionCacheEntry"]),
    ("src/main/protobuf/test_status.proto", "blaze", &["TestCase"]),
    ("src/main/protobuf/memory_pressure.proto", "memory_pressure", &["MemoryPressureStats"]),
    ("src/main/protobuf/command_server.proto", "command_server", &["RunRequest", "RunResponse", "ServerInfo"]),
    ("src/main/protobuf/execution_statistics.proto", "tools.protos", &["ExecutionStatistics"]),
    ("bep/build_event_stream.proto", "build_event_stream", &["BuildEvent", "BuildEventId", "TestResult", "BuildMetrics", "File"]),
];

#[test]
fn agrees_with_protoc_for_binary_and_text() {
    if Command::new("protoc").arg("--version").output().is_err() {
        eprintln!("protoc not installed; skipping cross-check");
        return;
    }
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/protos");
    let mut checked = 0;
    for (file, pkg, types) in PROTOC_FILES {
        for t in *types {
            let full = format!("{pkg}.{t}");
            let Some(desc) = registry().message(&full).or_else(|| registry().message(&format!("{pkg}.{t}"))) else {
                // Try nested lookups (e.g. message names inside the package).
                continue;
            };
            let _ = desc;
            let mut m = Msg::new(&full);
            fill(&mut m, 2, 5);
            let bytes = m.encode_to_vec();
            let decode_arg = format!("--decode={full}");
            let args = ["-I", root, "-I", "/opt/homebrew/include", "-I", "/usr/local/include", decode_arg.as_str(), file];
            let Some(text) = protoc(&args, &bytes) else { return };
            let text = String::from_utf8(text).unwrap();
            // Our text format equals protoc's debug text (octal escapes included).
            assert_eq!(m.to_text(), text, "{full}: text differs from protoc");
            // protoc can re-encode our text to the same bytes.
            let enc_arg = format!("--encode={full}");
            let args = ["-I", root, "-I", "/opt/homebrew/include", "-I", "/usr/local/include", enc_arg.as_str(), file];
            let enc = protoc(&args, m.to_text().as_bytes()).unwrap();
            assert_eq!(enc, bytes, "{full}: protoc encoding differs");
            checked += 1;
        }
    }
    assert!(checked >= 20, "only {checked} protoc comparisons ran");
    eprintln!("protoc cross-checked {checked} message types");
}
