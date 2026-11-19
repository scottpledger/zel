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

//! Interface JAR (`ijar`) archive processor and CLI matching Bazel's `third_party/ijar/ijar.cc`.

use crate::classfile::strip_class;
use crate::deflate::{crc32, inflate};
use std::path::{Path, PathBuf};

const LOCAL_FILE_HEADER_SIG: u32 = 0x0403_4b50;
const CENTRAL_FILE_HEADER_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
const ZIP64_EOCD_LOCATOR_SIG: u32 = 0x0706_4b50;
const ZIP64_EXTRA_TAG: u16 = 0x0001;

const METHOD_STORED: u16 = 0;
const METHOD_DEFLATED: u16 = 8;

/// Normalized DOS timestamp (`2010-01-01 00:00:00`) matching `third_party/ijar/zip.cc`.
const DEFAULT_DOS_TIMESTAMP: u32 = (30 << 25) | (1 << 21) | (1 << 16);
const UNIX_DIR_ATTR: u32 = (0o040755 << 16) | 0x10;
const UNIX_FILE_ATTR: u32 = 0o100644 << 16;

const MANIFEST_DIR: &str = "META-INF/";
const MANIFEST_PATH: &str = "META-INF/MANIFEST.MF";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IjarOptions {
    pub input_jar: PathBuf,
    pub output_jar: PathBuf,
    pub strip_jar: bool,
    pub verbose: bool,
    pub target_label: Option<String>,
    pub injecting_rule_kind: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JarEntry {
    pub name: String,
    pub external_attr: u32,
    pub data: Vec<u8>,
}

impl JarEntry {
    pub fn is_dir(&self) -> bool {
        (self.external_attr & 0x10) != 0 || self.name.ends_with('/')
    }
}

/// Expands `@paramfile` arguments relative to `base_dir` and parses Bazel `ijar` CLI arguments.
pub fn parse_ijar_args(raw_args: &[String], base_dir: &Path) -> Result<IjarOptions, String> {
    parse_ijar_args_with_reader(raw_args, base_dir, &|p| {
        std::fs::read_to_string(p)
            .map_err(|e| format!("failed to read param file '{}': {e}", p.display()))
    })
}

/// Like [`parse_ijar_args`], reading `@paramfile` contents through `read_to_string`.
pub fn parse_ijar_args_with_reader(
    raw_args: &[String],
    base_dir: &Path,
    read_to_string: &dyn Fn(&Path) -> Result<String, String>,
) -> Result<IjarOptions, String> {
    let mut expanded = Vec::new();
    expand_args(raw_args, base_dir, read_to_string, &mut expanded)?;

    let mut strip_jar = true;
    let mut verbose = false;
    let mut target_label: Option<String> = None;
    let mut injecting_rule_kind: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0usize;
    while i < expanded.len() {
        let a = expanded[i].as_str();
        match a {
            "-v" => {
                verbose = true;
                i += 1;
            }
            "--strip_jar" => {
                strip_jar = true;
                i += 1;
            }
            "--nostrip_jar" => {
                strip_jar = false;
                i += 1;
            }
            "--target_label" => {
                let val = expanded
                    .get(i + 1)
                    .ok_or_else(|| "missing argument after --target_label".to_string())?;
                target_label = Some(val.clone());
                i += 2;
            }
            "--injecting_rule_kind" => {
                let val = expanded
                    .get(i + 1)
                    .ok_or_else(|| "missing argument after --injecting_rule_kind".to_string())?;
                injecting_rule_kind = Some(val.clone());
                i += 2;
            }
            _ if a.starts_with("--target_label=") => {
                target_label = Some(a["--target_label=".len()..].to_string());
                i += 1;
            }
            _ if a.starts_with("--injecting_rule_kind=") => {
                injecting_rule_kind = Some(a["--injecting_rule_kind=".len()..].to_string());
                i += 1;
            }
            _ if a.starts_with('-') => {
                return Err(format!("unknown ijar option: {a}"));
            }
            _ => {
                positional.push(expanded[i].clone());
                i += 1;
            }
        }
    }

    if positional.is_empty() || positional.len() > 2 {
        return Err(
            "Usage: ijar [-v] [--strip_jar|--nostrip_jar] [--target_label label] [--injecting_rule_kind kind] x.jar [x-interface.jar]"
                .to_string(),
        );
    }

    let in_str = &positional[0];
    let out_str = if positional.len() == 2 {
        positional[1].clone()
    } else if let Some(stem) = in_str.strip_suffix(".jar") {
        format!("{stem}-interface.jar")
    } else {
        return Err(format!(
            "missing output jar argument and input '{in_str}' does not end with '.jar'"
        ));
    };

    let resolve = |p: &str| {
        let path = Path::new(p);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            base_dir.join(path)
        }
    };

    Ok(IjarOptions {
        input_jar: resolve(in_str),
        output_jar: resolve(&out_str),
        strip_jar,
        verbose,
        target_label,
        injecting_rule_kind,
    })
}

fn expand_args(
    args: &[String],
    base_dir: &Path,
    read_to_string: &dyn Fn(&Path) -> Result<String, String>,
    out: &mut Vec<String>,
) -> Result<(), String> {
    for a in args {
        if let Some(pf) = a.strip_prefix('@') {
            if pf.starts_with('@') {
                out.push(pf.to_string());
                continue;
            }
            let p = if Path::new(pf).is_absolute() {
                PathBuf::from(pf)
            } else {
                base_dir.join(pf)
            };
            let content = read_to_string(&p)?;
            let mut sub = Vec::new();
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    sub.push(unquote_param_line(trimmed));
                }
            }
            expand_args(&sub, base_dir, read_to_string, out)?;
        } else {
            out.push(a.clone());
        }
    }
    Ok(())
}

fn unquote_param_line(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        s[1..s.len() - 1].replace("'\\''", "'")
    } else {
        s.to_string()
    }
}

/// Runs `ijar` from disk according to `opts`, writing the output JAR atomically to `opts.output_jar`.
pub fn run_ijar(opts: &IjarOptions) -> Result<Vec<u8>, String> {
    let in_bytes = std::fs::read(&opts.input_jar)
        .map_err(|e| format!("Opening zip \"{}\": {e}", opts.input_jar.display()))?;
    let (out_bytes, log) = process_jar_bytes(
        &in_bytes,
        opts.strip_jar,
        opts.target_label.as_deref(),
        opts.injecting_rule_kind.as_deref(),
        opts.verbose,
    )?;
    if let Some(parent) = opts.output_jar.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating parent dir '{}': {e}", parent.display()))?;
        }
    }
    std::fs::write(&opts.output_jar, &out_bytes)
        .map_err(|e| format!("writing output jar '{}': {e}", opts.output_jar.display()))?;
    Ok(log)
}

/// Processes an input `.jar` buffer in memory and returns `(output_jar_bytes, stderr_log)`.
pub fn process_jar_bytes(
    input_bytes: &[u8],
    strip_jar: bool,
    target_label: Option<&str>,
    injecting_rule_kind: Option<&str>,
    verbose: bool,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let entries = read_jar_entries(input_bytes)?;
    let mut out_entries: Vec<JarEntry> = Vec::new();
    let mut log = String::new();

    if strip_jar {
        if let Some(lbl) = target_label {
            out_entries.push(JarEntry {
                name: MANIFEST_DIR.to_string(),
                external_attr: UNIX_DIR_ATTR,
                data: Vec::new(),
            });
            out_entries.push(JarEntry {
                name: MANIFEST_PATH.to_string(),
                external_attr: UNIX_FILE_ATTR,
                data: generate_manifest(lbl, injecting_rule_kind),
            });
        }

        for entry in entries {
            if entry.is_dir() {
                continue;
            }
            let name = entry.name.as_str();
            if name.starts_with("META-INF/TRANSITIVE/") && name.ends_with(".class") {
                continue;
            }
            if is_verbatim_strip_entry(name) {
                if verbose {
                    log.push_str(&format!("copy: {name}\n"));
                }
                out_entries.push(JarEntry {
                    name: entry.name,
                    external_attr: UNIX_FILE_ATTR,
                    data: entry.data,
                });
            } else if name.ends_with(".class") {
                if verbose {
                    log.push_str(&format!("strip: {name}\n"));
                }
                if let Some(stripped) =
                    strip_class(&entry.data).map_err(|e| format!("{name}: {e}"))?
                {
                    out_entries.push(JarEntry {
                        name: entry.name,
                        external_attr: UNIX_FILE_ATTR,
                        data: stripped,
                    });
                }
            }
        }
    } else {
        // `--nostrip_jar`: copy every entry stored with CRC, placing the (rewritten or original)
        // `META-INF/` and `META-INF/MANIFEST.MF` first.
        let existing_manifest = entries
            .iter()
            .find(|e| e.name == MANIFEST_PATH)
            .map(|e| e.data.as_slice());

        let manifest_bytes: Option<Vec<u8>> = match (existing_manifest, target_label) {
            (Some(raw), Some(lbl)) => Some(rewrite_manifest(raw, lbl, injecting_rule_kind)),
            (None, Some(lbl)) => Some(generate_manifest(lbl, injecting_rule_kind)),
            (Some(raw), None) => Some(raw.to_vec()),
            (None, None) => None,
        };

        let has_meta_inf_dir = entries.iter().any(|e| e.name == MANIFEST_DIR);
        if manifest_bytes.is_some() || has_meta_inf_dir {
            out_entries.push(JarEntry {
                name: MANIFEST_DIR.to_string(),
                external_attr: UNIX_DIR_ATTR,
                data: Vec::new(),
            });
        }
        if let Some(mf) = manifest_bytes {
            out_entries.push(JarEntry {
                name: MANIFEST_PATH.to_string(),
                external_attr: UNIX_FILE_ATTR,
                data: mf,
            });
        }

        for entry in entries {
            if entry.name == MANIFEST_DIR || entry.name == MANIFEST_PATH {
                continue;
            }
            if verbose {
                log.push_str(&format!("copy: {}\n", entry.name));
            }
            let is_dir = entry.is_dir();
            let attr = if is_dir {
                UNIX_DIR_ATTR
            } else if entry.external_attr != 0 {
                entry.external_attr
            } else {
                UNIX_FILE_ATTR
            };
            out_entries.push(JarEntry {
                name: entry.name,
                external_attr: attr,
                data: if is_dir { Vec::new() } else { entry.data },
            });
        }
    }

    // `javac` rejects completely empty JAR archives on `-classpath`; emit a 0-byte `dummy` entry.
    if out_entries.is_empty() {
        out_entries.push(JarEntry {
            name: "dummy".to_string(),
            external_attr: UNIX_FILE_ATTR,
            data: Vec::new(),
        });
    }

    let out_bytes = write_stored_jar(&out_entries)?;
    if verbose && !input_bytes.is_empty() {
        let pct = (out_bytes.len() as f64 * 100.0) / (input_bytes.len() as f64);
        log.push_str(&format!(
            "ijar: {} -> {} bytes ({pct:.1}%)\n",
            input_bytes.len(),
            out_bytes.len()
        ));
    }
    Ok((out_bytes, log.into_bytes()))
}

fn is_verbatim_strip_entry(name: &str) -> bool {
    name == "module-info.class"
        || name.ends_with("/module-info.class")
        || (name.starts_with("META-INF/") && name.ends_with(".kotlin_module"))
        || (name.starts_with("kotlin/") && name.ends_with(".kotlin_builtins"))
        || name.ends_with(".tasty")
}

/// Formats a single manifest header (`Name: Value`) wrapped at 72 UTF-8 bytes per line.
pub fn write_manifest_attr(out: &mut Vec<u8>, key: &str, value: &str) {
    let full = format!("{key}: {value}");
    let bytes = full.as_bytes();
    if bytes.len() <= 72 {
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\r\n");
        return;
    }
    let first_end = utf8_floor_boundary(&full, 72);
    out.extend_from_slice(&bytes[..first_end]);
    out.extend_from_slice(b"\r\n");
    let mut rest = &full[first_end..];
    while !rest.is_empty() {
        let n = utf8_floor_boundary(rest, 71);
        out.push(b' ');
        out.extend_from_slice(&rest.as_bytes()[..n]);
        out.extend_from_slice(b"\r\n");
        rest = &rest[n..];
    }
}

fn utf8_floor_boundary(s: &str, max_bytes: usize) -> usize {
    if s.len() <= max_bytes {
        return s.len();
    }
    let mut i = max_bytes;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    if i == 0 {
        s.chars().next().map(char::len_utf8).unwrap_or(1)
    } else {
        i
    }
}

fn generate_manifest(target_label: &str, injecting_rule_kind: Option<&str>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"Manifest-Version: 1.0\r\n");
    out.extend_from_slice(b"Created-By: bazel\r\n");
    write_manifest_attr(&mut out, "Target-Label", target_label);
    if let Some(kind) = injecting_rule_kind {
        write_manifest_attr(&mut out, "Injecting-Rule-Kind", kind);
    }
    out.extend_from_slice(b"\r\n");
    out
}

fn rewrite_manifest(
    existing: &[u8],
    target_label: &str,
    injecting_rule_kind: Option<&str>,
) -> Vec<u8> {
    let text = String::from_utf8_lossy(existing);
    // Split at the first blank line separating the main section from named sections.
    let mut lines: Vec<&str> = Vec::new();
    let mut rest_offset: Option<usize> = None;
    let mut pos = 0usize;
    let bytes = text.as_bytes();
    while pos < bytes.len() {
        let line_start = pos;
        while pos < bytes.len() && bytes[pos] != b'\n' && bytes[pos] != b'\r' {
            pos += 1;
        }
        let line_end = pos;
        if pos < bytes.len() && bytes[pos] == b'\r' {
            pos += 1;
        }
        if pos < bytes.len() && bytes[pos] == b'\n' {
            pos += 1;
        }
        let line = &text[line_start..line_end];
        if line.is_empty() {
            rest_offset = Some(pos);
            break;
        }
        lines.push(line);
    }

    let mut out = Vec::new();
    if lines.is_empty() {
        out.extend_from_slice(b"Manifest-Version: 1.0\r\n");
    } else {
        let mut skip_continuations = false;
        for line in lines {
            if line.starts_with(' ') {
                if !skip_continuations {
                    out.extend_from_slice(line.as_bytes());
                    out.extend_from_slice(b"\r\n");
                }
            } else {
                let lower = line.to_ascii_lowercase();
                skip_continuations =
                    lower.starts_with("target-label:") || lower.starts_with("injecting-rule-kind:");
                if !skip_continuations {
                    out.extend_from_slice(line.as_bytes());
                    out.extend_from_slice(b"\r\n");
                }
            }
        }
    }
    write_manifest_attr(&mut out, "Target-Label", target_label);
    if let Some(kind) = injecting_rule_kind {
        write_manifest_attr(&mut out, "Injecting-Rule-Kind", kind);
    }
    out.extend_from_slice(b"\r\n");
    if let Some(off) = rest_offset {
        if off < bytes.len() {
            out.extend_from_slice(&bytes[off..]);
        }
    }
    out
}

/// Reads all entries from a ZIP/JAR byte slice via its Central Directory.
pub fn read_jar_entries(buf: &[u8]) -> Result<Vec<JarEntry>, String> {
    if buf.len() < 22 {
        return Err("file is too short to be a valid zip archive".to_string());
    }
    let min_eocd = buf.len().saturating_sub(22 + 65535);
    let mut eocd_pos = None;
    let mut p = buf.len() - 22;
    loop {
        if u32_le(&buf[p..]) == EOCD_SIG {
            let comment_len = u16_le(&buf[p + 20..]) as usize;
            if p + 22 + comment_len == buf.len() {
                eocd_pos = Some(p);
                break;
            }
        }
        if p == min_eocd {
            break;
        }
        p -= 1;
    }
    let eocd = eocd_pos.ok_or_else(|| "End of Central Directory record not found".to_string())?;

    let disk_num = u16_le(&buf[eocd + 4..]);
    let cd_disk = u16_le(&buf[eocd + 6..]);
    let mut total_entries = u16_le(&buf[eocd + 10..]) as u64;
    let mut cd_size = u32_le(&buf[eocd + 12..]) as u64;
    let mut cd_offset = u32_le(&buf[eocd + 16..]) as u64;
    if disk_num != 0 && disk_num != 0xFFFF || cd_disk != 0 && cd_disk != 0xFFFF {
        return Err("multi-disk JAR files are not supported".to_string());
    }

    let mut cd_end = eocd;
    if eocd >= 20 && u32_le(&buf[eocd - 20..]) == ZIP64_EOCD_LOCATOR_SIG {
        let z64_off = u64_le(&buf[eocd - 12..]) as usize;
        if z64_off + 56 <= eocd - 20 && u32_le(&buf[z64_off..]) == ZIP64_EOCD_SIG {
            total_entries = u64_le(&buf[z64_off + 32..]);
            cd_size = u64_le(&buf[z64_off + 40..]);
            cd_offset = u64_le(&buf[z64_off + 48..]);
            cd_end = z64_off;
        }
    }

    let cd_size_usize = usize::try_from(cd_size).map_err(|_| "central directory size overflow")?;
    if cd_size_usize > cd_end {
        return Err("central directory size exceeds archive bounds".to_string());
    }
    let cd_actual_start = cd_end - cd_size_usize;
    let base_bias = cd_actual_start.saturating_sub(cd_offset as usize);

    let mut entries = Vec::with_capacity(total_entries.min(65536) as usize);
    let mut cur = cd_actual_start;
    while cur < cd_end {
        if cur + 46 > cd_end {
            return Err("truncated central directory entry".to_string());
        }
        if u32_le(&buf[cur..]) != CENTRAL_FILE_HEADER_SIG {
            return Err("invalid central directory file header signature".to_string());
        }
        let method = u16_le(&buf[cur + 10..]);
        let mut comp_size = u32_le(&buf[cur + 20..]) as u64;
        let mut uncomp_size = u32_le(&buf[cur + 24..]) as u64;
        let name_len = u16_le(&buf[cur + 28..]) as usize;
        let extra_len = u16_le(&buf[cur + 30..]) as usize;
        let comment_len = u16_le(&buf[cur + 32..]) as usize;
        let external_attr = u32_le(&buf[cur + 38..]);
        let mut local_off = u32_le(&buf[cur + 42..]) as u64;

        let var_end = cur
            .checked_add(46 + name_len + extra_len + comment_len)
            .filter(|&e| e <= cd_end)
            .ok_or_else(|| "central directory entry overflows central directory".to_string())?;
        let name_bytes = &buf[cur + 46..cur + 46 + name_len];
        let extra_bytes = &buf[cur + 46 + name_len..cur + 46 + name_len + extra_len];
        parse_zip64_extra(
            extra_bytes,
            &mut uncomp_size,
            &mut comp_size,
            &mut local_off,
        );

        let name = String::from_utf8_lossy(name_bytes).into_owned();
        let lfh = base_bias
            .checked_add(local_off as usize)
            .filter(|&o| o + 30 <= buf.len())
            .ok_or_else(|| format!("invalid local header offset for '{name}'"))?;
        if u32_le(&buf[lfh..]) != LOCAL_FILE_HEADER_SIG {
            return Err(format!("invalid local file header signature for '{name}'"));
        }
        let lfh_name_len = u16_le(&buf[lfh + 26..]) as usize;
        let lfh_extra_len = u16_le(&buf[lfh + 28..]) as usize;
        let data_start = lfh
            .checked_add(30 + lfh_name_len + lfh_extra_len)
            .ok_or_else(|| format!("local header overflow for '{name}'"))?;
        let data_end = data_start
            .checked_add(comp_size as usize)
            .filter(|&e| e <= buf.len())
            .ok_or_else(|| format!("entry payload out of bounds for '{name}'"))?;
        let raw_payload = &buf[data_start..data_end];

        let data = match method {
            METHOD_STORED => raw_payload.to_vec(),
            METHOD_DEFLATED => {
                let (inflated, _) =
                    inflate(raw_payload).map_err(|e| format!("failed to inflate '{name}': {e}"))?;
                if inflated.len() as u64 != uncomp_size {
                    return Err(format!(
                        "inflated size mismatch for '{name}': got {}, expected {uncomp_size}",
                        inflated.len()
                    ));
                }
                inflated
            }
            other => {
                return Err(format!(
                    "unsupported compression method {other} for '{name}'"
                ))
            }
        };

        entries.push(JarEntry {
            name,
            external_attr,
            data,
        });
        cur = var_end;
    }

    Ok(entries)
}

fn parse_zip64_extra(extra: &[u8], uncomp: &mut u64, comp: &mut u64, offset: &mut u64) {
    let mut i = 0usize;
    while i + 4 <= extra.len() {
        let tag = u16_le(&extra[i..]);
        let size = u16_le(&extra[i + 2..]) as usize;
        let end = i + 4 + size;
        if end > extra.len() {
            break;
        }
        if tag == ZIP64_EXTRA_TAG {
            let mut p = i + 4;
            if *uncomp == 0xFFFF_FFFF && p + 8 <= end {
                *uncomp = u64_le(&extra[p..]);
                p += 8;
            }
            if *comp == 0xFFFF_FFFF && p + 8 <= end {
                *comp = u64_le(&extra[p..]);
                p += 8;
            }
            if *offset == 0xFFFF_FFFF && p + 8 <= end {
                *offset = u64_le(&extra[p..]);
            }
        }
        i = end;
    }
}

/// Writes `entries` into a deterministic STORED JAR archive with IEEE CRC-32 checksums.
pub fn write_stored_jar(entries: &[JarEntry]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut cd = Vec::new();

    for e in entries {
        let local_offset = out.len() as u64;
        let name_bytes = e.name.as_bytes();
        if name_bytes.len() > u16::MAX as usize {
            return Err(format!("entry name too long: '{}'", e.name));
        }
        let size =
            u32::try_from(e.data.len()).map_err(|_| format!("entry '{}' exceeds 4 GiB", e.name))?;
        let crc = if e.data.is_empty() { 0 } else { crc32(&e.data) };

        // Local file header (30 bytes + filename).
        put_u32_le(&mut out, LOCAL_FILE_HEADER_SIG);
        put_u16_le(&mut out, 10); // version needed to extract (1.0)
        put_u16_le(&mut out, 0); // general purpose bit flag
        put_u16_le(&mut out, METHOD_STORED);
        put_u32_le(&mut out, DEFAULT_DOS_TIMESTAMP);
        put_u32_le(&mut out, crc);
        put_u32_le(&mut out, size);
        put_u32_le(&mut out, size);
        put_u16_le(&mut out, name_bytes.len() as u16);
        put_u16_le(&mut out, 0); // extra len
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(&e.data);

        // Central directory header (46 bytes + filename).
        let needs_zip64_off = local_offset >= 0xFFFF_FFFF;
        let cd_extra_len: u16 = if needs_zip64_off { 12 } else { 0 };
        put_u32_le(&mut cd, CENTRAL_FILE_HEADER_SIG);
        put_u16_le(&mut cd, 0x030A); // Unix + version 1.0
        put_u16_le(&mut cd, if needs_zip64_off { 45 } else { 10 });
        put_u16_le(&mut cd, 0);
        put_u16_le(&mut cd, METHOD_STORED);
        put_u32_le(&mut cd, DEFAULT_DOS_TIMESTAMP);
        put_u32_le(&mut cd, crc);
        put_u32_le(&mut cd, size);
        put_u32_le(&mut cd, size);
        put_u16_le(&mut cd, name_bytes.len() as u16);
        put_u16_le(&mut cd, cd_extra_len);
        put_u16_le(&mut cd, 0); // comment len
        put_u16_le(&mut cd, 0); // disk number start
        put_u16_le(&mut cd, 0); // internal file attributes
        put_u32_le(&mut cd, e.external_attr);
        put_u32_le(
            &mut cd,
            if needs_zip64_off {
                0xFFFF_FFFF
            } else {
                local_offset as u32
            },
        );
        cd.extend_from_slice(name_bytes);
        if needs_zip64_off {
            put_u16_le(&mut cd, ZIP64_EXTRA_TAG);
            put_u16_le(&mut cd, 8);
            put_u64_le(&mut cd, local_offset);
        }
    }

    let cd_offset = out.len() as u64;
    let cd_size = cd.len() as u64;
    out.extend_from_slice(&cd);

    let count = entries.len() as u64;
    let needs_zip64 = count >= 0xFFFF || cd_offset >= 0xFFFF_FFFF || cd_size >= 0xFFFF_FFFF;
    if needs_zip64 {
        let z64_eocd_offset = out.len() as u64;
        put_u32_le(&mut out, ZIP64_EOCD_SIG);
        put_u64_le(&mut out, 44);
        put_u16_le(&mut out, 45);
        put_u16_le(&mut out, 45);
        put_u32_le(&mut out, 0);
        put_u32_le(&mut out, 0);
        put_u64_le(&mut out, count);
        put_u64_le(&mut out, count);
        put_u64_le(&mut out, cd_size);
        put_u64_le(&mut out, cd_offset);

        put_u32_le(&mut out, ZIP64_EOCD_LOCATOR_SIG);
        put_u32_le(&mut out, 0);
        put_u64_le(&mut out, z64_eocd_offset);
        put_u32_le(&mut out, 1);
    }

    put_u32_le(&mut out, EOCD_SIG);
    put_u16_le(&mut out, 0);
    put_u16_le(&mut out, 0);
    put_u16_le(&mut out, count.min(0xFFFF) as u16);
    put_u16_le(&mut out, count.min(0xFFFF) as u16);
    put_u32_le(&mut out, cd_size.min(0xFFFF_FFFF) as u32);
    put_u32_le(&mut out, cd_offset.min(0xFFFF_FFFF) as u32);
    put_u16_le(&mut out, 0);

    Ok(out)
}

fn u16_le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn u64_le(b: &[u8]) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

fn put_u16_le(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32_le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u64_le(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn cli_parsing_defaults_and_param_files() {
        let dir = std::env::temp_dir().join(format!("zel_ijar_cli_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("args.params"),
            "--target_label\n'//pkg:my_ very_long_target'\n--injecting_rule_kind=java_library\n",
        )
        .unwrap();

        let opts = parse_ijar_args(
            &[
                "foo.jar".to_string(),
                "@args.params".to_string(),
                "-v".to_string(),
            ],
            &dir,
        )
        .unwrap();
        assert_eq!(opts.input_jar, dir.join("foo.jar"));
        assert_eq!(opts.output_jar, dir.join("foo-interface.jar"));
        assert!(opts.strip_jar);
        assert!(opts.verbose);
        assert_eq!(
            opts.target_label.as_deref(),
            Some("//pkg:my_ very_long_target")
        );
        assert_eq!(opts.injecting_rule_kind.as_deref(), Some("java_library"));

        assert!(parse_ijar_args(&["not_a_jar.zip".to_string()], &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_jar_gets_dummy_and_nostrip_stamps_manifest() {
        let src_jar = write_stored_jar(&[JarEntry {
            name: "res/readme.txt".to_string(),
            external_attr: UNIX_FILE_ATTR,
            data: b"hello".to_vec(),
        }])
        .unwrap();

        // Strip mode with no classes and no target_label -> single `dummy` entry.
        let (stripped, _) = process_jar_bytes(&src_jar, true, None, None, false).unwrap();
        let stripped_entries = read_jar_entries(&stripped).unwrap();
        assert_eq!(stripped_entries.len(), 1);
        assert_eq!(stripped_entries[0].name, "dummy");

        // `--nostrip_jar` with target_label + long label line wrapping + existing manifest update.
        let long_label =
            "@@very_long_repository_name_for_manifest_wrapping//some/deeply/nested/package/path:target_name";
        let (stamped1, _) = process_jar_bytes(
            &src_jar,
            false,
            Some(long_label),
            Some("java_import"),
            false,
        )
        .unwrap();
        let e1 = read_jar_entries(&stamped1).unwrap();
        assert_eq!(e1[0].name, "META-INF/");
        assert_eq!(e1[1].name, "META-INF/MANIFEST.MF");
        assert_eq!(e1[2].name, "res/readme.txt");
        let mf1 = String::from_utf8(e1[1].data.clone()).unwrap();
        for line in mf1.split("\r\n") {
            assert!(line.len() <= 72, "manifest line exceeded 72 bytes: {line}");
        }
        assert!(mf1.contains("Created-By: bazel\r\n"));
        assert!(mf1.contains("Injecting-Rule-Kind: java_import\r\n"));

        // Re-stamping replaces previous Target-Label and drops Injecting-Rule-Kind when None.
        let (stamped2, _) =
            process_jar_bytes(&stamped1, false, Some("//new:label"), None, false).unwrap();
        let e2 = read_jar_entries(&stamped2).unwrap();
        let mf2 = String::from_utf8(e2[1].data.clone()).unwrap();
        assert!(mf2.contains("Target-Label: //new:label\r\n"), "{mf2}");
        assert!(!mf2.contains("very_long_repository_name"), "{mf2}");
        assert!(!mf2.contains("Injecting-Rule-Kind"), "{mf2}");
    }

    #[test]
    fn strips_real_javac_classes_and_compiles_dependent_against_ijar() {
        let javac = Path::new("/usr/bin/javac");
        if !javac.exists() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("zel_ijar_javac_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src_dir = dir.join("src");
        let cls_dir = dir.join("classes");
        let dep_dir = dir.join("dep_classes");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&cls_dir).unwrap();
        std::fs::create_dir_all(&dep_dir).unwrap();

        let java_src = r#"
package pkg;
import java.io.IOException;
import java.lang.annotation.*;
import java.util.List;

public class A {
    public enum Kind { ONE, TWO }

    @Retention(RetentionPolicy.RUNTIME)
    public @interface RuntimeAnn {
        String value() default "def";
        Kind kind() default Kind.TWO;
        Class<?> type() default String.class;
        String[] tags() default {"x", "y"};
    }

    public static final long L1 = 123L;
    protected int protectedField = 7;
    private String privateField = "foofoofoofoo";

    static {
        System.out.println("clinit body");
    }

    public A() {
        Runnable anon = new Runnable() {
            @Override public void run() { System.out.println(privateField); }
        };
        anon.run();
    }

    private void privateHelper() {
        System.out.println("secret_body_constant");
    }

    @Deprecated
    @RuntimeAnn(value = "custom")
    public <T extends CharSequence> List<T> genericMethod(@RuntimeAnn String param) throws IOException {
        privateHelper();
        if (param == null) throw new IOException("boom");
        return null;
    }

    private class PrivateInner {}
    public static class PublicInner {}
}
"#;
        let a_java = src_dir.join("A.java");
        std::fs::write(&a_java, java_src).unwrap();
        let st = Command::new(javac)
            .arg("-parameters")
            .arg("-d")
            .arg(&cls_dir)
            .arg(&a_java)
            .status()
            .unwrap();
        assert!(st.success(), "javac failed on A.java");

        // Package classes (DEFLATED via `jar` if available, else stored) + non-class resource +
        // transitive class that must be filtered out.
        let mut raw_entries = Vec::new();
        let mut stack = vec![cls_dir.clone()];
        while let Some(d) = stack.pop() {
            for ent in std::fs::read_dir(&d).unwrap() {
                let p = ent.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p
                        .strip_prefix(&cls_dir)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    raw_entries.push(JarEntry {
                        name: rel,
                        external_attr: UNIX_FILE_ATTR,
                        data: std::fs::read(&p).unwrap(),
                    });
                }
            }
        }
        raw_entries.push(JarEntry {
            name: "META-INF/TRANSITIVE/pkg/SkipMe.class".to_string(),
            external_attr: UNIX_FILE_ATTR,
            data: vec![0xCA, 0xFE, 0xBA, 0xBE],
        });
        raw_entries.push(JarEntry {
            name: "pkg/resource.txt".to_string(),
            external_attr: UNIX_FILE_ATTR,
            data: b"omit me".to_vec(),
        });
        raw_entries.push(JarEntry {
            name: "META-INF/my.kotlin_module".to_string(),
            external_attr: UNIX_FILE_ATTR,
            data: b"ktmod".to_vec(),
        });
        let in_jar = dir.join("in.jar");
        let out_jar = dir.join("out-ijar.jar");
        std::fs::write(&in_jar, write_stored_jar(&raw_entries).unwrap()).unwrap();

        let opts = parse_ijar_args(
            &[
                in_jar.to_string_lossy().into_owned(),
                out_jar.to_string_lossy().into_owned(),
                "--target_label".to_string(),
                "//pkg:a".to_string(),
            ],
            &dir,
        )
        .unwrap();
        run_ijar(&opts).unwrap();

        let out_bytes = std::fs::read(&out_jar).unwrap();
        let out_entries = read_jar_entries(&out_bytes).unwrap();
        let names: Vec<&str> = out_entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"META-INF/MANIFEST.MF"));
        assert!(names.contains(&"META-INF/my.kotlin_module"));
        assert!(names.contains(&"pkg/A.class"));
        assert!(names.contains(&"pkg/A$Kind.class"));
        assert!(names.contains(&"pkg/A$RuntimeAnn.class"));
        assert!(names.contains(&"pkg/A$PrivateInner.class"));
        assert!(names.contains(&"pkg/A$PublicInner.class"));
        assert!(
            !names.contains(&"pkg/A$1.class"),
            "anonymous class with EnclosingMethod must be omitted: {names:?}"
        );
        assert!(!names.contains(&"pkg/resource.txt"));
        assert!(!names.contains(&"META-INF/TRANSITIVE/pkg/SkipMe.class"));

        let stripped_a = &out_entries
            .iter()
            .find(|e| e.name == "pkg/A.class")
            .unwrap()
            .data;
        assert!(
            !stripped_a.windows(12).any(|w| w == b"foofoofoofoo"),
            "private field initializer constant leaked into stripped class"
        );
        assert!(
            !stripped_a.windows(20).any(|w| w == b"secret_body_constant"),
            "method body constant leaked into stripped class"
        );
        assert!(
            !stripped_a.windows(8).any(|w| w == b"<clinit>"),
            "<clinit> must be omitted"
        );

        // Compile a dependent class solely against `out-ijar.jar` using real `javac`.
        let b_java = src_dir.join("B.java");
        std::fs::write(
            &b_java,
            r#"
package pkg;
import java.io.IOException;
import java.util.List;

public class B extends A {
    public long useConst() { return A.L1; }
    @SuppressWarnings("deprecation")
    public List<String> callIt(A.PublicInner p) throws IOException {
        return genericMethod("hi") ;
    }
    public Class<?> annType() { return A.RuntimeAnn.class; }
}
"#,
        )
        .unwrap();
        let st_b = Command::new(javac)
            .arg("-cp")
            .arg(&out_jar)
            .arg("-d")
            .arg(&dep_dir)
            .arg(&b_java)
            .status()
            .unwrap();
        assert!(
            st_b.success(),
            "dependent javac compilation against interface jar failed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
