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

//! Pure-Rust port of Bazel's `JarHelper` and `JarCreator`
//! (`com.google.devtools.build.buildjar.jarhelper`).

use crate::zip::{CompressionMethod, DosDateTime, ZipFileEntry, ZipReader, ZipWriter, FLAG_UTF8};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

pub const MANIFEST_DIR: &str = "META-INF/";
pub const MANIFEST_NAME: &str = "META-INF/MANIFEST.MF";
pub const SERVICES_DIR: &str = "META-INF/services/";

/// String wrapper ordered by UTF-16 code units (`java.lang.String::compareTo`), matching
/// Java's `TreeMap<String, ...>` entry order even for characters above `U+FFFF` vs `U+E000..U+FFFF`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Utf16Key(pub String);

impl Utf16Key {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Ord for Utf16Key {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.encode_utf16().cmp(other.0.encode_utf16())
    }
}

impl PartialOrd for Utf16Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Java `String.compareTo` over two Rust `&str` values (UTF-16 code-unit lexicographic order).
pub fn utf16_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Java `String.hashCode()`: `s[0]*31^(n-1) + ... + s[n-1]` over UTF-16 code units with
/// 32-bit two's-complement wrapping arithmetic.
pub fn java_string_hash_code(s: &str) -> i32 {
    let mut h: i32 = 0;
    for unit in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(unit as i32);
    }
    h
}

/// Low-level JAR writer enforcing Bazel's deterministic timestamps, compression rules, UTF-8
/// entry names, and duplicate-entry suppression (`JarHelper.java`).
pub struct JarHelper {
    writer: ZipWriter,
    names: HashSet<String>,
    storage_method: CompressionMethod,
    normalize: bool,
}

impl JarHelper {
    pub fn new() -> Self {
        Self {
            writer: ZipWriter::new(false),
            names: HashSet::new(),
            storage_method: CompressionMethod::Deflated,
            normalize: true,
        }
    }

    pub fn set_normalize(&mut self, normalize: bool) {
        self.normalize = normalize;
    }

    pub fn set_compression(&mut self, compression: bool) {
        self.storage_method = if compression {
            CompressionMethod::Deflated
        } else {
            CompressionMethod::Stored
        };
    }

    pub fn storage_method(&self) -> CompressionMethod {
        self.storage_method
    }

    /// Returns `2010-01-01 00:00:02` for `.class` entries and `2010-01-01 00:00:00` otherwise.
    pub fn normalized_timestamp(name: &str) -> DosDateTime {
        if name.ends_with(".class") {
            DosDateTime::JAR_DEFAULT_CLASS
        } else {
            DosDateTime::JAR_DEFAULT
        }
    }

    fn effective_timestamp(&self, name: &str, fallback: Option<DosDateTime>) -> DosDateTime {
        if self.normalize {
            Self::normalized_timestamp(name)
        } else {
            fallback.unwrap_or_else(DosDateTime::now_utc)
        }
    }

    /// Writes `META-INF/` followed by `META-INF/MANIFEST.MF`. Manifests smaller than 256 bytes
    /// are forced `STORED` (saving compression overhead and allowing mmap-friendly header scans),
    /// and the previous `storage_method` is restored afterward.
    pub fn write_manifest_entry(&mut self, content: &[u8]) -> io::Result<()> {
        self.write_entry(MANIFEST_DIR, &[])?;
        let old_method = self.storage_method;
        if content.len() < 256 {
            self.storage_method = CompressionMethod::Stored;
        }
        let res = self.write_entry(MANIFEST_NAME, content);
        self.storage_method = old_method;
        res
    }

    /// Writes a single entry if its name has not yet been emitted. Empty payloads (including
    /// directory entries ending with `/`) are always `STORED` with CRC 0 and size 0.
    pub fn write_entry(&mut self, name: &str, content: &[u8]) -> io::Result<()> {
        self.write_entry_with_time(name, content, None)
    }

    pub fn write_entry_with_time(
        &mut self,
        name: &str,
        content: &[u8],
        source_time: Option<DosDateTime>,
    ) -> io::Result<()> {
        if !self.names.insert(name.to_string()) {
            return Ok(());
        }
        let mut entry = ZipFileEntry::new(name);
        entry.flags = FLAG_UTF8;
        entry.time = Some(self.effective_timestamp(name, source_time));
        if content.is_empty() {
            entry.method = CompressionMethod::Stored;
            entry.size = 0;
            entry.csize = 0;
            entry.crc = 0;
            self.writer.write_raw_entry(entry, &[])
        } else {
            entry.method = self.storage_method;
            self.writer.write_entry(entry, content)
        }
    }

    pub fn copy_entry(&mut self, reader: &ZipReader, src: &ZipFileEntry) -> io::Result<()> {
        if src.name.ends_with('/') {
            return self.write_entry_with_time(&src.name, &[], src.time);
        }
        let data = reader.read_entry_bytes(src, false)?;
        self.write_entry_with_time(&src.name, &data, src.time)
    }

    pub fn finish(self) -> io::Result<Vec<u8>> {
        self.writer.finish()
    }
}

impl Default for JarHelper {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
enum JarEntrySource {
    Directory,
    File(PathBuf),
    Bytes {
        data: Vec<u8>,
        source_time: Option<DosDateTime>,
    },
}

/// Deterministic JAR builder matching Bazel's `JarCreator.java`.
#[derive(Debug, Clone)]
pub struct JarCreator {
    output_jar: PathBuf,
    jar_entries: BTreeMap<Utf16Key, JarEntrySource>,
    manifest_file: Option<PathBuf>,
    manifest_bytes: Option<Vec<u8>>,
    main_class: Option<String>,
    target_label: Option<String>,
    injecting_rule_kind: Option<String>,
    multi_release: bool,
    compression: bool,
    normalize: bool,
}

impl JarCreator {
    pub fn new(output_jar: impl Into<PathBuf>) -> Self {
        Self {
            output_jar: output_jar.into(),
            jar_entries: BTreeMap::new(),
            manifest_file: None,
            manifest_bytes: None,
            main_class: None,
            target_label: None,
            injecting_rule_kind: None,
            multi_release: false,
            compression: true,
            normalize: true,
        }
    }

    pub fn output_jar(&self) -> &Path {
        &self.output_jar
    }

    pub fn set_compression(&mut self, compression: bool) {
        self.compression = compression;
    }

    pub fn set_normalize(&mut self, normalize: bool) {
        self.normalize = normalize;
    }

    pub fn set_main_class(&mut self, main_class: impl Into<String>) {
        self.main_class = Some(main_class.into());
    }

    pub fn set_manifest_file(&mut self, manifest_file: impl Into<PathBuf>) {
        self.manifest_file = Some(manifest_file.into());
    }

    pub fn set_manifest_bytes(&mut self, bytes: impl Into<Vec<u8>>) {
        self.manifest_bytes = Some(bytes.into());
    }

    pub fn set_jar_owner(
        &mut self,
        target_label: Option<String>,
        injecting_rule_kind: Option<String>,
    ) {
        self.target_label = target_label;
        self.injecting_rule_kind = injecting_rule_kind;
    }

    pub fn set_multi_release(&mut self, multi_release: bool) {
        self.multi_release = multi_release;
    }

    /// Strips a leading `/`, Windows drive prefix (`C:/` or `C:\`), or `./` (`JarCreator.addEntry`).
    pub fn normalize_entry_name(entry_name: &str) -> String {
        let bytes = entry_name.as_bytes();
        if let Some(rest) = entry_name.strip_prefix('/') {
            rest.to_string()
        } else if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'/' || bytes[2] == b'\\')
        {
            entry_name[3..].to_string()
        } else if let Some(rest) = entry_name.strip_prefix("./") {
            rest.to_string()
        } else {
            entry_name.to_string()
        }
    }

    /// Adds a filesystem entry under `entry_name`. Returns `true` if the key was newly added.
    pub fn add_entry(&mut self, entry_name: &str, path: impl Into<PathBuf>) -> bool {
        let normalized = Self::normalize_entry_name(entry_name);
        let p = path.into();
        let src = if normalized.ends_with('/') || p.is_dir() {
            let dir_name = if normalized.ends_with('/') {
                normalized
            } else {
                format!("{normalized}/")
            };
            if dir_name == "/" || dir_name.is_empty() {
                return false;
            }
            return self
                .jar_entries
                .insert(Utf16Key::new(dir_name), JarEntrySource::Directory)
                .is_none();
        } else {
            JarEntrySource::File(p)
        };
        self.jar_entries
            .insert(Utf16Key::new(normalized), src)
            .is_none()
    }

    /// Adds an in-memory byte slice entry under `entry_name`. Returns `true` if newly added.
    pub fn add_bytes_entry(&mut self, entry_name: &str, data: impl Into<Vec<u8>>) -> bool {
        self.add_bytes_entry_with_time(entry_name, data, None)
    }

    pub fn add_bytes_entry_with_time(
        &mut self,
        entry_name: &str,
        data: impl Into<Vec<u8>>,
        source_time: Option<DosDateTime>,
    ) -> bool {
        let normalized = Self::normalize_entry_name(entry_name);
        let src = if normalized.ends_with('/') {
            JarEntrySource::Directory
        } else {
            JarEntrySource::Bytes {
                data: data.into(),
                source_time,
            }
        };
        self.jar_entries
            .insert(Utf16Key::new(normalized), src)
            .is_none()
    }

    /// Recursively walks `directory`, adding intermediate subdirectories (with `/` suffix) and
    /// regular files relative to `directory`.
    pub fn add_directory(&mut self, directory: &Path) -> io::Result<()> {
        if !directory.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("directory '{}' does not exist", directory.display()),
            ));
        }
        self.walk_dir(directory, "")
    }

    fn walk_dir(&mut self, current: &Path, prefix: &str) -> io::Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(current)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name_os = entry.file_name();
            let name = name_os.to_string_lossy();
            let rel = if prefix.is_empty() {
                name.into_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let p = entry.path();
            let meta = std::fs::metadata(&p)?;
            if meta.is_dir() {
                let dir_key = format!("{rel}/");
                self.jar_entries
                    .insert(Utf16Key::new(dir_key), JarEntrySource::Directory);
                self.walk_dir(&p, &rel)?;
            } else {
                self.jar_entries
                    .insert(Utf16Key::new(rel), JarEntrySource::File(p));
            }
        }
        Ok(())
    }

    /// Adds all entries from an existing JAR/ZIP file (`-j`).
    pub fn add_jar(&mut self, jar_path: &Path) -> io::Result<()> {
        let reader = ZipReader::open(jar_path)?;
        for entry in reader.entries() {
            if entry.name.ends_with('/') {
                self.add_bytes_entry_with_time(&entry.name, Vec::new(), entry.time);
            } else {
                let bytes = reader.read_entry_bytes(entry, false)?;
                self.add_bytes_entry_with_time(&entry.name, bytes, entry.time);
            }
        }
        Ok(())
    }

    /// Adds root entries in `jar_path=fs_path` or `fs_path` syntax.
    pub fn add_root_entries<I, S>(&mut self, entries: I) -> io::Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for raw in entries {
            let s = raw.as_ref();
            if let Some((jar_name, fs_path)) = s.split_once('=') {
                if !jar_name.is_empty() {
                    self.add_entry(jar_name, PathBuf::from(fs_path));
                    continue;
                }
            }
            let p = PathBuf::from(s);
            let file_name = p
                .file_name()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("entry '{s}' has no file name"),
                    )
                })?
                .to_string_lossy()
                .into_owned();
            self.add_entry(&file_name, p);
        }
        Ok(())
    }

    /// Formats the `META-INF/MANIFEST.MF` payload per the JAR specification (72-byte UTF-8 line
    /// wrapping with `\r\n ` continuations).
    pub fn manifest_content(&self) -> io::Result<Vec<u8>> {
        let base_bytes = match (&self.manifest_bytes, &self.manifest_file) {
            (Some(b), _) => Some(b.clone()),
            (None, Some(p)) => Some(std::fs::read(p)?),
            (None, None) => None,
        };
        let mut attrs: Vec<(String, String)> = Vec::new();
        if let Some(raw) = base_bytes {
            attrs = parse_manifest_main_attributes(&raw);
        }

        upsert_manifest_attr(&mut attrs, "Manifest-Version", "1.0", true);
        if !has_manifest_attr(&attrs, "Created-By") {
            upsert_manifest_attr(&mut attrs, "Created-By", "bazel", false);
        }
        if let Some(mc) = &self.main_class {
            upsert_manifest_attr(&mut attrs, "Main-Class", mc, false);
        }
        if let Some(tl) = &self.target_label {
            upsert_manifest_attr(&mut attrs, "Target-Label", tl, false);
        }
        if let Some(irk) = &self.injecting_rule_kind {
            upsert_manifest_attr(&mut attrs, "Injecting-Rule-Kind", irk, false);
        }
        if self.multi_release {
            upsert_manifest_attr(&mut attrs, "Multi-Release", "true", false);
        }

        let mut out = Vec::new();
        // Ensure Manifest-Version is always emitted first.
        if let Some(pos) = attrs
            .iter()
            .position(|(k, _)| k.eq_ignore_ascii_case("Manifest-Version"))
        {
            let (k, v) = attrs.remove(pos);
            write_manifest_header_72(&mut out, &k, &v);
        }
        for (k, v) in &attrs {
            write_manifest_header_72(&mut out, k, v);
        }
        out.extend_from_slice(b"\r\n");
        Ok(out)
    }

    /// Builds the JAR in memory and returns its bytes.
    pub fn build_bytes(&self) -> io::Result<Vec<u8>> {
        let manifest = self.manifest_content()?;
        let mut helper = JarHelper::new();
        helper.set_compression(self.compression);
        helper.set_normalize(self.normalize);
        helper.write_manifest_entry(&manifest)?;

        for (key, src) in &self.jar_entries {
            let name = key.as_str();
            if name == MANIFEST_DIR || name == MANIFEST_NAME {
                continue;
            }
            match src {
                JarEntrySource::Directory => {
                    let dir_name = if name.ends_with('/') {
                        name.to_string()
                    } else {
                        format!("{name}/")
                    };
                    helper.write_entry(&dir_name, &[])?;
                }
                JarEntrySource::File(path) => {
                    let mtime = std::fs::metadata(path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| DosDateTime::from_unix_seconds(d.as_secs()));
                    let bytes = std::fs::read(path)?;
                    helper.write_entry_with_time(name, &bytes, mtime)?;
                }
                JarEntrySource::Bytes { data, source_time } => {
                    helper.write_entry_with_time(name, data, *source_time)?;
                }
            }
        }
        helper.finish()
    }

    /// Builds and writes the output JAR to `self.output_jar`.
    pub fn execute(&self) -> io::Result<()> {
        let bytes = self.build_bytes()?;
        if let Some(parent) = self.output_jar.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&self.output_jar, bytes)
    }

    /// Parses `JarCreator.main` command-line arguments (including `@params` files).
    pub fn from_args<I, S>(raw_args: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut expanded = Vec::new();
        for a in raw_args {
            expand_jar_creator_arg(a.as_ref(), &mut expanded, 0)?;
        }
        let mut it = expanded.into_iter();
        let output = it.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "JarCreator requires an output jar argument",
            )
        })?;
        let mut creator = Self::new(output);
        let mut root_entries = Vec::new();
        let mut target_label: Option<String> = None;
        let mut injecting_rule_kind: Option<String> = None;

        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-m" => {
                    let p = it.next().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "missing argument after -m")
                    })?;
                    creator.set_manifest_file(p);
                }
                "-c" => {
                    let mc = it.next().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "missing argument after -c")
                    })?;
                    creator.set_main_class(mc);
                }
                "-t" | "--target_label" => {
                    target_label = Some(it.next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("missing argument after {arg}"),
                        )
                    })?);
                }
                "-i" | "--injecting_rule_kind" => {
                    injecting_rule_kind = Some(it.next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("missing argument after {arg}"),
                        )
                    })?);
                }
                "--multi_release" => creator.set_multi_release(true),
                "--compression" => creator.set_compression(true),
                "--nocompression" => creator.set_compression(false),
                "--normalize" => creator.set_normalize(true),
                "--nonormalize" => creator.set_normalize(false),
                "-d" => {
                    let d = it.next().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "missing argument after -d")
                    })?;
                    creator.add_directory(Path::new(&d))?;
                }
                "-j" => {
                    let j = it.next().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "missing argument after -j")
                    })?;
                    creator.add_jar(Path::new(&j))?;
                }
                other => root_entries.push(other.to_string()),
            }
        }
        creator.set_jar_owner(target_label, injecting_rule_kind);
        creator.add_root_entries(root_entries)?;
        Ok(creator)
    }
}

fn expand_jar_creator_arg(arg: &str, out: &mut Vec<String>, depth: usize) -> io::Result<()> {
    if depth > 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "parameter file recursion limit exceeded",
        ));
    }
    if let Some(escaped) = arg.strip_prefix("@@") {
        out.push(format!("@{escaped}"));
        return Ok(());
    }
    if let Some(path) = arg.strip_prefix('@') {
        let content = std::fs::read_to_string(path)?;
        for line in split_java_lines(&content) {
            if !line.is_empty() {
                expand_jar_creator_arg(line, out, depth + 1)?;
            }
        }
        return Ok(());
    }
    out.push(arg.to_string());
    Ok(())
}

/// Splits lines identically to `java.io.BufferedReader.readLine`: `\r\n`, `\n`, and lone `\r`
/// are all line terminators.
pub fn split_java_lines(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                lines.push(&s[start..i]);
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    i += 2;
                } else {
                    i += 1;
                }
                start = i;
            }
            b'\n' => {
                lines.push(&s[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        lines.push(&s[start..]);
    }
    lines
}

/// Parses the main attributes (`Name: Value`) of a `META-INF/MANIFEST.MF` byte slice.
pub fn parse_manifest_main_attributes(raw: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(raw);
    let mut unfolded: Vec<String> = Vec::new();
    for line in split_java_lines(&text) {
        if line.is_empty() {
            // End of main section.
            break;
        }
        if let Some(cont) = line.strip_prefix(' ') {
            if let Some(prev) = unfolded.last_mut() {
                prev.push_str(cont);
            }
        } else {
            unfolded.push(line.to_string());
        }
    }
    let mut out = Vec::new();
    for line in unfolded {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            let v = v.strip_prefix(' ').unwrap_or(v).to_string();
            if !k.is_empty() {
                upsert_manifest_attr(&mut out, &k, &v, false);
            }
        }
    }
    out
}

fn has_manifest_attr(attrs: &[(String, String)], key: &str) -> bool {
    attrs.iter().any(|(k, _)| k.eq_ignore_ascii_case(key))
}

fn upsert_manifest_attr(
    attrs: &mut Vec<(String, String)>,
    key: &str,
    value: &str,
    prepend_if_new: bool,
) {
    if let Some((_, existing)) = attrs.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
        *existing = value.to_string();
    } else if prepend_if_new {
        attrs.insert(0, (key.to_string(), value.to_string()));
    } else {
        attrs.push((key.to_string(), value.to_string()));
    }
}

/// Writes `Key: Value` wrapped at 72 UTF-8 bytes per line (`java.util.jar.Manifest.make72Safe`).
pub fn write_manifest_header_72(out: &mut Vec<u8>, key: &str, value: &str) {
    let full = format!("{key}: {value}");
    let bytes = full.as_bytes();
    if bytes.len() <= 72 {
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\r\n");
        return;
    }

    let mut pos = 0;
    let mut first = true;
    while pos < bytes.len() {
        let max_chunk = if first { 72 } else { 71 };
        let mut end = (pos + max_chunk).min(bytes.len());
        // Never split in the middle of a multi-byte UTF-8 sequence.
        while end > pos && !full.is_char_boundary(end) {
            end -= 1;
        }
        if !first {
            out.push(b' ');
        }
        out.extend_from_slice(&bytes[pos..end]);
        out.extend_from_slice(b"\r\n");
        pos = end;
        first = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_order_matches_java_string_compare_to() {
        // U+10000 encodes to surrogate pair [0xD800, 0xDC00] < U+E000 [0xE000] in UTF-16,
        // whereas in UTF-8 / Unicode scalar order U+E000 < U+10000.
        let sup = "\u{10000}";
        let pua = "\u{E000}";
        assert!(sup > pua); // Rust scalar order
        assert_eq!(utf16_cmp(sup, pua), Ordering::Less); // Java UTF-16 order!
    }

    #[test]
    fn java_string_hash_code_vectors() {
        assert_eq!(java_string_hash_code(""), 0);
        assert_eq!(java_string_hash_code("a"), 97);
        assert_eq!(java_string_hash_code("abc"), 96354);
        assert_eq!(java_string_hash_code("polygenelubricants"), i32::MIN);
    }

    #[test]
    fn jar_helper_small_manifest_stored_then_restores_deflate_and_normalizes_timestamps() {
        let mut h = JarHelper::new();
        let small_mf = b"Manifest-Version: 1.0\r\nCreated-By: bazel\r\n\r\n";
        assert!(small_mf.len() < 256);
        h.write_manifest_entry(small_mf).unwrap();
        assert_eq!(h.storage_method(), CompressionMethod::Deflated);

        h.write_entry(
            "com/example/Foo.class",
            &[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 61],
        )
        .unwrap();
        h.write_entry("com/example/Empty.txt", b"").unwrap();
        // Duplicate entry must be ignored.
        h.write_entry("com/example/Foo.class", b"ignored").unwrap();

        let jar_bytes = h.finish().unwrap();
        let r = ZipReader::from_bytes(jar_bytes).unwrap();
        let names: Vec<&str> = r.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "META-INF/",
                "META-INF/MANIFEST.MF",
                "com/example/Foo.class",
                "com/example/Empty.txt",
            ]
        );

        let mf_dir = r.get_entry("META-INF/").unwrap();
        assert_eq!(mf_dir.method, CompressionMethod::Stored);
        assert_eq!(mf_dir.crc, 0);
        assert_eq!(mf_dir.time, Some(DosDateTime::JAR_DEFAULT));

        let mf = r.get_entry("META-INF/MANIFEST.MF").unwrap();
        assert_eq!(mf.method, CompressionMethod::Stored);
        assert_eq!(mf.time, Some(DosDateTime::JAR_DEFAULT));

        let cls = r.get_entry("com/example/Foo.class").unwrap();
        assert_eq!(cls.method, CompressionMethod::Deflated);
        assert_eq!(cls.time, Some(DosDateTime::JAR_DEFAULT_CLASS));
        assert_eq!(cls.flags & FLAG_UTF8, FLAG_UTF8);
        assert_eq!(
            r.read_entry_bytes(cls, true).unwrap(),
            vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 61]
        );

        let empty = r.get_entry("com/example/Empty.txt").unwrap();
        assert_eq!(empty.method, CompressionMethod::Stored);
        assert_eq!(empty.crc, 0);
    }

    #[test]
    fn jar_creator_orders_entries_strips_prefixes_and_wraps_manifest_at_72_bytes() {
        let mut jc = JarCreator::new("out.jar");
        let long_label =
            "//very/long/package/path/that/definitely/exceeds/seventy/two/bytes/in/manifest:target_name";
        jc.set_jar_owner(
            Some(long_label.to_string()),
            Some("java_library".to_string()),
        );
        jc.set_main_class("com.example.Main");
        jc.set_multi_release(true);

        assert!(jc.add_bytes_entry("/z/Last.txt", b"z"));
        assert!(jc.add_bytes_entry("./a/First.class", b"classbytes"));
        assert!(jc.add_bytes_entry("C:/win/Mid.txt", b"win"));
        assert!(jc.add_bytes_entry("D:\\bslash/Sub.txt", b"bs"));
        // Explicit META-INF/MANIFEST.MF in entries is skipped because manifest is written first.
        jc.add_bytes_entry("META-INF/MANIFEST.MF", b"ignored");

        let bytes = jc.build_bytes().unwrap();
        let r = ZipReader::from_bytes(bytes).unwrap();
        let names: Vec<&str> = r.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "META-INF/",
                "META-INF/MANIFEST.MF",
                "a/First.class",
                "bslash/Sub.txt",
                "win/Mid.txt",
                "z/Last.txt",
            ]
        );

        let mf_bytes = r
            .read_entry_bytes(r.get_entry("META-INF/MANIFEST.MF").unwrap(), true)
            .unwrap();
        for line in mf_bytes.split(|&b| b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            assert!(
                line.len() <= 72,
                "manifest line exceeds 72 UTF-8 bytes ({}): {:?}",
                line.len(),
                String::from_utf8_lossy(line)
            );
        }
        let parsed: BTreeMap<_, _> = parse_manifest_main_attributes(&mf_bytes)
            .into_iter()
            .collect();
        assert_eq!(parsed["Manifest-Version"], "1.0");
        assert_eq!(parsed["Created-By"], "bazel");
        assert_eq!(parsed["Main-Class"], "com.example.Main");
        assert_eq!(parsed["Target-Label"], long_label);
        assert_eq!(parsed["Injecting-Rule-Kind"], "java_library");
        assert_eq!(parsed["Multi-Release"], "true");
    }

    #[test]
    fn interoperates_with_jdk_jar_tool_when_available() {
        use std::process::Command;
        let tmp = std::env::temp_dir().join(format!("zel-jar-interop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("in/sub")).unwrap();
        std::fs::write(tmp.join("in/sub/Hello.txt"), b"hello from zel jar creator").unwrap();

        let jar_path = tmp.join("out.jar");
        let mut jc = JarCreator::new(&jar_path);
        jc.set_main_class("com.example.App");
        jc.set_jar_owner(Some("//pkg:app".into()), Some("java_binary".into()));
        jc.add_directory(&tmp.join("in")).unwrap();
        jc.execute().unwrap();

        if let Ok(out) = Command::new("jar").arg("tf").arg(&jar_path).output() {
            if out.status.success() {
                let listing = String::from_utf8_lossy(&out.stdout);
                assert!(listing.contains("META-INF/MANIFEST.MF"), "{listing}");
                assert!(listing.contains("sub/"), "{listing}");
                assert!(listing.contains("sub/Hello.txt"), "{listing}");
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
