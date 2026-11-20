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

//! Pure-Rust port of Bazel's `com.google.devtools.build.zip` (`ZipFileEntry`,
//! `EndOfCentralDirectoryRecord`, `Zip64EndOfCentralDirectory`, `CentralDirectory`,
//! `ZipReader`, and `ZipWriter`).

use crate::deflate::{crc32, deflate, inflate};
use std::collections::HashMap;
use std::io;

pub const LOCAL_FILE_HEADER_SIG: u32 = 0x0403_4b50;
pub const CENTRAL_DIR_HEADER_SIG: u32 = 0x0201_4b50;
pub const EOCD_SIG: u32 = 0x0605_4b50;
pub const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
pub const ZIP64_EOCD_LOCATOR_SIG: u32 = 0x0706_4b50;
pub const ZIP64_EXTRA_ID: u16 = 0x0001;

pub const FLAG_DATA_DESCRIPTOR: u16 = 1 << 3;
pub const FLAG_UTF8: u16 = 1 << 11;

pub const VERSION_STORED: u16 = 0x000a;
pub const VERSION_DEFLATED: u16 = 0x0014;
pub const VERSION_ZIP64: u16 = 0x002d;

/// Packed MS-DOS date and time used in ZIP local and central directory headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DosDateTime {
    pub date: u16,
    pub time: u16,
}

impl DosDateTime {
    /// `1980-01-01 00:00:00` (`ZipCombiner.DOS_EPOCH`).
    pub const DOS_EPOCH: Self = Self::from_ymdhms(1980, 1, 1, 0, 0, 0);
    /// `1980-01-01 00:00:02` (`DefaultJarEntryFilter.DOS_EPOCH_PLUS_2_SECONDS`).
    pub const DOS_EPOCH_PLUS_2_SECONDS: Self = Self::from_ymdhms(1980, 1, 1, 0, 0, 2);
    /// `2010-01-01 00:00:00` (`JarHelper.DEFAULT_TIMESTAMP`).
    pub const JAR_DEFAULT: Self = Self::from_ymdhms(2010, 1, 1, 0, 0, 0);
    /// `2010-01-01 00:00:02` (`JarHelper.DEFAULT_TIMESTAMP + MINIMUM_TIMESTAMP_INCREMENT`).
    pub const JAR_DEFAULT_CLASS: Self = Self::from_ymdhms(2010, 1, 1, 0, 0, 2);

    pub const fn new(date: u16, time: u16) -> Self {
        Self { date, time }
    }

    pub const fn from_ymdhms(
        year: u16,
        month: u16,
        day: u16,
        hour: u16,
        min: u16,
        sec: u16,
    ) -> Self {
        let y = if year < 1980 { 0 } else { (year - 1980) & 0x7f };
        let date = (y << 9) | ((month & 0x0f) << 5) | (day & 0x1f);
        let time = ((hour & 0x1f) << 11) | ((min & 0x3f) << 5) | ((sec / 2) & 0x1f);
        Self { date, time }
    }

    pub const fn packed(self) -> u32 {
        ((self.date as u32) << 16) | (self.time as u32)
    }

    pub const fn from_packed(v: u32) -> Self {
        Self {
            date: (v >> 16) as u16,
            time: (v & 0xFFFF) as u16,
        }
    }

    pub fn now_utc() -> Self {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self::from_unix_seconds(secs)
    }

    pub const fn from_unix_seconds(secs: u64) -> Self {
        let days = (secs / 86_400) as i64;
        let rem = (secs % 86_400) as u16;
        let hour = rem / 3600;
        let min = (rem % 3600) / 60;
        let sec = rem % 60;
        // Howard Hinnant civil-from-days algorithm.
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = y + if m <= 2 { 1 } else { 0 };
        if year < 1980 {
            Self::DOS_EPOCH
        } else {
            Self::from_ymdhms(year as u16, m as u16, d as u16, hour, min, sec)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompressionMethod {
    Stored,
    Deflated,
    Other(u16),
}

impl CompressionMethod {
    pub const fn from_code(code: u16) -> Self {
        match code {
            0 => Self::Stored,
            8 => Self::Deflated,
            other => Self::Other(other),
        }
    }

    pub const fn code(self) -> u16 {
        match self {
            Self::Stored => 0,
            Self::Deflated => 8,
            Self::Other(c) => c,
        }
    }
}

/// Metadata for a single ZIP archive entry (`ZipFileEntry.java`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipFileEntry {
    pub name: String,
    /// Optional `i64`-style time representation where `None` represents unset (`-1` in Java).
    pub time: Option<DosDateTime>,
    pub crc: u32,
    pub csize: u64,
    pub size: u64,
    pub method: CompressionMethod,
    pub local_header_offset: u64,
    pub version: u16,
    pub version_needed: u16,
    pub flags: u16,
    pub internal_attributes: u16,
    pub external_attributes: u32,
    pub extra: Vec<u8>,
    pub comment: Vec<u8>,
}

impl ZipFileEntry {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            time: Some(DosDateTime::DOS_EPOCH),
            crc: 0,
            csize: 0,
            size: 0,
            method: CompressionMethod::Stored,
            local_header_offset: 0,
            version: VERSION_DEFLATED,
            version_needed: VERSION_STORED,
            flags: 0,
            internal_attributes: 0,
            external_attributes: 0,
            extra: Vec::new(),
            comment: Vec::new(),
        }
    }

    /// Sets the CRC-32 after validating the unsigned 32-bit range `0..=0xFFFF_FFFF`
    /// (`ZipFileEntry.setCrc`).
    pub fn try_set_crc(&mut self, crc: i64) -> io::Result<()> {
        if !(0..=0xFFFF_FFFF).contains(&crc) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid entry crc-32: {crc:#x}"),
            ));
        }
        self.crc = crc as u32;
        Ok(())
    }

    pub fn requires_zip64(&self) -> bool {
        self.size >= 0xFFFF_FFFF
            || self.csize >= 0xFFFF_FFFF
            || self.local_header_offset >= 0xFFFF_FFFF
    }

    pub fn min_version(&self) -> u16 {
        if self.requires_zip64() {
            VERSION_ZIP64
        } else if self.method == CompressionMethod::Deflated
            || (self.flags & FLAG_DATA_DESCRIPTOR) != 0
            || self.name.ends_with('/')
        {
            VERSION_DEFLATED
        } else {
            VERSION_STORED
        }
    }

    pub fn extra_without_zip64(&self) -> Vec<u8> {
        strip_extra_field(&self.extra, ZIP64_EXTRA_ID)
    }
}

fn strip_extra_field(extra: &[u8], remove_id: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(extra.len());
    let mut pos = 0;
    while pos + 4 <= extra.len() {
        let id = u16::from_le_bytes([extra[pos], extra[pos + 1]]);
        let sz = u16::from_le_bytes([extra[pos + 2], extra[pos + 3]]) as usize;
        if pos + 4 + sz > extra.len() {
            out.extend_from_slice(&extra[pos..]);
            return out;
        }
        if id != remove_id {
            out.extend_from_slice(&extra[pos..pos + 4 + sz]);
        }
        pos += 4 + sz;
    }
    if pos < extra.len() {
        out.extend_from_slice(&extra[pos..]);
    }
    out
}

/// Classic 22-byte End of Central Directory record (`EndOfCentralDirectoryRecord.java`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndOfCentralDirectoryRecord {
    pub disk_number: u16,
    pub cd_disk: u16,
    pub disk_entries: u16,
    pub total_entries: u16,
    pub cd_size: u32,
    pub cd_offset: u32,
    pub comment: Vec<u8>,
    pub file_offset: usize,
}

impl EndOfCentralDirectoryRecord {
    /// Checks all six numeric EOCD fields for all-ones ZIP64 sentinel values.
    pub const fn maybe_zip64(&self) -> bool {
        self.disk_number == 0xFFFF
            || self.cd_disk == 0xFFFF
            || self.disk_entries == 0xFFFF
            || self.total_entries == 0xFFFF
            || self.cd_size == 0xFFFF_FFFF
            || self.cd_offset == 0xFFFF_FFFF
    }

    pub fn find_and_parse(buf: &[u8]) -> io::Result<Self> {
        let eocd = find_eocd_offset(buf).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Zip file is malformed: End of Central Directory record not found",
            )
        })?;
        let comment_len = u16le(buf, eocd + 20)? as usize;
        let comment = buf
            .get(eocd + 22..eocd + 22 + comment_len)
            .unwrap_or_default()
            .to_vec();
        Ok(Self {
            disk_number: u16le(buf, eocd + 4)?,
            cd_disk: u16le(buf, eocd + 6)?,
            disk_entries: u16le(buf, eocd + 8)?,
            total_entries: u16le(buf, eocd + 10)?,
            cd_size: u32le(buf, eocd + 12)?,
            cd_offset: u32le(buf, eocd + 16)?,
            comment,
            file_offset: eocd,
        })
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&EOCD_SIG.to_le_bytes());
        out.extend_from_slice(&self.disk_number.to_le_bytes());
        out.extend_from_slice(&self.cd_disk.to_le_bytes());
        out.extend_from_slice(&self.disk_entries.to_le_bytes());
        out.extend_from_slice(&self.total_entries.to_le_bytes());
        out.extend_from_slice(&self.cd_size.to_le_bytes());
        out.extend_from_slice(&self.cd_offset.to_le_bytes());
        let clen = self.comment.len().min(0xFFFF);
        out.extend_from_slice(&(clen as u16).to_le_bytes());
        out.extend_from_slice(&self.comment[..clen]);
    }
}

/// ZIP64 End of Central Directory record and locator (`Zip64EndOfCentralDirectory.java`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zip64EndOfCentralDirectory {
    pub version_made_by: u16,
    pub version_needed: u16,
    pub disk_number: u32,
    pub cd_disk: u32,
    pub disk_entries: u64,
    pub total_entries: u64,
    pub cd_size: u64,
    pub cd_offset: u64,
}

impl Zip64EndOfCentralDirectory {
    /// Attempts to read a ZIP64 EOCD locator immediately preceding `eocd_offset` and its target
    /// ZIP64 EOCD record. Returns `None` if no valid locator + ZIP64 EOCD pair is present so a
    /// non-ZIP64 archive with sentinel-looking 32-bit fields remains readable.
    pub fn try_read_before_eocd(buf: &[u8], eocd_offset: usize) -> Option<Self> {
        if eocd_offset < 20 {
            return None;
        }
        let loc = eocd_offset - 20;
        if u32le(buf, loc).ok()? != ZIP64_EOCD_LOCATOR_SIG {
            return None;
        }
        let z64_off_u64 = u64le(buf, loc + 8).ok()?;
        let z64 = usize::try_from(z64_off_u64).ok()?;
        if z64.checked_add(56)? > buf.len() {
            return None;
        }
        if u32le(buf, z64).ok()? != ZIP64_EOCD_SIG {
            return None;
        }
        Some(Self {
            version_made_by: u16le(buf, z64 + 12).ok()?,
            version_needed: u16le(buf, z64 + 14).ok()?,
            disk_number: u32le(buf, z64 + 16).ok()?,
            cd_disk: u32le(buf, z64 + 20).ok()?,
            disk_entries: u64le(buf, z64 + 24).ok()?,
            total_entries: u64le(buf, z64 + 32).ok()?,
            cd_size: u64le(buf, z64 + 40).ok()?,
            cd_offset: u64le(buf, z64 + 48).ok()?,
        })
    }

    pub fn write_with_locator(&self, out: &mut Vec<u8>) {
        let z64_offset = out.len() as u64;
        out.extend_from_slice(&ZIP64_EOCD_SIG.to_le_bytes());
        out.extend_from_slice(&44u64.to_le_bytes()); // remaining record size (56 - 12)
        out.extend_from_slice(&self.version_made_by.to_le_bytes());
        out.extend_from_slice(&self.version_needed.to_le_bytes());
        out.extend_from_slice(&self.disk_number.to_le_bytes());
        out.extend_from_slice(&self.cd_disk.to_le_bytes());
        out.extend_from_slice(&self.disk_entries.to_le_bytes());
        out.extend_from_slice(&self.total_entries.to_le_bytes());
        out.extend_from_slice(&self.cd_size.to_le_bytes());
        out.extend_from_slice(&self.cd_offset.to_le_bytes());

        out.extend_from_slice(&ZIP64_EOCD_LOCATOR_SIG.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // disk with Zip64 EOCD
        out.extend_from_slice(&z64_offset.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes()); // total disks
    }
}

/// Parsed central directory (`CentralDirectory.java`).
#[derive(Debug, Clone, Default)]
pub struct CentralDirectory {
    raw_entries: Vec<ZipFileEntry>,
    deduped_entries: Vec<ZipFileEntry>,
    by_name: HashMap<String, usize>,
    pub is_zip64: bool,
    pub comment: Vec<u8>,
}

impl CentralDirectory {
    pub fn parse(buf: &[u8]) -> io::Result<Self> {
        let eocd = EndOfCentralDirectoryRecord::find_and_parse(buf)?;
        let mut total = eocd.total_entries as u64;
        let mut cd_size = eocd.cd_size as u64;
        let mut cd_off = eocd.cd_offset as u64;
        let mut is_zip64 = false;

        if eocd.maybe_zip64() {
            if let Some(z64) =
                Zip64EndOfCentralDirectory::try_read_before_eocd(buf, eocd.file_offset)
            {
                total = z64.total_entries;
                cd_size = z64.cd_size;
                cd_off = z64.cd_offset;
                is_zip64 = true;
            }
        }

        let computed = eocd.file_offset as i64 - cd_size as i64;
        let (mut pos, by_size) =
            if !is_zip64 && computed >= 0 && (computed as u32) == (cd_off as u32) {
                (computed as usize, true)
            } else {
                let off = usize::try_from(cd_off).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "central directory offset overflow",
                    )
                })?;
                (off, false)
            };

        let start_pos = pos;
        let mut raw_entries = Vec::new();
        let mut count = 0u64;

        loop {
            if by_size {
                if ((pos - start_pos) as u64) >= cd_size {
                    break;
                }
            } else if count >= total {
                break;
            }
            count += 1;
            if pos + 46 > buf.len() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Unexpected end of file while reading Central Directory File Header",
                ));
            }
            if u32le(buf, pos)? != CENTRAL_DIR_HEADER_SIG {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Malformed Central Directory File Header; does not start with 02014b50",
                ));
            }
            let version = u16le(buf, pos + 4)?;
            let version_needed = u16le(buf, pos + 6)?;
            let flags = u16le(buf, pos + 8)?;
            let method = CompressionMethod::from_code(u16le(buf, pos + 10)?);
            let dos_time = u16le(buf, pos + 12)?;
            let dos_date = u16le(buf, pos + 14)?;
            let crc = u32le(buf, pos + 16)?;
            let mut csize = u32le(buf, pos + 20)? as u64;
            let mut size = u32le(buf, pos + 24)? as u64;
            let nlen = u16le(buf, pos + 28)? as usize;
            let elen = u16le(buf, pos + 30)? as usize;
            let clen = u16le(buf, pos + 32)? as usize;
            let internal_attributes = u16le(buf, pos + 36)?;
            let external_attributes = u32le(buf, pos + 38)?;
            let mut lho = u32le(buf, pos + 42)? as u64;

            let end = pos
                .checked_add(46)
                .and_then(|v| v.checked_add(nlen))
                .and_then(|v| v.checked_add(elen))
                .and_then(|v| v.checked_add(clen))
                .filter(|&e| e <= buf.len())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Unexpected end of file while reading Central Directory variable fields",
                    )
                })?;

            let name_bytes = &buf[pos + 46..pos + 46 + nlen];
            let extra = buf[pos + 46 + nlen..pos + 46 + nlen + elen].to_vec();
            let comment = buf[pos + 46 + nlen + elen..end].to_vec();
            pos = end;

            if size == 0xFFFF_FFFF || csize == 0xFFFF_FFFF || lho == 0xFFFF_FFFF {
                let mut e = 0;
                while e + 4 <= extra.len() {
                    let id = u16le(&extra, e)?;
                    let sz = u16le(&extra, e + 2)? as usize;
                    if e + 4 + sz > extra.len() {
                        break;
                    }
                    if id == ZIP64_EXTRA_ID {
                        let mut p = e + 4;
                        if size == 0xFFFF_FFFF && p + 8 <= e + 4 + sz {
                            size = u64le(&extra, p)?;
                            p += 8;
                        }
                        if csize == 0xFFFF_FFFF && p + 8 <= e + 4 + sz {
                            csize = u64le(&extra, p)?;
                            p += 8;
                        }
                        if lho == 0xFFFF_FFFF && p + 8 <= e + 4 + sz {
                            lho = u64le(&extra, p)?;
                        }
                    }
                    e += 4 + sz;
                }
            }

            let name = String::from_utf8_lossy(name_bytes).into_owned();
            raw_entries.push(ZipFileEntry {
                name,
                time: Some(DosDateTime::new(dos_date, dos_time)),
                crc,
                csize,
                size,
                method,
                local_header_offset: lho,
                version,
                version_needed,
                flags,
                internal_attributes,
                external_attributes,
                extra,
                comment,
            });
        }

        // `ZipFileData` stores entries in a `LinkedHashMap`: duplicate entry names keep the first
        // sighting's iteration position while replacing the entry value with the last record.
        let mut deduped_entries: Vec<ZipFileEntry> = Vec::with_capacity(raw_entries.len());
        let mut by_name: HashMap<String, usize> = HashMap::with_capacity(raw_entries.len());
        for entry in &raw_entries {
            if let Some(&idx) = by_name.get(&entry.name) {
                deduped_entries[idx] = entry.clone();
            } else {
                let idx = deduped_entries.len();
                by_name.insert(entry.name.clone(), idx);
                deduped_entries.push(entry.clone());
            }
        }

        Ok(Self {
            raw_entries,
            deduped_entries,
            by_name,
            is_zip64,
            comment: eocd.comment,
        })
    }

    pub fn entries(&self) -> &[ZipFileEntry] {
        &self.deduped_entries
    }

    pub fn raw_entries(&self) -> &[ZipFileEntry] {
        &self.raw_entries
    }

    pub fn get(&self, name: &str) -> Option<&ZipFileEntry> {
        self.by_name.get(name).map(|&i| &self.deduped_entries[i])
    }
}

/// Reader for 32-bit and ZIP64 archives (`ZipReader.java`).
#[derive(Debug, Clone)]
pub struct ZipReader {
    data: Vec<u8>,
    cd: CentralDirectory,
}

impl ZipReader {
    pub fn from_bytes(data: Vec<u8>) -> io::Result<Self> {
        let cd = CentralDirectory::parse(&data)?;
        Ok(Self { data, cd })
    }

    pub fn open(path: &std::path::Path) -> io::Result<Self> {
        Self::from_bytes(std::fs::read(path)?)
    }

    pub fn is_zip64(&self) -> bool {
        self.cd.is_zip64
    }

    pub fn entries(&self) -> &[ZipFileEntry] {
        self.cd.entries()
    }

    pub fn raw_entries(&self) -> &[ZipFileEntry] {
        self.cd.raw_entries()
    }

    pub fn get_entry(&self, name: &str) -> Option<&ZipFileEntry> {
        self.cd.get(name)
    }

    /// Returns the raw compressed payload slice for `entry` (`ZipReader.getRawInputStream`).
    pub fn raw_entry_bytes(&self, entry: &ZipFileEntry) -> io::Result<&[u8]> {
        let lho = usize::try_from(entry.local_header_offset).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "local header offset overflow")
        })?;
        if lho + 30 > self.data.len() || u32le(&self.data, lho)? != LOCAL_FILE_HEADER_SIG {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Expected Local File Header at offset {lho} for '{}'",
                    entry.name
                ),
            ));
        }
        let lname = u16le(&self.data, lho + 26)? as usize;
        let lextra = u16le(&self.data, lho + 28)? as usize;
        let start = lho
            .checked_add(30)
            .and_then(|v| v.checked_add(lname))
            .and_then(|v| v.checked_add(lextra))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "local header overflow"))?;
        let csize = usize::try_from(entry.csize)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "compressed size overflow"))?;
        let end = start
            .checked_add(csize)
            .filter(|&e| e <= self.data.len())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("Truncated entry payload for '{}'", entry.name),
                )
            })?;
        Ok(&self.data[start..end])
    }

    /// Decompresses `entry` and optionally verifies its CRC-32 (`ZipReader.getInputStream`).
    pub fn read_entry_bytes(&self, entry: &ZipFileEntry, verify_crc: bool) -> io::Result<Vec<u8>> {
        let raw = self.raw_entry_bytes(entry)?;
        let out = match entry.method {
            CompressionMethod::Stored => raw.to_vec(),
            CompressionMethod::Deflated => {
                if entry.size == 0 && raw.is_empty() {
                    Vec::new()
                } else {
                    inflate(raw)
                        .map(|(bytes, _)| bytes)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                }
            }
            CompressionMethod::Other(code) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "Unsupported ZIP compression method {code} for '{}'",
                        entry.name
                    ),
                ));
            }
        };
        if verify_crc {
            if out.len() as u64 != entry.size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Size mismatch for '{}': expected {}, got {}",
                        entry.name,
                        entry.size,
                        out.len()
                    ),
                ));
            }
            let actual_crc = crc32(&out);
            if actual_crc != entry.crc {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "CRC-32 mismatch for '{}': expected {:#010x}, got {:#010x}",
                        entry.name, entry.crc, actual_crc
                    ),
                ));
            }
        }
        Ok(out)
    }
}

/// Streaming archive writer supporting 32-bit ZIP and optional ZIP64 (`ZipWriter.java`).
#[derive(Debug)]
pub struct ZipWriter {
    allow_zip64: bool,
    buf: Vec<u8>,
    entries: Vec<ZipFileEntry>,
    comment: Vec<u8>,
    finished: bool,
}

impl ZipWriter {
    pub fn new(allow_zip64: bool) -> Self {
        Self {
            allow_zip64,
            buf: Vec::new(),
            entries: Vec::new(),
            comment: Vec::new(),
            finished: false,
        }
    }

    pub fn set_comment(&mut self, comment: impl Into<Vec<u8>>) {
        self.comment = comment.into();
    }

    /// Writes a pre-encoded (raw) entry payload (`putNextEntry` + raw bytes).
    pub fn write_raw_entry(
        &mut self,
        mut entry: ZipFileEntry,
        raw_payload: &[u8],
    ) -> io::Result<()> {
        if self.finished {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ZipWriter already finished",
            ));
        }
        let Some(dt) = entry.time else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("entry '{}' has unset timestamp (-1)", entry.name),
            ));
        };
        entry.local_header_offset = self.buf.len() as u64;
        entry.csize = raw_payload.len() as u64;
        // `ZipWriter` clears the data-descriptor bit because sizes and CRC are written up-front.
        entry.flags &= !FLAG_DATA_DESCRIPTOR;

        let need_local_z64 = entry.size >= 0xFFFF_FFFF || entry.csize >= 0xFFFF_FFFF;
        let need_cd_z64 = need_local_z64 || entry.local_header_offset >= 0xFFFF_FFFF;
        if (need_cd_z64 || self.entries.len() >= 0xFFFF) && !self.allow_zip64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "entry '{}' exceeds 32-bit ZIP limits and Zip64 is disabled",
                    entry.name
                ),
            ));
        }

        let base_extra = entry.extra_without_zip64();
        let mut local_extra = base_extra.clone();
        if need_local_z64 {
            local_extra.extend_from_slice(&ZIP64_EXTRA_ID.to_le_bytes());
            local_extra.extend_from_slice(&16u16.to_le_bytes());
            local_extra.extend_from_slice(&entry.size.to_le_bytes());
            local_extra.extend_from_slice(&entry.csize.to_le_bytes());
        }

        let min_ver = entry.min_version();
        entry.version_needed = entry.version_needed.max(min_ver);
        entry.version = entry.version.max(entry.version_needed);

        let name_bytes = entry.name.as_bytes();
        self.buf
            .extend_from_slice(&LOCAL_FILE_HEADER_SIG.to_le_bytes());
        self.buf
            .extend_from_slice(&entry.version_needed.to_le_bytes());
        self.buf.extend_from_slice(&entry.flags.to_le_bytes());
        self.buf
            .extend_from_slice(&entry.method.code().to_le_bytes());
        self.buf.extend_from_slice(&dt.time.to_le_bytes());
        self.buf.extend_from_slice(&dt.date.to_le_bytes());
        self.buf.extend_from_slice(&entry.crc.to_le_bytes());
        let local_csize = if need_local_z64 {
            0xFFFF_FFFFu32
        } else {
            entry.csize as u32
        };
        let local_size = if need_local_z64 {
            0xFFFF_FFFFu32
        } else {
            entry.size as u32
        };
        self.buf.extend_from_slice(&local_csize.to_le_bytes());
        self.buf.extend_from_slice(&local_size.to_le_bytes());
        self.buf
            .extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        self.buf
            .extend_from_slice(&(local_extra.len() as u16).to_le_bytes());
        self.buf.extend_from_slice(name_bytes);
        self.buf.extend_from_slice(&local_extra);
        self.buf.extend_from_slice(raw_payload);

        entry.extra = base_extra;
        self.entries.push(entry);
        Ok(())
    }

    /// Compresses (or stores) `uncompressed` according to `entry.method`, computing CRC-32 and
    /// uncompressed/compressed sizes automatically.
    pub fn write_entry(&mut self, mut entry: ZipFileEntry, uncompressed: &[u8]) -> io::Result<()> {
        entry.size = uncompressed.len() as u64;
        entry.crc = crc32(uncompressed);
        match entry.method {
            CompressionMethod::Stored => self.write_raw_entry(entry, uncompressed),
            CompressionMethod::Deflated => {
                let compressed = deflate(uncompressed);
                self.write_raw_entry(entry, &compressed)
            }
            CompressionMethod::Other(code) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("Unsupported compression method {code}"),
            )),
        }
    }

    pub fn finish(mut self) -> io::Result<Vec<u8>> {
        self.finish_in_place()?;
        Ok(self.buf)
    }

    fn finish_in_place(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let cd_offset = self.buf.len() as u64;

        for entry in &self.entries {
            let dt = entry.time.unwrap_or(DosDateTime::DOS_EPOCH);
            let mut cd_extra = entry.extra.clone();
            let mut z64_fields = Vec::new();
            let cd_size_field = if entry.size >= 0xFFFF_FFFF {
                z64_fields.extend_from_slice(&entry.size.to_le_bytes());
                0xFFFF_FFFFu32
            } else {
                entry.size as u32
            };
            let cd_csize_field = if entry.csize >= 0xFFFF_FFFF {
                z64_fields.extend_from_slice(&entry.csize.to_le_bytes());
                0xFFFF_FFFFu32
            } else {
                entry.csize as u32
            };
            let cd_lho_field = if entry.local_header_offset >= 0xFFFF_FFFF {
                z64_fields.extend_from_slice(&entry.local_header_offset.to_le_bytes());
                0xFFFF_FFFFu32
            } else {
                entry.local_header_offset as u32
            };
            if !z64_fields.is_empty() {
                cd_extra.extend_from_slice(&ZIP64_EXTRA_ID.to_le_bytes());
                cd_extra.extend_from_slice(&(z64_fields.len() as u16).to_le_bytes());
                cd_extra.extend_from_slice(&z64_fields);
            }

            let name_bytes = entry.name.as_bytes();
            self.buf
                .extend_from_slice(&CENTRAL_DIR_HEADER_SIG.to_le_bytes());
            self.buf.extend_from_slice(&entry.version.to_le_bytes());
            self.buf
                .extend_from_slice(&entry.version_needed.to_le_bytes());
            self.buf.extend_from_slice(&entry.flags.to_le_bytes());
            self.buf
                .extend_from_slice(&entry.method.code().to_le_bytes());
            self.buf.extend_from_slice(&dt.time.to_le_bytes());
            self.buf.extend_from_slice(&dt.date.to_le_bytes());
            self.buf.extend_from_slice(&entry.crc.to_le_bytes());
            self.buf.extend_from_slice(&cd_csize_field.to_le_bytes());
            self.buf.extend_from_slice(&cd_size_field.to_le_bytes());
            self.buf
                .extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            self.buf
                .extend_from_slice(&(cd_extra.len() as u16).to_le_bytes());
            self.buf
                .extend_from_slice(&(entry.comment.len() as u16).to_le_bytes());
            self.buf.extend_from_slice(&0u16.to_le_bytes()); // disk number start
            self.buf
                .extend_from_slice(&entry.internal_attributes.to_le_bytes());
            self.buf
                .extend_from_slice(&entry.external_attributes.to_le_bytes());
            self.buf.extend_from_slice(&cd_lho_field.to_le_bytes());
            self.buf.extend_from_slice(name_bytes);
            self.buf.extend_from_slice(&cd_extra);
            self.buf.extend_from_slice(&entry.comment);
        }

        let cd_size = (self.buf.len() as u64) - cd_offset;
        let total = self.entries.len() as u64;
        let needs_z64_eocd = total >= 0xFFFF || cd_size >= 0xFFFF_FFFF || cd_offset >= 0xFFFF_FFFF;

        if needs_z64_eocd {
            if !self.allow_zip64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Archive exceeds 32-bit ZIP central directory limits and Zip64 is disabled",
                ));
            }
            let z64 = Zip64EndOfCentralDirectory {
                version_made_by: VERSION_ZIP64,
                version_needed: VERSION_ZIP64,
                disk_number: 0,
                cd_disk: 0,
                disk_entries: total,
                total_entries: total,
                cd_size,
                cd_offset,
            };
            z64.write_with_locator(&mut self.buf);
        }

        let eocd = EndOfCentralDirectoryRecord {
            disk_number: 0,
            cd_disk: 0,
            disk_entries: if total >= 0xFFFF {
                0xFFFF
            } else {
                total as u16
            },
            total_entries: if total >= 0xFFFF {
                0xFFFF
            } else {
                total as u16
            },
            cd_size: if cd_size >= 0xFFFF_FFFF {
                0xFFFF_FFFF
            } else {
                cd_size as u32
            },
            cd_offset: if cd_offset >= 0xFFFF_FFFF {
                0xFFFF_FFFF
            } else {
                cd_offset as u32
            },
            comment: self.comment.clone(),
            file_offset: self.buf.len(),
        };
        eocd.write(&mut self.buf);
        Ok(())
    }
}

pub fn find_eocd_offset(buf: &[u8]) -> Option<usize> {
    if buf.len() < 22 {
        return None;
    }
    let min = buf.len().saturating_sub(22 + 0xFFFF);
    let mut i = buf.len() - 22;
    loop {
        if buf[i..i + 4] == [0x50, 0x4b, 0x05, 0x06] {
            let clen = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
            if i + 22 + clen <= buf.len() {
                return Some(i);
            }
        }
        if i == min {
            break;
        }
        i -= 1;
    }
    None
}

fn u16le(b: &[u8], o: usize) -> io::Result<u16> {
    let s = b
        .get(o..o + 2)
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated zip header"))?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn u32le(b: &[u8], o: usize) -> io::Result<u32> {
    let s = b
        .get(o..o + 4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated zip header"))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64le(b: &[u8], o: usize) -> io::Result<u64> {
    let s = b
        .get(o..o + 8)
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated zip64 header"))?;
    Ok(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_range_validation_and_roundtrip() {
        let mut e = ZipFileEntry::new("hello.txt");
        assert!(e.try_set_crc(-1).is_err());
        assert!(e.try_set_crc(0x1_0000_0000).is_err());
        e.try_set_crc(0xFFFF_FFFF).unwrap();
        assert_eq!(e.crc, 0xFFFF_FFFF);

        let mut w = ZipWriter::new(false);
        let mut f1 = ZipFileEntry::new("dir/a.txt");
        f1.method = CompressionMethod::Deflated;
        f1.flags = FLAG_DATA_DESCRIPTOR | FLAG_UTF8;
        f1.time = Some(DosDateTime::JAR_DEFAULT);
        w.write_entry(f1, b"hello zip world hello zip world")
            .unwrap();

        let mut f2 = ZipFileEntry::new("dir/b.bin");
        f2.method = CompressionMethod::Stored;
        f2.time = Some(DosDateTime::JAR_DEFAULT_CLASS);
        w.write_entry(f2, b"\x00\x01\x02\x03").unwrap();

        let bytes = w.finish().unwrap();
        let r = ZipReader::from_bytes(bytes).unwrap();
        assert!(!r.is_zip64());
        assert_eq!(r.entries().len(), 2);

        let a = r.get_entry("dir/a.txt").unwrap();
        assert_eq!(
            a.flags & FLAG_DATA_DESCRIPTOR,
            0,
            "data descriptor bit cleared"
        );
        assert_eq!(a.flags & FLAG_UTF8, FLAG_UTF8);
        assert_eq!(a.time, Some(DosDateTime::JAR_DEFAULT));
        assert_eq!(
            r.read_entry_bytes(a, true).unwrap(),
            b"hello zip world hello zip world"
        );

        let b = r.get_entry("dir/b.bin").unwrap();
        assert_eq!(b.time, Some(DosDateTime::JAR_DEFAULT_CLASS));
        assert_eq!(r.read_entry_bytes(b, true).unwrap(), b"\x00\x01\x02\x03");
    }

    #[test]
    fn rejects_unset_timestamp() {
        let mut w = ZipWriter::new(false);
        let mut e = ZipFileEntry::new("bad.txt");
        e.time = None;
        assert!(w.write_entry(e, b"x").is_err());
    }

    #[test]
    fn duplicate_central_directory_entries_keep_first_position_and_last_record() {
        let mut w = ZipWriter::new(false);
        let mut e1 = ZipFileEntry::new("dup.txt");
        e1.method = CompressionMethod::Stored;
        w.write_entry(e1, b"first").unwrap();
        let mut e2 = ZipFileEntry::new("middle.txt");
        e2.method = CompressionMethod::Stored;
        w.write_entry(e2, b"mid").unwrap();
        let mut e3 = ZipFileEntry::new("dup.txt");
        e3.method = CompressionMethod::Stored;
        w.write_entry(e3, b"second").unwrap();

        let r = ZipReader::from_bytes(w.finish().unwrap()).unwrap();
        assert_eq!(r.raw_entries().len(), 3);
        let names: Vec<&str> = r.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["dup.txt", "middle.txt"]);
        assert_eq!(
            r.read_entry_bytes(r.get_entry("dup.txt").unwrap(), true)
                .unwrap(),
            b"second"
        );
    }

    #[test]
    fn zip64_eocd_promotes_cd_size_and_checks_all_six_eocd_fields() {
        // Build a synthetic archive whose classic EOCD has ONLY cd_size == 0xFFFFFFFF (total_entries
        // and cd_offset fit in 32 bits), preceded by a valid Zip64 EOCD + Locator.
        let mut w = ZipWriter::new(true);
        let mut e = ZipFileEntry::new("only.txt");
        e.method = CompressionMethod::Stored;
        w.write_entry(e, b"zip64-cd-size").unwrap();
        let plain = w.finish().unwrap();

        let eocd_pos = find_eocd_offset(&plain).unwrap();
        let cd_off = u32le(&plain, eocd_pos + 16).unwrap() as u64;
        let cd_sz = u32le(&plain, eocd_pos + 12).unwrap() as u64;

        let mut z64_archive = plain[..eocd_pos].to_vec();
        let z64 = Zip64EndOfCentralDirectory {
            version_made_by: VERSION_ZIP64,
            version_needed: VERSION_ZIP64,
            disk_number: 0,
            cd_disk: 0,
            disk_entries: 1,
            total_entries: 1,
            cd_size: cd_sz,
            cd_offset: cd_off,
        };
        z64.write_with_locator(&mut z64_archive);
        let eocd = EndOfCentralDirectoryRecord {
            disk_number: 0,
            cd_disk: 0,
            disk_entries: 1,
            total_entries: 1,
            cd_size: 0xFFFF_FFFF, // ONLY cd_size uses the sentinel!
            cd_offset: cd_off as u32,
            comment: Vec::new(),
            file_offset: z64_archive.len(),
        };
        assert!(eocd.maybe_zip64());
        eocd.write(&mut z64_archive);

        let reader = ZipReader::from_bytes(z64_archive).unwrap();
        assert!(reader.is_zip64());
        assert_eq!(reader.entries().len(), 1);
        assert_eq!(
            reader.read_entry_bytes(&reader.entries()[0], true).unwrap(),
            b"zip64-cd-size"
        );
    }

    #[test]
    fn corrupted_crc_detected_when_verified() {
        let mut w = ZipWriter::new(false);
        let mut e = ZipFileEntry::new("check.txt");
        e.method = CompressionMethod::Stored;
        w.write_entry(e, b"hello").unwrap();
        let mut bytes = w.finish().unwrap();
        // Flip one payload byte right before the central directory.
        let lho_payload = 30 + "check.txt".len();
        bytes[lho_payload] ^= 0xFF;
        let r = ZipReader::from_bytes(bytes).unwrap();
        assert!(r.read_entry_bytes(&r.entries()[0], true).is_err());
    }
}
