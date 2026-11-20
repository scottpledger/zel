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

//! Pure-Rust port of Bazel's `com.google.devtools.build.singlejar` (`CustomMergeStrategy`,
//! `ConcatenateStrategy`, `ZipEntryFilter`, `CopyEntryFilter`, `DefaultJarEntryFilter`,
//! and `ZipCombiner`).

use crate::deflate::{crc32, deflate, inflate};
use crate::zip::{CompressionMethod, DosDateTime, ZipFileEntry, ZipReader, ZipWriter};
use std::collections::{HashMap, HashSet};
use std::io;

/// Strategy for combining multiple archive entries sharing the same filename
/// (`ZipEntryFilter.CustomMergeStrategy`).
pub trait CustomMergeStrategy {
    fn merge(&mut self, input: &[u8], out: &mut Vec<u8>) -> io::Result<()>;

    fn finish(&mut self, _out: &mut Vec<u8>) -> io::Result<()> {
        Ok(())
    }

    fn skip_empty(&self) -> bool {
        false
    }
}

/// Concatenates duplicate entries in encounter order, optionally inserting a single `\n` (`0x0A`)
/// between streams when the preceding non-empty stream did not end with `\n`
/// (`ConcatenateStrategy.java`).
#[derive(Debug, Clone)]
pub struct ConcatenateStrategy {
    append_new_line: bool,
    last_byte_copied: u8,
}

impl ConcatenateStrategy {
    pub fn new(append_new_line: bool) -> Self {
        Self {
            append_new_line,
            last_byte_copied: b'\n',
        }
    }
}

impl Default for ConcatenateStrategy {
    fn default() -> Self {
        Self::new(true)
    }
}

impl CustomMergeStrategy for ConcatenateStrategy {
    fn merge(&mut self, input: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
        if input.is_empty() {
            return Ok(());
        }
        if self.append_new_line && self.last_byte_copied != b'\n' {
            out.push(b'\n');
        }
        out.extend_from_slice(input);
        self.last_byte_copied = *input.last().unwrap();
        Ok(())
    }
}

enum CallbackChoice {
    Skip,
    Copy(Option<DosDateTime>),
    Rename {
        filename: String,
        date: Option<DosDateTime>,
    },
    CustomMerge {
        date: Option<DosDateTime>,
        strategy: Box<dyn CustomMergeStrategy>,
    },
}

/// Callback passed to [`ZipEntryFilter::accept`]. Enforces that the filter selects at most one
/// strategy per invocation (`ZipEntryFilter.StrategyCallback`).
#[derive(Default)]
pub struct StrategyCallback {
    choice: Option<CallbackChoice>,
}

impl StrategyCallback {
    pub fn new() -> Self {
        Self { choice: None }
    }

    fn set(&mut self, next: CallbackChoice) -> io::Result<()> {
        if self.choice.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "StrategyCallback invoked more than once for the same entry",
            ));
        }
        self.choice = Some(next);
        Ok(())
    }

    pub fn skip(&mut self) -> io::Result<()> {
        self.set(CallbackChoice::Skip)
    }

    pub fn copy(&mut self, date: Option<DosDateTime>) -> io::Result<()> {
        self.set(CallbackChoice::Copy(date))
    }

    pub fn rename(
        &mut self,
        filename: impl Into<String>,
        date: Option<DosDateTime>,
    ) -> io::Result<()> {
        self.set(CallbackChoice::Rename {
            filename: filename.into(),
            date,
        })
    }

    pub fn custom_merge(
        &mut self,
        date: Option<DosDateTime>,
        strategy: Box<dyn CustomMergeStrategy>,
    ) -> io::Result<()> {
        self.set(CallbackChoice::CustomMerge { date, strategy })
    }
}

/// Policy interface choosing how [`ZipCombiner`] handles entries of each filename
/// (`ZipEntryFilter.java`).
pub trait ZipEntryFilter {
    fn accept(&self, filename: &str, callback: &mut StrategyCallback) -> io::Result<()>;
}

/// Filter that requests a plain timestamp-preserving copy of every entry (`CopyEntryFilter.java`).
#[derive(Debug, Clone, Copy, Default)]
pub struct CopyEntryFilter;

impl ZipEntryFilter for CopyEntryFilter {
    fn accept(&self, _filename: &str, callback: &mut StrategyCallback) -> io::Result<()> {
        callback.copy(None)
    }
}

type PathPredicate = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// Standard Bazel singlejar entry filter (`DefaultJarEntryFilter.java`).
pub struct DefaultJarEntryFilter {
    date: Option<DosDateTime>,
    class_date: Option<DosDateTime>,
    allowed_paths: Option<PathPredicate>,
}

impl DefaultJarEntryFilter {
    pub fn new() -> Self {
        Self::with_normalize(true)
    }

    pub fn with_normalize(normalize: bool) -> Self {
        let (date, class_date) = if normalize {
            (
                Some(DosDateTime::DOS_EPOCH),
                Some(DosDateTime::DOS_EPOCH_PLUS_2_SECONDS),
            )
        } else {
            (None, None)
        };
        Self {
            date,
            class_date,
            allowed_paths: None,
        }
    }

    pub fn with_allowed_paths<F>(normalize: bool, allowed: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        let mut f = Self::with_normalize(normalize);
        f.allowed_paths = Some(Box::new(allowed));
        f
    }

    fn is_allowed(&self, filename: &str) -> bool {
        self.allowed_paths
            .as_ref()
            .is_none_or(|pred| pred(filename))
    }
}

impl Default for DefaultJarEntryFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl ZipEntryFilter for DefaultJarEntryFilter {
    fn accept(&self, filename: &str, callback: &mut StrategyCallback) -> io::Result<()> {
        if !self.is_allowed(filename) {
            callback.skip()
        } else if filename == "META-INF/spring.handlers"
            || filename == "META-INF/spring.schemas"
            || filename == "reference.conf"
            || filename.starts_with("META-INF/services/")
        {
            callback.custom_merge(self.date, Box::new(ConcatenateStrategy::new(true)))
        } else if filename == "META-INF/MANIFEST.MF"
            || filename.ends_with(".SF")
            || filename.ends_with(".DSA")
            || filename.ends_with(".RSA")
        {
            callback.skip()
        } else if filename.ends_with(".class") {
            callback.copy(self.class_date)
        } else if filename == "protobuf.meta" {
            callback.custom_merge(self.date, Box::new(ConcatenateStrategy::new(false)))
        } else {
            callback.copy(self.date)
        }
    }
}

/// Compression mode for [`ZipCombiner`] (`ZipCombiner.OutputMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputMode {
    #[default]
    DontCare,
    ForceDeflate,
    ForceStored,
}

struct MergeState {
    date: Option<DosDateTime>,
    strategy: Box<dyn CustomMergeStrategy>,
    buffer: Vec<u8>,
}

enum ActionState {
    Skip,
    Copy(Option<DosDateTime>),
    Rename {
        filename: String,
        date: Option<DosDateTime>,
    },
    Merge(usize),
}

/// Combines multiple ZIP/JAR archives, directories, and standalone files into a single 32-bit ZIP
/// archive (`ZipCombiner.java`).
pub struct ZipCombiner<F: ZipEntryFilter = DefaultJarEntryFilter> {
    mode: OutputMode,
    filter: F,
    writer: ZipWriter,
    entries: HashSet<String>,
    actions: HashMap<String, ActionState>,
    merges: Vec<(String, MergeState)>,
}

impl ZipCombiner<DefaultJarEntryFilter> {
    pub fn new(mode: OutputMode) -> Self {
        Self::with_filter(mode, DefaultJarEntryFilter::new())
    }
}

impl<F: ZipEntryFilter> ZipCombiner<F> {
    pub fn with_filter(mode: OutputMode, filter: F) -> Self {
        Self {
            mode,
            filter,
            writer: ZipWriter::new(false),
            entries: HashSet::new(),
            actions: HashMap::new(),
            merges: Vec::new(),
        }
    }

    pub fn contains_entry(&self, name: &str) -> bool {
        self.entries.contains(name)
    }

    /// Writes a `STORED` directory entry (`CRC = 0`, `size = 0`). Requires a trailing `/` and
    /// fails if `dir_name` is already present in the output (`IllegalStateException` in Bazel).
    pub fn add_directory(
        &mut self,
        dir_name: &str,
        date: Option<DosDateTime>,
        extra: Vec<u8>,
    ) -> io::Result<()> {
        if !dir_name.ends_with('/') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Directory name '{dir_name}' must end with '/'"),
            ));
        }
        if self.entries.contains(dir_name) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("Duplicate directory entry '{dir_name}'"),
            ));
        }
        self.entries.insert(dir_name.to_string());
        let mut entry = ZipFileEntry::new(dir_name);
        entry.time = Some(date.unwrap_or_else(DosDateTime::now_utc));
        entry.method = CompressionMethod::Stored;
        entry.crc = 0;
        entry.size = 0;
        entry.csize = 0;
        entry.flags = 0;
        entry.extra = extra;
        self.writer.write_raw_entry(entry, &[])
    }

    /// Buffers `content` and writes a new file entry (deflated unless `OutputMode::ForceStored`).
    /// Fails if `filename` is already present in the output (`IllegalArgumentException` in Bazel).
    pub fn add_file(
        &mut self,
        filename: &str,
        date: Option<DosDateTime>,
        content: &[u8],
    ) -> io::Result<()> {
        if self.entries.contains(filename) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Duplicate file entry '{filename}'"),
            ));
        }
        self.entries.insert(filename.to_string());
        self.write_buffered_entry(filename, date.unwrap_or_else(DosDateTime::now_utc), content)
    }

    fn write_buffered_entry(
        &mut self,
        filename: &str,
        time: DosDateTime,
        content: &[u8],
    ) -> io::Result<()> {
        let mut entry = ZipFileEntry::new(filename);
        entry.time = Some(time);
        entry.flags = 0;
        entry.size = content.len() as u64;
        entry.crc = crc32(content);
        if self.mode == OutputMode::ForceStored {
            entry.method = CompressionMethod::Stored;
            self.writer.write_raw_entry(entry, content)
        } else {
            entry.method = CompressionMethod::Deflated;
            let compressed = deflate(content);
            self.writer.write_raw_entry(entry, &compressed)
        }
    }

    /// Streams all entries from `reader` through the configured [`ZipEntryFilter`].
    pub fn add_zip(&mut self, reader: &ZipReader) -> io::Result<()> {
        for src in reader.entries() {
            let name = &src.name;
            let needs_filter = matches!(
                self.actions.get(name),
                None | Some(ActionState::Rename { .. })
            );
            if needs_filter {
                let mut cb = StrategyCallback::new();
                self.filter.accept(name, &mut cb)?;
                let choice = cb.choice.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("ZipEntryFilter made no decision for '{name}'"),
                    )
                })?;
                match choice {
                    CallbackChoice::Skip => {
                        self.actions.insert(name.clone(), ActionState::Skip);
                    }
                    CallbackChoice::Copy(date) => {
                        if self.entries.contains(name) {
                            self.actions.insert(name.clone(), ActionState::Skip);
                        } else {
                            self.actions.insert(name.clone(), ActionState::Copy(date));
                        }
                    }
                    CallbackChoice::Rename { filename, date } => {
                        if self.entries.contains(&filename) {
                            // Skip this sighting without storing SKIP, matching ZipCombiner.getAction.
                            continue;
                        }
                        self.actions
                            .insert(name.clone(), ActionState::Rename { filename, date });
                    }
                    CallbackChoice::CustomMerge { date, strategy } => {
                        let idx = self.merges.len();
                        self.merges.push((
                            name.clone(),
                            MergeState {
                                date,
                                strategy,
                                buffer: Vec::new(),
                            },
                        ));
                        self.actions.insert(name.clone(), ActionState::Merge(idx));
                    }
                }
            }

            // Re-evaluate COPY if a later `add_file` / `add_directory` / `RENAME` claimed `name`,
            // or after the first copy already inserted `name` into `self.entries`.
            enum Step {
                Skip,
                Copy {
                    out_name: String,
                    date: Option<DosDateTime>,
                },
                Merge(usize),
            }

            let step = match self.actions.get(name) {
                None | Some(ActionState::Skip) => Step::Skip,
                Some(ActionState::Copy(date)) => {
                    if self.entries.contains(name) {
                        let d = *date;
                        let _ = d;
                        self.actions.insert(name.clone(), ActionState::Skip);
                        Step::Skip
                    } else {
                        Step::Copy {
                            out_name: name.clone(),
                            date: *date,
                        }
                    }
                }
                Some(ActionState::Rename { filename, date }) => {
                    if self.entries.contains(filename) {
                        Step::Skip
                    } else {
                        Step::Copy {
                            out_name: filename.clone(),
                            date: *date,
                        }
                    }
                }
                Some(ActionState::Merge(idx)) => Step::Merge(*idx),
            };

            match step {
                Step::Skip => {}
                Step::Copy { out_name, date } => {
                    self.entries.insert(out_name.clone());
                    self.copy_zip_entry(reader, src, &out_name, date)?;
                }
                Step::Merge(idx) => {
                    let data = reader.read_entry_bytes(src, false)?;
                    let (_, m) = &mut self.merges[idx];
                    m.strategy.merge(&data, &mut m.buffer)?;
                }
            }
        }
        Ok(())
    }

    fn copy_zip_entry(
        &mut self,
        reader: &ZipReader,
        src: &ZipFileEntry,
        out_name: &str,
        override_date: Option<DosDateTime>,
    ) -> io::Result<()> {
        let mut dst = src.clone();
        dst.name = out_name.to_string();
        if let Some(d) = override_date {
            dst.time = Some(d);
        } else if dst.time.is_none() {
            dst.time = Some(DosDateTime::now_utc());
        }

        let raw = reader.raw_entry_bytes(src)?;
        match (self.mode, src.method) {
            (OutputMode::DontCare, CompressionMethod::Stored | CompressionMethod::Deflated) => {
                self.writer.write_raw_entry(dst, raw)
            }
            (OutputMode::ForceDeflate, CompressionMethod::Deflated) => {
                self.writer.write_raw_entry(dst, raw)
            }
            (OutputMode::ForceDeflate, CompressionMethod::Stored) => {
                dst.method = CompressionMethod::Deflated;
                let compressed = deflate(raw);
                self.writer.write_raw_entry(dst, &compressed)
            }
            (OutputMode::ForceStored, CompressionMethod::Stored) => {
                self.writer.write_raw_entry(dst, raw)
            }
            (OutputMode::ForceStored, CompressionMethod::Deflated) => {
                let uncompressed = if src.size == 0 && raw.is_empty() {
                    Vec::new()
                } else {
                    inflate(raw)
                        .map(|(b, _)| b)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                };
                dst.method = CompressionMethod::Stored;
                // Keep source CRC and uncompressed size per ZipCombiner.copyEntry.
                self.writer.write_raw_entry(dst, &uncompressed)
            }
            (_, CompressionMethod::Other(code)) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("Unsupported compression method {code} for '{}'", src.name),
            )),
        }
    }

    /// Finishes any deferred custom merges and writes the central directory and EOCD record.
    pub fn finish(mut self) -> io::Result<Vec<u8>> {
        let merges = std::mem::take(&mut self.merges);
        for (name, mut m) in merges {
            if self.entries.contains(&name) {
                continue;
            }
            m.strategy.finish(&mut m.buffer)?;
            if m.buffer.is_empty() && m.strategy.skip_empty() {
                continue;
            }
            self.entries.insert(name.clone());
            let time = m.date.unwrap_or_else(DosDateTime::now_utc);
            self.write_buffered_entry(&name, time, &m.buffer)?;
        }
        self.writer.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concatenate_strategy_newline_and_raw_modes() {
        let mut s = ConcatenateStrategy::default();
        let mut out = Vec::new();
        s.merge(b"a", &mut out).unwrap();
        s.merge(b"", &mut out).unwrap(); // Empty stream does not alter lastByteCopied!
        s.merge(b"b", &mut out).unwrap();
        s.finish(&mut out).unwrap();
        assert_eq!(out, b"a\nb");

        let mut s2 = ConcatenateStrategy::default();
        let mut out2 = Vec::new();
        s2.merge(b"a\n", &mut out2).unwrap();
        s2.merge(b"b\r", &mut out2).unwrap();
        s2.merge(b"c", &mut out2).unwrap();
        s2.finish(&mut out2).unwrap();
        // `\r` is NOT `\n`, so `\n` is inserted after `b\r`; no trailing `\n` at finish.
        assert_eq!(out2, b"a\nb\r\nc");

        let mut raw = ConcatenateStrategy::new(false);
        let mut out3 = Vec::new();
        raw.merge(b"\x01\x02", &mut out3).unwrap();
        raw.merge(b"\x03", &mut out3).unwrap();
        raw.finish(&mut out3).unwrap();
        assert_eq!(out3, b"\x01\x02\x03");
    }

    fn make_zip(items: &[(&str, &[u8], CompressionMethod)]) -> ZipReader {
        let mut w = ZipWriter::new(false);
        for &(name, data, method) in items {
            let mut e = ZipFileEntry::new(name);
            e.method = method;
            e.time = Some(DosDateTime::JAR_DEFAULT);
            w.write_entry(e, data).unwrap();
        }
        ZipReader::from_bytes(w.finish().unwrap()).unwrap()
    }

    #[test]
    fn default_jar_entry_filter_and_zip_combiner_precedence() {
        let z1 = make_zip(&[
            (
                "META-INF/MANIFEST.MF",
                b"Manifest-Version: 1.0\r\n\r\n",
                CompressionMethod::Stored,
            ),
            ("META-INF/SIG.SF", b"sig", CompressionMethod::Stored),
            ("pkg/nested.DSA", b"sig2", CompressionMethod::Stored),
            (
                "META-INF/services/foo.SF",
                b"impl.One",
                CompressionMethod::Deflated,
            ),
            ("META-INF/spring.handlers", b"h1", CompressionMethod::Stored),
            ("sub/reference.conf", b"first", CompressionMethod::Stored),
            ("reference.conf", b"k=1", CompressionMethod::Stored),
            ("protobuf.meta.class", b"class1", CompressionMethod::Stored),
            ("protobuf.meta", b"\x0a\x01a", CompressionMethod::Deflated),
            ("com/Foo.class", b"foo1", CompressionMethod::Deflated),
        ]);
        let z2 = make_zip(&[
            (
                "META-INF/services/foo.SF",
                b"impl.Two",
                CompressionMethod::Stored,
            ),
            (
                "META-INF/spring.handlers",
                b"h2",
                CompressionMethod::Deflated,
            ),
            ("sub/reference.conf", b"second", CompressionMethod::Stored),
            ("reference.conf", b"k=2", CompressionMethod::Stored),
            ("protobuf.meta.class", b"class2", CompressionMethod::Stored),
            ("protobuf.meta", b"\x0a\x01b", CompressionMethod::Stored),
            ("com/Foo.class", b"foo2", CompressionMethod::Deflated),
        ]);

        let mut combiner = ZipCombiner::new(OutputMode::DontCare);
        combiner.add_zip(&z1).unwrap();
        combiner.add_zip(&z2).unwrap();
        let out = ZipReader::from_bytes(combiner.finish().unwrap()).unwrap();

        assert!(out.get_entry("META-INF/MANIFEST.MF").is_none());
        assert!(out.get_entry("META-INF/SIG.SF").is_none());
        assert!(out.get_entry("pkg/nested.DSA").is_none());

        // `META-INF/services/foo.SF` is merged rather than skipped as `.SF`!
        let svc = out.get_entry("META-INF/services/foo.SF").unwrap();
        assert_eq!(svc.time, Some(DosDateTime::DOS_EPOCH));
        assert_eq!(
            out.read_entry_bytes(svc, true).unwrap(),
            b"impl.One\nimpl.Two"
        );

        let spring = out.get_entry("META-INF/spring.handlers").unwrap();
        assert_eq!(out.read_entry_bytes(spring, true).unwrap(), b"h1\nh2");

        // Exact `reference.conf` is merged; `sub/reference.conf` is first-wins copy.
        assert_eq!(
            out.read_entry_bytes(out.get_entry("reference.conf").unwrap(), true)
                .unwrap(),
            b"k=1\nk=2"
        );
        assert_eq!(
            out.read_entry_bytes(out.get_entry("sub/reference.conf").unwrap(), true)
                .unwrap(),
            b"first"
        );

        // `protobuf.meta.class` matches `.class` BEFORE `protobuf.meta`: first-wins + +2s!
        let pm_cls = out.get_entry("protobuf.meta.class").unwrap();
        assert_eq!(pm_cls.time, Some(DosDateTime::DOS_EPOCH_PLUS_2_SECONDS));
        assert_eq!(out.read_entry_bytes(pm_cls, true).unwrap(), b"class1");

        // Exact `protobuf.meta` uses raw concatenation (`ConcatenateStrategy(false)`).
        let pm = out.get_entry("protobuf.meta").unwrap();
        assert_eq!(pm.time, Some(DosDateTime::DOS_EPOCH));
        assert_eq!(
            out.read_entry_bytes(pm, true).unwrap(),
            b"\x0a\x01a\x0a\x01b"
        );

        // Regular `.class` is first-wins with DOS_EPOCH + 2s.
        let foo = out.get_entry("com/Foo.class").unwrap();
        assert_eq!(foo.time, Some(DosDateTime::DOS_EPOCH_PLUS_2_SECONDS));
        assert_eq!(out.read_entry_bytes(foo, true).unwrap(), b"foo1");
    }

    #[test]
    fn combiner_output_modes_and_duplicate_errors() {
        let z = make_zip(&[
            (
                "stored.txt",
                b"compressible compressible compressible",
                CompressionMethod::Stored,
            ),
            (
                "deflated.txt",
                b"deflated payload deflated payload",
                CompressionMethod::Deflated,
            ),
        ]);

        // ForceDeflate deflates Stored entries.
        let mut c_def = ZipCombiner::with_filter(OutputMode::ForceDeflate, CopyEntryFilter);
        c_def.add_zip(&z).unwrap();
        let r_def = ZipReader::from_bytes(c_def.finish().unwrap()).unwrap();
        assert_eq!(
            r_def.get_entry("stored.txt").unwrap().method,
            CompressionMethod::Deflated
        );
        assert_eq!(
            r_def
                .read_entry_bytes(r_def.get_entry("stored.txt").unwrap(), true)
                .unwrap(),
            b"compressible compressible compressible"
        );

        // ForceStored inflates Deflated entries and stores new files uncompressed.
        let mut c_st = ZipCombiner::with_filter(OutputMode::ForceStored, CopyEntryFilter);
        c_st.add_zip(&z).unwrap();
        c_st.add_file("new.txt", Some(DosDateTime::DOS_EPOCH), b"plain")
            .unwrap();
        c_st.add_directory("dir/", Some(DosDateTime::DOS_EPOCH), vec![])
            .unwrap();
        assert!(c_st.add_file("new.txt", None, b"dup").is_err());
        assert!(c_st.add_directory("dir/", None, vec![]).is_err());
        assert!(c_st.add_directory("no_slash", None, vec![]).is_err());

        let r_st = ZipReader::from_bytes(c_st.finish().unwrap()).unwrap();
        for e in r_st.entries() {
            assert_eq!(e.method, CompressionMethod::Stored);
        }
        assert_eq!(
            r_st.read_entry_bytes(r_st.get_entry("deflated.txt").unwrap(), true)
                .unwrap(),
            b"deflated payload deflated payload"
        );
    }

    #[test]
    fn allowed_paths_predicate_skips_disallowed_entries_before_merge() {
        let f = DefaultJarEntryFilter::with_allowed_paths(true, |p| {
            !p.starts_with("META-INF/services/")
        });
        let z = make_zip(&[(
            "META-INF/services/MyService",
            b"impl.A",
            CompressionMethod::Stored,
        )]);
        let mut c = ZipCombiner::with_filter(OutputMode::DontCare, f);
        c.add_zip(&z).unwrap();
        let r = ZipReader::from_bytes(c.finish().unwrap()).unwrap();
        assert!(r.entries().is_empty());
    }
}
