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

//! Decodes the output of `bazel help flags-as-proto` (a base64 `bazel_flags.FlagCollection`) into protobuf text
//! format on stdout, like `protoc --decode=bazel_flags.FlagCollection bazel_flags.proto`. Used by
//! `scripts/verify_flags_vs_bazel.sh` when `protoc` is not installed. Reads the base64 text from stdin.

use std::io::{Read, Write};

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in input.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            other => return Err(format!("invalid base64 byte {other:#x}")),
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).expect("read stdin");
    let bytes = match base64_decode(&input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("zel-flags-dump: {e}");
            std::process::exit(2);
        }
    };
    match zel_proto::dynamic::Msg::decode("bazel_flags.FlagCollection", &bytes) {
        Ok(m) => {
            // A closed pipe (e.g. `| head`) is not an error.
            let _ = std::io::stdout().write_all(m.to_text().as_bytes());
        }
        Err(e) => {
            eprintln!("zel-flags-dump: cannot decode FlagCollection: {e:?}");
            std::process::exit(1);
        }
    }
}
