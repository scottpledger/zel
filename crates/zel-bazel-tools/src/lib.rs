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

//! The built-in `@bazel_tools` repository. Content is Bazel's own `//tools` tree (BUILD.tools
//! renamed to BUILD like `embedded_tools`), embedded into the binary from `resources/`.

use std::path::Path;

include!(concat!(env!("OUT_DIR"), "/files.rs"));

/// All embedded files as `(repo-relative path, bytes)`.
pub fn files() -> &'static [(&'static str, &'static [u8])] {
    FILES
}

/// Files of Bazel's Starlark builtins (`@_builtins`, from `src/main/starlark/builtins_bzl`).
pub fn builtins_files() -> &'static [(&'static str, &'static [u8])] {
    BUILTINS
}

/// Fixed past timestamp (2000-01-01 UTC) so materialized embedded trees are never treated as racy.
fn non_racy_mtime() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800)
}

fn write_embedded(dir: &Path, entries: &[(&str, &[u8])]) -> Result<(), String> {
    use std::io::Write;
    let mtime = non_racy_mtime();
    for (rel, bytes) in entries {
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut f = std::fs::File::create(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        f.write_all(bytes)
            .map_err(|e| format!("{}: {e}", dest.display()))?;
        let _ = f.set_modified(mtime);
    }
    Ok(())
}

/// Writes the `@_builtins` tree to `dir`.
pub fn materialize_builtins(dir: &Path) -> Result<(), String> {
    write_embedded(dir, BUILTINS)
}

/// Returns the content of a repo-relative file (e.g. `tools/cpp/unix_cc_toolchain_config.bzl`).
pub fn read(path: &str) -> Option<&'static [u8]> {
    FILES.iter().find(|(p, _)| *p == path).map(|(_, b)| *b)
}

/// Writes the repository to `dir`.
pub fn materialize(dir: &Path) -> Result<(), String> {
    write_embedded(dir, FILES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_real_toolchain_sources() {
        let cfg = read("tools/cpp/unix_cc_toolchain_config.bzl").expect("unix cc config");
        assert!(std::str::from_utf8(cfg).unwrap().contains("rules_cc"));
        assert!(read("tools/cpp/cc_configure.bzl").is_some());
        assert!(read("tools/cpp/lib_cc_configure.bzl").is_some());
        assert!(read("tools/python/python_bootstrap_template.txt").is_some());
        assert!(read("tools/jdk/default_java_toolchain.bzl").is_some());
        assert!(read("tools/build_defs/repo/http.bzl").is_some());
        assert!(read("MODULE.bazel").is_some());
        let b = std::str::from_utf8(read("tools/cpp/BUILD").unwrap()).unwrap();
        assert!(b.contains("toolchain_type(name = \"toolchain_type\")"));
        assert!(read("third_party/ijar/classfile.cc").is_some());
        assert!(read("third_party/ijar/ijar.cc").is_some());
        assert!(read("third_party/grpc/BUILD").is_some());
        assert!(read("third_party/grpc/build_defs.bzl").is_some());
        assert!(read("third_party/grpc/bazel/BUILD").is_some());
        assert!(read("third_party/grpc/bazel/protobuf.bzl").is_some());
        assert!(read("third_party/grpc/bazel/generate_cc.bzl").is_some());
        assert!(read("third_party/grpc/bazel/cc_grpc_library.bzl").is_some());
        assert!(read("third_party/protobuf/BUILD").is_some());
    }

    #[test]
    fn builtins_present() {
        let exports = builtins_files()
            .iter()
            .find(|(p, _)| *p == "exports.bzl")
            .expect("exports.bzl");
        assert!(std::str::from_utf8(exports.1)
            .unwrap()
            .contains("@_builtins//:bazel/exports.bzl"));
        assert!(builtins_files()
            .iter()
            .any(|(p, _)| *p == "common/cc/cc_common_bazel.bzl"));
    }

    #[test]
    fn materializes() {
        let d = std::env::temp_dir().join(format!("zel-bt-{}", std::process::id()));
        materialize(&d).unwrap();
        assert!(d.join("tools/cpp/BUILD").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn crc32_ieee(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg() & 0xEDB8_8320;
                crc = (crc >> 1) ^ mask;
            }
        }
        !crc
    }

    fn build_stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for &(name, data) in entries {
            let offset = out.len() as u32;
            let crc = crc32_ieee(data);
            let len = data.len() as u32;
            let name_bytes = name.as_bytes();
            let name_len = name_bytes.len() as u16;

            out.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&name_len.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name_bytes);
            out.extend_from_slice(data);

            cd.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
            cd.extend_from_slice(&0x0314u16.to_le_bytes());
            cd.extend_from_slice(&20u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&crc.to_le_bytes());
            cd.extend_from_slice(&len.to_le_bytes());
            cd.extend_from_slice(&len.to_le_bytes());
            cd.extend_from_slice(&name_len.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0x81A4_0000u32.to_le_bytes());
            cd.extend_from_slice(&offset.to_le_bytes());
            cd.extend_from_slice(name_bytes);
        }
        let cd_offset = out.len() as u32;
        let cd_size = cd.len() as u32;
        let count = entries.len() as u16;
        out.extend_from_slice(&cd);
        out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// Builds a minimal valid Java 8 `.class` file; when `enclosing_method` is true, attaches an
    /// `EnclosingMethod` class attribute so `StripClass` returns `false` and omits the class.
    fn synthetic_class_bytes(internal_name: &str, enclosing_method: bool) -> Vec<u8> {
        fn push_utf8(cp: &mut Vec<u8>, s: &str) {
            cp.push(1);
            cp.extend_from_slice(&(s.len() as u16).to_be_bytes());
            cp.extend_from_slice(s.as_bytes());
        }
        fn push_class(cp: &mut Vec<u8>, name_idx: u16) {
            cp.push(7);
            cp.extend_from_slice(&name_idx.to_be_bytes());
        }

        let mut cp = Vec::new();
        // 1: this_class utf8, 2: this_class Class, 3: super utf8, 4: super Class,
        // 5: method name "run", 6: method desc "()V", 7: "Code", 8: "EnclosingMethod"
        push_utf8(&mut cp, internal_name);
        push_class(&mut cp, 1);
        push_utf8(&mut cp, "java/lang/Object");
        push_class(&mut cp, 3);
        push_utf8(&mut cp, "run");
        push_utf8(&mut cp, "()V");
        push_utf8(&mut cp, "Code");
        push_utf8(&mut cp, "EnclosingMethod");

        let mut cls = Vec::new();
        cls.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        cls.extend_from_slice(&0u16.to_be_bytes());
        cls.extend_from_slice(&52u16.to_be_bytes());
        cls.extend_from_slice(&9u16.to_be_bytes()); // constant_pool_count = 8 + 1
        cls.extend_from_slice(&cp);
        cls.extend_from_slice(&0x0021u16.to_be_bytes()); // ACC_PUBLIC | ACC_SUPER
        cls.extend_from_slice(&2u16.to_be_bytes()); // this_class
        cls.extend_from_slice(&4u16.to_be_bytes()); // super_class
        cls.extend_from_slice(&0u16.to_be_bytes()); // interfaces_count
        cls.extend_from_slice(&0u16.to_be_bytes()); // fields_count

        // One public method `public void run()` carrying a `Code` attribute (`return`).
        cls.extend_from_slice(&1u16.to_be_bytes()); // methods_count = 1
        cls.extend_from_slice(&0x0001u16.to_be_bytes()); // ACC_PUBLIC
        cls.extend_from_slice(&5u16.to_be_bytes()); // name_index = "run"
        cls.extend_from_slice(&6u16.to_be_bytes()); // descriptor_index = "()V"
        cls.extend_from_slice(&1u16.to_be_bytes()); // method attributes_count = 1
        cls.extend_from_slice(&7u16.to_be_bytes()); // attribute_name_index = "Code"
        let code_attr = [
            0, 1, // max_stack = 1
            0, 1, // max_locals = 1
            0, 0, 0, 1,    // code_length = 1
            0xB1, // return
            0, 0, // exception_table_length = 0
            0, 0, // code attributes_count = 0
        ];
        cls.extend_from_slice(&(code_attr.len() as u32).to_be_bytes());
        cls.extend_from_slice(&code_attr);

        if enclosing_method {
            cls.extend_from_slice(&1u16.to_be_bytes()); // class attributes_count = 1
            cls.extend_from_slice(&8u16.to_be_bytes()); // "EnclosingMethod"
            cls.extend_from_slice(&4u32.to_be_bytes()); // attribute_length = 4
            cls.extend_from_slice(&4u16.to_be_bytes()); // class_index = java/lang/Object
            cls.extend_from_slice(&0u16.to_be_bytes()); // method_index = 0
        } else {
            cls.extend_from_slice(&0u16.to_be_bytes()); // class attributes_count = 0
        }
        cls
    }

    /// Walks both the contiguous local-file-header stream from byte 0 to `cd_offset` and the
    /// Central Directory, asserting every member is finished, CRC-valid, and identical in both.
    fn verify_strict_zip_entries(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
        assert!(zip.len() >= 22, "zip smaller than EOCD: {}", zip.len());
        let eocd = zip.len() - 22;
        assert_eq!(
            u32::from_le_bytes(zip[eocd..eocd + 4].try_into().unwrap()),
            0x0605_4B50
        );
        let cd_count = u16::from_le_bytes(zip[eocd + 10..eocd + 12].try_into().unwrap()) as usize;
        let cd_size = u32::from_le_bytes(zip[eocd + 12..eocd + 16].try_into().unwrap()) as usize;
        let cd_offset = u32::from_le_bytes(zip[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        assert_eq!(
            cd_offset + cd_size,
            eocd,
            "central directory does not end cleanly at EOCD"
        );

        let mut local_entries = Vec::new();
        let mut pos = 0usize;
        while pos < cd_offset {
            assert!(
                pos + 30 <= cd_offset,
                "truncated local file header at offset {pos}"
            );
            let sig = u32::from_le_bytes(zip[pos..pos + 4].try_into().unwrap());
            assert_eq!(
                sig, 0x0403_4B50,
                "unexpected bytes / unfinished local entry at offset {pos}"
            );
            let method = u16::from_le_bytes(zip[pos + 8..pos + 10].try_into().unwrap());
            assert_eq!(method, 0, "ijar output entries must be stored uncompressed");
            let crc = u32::from_le_bytes(zip[pos + 14..pos + 18].try_into().unwrap());
            let comp_len = u32::from_le_bytes(zip[pos + 18..pos + 22].try_into().unwrap()) as usize;
            let uncomp_len =
                u32::from_le_bytes(zip[pos + 22..pos + 26].try_into().unwrap()) as usize;
            assert_eq!(comp_len, uncomp_len);
            let name_len = u16::from_le_bytes(zip[pos + 26..pos + 28].try_into().unwrap()) as usize;
            let extra_len =
                u16::from_le_bytes(zip[pos + 28..pos + 30].try_into().unwrap()) as usize;
            let data_start = pos + 30 + name_len + extra_len;
            let data_end = data_start + comp_len;
            assert!(
                data_end <= cd_offset,
                "local entry overflows central directory"
            );
            let name = std::str::from_utf8(&zip[pos + 30..pos + 30 + name_len])
                .unwrap()
                .to_string();
            let payload = zip[data_start..data_end].to_vec();
            assert_eq!(crc32_ieee(&payload), crc, "CRC mismatch for {name}");
            local_entries.push((pos, name, crc, comp_len, payload));
            pos = data_end;
        }
        assert_eq!(
            pos, cd_offset,
            "local entries did not pack contiguously up to central directory"
        );
        assert_eq!(
            local_entries.len(),
            cd_count,
            "local entry count ({}) != central directory count ({cd_count})",
            local_entries.len()
        );

        let mut cd_pos = cd_offset;
        for (idx, (local_off, local_name, local_crc, local_len, _)) in
            local_entries.iter().enumerate()
        {
            let sig = u32::from_le_bytes(zip[cd_pos..cd_pos + 4].try_into().unwrap());
            assert_eq!(sig, 0x0201_4B50, "bad central directory entry #{idx}");
            let crc = u32::from_le_bytes(zip[cd_pos + 16..cd_pos + 20].try_into().unwrap());
            let comp_len =
                u32::from_le_bytes(zip[cd_pos + 20..cd_pos + 24].try_into().unwrap()) as usize;
            let uncomp_len =
                u32::from_le_bytes(zip[cd_pos + 24..cd_pos + 28].try_into().unwrap()) as usize;
            let name_len =
                u16::from_le_bytes(zip[cd_pos + 28..cd_pos + 30].try_into().unwrap()) as usize;
            let extra_len =
                u16::from_le_bytes(zip[cd_pos + 30..cd_pos + 32].try_into().unwrap()) as usize;
            let comment_len =
                u16::from_le_bytes(zip[cd_pos + 32..cd_pos + 34].try_into().unwrap()) as usize;
            let rel_off =
                u32::from_le_bytes(zip[cd_pos + 42..cd_pos + 46].try_into().unwrap()) as usize;
            let name = std::str::from_utf8(&zip[cd_pos + 46..cd_pos + 46 + name_len]).unwrap();
            assert_eq!(name, local_name);
            assert_eq!(crc, *local_crc);
            assert_eq!(comp_len, *local_len);
            assert_eq!(uncomp_len, *local_len);
            assert_eq!(rel_off, *local_off);
            cd_pos += 46 + name_len + extra_len + comment_len;
        }
        assert_eq!(cd_pos, eocd);

        local_entries
            .into_iter()
            .map(|(_, name, _, _, payload)| (name, payload))
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn cpp_ijar_omits_dropped_classes_without_unfinished_zip_entries() {
        use std::process::Command;

        let dir = std::env::temp_dir().join(format!("zel-bt-ijar-cpp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        materialize(&dir).unwrap();

        let ijar_bin = dir.join("ijar_bin");
        let mut compiled = false;
        let mut last_stderr = String::new();
        for cxx in ["g++", "c++"] {
            let Ok(out) = Command::new(cxx)
                .arg("-std=c++17")
                .arg("-O2")
                .arg("-I")
                .arg(&dir)
                .arg(dir.join("third_party/ijar/ijar.cc"))
                .arg(dir.join("third_party/ijar/classfile.cc"))
                .arg(dir.join("third_party/ijar/zip.cc"))
                .arg(dir.join("third_party/ijar/mapped_file_unix.cc"))
                .arg(dir.join("third_party/ijar/zlib_client.cc"))
                .arg("-lz")
                .arg("-o")
                .arg(&ijar_bin)
                .output()
            else {
                continue;
            };
            if out.status.success() {
                compiled = true;
                break;
            }
            last_stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        }
        if !compiled {
            let _ = std::fs::remove_dir_all(&dir);
            if last_stderr.is_empty()
                || last_stderr.contains("'memory' file not found")
                || last_stderr.contains("memory: No such file")
                || last_stderr.contains("zlib.h")
                || last_stderr.contains("-lz")
            {
                return;
            }
            panic!("failed to compile embedded third_party/ijar C++ sources:\n{last_stderr}");
        }

        let dropped_before = synthetic_class_bytes("pkg/Outer$1", true);
        let kept_public = synthetic_class_bytes("pkg/KeptPublic", false);
        let dropped_after = synthetic_class_bytes("pkg/Outer$2", true);
        let in_jar = dir.join("input.jar");
        let out_jar = dir.join("output-ijar.jar");
        let in_bytes = build_stored_zip(&[
            ("pkg/Outer$1.class", &dropped_before),
            ("pkg/KeptPublic.class", &kept_public),
            ("pkg/Outer$2.class", &dropped_after),
        ]);
        std::fs::write(&in_jar, in_bytes).unwrap();

        let run_out = Command::new(&ijar_bin)
            .arg(&in_jar)
            .arg(&out_jar)
            .output()
            .expect("run compiled C++ ijar");
        assert!(
            run_out.status.success(),
            "C++ ijar failed: {}",
            String::from_utf8_lossy(&run_out.stderr)
        );

        let out_bytes = std::fs::read(&out_jar).unwrap();
        let entries = verify_strict_zip_entries(&out_bytes);
        assert_eq!(
            entries.len(),
            1,
            "expected only kept entry, got: {:?}",
            entries.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
        assert_eq!(entries[0].0, "pkg/KeptPublic.class");
        assert_eq!(&entries[0].1[..4], &[0xCA, 0xFE, 0xBA, 0xBE]);
        assert!(
            !entries[0].1.windows(4).any(|w| w == b"Code"),
            "stripped class must omit Code attribute"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
