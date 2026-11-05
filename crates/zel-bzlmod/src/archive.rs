//! Pure Rust archive extraction: inflate (RFC 1951), gzip (RFC 1952), zip, tar (ustar, pax,
//! GNU long names) and ar/deb, following the semantics of Bazel's `DecompressorValue`,
//! `StripPrefixedPath`, `ZipDecompressor` and `CompressedTarFunction`.
//!
//! Formats that need an external codec (xz, zstd, bzip2, brotli, 7z) return an error whose
//! message begins with "Unsupported archive format".

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

// ===========================================================================
// CRC32
// ===========================================================================

fn crc_table() -> &'static [u32; 256] {
    use std::sync::OnceLock;
    static T: OnceLock<[u32; 256]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    })
}

/// CRC-32 (IEEE) of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let t = crc_table();
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

// ===========================================================================
// Inflate
// ===========================================================================

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    cnt: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, buf: 0, cnt: 0 }
    }
    #[inline]
    fn refill(&mut self) {
        while self.cnt <= 56 && self.pos < self.data.len() {
            self.buf |= (self.data[self.pos] as u64) << self.cnt;
            self.pos += 1;
            self.cnt += 8;
        }
    }
    fn bits(&mut self, n: u32) -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        if self.cnt < n {
            self.refill();
            if self.cnt < n {
                return Err("Unexpected end of deflate stream".to_string());
            }
        }
        let v = (self.buf & ((1u64 << n) - 1)) as u32;
        self.buf >>= n;
        self.cnt -= n;
        Ok(v)
    }
}

struct Huff {
    table: Vec<u32>,
    bits: u32,
}

impl Huff {
    fn build(lengths: &[u8]) -> Result<Huff, String> {
        let mut count = [0i32; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        count[0] = 0;
        let mut left = 1i32;
        let mut max_len = 0;
        for len in 1..16 {
            left <<= 1;
            left -= count[len];
            if left < 0 {
                return Err("Invalid deflate stream: oversubscribed Huffman code".to_string());
            }
            if count[len] > 0 {
                max_len = len;
            }
        }
        let bits = max_len.max(1) as u32;
        let size = 1usize << bits;
        let mut table = vec![0u32; size];
        let mut next = [0u32; 17];
        let mut code = 0u32;
        for len in 1..16 {
            code = (code + count[len - 1] as u32) << 1;
            next[len] = code;
        }
        for (sym, &l) in lengths.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let l = l as u32;
            let c = next[l as usize];
            next[l as usize] += 1;
            let rev = c.reverse_bits() >> (32 - l);
            let entry = ((sym as u32) << 4) | l;
            let mut idx = rev as usize;
            while idx < size {
                table[idx] = entry;
                idx += 1 << l;
            }
        }
        Ok(Huff { table, bits })
    }

    #[inline]
    fn decode(&self, br: &mut BitReader) -> Result<usize, String> {
        if br.cnt < self.bits {
            br.refill();
        }
        let e = self.table[(br.buf & ((1u64 << self.bits) - 1)) as usize];
        let len = e & 15;
        if len == 0 || len > br.cnt {
            return Err(if len == 0 && br.cnt >= self.bits {
                "Invalid deflate stream: bad Huffman code".to_string()
            } else {
                "Unexpected end of deflate stream".to_string()
            });
        }
        br.buf >>= len;
        br.cnt -= len;
        Ok((e >> 4) as usize)
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const LEN64_BASE: [usize; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 3,
];
const LEN64_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 16,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const DIST64_BASE: [usize; 32] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577, 32769, 49153,
];
const DIST64_EXTRA: [u8; 32] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13, 14, 14,
];

/// Inflate a raw deflate stream. Returns the output and the number of input bytes consumed.
pub fn inflate(data: &[u8]) -> Result<(Vec<u8>, usize), String> {
    inflate_limit(data, usize::MAX)
}

/// Like [`inflate`] but fails if the output would exceed `limit` bytes.
pub fn inflate_limit(data: &[u8], limit: usize) -> Result<(Vec<u8>, usize), String> {
    inflate_internal(data, limit, false)
}

/// Inflate a raw Deflate64 stream.
pub fn inflate64(data: &[u8]) -> Result<(Vec<u8>, usize), String> {
    inflate64_limit(data, usize::MAX)
}

/// Like [`inflate64`] but fails if the output would exceed `limit` bytes.
pub fn inflate64_limit(data: &[u8], limit: usize) -> Result<(Vec<u8>, usize), String> {
    inflate_internal(data, limit, true)
}

fn inflate_internal(data: &[u8], limit: usize, is_deflate64: bool) -> Result<(Vec<u8>, usize), String> {
    let mut br = BitReader::new(data);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = br.bits(1)?;
        let btype = br.bits(2)?;
        match btype {
            0 => {
                // discard to byte boundary and rewind whole bytes held in the buffer
                let drop = br.cnt % 8;
                br.buf >>= drop;
                br.cnt -= drop;
                br.pos -= (br.cnt / 8) as usize;
                br.buf = 0;
                br.cnt = 0;
                if br.pos + 4 > data.len() {
                    return Err("Unexpected end of deflate stream".to_string());
                }
                let len = u16::from_le_bytes([data[br.pos], data[br.pos + 1]]) as usize;
                let nlen = u16::from_le_bytes([data[br.pos + 2], data[br.pos + 3]]) as usize;
                if len != (!nlen & 0xFFFF) {
                    return Err("Invalid deflate stream: stored block length mismatch".to_string());
                }
                br.pos += 4;
                if br.pos + len > data.len() {
                    return Err("Unexpected end of deflate stream".to_string());
                }
                if out.len() + len > limit {
                    out.extend_from_slice(&data[br.pos..br.pos + (limit - out.len())]);
                    crate::codecs::stash_partial(out);
                    return Err("Deflate output exceeds size limit".to_string());
                }
                out.extend_from_slice(&data[br.pos..br.pos + len]);
                br.pos += len;
            }
            1 | 2 => {
                let (lit, dist) = if btype == 1 {
                    let mut l = [0u8; 288];
                    for (i, v) in l.iter_mut().enumerate() {
                        *v = match i {
                            0..=143 => 8,
                            144..=255 => 9,
                            256..=279 => 7,
                            _ => 8,
                        };
                    }
                    let dist_lengths = if is_deflate64 { vec![5u8; 32] } else { vec![5u8; 30] };
                    (Huff::build(&l)?, Huff::build(&dist_lengths)?)
                } else {
                    read_dynamic(&mut br, is_deflate64)?
                };
                loop {
                    let sym = lit.decode(&mut br)?;
                    if sym < 256 {
                        if out.len() >= limit {
                            crate::codecs::stash_partial(out);
                            return Err("Deflate output exceeds size limit".to_string());
                        }
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let s = sym - 257;
                        if s >= 29 {
                            return Err("Invalid deflate stream: bad length code".to_string());
                        }
                        let len = if is_deflate64 {
                            LEN64_BASE[s] + br.bits(LEN64_EXTRA[s] as u32)? as usize
                        } else {
                            LEN_BASE[s] as usize + br.bits(LEN_EXTRA[s] as u32)? as usize
                        };
                        let ds = dist.decode(&mut br)?;
                        let max_d_codes = if is_deflate64 { 32 } else { 30 };
                        if ds >= max_d_codes {
                            return Err("Invalid deflate stream: bad distance code".to_string());
                        }
                        let d = if is_deflate64 {
                            DIST64_BASE[ds] + br.bits(DIST64_EXTRA[ds] as u32)? as usize
                        } else {
                            DIST_BASE[ds] as usize + br.bits(DIST_EXTRA[ds] as u32)? as usize
                        };
                        if d > out.len() {
                            return Err("Invalid deflate stream: distance too far back".to_string());
                        }
                        if out.len() + len > limit {
                            let start = out.len() - d;
                            while out.len() < limit {
                                let b = out[start + (out.len() - (start + d))];
                                out.push(b);
                            }
                            crate::codecs::stash_partial(out);
                            return Err("Deflate output exceeds size limit".to_string());
                        }
                        let start = out.len() - d;
                        if d >= len {
                            out.extend_from_within(start..start + len);
                        } else {
                            for i in 0..len {
                                let b = out[start + i];
                                out.push(b);
                            }
                        }
                    }
                }
            }
            _ => return Err("Invalid deflate stream: bad block type".to_string()),
        }
        if last == 1 {
            break;
        }
    }
    let consumed = br.pos - (br.cnt / 8) as usize;
    Ok((out, consumed))
}

fn read_dynamic(br: &mut BitReader, is_deflate64: bool) -> Result<(Huff, Huff), String> {
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;
    let max_hlit = if is_deflate64 { 288 } else { 286 };
    let max_hdist = if is_deflate64 { 32 } else { 30 };
    if hlit > max_hlit || hdist > max_hdist {
        return Err("Invalid deflate stream: too many length or distance codes".to_string());
    }
    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let mut cl = [0u8; 19];
    for &o in ORDER.iter().take(hclen) {
        cl[o] = br.bits(3)? as u8;
    }
    let clh = Huff::build(&cl)?;
    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < hlit + hdist {
        let sym = clh.decode(br)?;
        if sym < 16 {
            lengths[i] = sym as u8;
            i += 1;
        } else {
            let (val, rep) = match sym {
                16 => {
                    if i == 0 {
                        return Err("Invalid deflate stream: repeat with no previous length".to_string());
                    }
                    (lengths[i - 1], 3 + br.bits(2)? as usize)
                }
                17 => (0, 3 + br.bits(3)? as usize),
                _ => (0, 11 + br.bits(7)? as usize),
            };
            if i + rep > hlit + hdist {
                return Err("Invalid deflate stream: too many code lengths".to_string());
            }
            for _ in 0..rep {
                lengths[i] = val;
                i += 1;
            }
        }
    }
    if lengths[256] == 0 {
        return Err("Invalid deflate stream: missing end-of-block code".to_string());
    }
    Ok((Huff::build(&lengths[..hlit])?, Huff::build(&lengths[hlit..])?))
}

// ===========================================================================
// Gzip
// ===========================================================================

/// Decompress a gzip file (possibly multi-member), verifying CRC-32 and ISIZE.
pub fn gunzip(data: &[u8]) -> Result<Vec<u8>, String> {
    gunzip_with_header(data).map(|(b, _)| b)
}

/// Decompress a gzip file, also returning the original filename if present in FNAME header.
pub fn gunzip_with_header(data: &[u8]) -> Result<(Vec<u8>, Option<String>), String> {
    let mut out = Vec::new();
    if data.is_empty() {
        return Err("Not in gzip format".to_string());
    }
    match gunzip_into(data, &mut out) {
        Ok(f) => Ok((out, f)),
        Err(e) => {
            crate::codecs::stash_partial(out);
            Err(e)
        }
    }
}

fn gunzip_into(data: &[u8], out: &mut Vec<u8>) -> Result<Option<String>, String> {
    let mut pos = 0;
    let mut first = true;
    let mut first_fname = None;
    while pos < data.len() {
        if data.len() - pos < 2 || data[pos] != 0x1f || data[pos + 1] != 0x8b {
            if first {
                return Err("Not in gzip format".to_string());
            }
            return Err("Garbage after a valid .gz stream".to_string());
        }
        first = false;
        if data.len() - pos < 3 {
            return Err("Unexpected end of gzip header".to_string());
        }
        if data[pos + 2] != 8 {
            return Err(format!("Unsupported gzip compression method {}", data[pos + 2]));
        }
        if data.len() - pos < 4 {
            return Err("Unexpected end of gzip header".to_string());
        }
        let flg = data[pos + 3];
        if flg & 0xE0 != 0 {
            return Err("Invalid gzip header: reserved flags set".to_string());
        }
        if data.len() - pos < 10 {
            return Err("Unexpected end of gzip header".to_string());
        }
        let mut p = pos + 10;
        let trunc = || "Unexpected end of gzip header".to_string();
        if flg & 4 != 0 {
            if p + 2 > data.len() {
                return Err(trunc());
            }
            let xlen = u16::from_le_bytes([data[p], data[p + 1]]) as usize;
            p += 2 + xlen;
        }
        if p > data.len() {
            return Err(trunc());
        }
        if flg & 8 != 0 {
            let start = p;
            while p < data.len() && data[p] != 0 {
                p += 1;
            }
            if first_fname.is_none() {
                if let Ok(s) = std::str::from_utf8(&data[start..p]) {
                    let base = s.rsplit(['/', '\\']).next().unwrap_or(s);
                    if !base.is_empty() {
                        first_fname = Some(base.to_string());
                    }
                }
            }
            p += 1;
        }
        if flg & 16 != 0 {
            while p < data.len() && data[p] != 0 {
                p += 1;
            }
            p += 1;
        }
        if flg & 2 != 0 {
            p += 2;
        }
        if p > data.len() {
            return Err(trunc());
        }
        let (member, used) = inflate(&data[p..])?;
        p += used;
        if p + 8 > data.len() {
            return Err("Unexpected end of gzip trailer".to_string());
        }
        let crc = u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]);
        let isize_ = u32::from_le_bytes([data[p + 4], data[p + 5], data[p + 6], data[p + 7]]);
        out.extend_from_slice(&member);
        if crc != crc32(&member) {
            return Err("Gzip CRC32 mismatch".to_string());
        }
        if isize_ != (member.len() as u32) {
            return Err("Gzip ISIZE mismatch".to_string());
        }
        pos = p + 8;
    }
    Ok(first_fname)
}

// ===========================================================================
// Options and type detection
// ===========================================================================

/// Options for [`extract`], mirroring `repository_ctx.extract`.
#[derive(Clone, Debug)]
pub struct ExtractOptions {
    /// `strip_prefix`: leading directory to remove; entries outside it are skipped.
    pub strip_prefix: String,
    /// `strip_components`: number of leading path segments to remove (exclusive with strip_prefix).
    pub strip_components: usize,
    /// `add_prefix`: directory under `dest` that receives the contents.
    pub add_prefix: String,
    /// `rename_files`: exact entry name (before prefix stripping) -> new name.
    pub rename_files: BTreeMap<String, String>,
    /// Forced archive type (extension without the dot), otherwise detected by file suffix.
    pub archive_type: Option<String>,
    /// Whether absolute symlink targets are allowed (Bazel allows them).
    pub allow_absolute: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            strip_prefix: String::new(),
            strip_components: 0,
            add_prefix: String::new(),
            rename_files: BTreeMap::new(),
            archive_type: None,
            allow_absolute: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
    Gz,
    TarXz,
    Xz,
    TarZst,
    Zst,
    TarBz2,
    Bz2,
    Ar,
    SevenZ,
    TarBr,
    Br,
}

/// Extensions in Bazel's lookup order.
const FORMATS: &[(&[&str], ArchiveKind)] = &[
    (&["zip", "jar", "war", "aar", "nupkg", "whl"], ArchiveKind::Zip),
    (&["tar"], ArchiveKind::Tar),
    (&["tar.gz", "tgz"], ArchiveKind::TarGz),
    (&["gz"], ArchiveKind::Gz),
    (&["tar.xz", "txz"], ArchiveKind::TarXz),
    (&["xz"], ArchiveKind::Xz),
    (&["tar.zst", "tzst"], ArchiveKind::TarZst),
    (&["zst"], ArchiveKind::Zst),
    (&["tar.bz2", "tbz"], ArchiveKind::TarBz2),
    (&["bz2"], ArchiveKind::Bz2),
    (&["ar", "deb"], ArchiveKind::Ar),
    (&["7z"], ArchiveKind::SevenZ),
    (&["tar.br"], ArchiveKind::TarBr),
    (&["br"], ArchiveKind::Br),
];

fn all_extensions(prefix: &str) -> Vec<String> {
    FORMATS
        .iter()
        .flat_map(|(exts, _)| exts.iter())
        .map(|e| format!("{}{}", prefix, e))
        .collect()
}

fn readable_formats(prefix: &str, conjunction: &str) -> String {
    let all = all_extensions(prefix);
    let (last, rest) = all.split_last().unwrap();
    format!("{} {} {}", rest.join(", "), conjunction, last)
}

/// Detect the archive kind from the file's base name suffix (Bazel's `getDecompressor(Path)`).
pub fn detect_kind(archive: &Path) -> Result<ArchiveKind, String> {
    let base = archive.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    for (exts, kind) in FORMATS {
        if exts.iter().any(|e| base.ends_with(&format!(".{}", e))) {
            return Ok(*kind);
        }
    }
    Err(format!(
        "Expected a file with a {} suffix (got {})",
        readable_formats(".", "or"),
        archive.display()
    ))
}

/// Look up an explicit archive type (Bazel's `getDecompressor(String)`).
pub fn kind_for_type(t: &str) -> Result<ArchiveKind, String> {
    for (exts, kind) in FORMATS {
        if exts.contains(&t) {
            return Ok(*kind);
        }
    }
    Err(format!(
        "No decompressor found for type {}. Available types are: {}",
        t,
        readable_formats("", "or")
    ))
}

fn unsupported(kind: ArchiveKind) -> String {
    format!(
        "Unsupported archive format {:?}: this codec is not implemented natively",
        kind
    )
}

// ===========================================================================
// Path helpers (Bazel PathFragment semantics)
// ===========================================================================

/// Normalise a path: drop empty and `.` segments, resolve `..` where possible (leading `..`
/// is preserved), make absolute paths relative.
pub(crate) fn normalize(path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(out.last(), Some(l) if l != "..") {
                    out.pop();
                } else {
                    out.push("..".to_string());
                }
            }
            s => out.push(s.to_string()),
        }
    }
    out
}

#[derive(Clone, Debug)]
pub(crate) struct Deprefixed {
    pub(crate) segs: Vec<String>,
    pub(crate) found: bool,
    pub(crate) skip: bool,
}

pub(crate) fn deprefix(name: &str, prefix: &[String], strip_components: usize) -> Deprefixed {
    let segs = normalize(name);
    if strip_components != 0 {
        if segs.len() <= strip_components {
            return Deprefixed { segs: Vec::new(), found: false, skip: true };
        }
        return Deprefixed { segs: segs[strip_components..].to_vec(), found: false, skip: false };
    }
    if prefix.is_empty() {
        return Deprefixed { segs, found: false, skip: false };
    }
    if segs.len() >= prefix.len() && segs[..prefix.len()] == *prefix {
        let rest = segs[prefix.len()..].to_vec();
        let skip = rest.is_empty();
        Deprefixed { segs: rest, found: true, skip }
    } else {
        Deprefixed { segs, found: false, skip: true }
    }
}

fn prefix_not_found(prefix: &str, available: &BTreeSet<String>) -> String {
    let mut msg = format!("Prefix \"{}\" was given, but not found in the archive. ", prefix);
    if available.is_empty() {
        msg.push_str(
            "We could not find any directory in this archive (maybe there is no need for `strip_prefix`?)",
        );
    } else {
        msg.push_str("Here are possible prefixes for this archive: ");
        let v: Vec<&str> = available.iter().map(|s| s.as_str()).collect();
        msg.push_str(&format!("\"{}\".", v.join("\", \"")));
    }
    msg
}

// ===========================================================================
// Generic entry processing
// ===========================================================================

#[derive(Debug, Clone)]
pub(crate) enum Kind {
    File,
    Dir,
    Symlink(String),
    Hardlink(String),
}

#[derive(Debug, Clone)]
enum Data {
    None,
    /// Zip entry read lazily like Java: (local header offset, uncompressed size, method, archive base name).
    Zip(usize, usize, u16, String),
    /// Uncompressed bytes at buf[start..start+len].
    Raw(usize, usize),
    /// Deflated bytes at buf[start..start+len]; expected uncompressed size and crc.
    Deflated(usize, usize, usize, u32),
    /// Deflate64 bytes at buf[start..start+len]; expected uncompressed size and crc.
    Deflated64(usize, usize, usize, u32),
}

#[derive(Debug, Clone)]
struct Entry {
    name: String,
    kind: Kind,
    mode: u32,
    data: Data,
}

fn entry_data(buf: &[u8], d: &Data, archive: &Path, name: &str) -> Result<Vec<u8>, String> {
    match d {
        Data::None => Ok(Vec::new()),
        Data::Zip(lho, size, method, base) => zip_entry_bytes(buf, *lho, *size, *method, base),
        Data::Raw(s, l) => Ok(buf[*s..*s + *l].to_vec()),
        Data::Deflated(s, l, size, crc) => {
            let (out, _) = inflate_limit(&buf[*s..*s + *l], *size)
                .map_err(|e| format!("{}: {} (entry {})", archive.display(), e, name))?;
            if out.len() != *size {
                return Err(format!("{}: size mismatch for entry {}", archive.display(), name));
            }
            if crc32(&out) != *crc {
                return Err(format!("{}: invalid CRC for entry {}", archive.display(), name));
            }
            Ok(out)
        }
        Data::Deflated64(s, l, size, crc) => {
            let (out, _) = inflate64_limit(&buf[*s..*s + *l], *size)
                .map_err(|e| format!("{}: {} (entry {})", archive.display(), e, name))?;
            if out.len() != *size {
                return Err(format!("{}: size mismatch for entry {}", archive.display(), name));
            }
            if crc32(&out) != *crc {
                return Err(format!("{}: invalid CRC for entry {}", archive.display(), name));
            }
            Ok(out)
        }
    }
}

pub(crate) fn join_rel(root: &Path, segs: &[String]) -> PathBuf {
    let mut p = root.to_path_buf();
    for s in segs {
        p.push(s);
    }
    p
}

pub(crate) fn io_err(what: &str, p: &Path, e: std::io::Error) -> String {
    format!("{} {}: {}", what, p.display(), e)
}

/// An entry produced by the 7z reader (data lives in the shared buffer at `start..start+len`).
pub(crate) struct SevenZEntry {
    pub name: String,
    pub is_dir: bool,
    pub mode: Option<u32>,
    pub start: usize,
    pub len: usize,
}

pub(crate) fn extract_entries_7z(
    buf: &[u8],
    entries: Vec<SevenZEntry>,
    archive: &Path,
    root: &Path,
    opts: &ExtractOptions,
) -> Result<(), String> {
    let entries = entries
        .into_iter()
        .map(|e| Entry {
            name: e.name,
            kind: if e.is_dir { Kind::Dir } else { Kind::File },
            mode: 0o644, // Bazel (commons-compress) ignores 7z unix modes: every file is 0644
            data: if e.is_dir { Data::None } else { Data::Raw(e.start, e.len) },
        })
        .collect();
    extract_entries(buf, entries, archive, root, opts, "7z")
}

fn extract_entries(
    buf: &[u8],
    entries: Vec<Entry>,
    archive: &Path,
    root: &Path,
    opts: &ExtractOptions,
    noun: &str, // "tar" / "zip"
) -> Result<(), String> {
    let prefix_segs = normalize(&opts.strip_prefix);
    let mut found_prefix = false;
    let mut available: BTreeSet<String> = BTreeSet::new();
    fs::create_dir_all(root).map_err(|e| io_err("Failed to create directory", root, e))?;

    let mut symlinks: Vec<(PathBuf, String)> = Vec::new();
    let mut hardlinks: Vec<(PathBuf, PathBuf, String)> = Vec::new();

    for entry in &entries {
        let name = opts.rename_files.get(&entry.name).cloned().unwrap_or_else(|| entry.name.clone());
        let dp = deprefix(&name, &prefix_segs, opts.strip_components);
        found_prefix = found_prefix || dp.found;
        if !prefix_segs.is_empty() && !found_prefix {
            let full = normalize(&name);
            if full.len() > 1 {
                available.insert(full[0].clone());
            }
        }
        let rel = dp.segs;
        if dp.skip || rel.is_empty() {
            continue;
        }
        if rel.first().map(|s| s == "..").unwrap_or(false) {
            return Err(format!(
                "Failed to extract {}, path is escaping the destination directory",
                rel.join("/")
            ));
        }
        let out_path = join_rel(root, &rel);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_err("Failed to create directory", parent, e))?;
        }
        match &entry.kind {
            Kind::Dir => {
                fs::create_dir_all(&out_path)
                    .map_err(|e| io_err("Failed to create directory", &out_path, e))?;
            }
            Kind::File => {
                let data = entry_data(buf, &entry.data, archive, &entry.name)?;
                if let Ok(md) = fs::symlink_metadata(&out_path) {
                    if md.is_dir() {
                        return Err(format!(
                            "Failed to extract {}: a directory with that name exists",
                            out_path.display()
                        ));
                    }
                }
                fs::write(&out_path, &data).map_err(|e| io_err("Failed to write", &out_path, e))?;
                let mode = (entry.mode & 0o777) | 0o400;
                fs::set_permissions(&out_path, fs::Permissions::from_mode(mode))
                    .map_err(|e| io_err("Failed to chmod", &out_path, e))?;
            }
            Kind::Symlink(target) => {
                let is_abs = target.starts_with('/');
                let final_target = if is_abs {
                    if !opts.allow_absolute {
                        return Err(format!(
                            "Failed to extract {}: absolute symlink targets are not allowed ({})",
                            rel.join("/"),
                            target
                        ));
                    }
                    let tdp = deprefix(target, &prefix_segs, opts.strip_components);
                    join_rel(root, &tdp.segs).to_string_lossy().to_string()
                } else {
                    if noun == "zip" {
                        // check for escaping the extraction root for zip archives
                        let mut depth = rel.len() as i64 - 1;
                        for seg in target.split('/') {
                            match seg {
                                "" | "." => {}
                                ".." => {
                                    depth -= 1;
                                    if depth < 0 {
                                        return Err(format!(
                                            "Zip entries cannot refer to files outside of their directory: {} has a symlink {} pointing to {}",
                                            archive.display(),
                                            rel.join("/"),
                                            target
                                        ));
                                    }
                                }
                                _ => depth += 1,
                            }
                        }
                    }
                    target.clone()
                };
                symlinks.push((out_path, final_target));
            }
            Kind::Hardlink(target) => {
                let tdp = deprefix(target, &prefix_segs, opts.strip_components);
                if tdp.segs.first().map(|s| s == "..").unwrap_or(false) {
                    return Err(format!(
                        "Tar entries cannot refer to files outside of their directory: {} has a link {} pointing to {}",
                        archive.display(),
                        entry.name,
                        target
                    ));
                }
                let resolved = join_rel(root, &tdp.segs);
                if resolved == out_path {
                    continue;
                }
                hardlinks.push((out_path, resolved, target.clone()));
            }
        }
    }

    if !prefix_segs.is_empty() && !found_prefix {
        return Err(prefix_not_found(&opts.strip_prefix, &available));
    }
    // Bazel always passes a (possibly empty) prefix and fails when the archive has no entries.
    if prefix_segs.is_empty() && opts.strip_components == 0 && entries.is_empty() {
        return Err(prefix_not_found(&opts.strip_prefix, &available));
    }

    for (link, target, raw) in hardlinks {
        if let Ok(_) = fs::symlink_metadata(&link) {
            let _ = fs::remove_file(&link);
        }
        if !target.exists() {
            return Err(format!(
                "Failed to extract hard link {} -> {}: target not found in archive",
                link.display(),
                raw
            ));
        }
        if fs::hard_link(&target, &link).is_err() {
            fs::copy(&target, &link).map_err(|e| io_err("Failed to copy hard link target to", &link, e))?;
        }
    }
    for (link, target) in symlinks {
        if let Ok(md) = fs::symlink_metadata(&link) {
            if md.is_dir() {
                if fs::remove_dir(&link).is_err() {
                    return Err(format!(
                        "Failed to create symlink {}: a directory with that name exists",
                        link.display()
                    ));
                }
            } else {
                let _ = fs::remove_file(&link);
            }
        }
        std::os::unix::fs::symlink(&target, &link)
            .map_err(|e| io_err("Failed to create symlink", &link, e))?;
    }
    let _ = archive;
    Ok(())
}

// ===========================================================================
// Tar
// ===========================================================================

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).to_string()
}

fn parse_octal(b: &[u8]) -> Result<u64, String> {
    if !b.is_empty() && b[0] & 0x80 != 0 {
        // GNU base-256
        let mut v: u64 = (b[0] & 0x7f) as u64;
        for &x in &b[1..] {
            v = v.checked_mul(256).ok_or("tar number too large")? | x as u64;
        }
        return Ok(v);
    }
    let s: Vec<u8> = b
        .iter()
        .copied()
        .take_while(|&c| c != 0)
        .collect();
    let s = String::from_utf8_lossy(&s).trim().to_string();
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(&s, 8).map_err(|_| format!("Invalid number in tar header: '{}'", s))
}

fn parse_pax(data: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let mut m = BTreeMap::new();
    let mut pos = 0;
    while pos < data.len() {
        let sp = data[pos..]
            .iter()
            .position(|&c| c == b' ')
            .ok_or("Invalid pax header record")?;
        let len: usize = std::str::from_utf8(&data[pos..pos + sp])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or("Invalid pax header record length")?;
        if len <= sp + 1 || pos + len > data.len() {
            return Err("Invalid pax header record length".to_string());
        }
        let rec = &data[pos + sp + 1..pos + len];
        let rec = rec.strip_suffix(b"\n").unwrap_or(rec);
        if let Some(eq) = rec.iter().position(|&c| c == b'=') {
            m.insert(
                String::from_utf8_lossy(&rec[..eq]).to_string(),
                String::from_utf8_lossy(&rec[eq + 1..]).to_string(),
            );
        }
        pos += len;
    }
    Ok(m)
}

fn parse_tar(buf: &[u8]) -> Result<Vec<Entry>, String> {
    parse_tar_ex(buf).map(|(e, _)| e)
}

/// Like [`parse_tar`], also reporting the offset of the first end-of-archive (zero) record, if reached.
fn parse_tar_ex(buf: &[u8]) -> Result<(Vec<Entry>, Option<usize>), String> {
    let mut eof_at = None;
    let mut pending_x = false;
    let mut entries = Vec::new();
    let mut pos = 0usize;
    let mut pax: BTreeMap<String, String> = BTreeMap::new();
    let mut long_name: Option<String> = None;
    let mut long_link: Option<String> = None;
    while pos + 512 <= buf.len() {
        let hdr = &buf[pos..pos + 512];
        if hdr.iter().all(|&b| b == 0) {
            eof_at = Some(pos);
            break;
        }
        // checksum
        let stored = parse_octal(&hdr[148..156])?;
        let mut sum: u64 = 0;
        for (i, &b) in hdr.iter().enumerate() {
            sum += if (148..156).contains(&i) { b' ' as u64 } else { b as u64 };
        }
        // commons-compress' TarArchiveInputStream does not verify the header checksum (only number fields).
        if false && sum != stored {
            let signed: i64 = hdr
                .iter()
                .enumerate()
                .map(|(i, &b)| if (148..156).contains(&i) { 32 } else { b as i8 as i64 })
                .sum();
            if signed != stored as i64 {
                return Err("Bad tar header checksum: not a valid tar archive".to_string());
            }
        }
        let mut name = cstr(&hdr[0..100]);
        let mode = parse_octal(&hdr[100..108])? as u32;
        let mut size = parse_octal(&hdr[124..136])? as usize;
        let typeflag = hdr[156];
        let mut link = cstr(&hdr[157..257]);
        if &hdr[257..263] == b"ustar\0" {
            let p = cstr(&hdr[345..500]);
            if !p.is_empty() {
                name = format!("{}/{}", p, name);
            }
        }
        if let Some(s) = pax.get("size") {
            if let Ok(v) = s.parse() {
                size = v;
            }
        }
        let data_start = pos + 512;
        let data_end = data_start
            .checked_add(size)
            .filter(|&e| e <= buf.len())
            .ok_or("Unexpected end of tar archive")?;
        pos = data_start + size.div_ceil(512) * 512;
        match typeflag {
            b'x' => {
                pax = parse_pax(&buf[data_start..data_end])?;
                pending_x = true;
                continue;
            }
            b'g' => continue,
            b'L' => {
                long_name = Some(cstr(&buf[data_start..data_end]));
                continue;
            }
            b'K' => {
                long_link = Some(cstr(&buf[data_start..data_end]));
                continue;
            }
            _ => {}
        }
        pending_x = false;
        if let Some(n) = long_name.take() {
            name = n;
        }
        if let Some(l) = long_link.take() {
            link = l;
        }
        // A name from a PAX `path` record is UTF-8; USTAR/GNU names are decoded with Bazel's marked latin-1
        // charset, so commons-compress' `isDirectory()` (name ends with "/") only holds for PAX names.
        let mut name_is_pax = false;
        if let Some(p) = pax.get("path") {
            name = p.clone();
            name_is_pax = true;
        }
        if let Some(l) = pax.get("linkpath") {
            link = l.clone();
        }
        pax.clear();
        let kind = match typeflag {
            0 | b'0' | b'7' => {
                if name_is_pax && name.ends_with('/') {
                    Kind::Dir
                } else {
                    Kind::File
                }
            }
            b'5' => Kind::Dir,
            b'2' => Kind::Symlink(link),
            b'1' => Kind::Hardlink(link),
            b'3' | b'4' | b'6' => Kind::File, // char, block, fifo: real bazel creates empty regular files
            _ => continue,
        };
        let data = if matches!(kind, Kind::File) {
            if matches!(typeflag, b'3' | b'4' | b'6') {
                Data::None
            } else {
                Data::Raw(data_start, size)
            }
        } else {
            Data::None
        };
        entries.push(Entry { name, kind, mode, data });
    }
    if pending_x {
        return Err("premature end of tar archive. Didn't find any entry after PAX header.".to_string());
    }
    Ok((entries, eof_at))
}

/// Emulates Bazel's streaming decompressor + `TarArchiveInputStream`: a codec error that happens after
/// everything the tar reader consumes (entries, end-of-archive records and the rest of the last 10240-byte
/// block) was already decoded is never observed, so the decoded prefix is used as the tar stream.
fn tar_stream_or_error(res: Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
    match res {
        Ok(v) => Ok(v),
        Err(e) => {
            let Some(mut p) = crate::codecs::take_partial() else { return Err(e) };
            // Errors raised while finishing a stream (checksum / trailing garbage) are thrown by the very
            // `read` call that would have returned the last 512-byte tar record, so that record is lost.
            let eager_end = ["Gzip CRC32", "Gzip ISIZE", "Garbage after", "bzip2 block CRC", "bzip2 stream CRC", "XZ block CRC", "XZ block SHA", "Zstd content checksum mismatch"]
                .iter()
                .any(|m| e.starts_with(m));
            if eager_end {
                let keep = p.len().saturating_sub(1) / 512 * 512;
                p.truncate(keep);
            }
            match parse_tar_ex(&p) {
                Ok((_, Some(eof))) => {
                    let end = eof + 1024;
                    let consumed = end.div_ceil(10240) * 10240;
                    if consumed <= p.len() { Ok(p) } else { Err(e) }
                }
                Ok((_, None)) => Err(e),
                Err(te) if te == "Unexpected end of tar archive" || te.starts_with("premature end of tar archive") => Err(e),
                Err(_) => Ok(p),
            }
        }
    }
}

fn extract_tar(buf: &[u8], archive: &Path, root: &Path, opts: &ExtractOptions) -> Result<(), String> {
    let entries = parse_tar(buf)?;
    extract_entries(buf, entries, archive, root, opts, "tar")
}

// ===========================================================================
// Zip
// ===========================================================================

fn u16le(b: &[u8], o: usize) -> Result<u16, String> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| "Truncated zip file".to_string())
}
fn u32le(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| "Truncated zip file".to_string())
}
fn u64le(b: &[u8], o: usize) -> Result<u64, String> {
    b.get(o..o + 8)
        .map(|s| {
            let mut a = [0u8; 8];
            a.copy_from_slice(s);
            u64::from_le_bytes(a)
        })
        .ok_or_else(|| "Truncated zip file".to_string())
}

/// Java's `ZipReader.findEndOfCentralDirectoryRecord`: scans backwards for an EOCD signature whose comment length
/// matches the number of bytes remaining in the file.
fn find_zip_eocd(buf: &[u8]) -> Option<usize> {
    if buf.len() < 22 {
        return None;
    }
    let mut i = buf.len() - 22;
    loop {
        if buf[i..i + 4] == [0x50, 0x4b, 0x05, 0x06] {
            let clen = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
            if i + 22 + clen == buf.len() {
                return Some(i);
            }
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

fn parse_zip(buf: &[u8], archive: &Path) -> Result<Vec<Entry>, String> {
    let base = archive.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let malformed = || format!("Zip file '{base}' is malformed. It does not contain an end of central directory record.");
    let eocd = find_zip_eocd(buf).ok_or_else(malformed)?;
    let mut total = u16le(buf, eocd + 10)? as u64;
    let cd_size = u32le(buf, eocd + 12)? as u64;
    let mut cd_off = u32le(buf, eocd + 16)? as u64;
    let mut zip64 = false;
    if (total == 0xFFFF || cd_off == 0xFFFF_FFFF) && eocd >= 20 && buf[eocd - 20..eocd - 16] == [0x50, 0x4b, 0x06, 0x07] {
        let z64 = u64le(buf, eocd - 20 + 8)? as usize;
        if buf.get(z64..z64 + 4) == Some(&[0x50, 0x4b, 0x06, 0x06]) {
            total = u64le(buf, z64 + 32)?;
            cd_off = u64le(buf, z64 + 48)?;
            zip64 = true;
        }
    }
    // Java: unless Zip64, trust `eocd - cdSize` when its low 32 bits match the recorded offset and then read
    // headers until `cdSize` bytes were consumed; otherwise read the recorded number of entries.
    let computed = eocd as i64 - cd_size as i64;
    let (mut pos, by_size) = if !zip64 && computed >= 0 && (computed as u32) == (cd_off as u32) {
        (computed as usize, true)
    } else {
        (cd_off as usize, false)
    };
    let start_pos = pos;
    let eof_err = || "Unexpected end of file while reading Central Directory File Header.".to_string();
    let mut entries = Vec::new();
    let mut n = 0u64;
    loop {
        if by_size {
            if ((pos - start_pos) as u64) >= cd_size {
                break;
            }
        } else if n >= total {
            break;
        }
        n += 1;
        let fixed = buf.get(pos..pos + 46).ok_or_else(eof_err)?;
        if fixed[0..4] != [0x50, 0x4b, 0x01, 0x02] {
            return Err("Malformed Central Directory File Header; does not start with 02014b50".to_string());
        }
        let made_by = u16le(buf, pos + 4)?;
        let flags = u16le(buf, pos + 8)?;
        let method = u16le(buf, pos + 10)?;
        let mut csize = u32le(buf, pos + 20)? as u64;
        let mut usize_ = u32le(buf, pos + 24)? as u64;
        let nlen = u16le(buf, pos + 28)? as usize;
        let elen = u16le(buf, pos + 30)? as usize;
        let clen = u16le(buf, pos + 32)? as usize;
        let eattr = u32le(buf, pos + 38)?;
        let mut lho = u32le(buf, pos + 42)? as u64;
        let name_bytes = buf.get(pos + 46..pos + 46 + nlen).ok_or_else(eof_err)?;
        let extra = buf.get(pos + 46 + nlen..pos + 46 + nlen + elen).ok_or_else(eof_err)?;
        buf.get(pos + 46 + nlen + elen..pos + 46 + nlen + elen + clen).ok_or_else(eof_err)?;
        let name = String::from_utf8_lossy(name_bytes).to_string();
        pos += 46 + nlen + elen + clen;
        if csize == 0xFFFF_FFFF || usize_ == 0xFFFF_FFFF || lho == 0xFFFF_FFFF {
            let mut e = 0;
            while e + 4 <= extra.len() {
                let id = u16le(extra, e)?;
                let sz = u16le(extra, e + 2)? as usize;
                if id == 1 {
                    let mut p = e + 4;
                    if usize_ == 0xFFFF_FFFF {
                        usize_ = u64le(extra, p)?;
                        p += 8;
                    }
                    if csize == 0xFFFF_FFFF {
                        csize = u64le(extra, p)?;
                        p += 8;
                    }
                    if lho == 0xFFFF_FFFF {
                        lho = u64le(extra, p)?;
                    }
                }
                e += 4 + sz;
            }
        }
        if flags & 1 != 0 {
            // Java's ZipReader does not look at the encryption flag; the (garbage) data is inflated as-is.
        }
        let _ = csize;

        // permissions (Bazel ZipDecompressor.getPermissions)
        let unix = (made_by >> 8) == 3;
        let mut perm = eattr >> 16;
        let is_dir_name = name.ends_with('/');
        let mut kind = Kind::File;
        if is_dir_name {
            kind = Kind::Dir;
            perm = 0o755;
        } else if unix && perm != 0 {
            match perm & 0o170000 {
                0o040000 => kind = Kind::Dir,
                0o120000 => kind = Kind::Symlink(String::new()),
                _ => {}
            }
        } else {
            perm = 0o644;
            if eattr & 0x10 != 0 {
                kind = Kind::Dir;
            }
        }
        let data = Data::Zip(lho as usize, usize_ as usize, method, base.clone());
        if let Kind::Symlink(_) = kind {
            let target = entry_data(buf, &data, archive, &name)?;
            if target.len() > 4096 {
                return Err(format!("Symlink target of zip entry {} is too long", name));
            }
            kind = Kind::Symlink(String::from_utf8_lossy(&target).to_string());
        }
        entries.push(Entry { name, kind, mode: perm, data });
    }
    Ok(entries)
}

/// Reads one zip entry the way Java's `ZipEntryInputStream` does: no CRC or size verification, the inflater is
/// not bounded by the compressed size, and only `size` bytes are ever requested.
fn zip_entry_bytes(buf: &[u8], lho: usize, size: usize, method: u16, base: &str) -> Result<Vec<u8>, String> {
    let sig_ok = buf.get(lho..lho + 4) == Some(&[0x50, 0x4b, 0x03, 0x04]);
    if !sig_ok || buf.len() < lho + 30 {
        return Err(format!(
            "The file '{base}' is not a correctly formatted zip file: Expected a File Header at file offset {lho}, but was not present."
        ));
    }
    let lname = u16le(buf, lho + 26)? as usize;
    let lextra = u16le(buf, lho + 28)? as usize;
    let start = (lho + 30 + lname + lextra).min(buf.len());
    if size == 0 {
        return Ok(Vec::new());
    }
    match method {
        8 | 9 => {
            let r = if method == 8 { inflate_limit(&buf[start..], size) } else { inflate64_limit(&buf[start..], size) };
            match r {
                Ok((out, _)) => Ok(out),
                Err(e) if e == "Deflate output exceeds size limit" => {
                    let mut p = crate::codecs::take_partial().unwrap_or_default();
                    p.truncate(size);
                    Ok(p)
                }
                Err(e) => Err(java_zlib_message(&e)),
            }
        }
        _ => Ok(buf[start..(start.saturating_add(size)).min(buf.len())].to_vec()),
    }
}

fn java_zlib_message(e: &str) -> String {
    match e {
        "Unexpected end of deflate stream" => "Unexpected end of ZLIB input stream".to_string(),
        "Invalid deflate stream: distance too far back" => "invalid distance too far back".to_string(),
        "Invalid deflate stream: stored block length mismatch" => "invalid stored block lengths".to_string(),
        "Invalid deflate stream: bad block type" => "invalid block type".to_string(),
        "Invalid deflate stream: bad distance code" => "invalid distance code".to_string(),
        "Invalid deflate stream: bad length code" => "invalid literal/length code".to_string(),
        "Invalid deflate stream: too many length or distance codes" => "too many length or distance symbols".to_string(),
        "Invalid deflate stream: repeat with no previous length" => "invalid bit length repeat".to_string(),
        "Invalid deflate stream: too many code lengths" => "invalid bit length repeat".to_string(),
        "Invalid deflate stream: missing end-of-block code" => "invalid code -- missing end-of-block".to_string(),
        "Invalid deflate stream: oversubscribed Huffman code" => "invalid code lengths set".to_string(),
        other => other.to_string(),
    }
}

fn extract_zip(buf: &[u8], archive: &Path, root: &Path, opts: &ExtractOptions) -> Result<(), String> {
    let entries = parse_zip(buf, archive)?;
    extract_entries(buf, entries, archive, root, opts, "zip")
}

// ===========================================================================
// Ar
// ===========================================================================

fn extract_ar(buf: &[u8], archive: &Path, root: &Path, opts: &ExtractOptions) -> Result<(), String> {
    // commons-compress ArArchiveInputStream
    if buf.len() < 8 {
        return Err(format!("Failed to read header. Occurred at byte: {}", buf.len()));
    }
    if &buf[..8] != b"!<arch>\n" {
        let ascii: String = buf[..8].iter().map(|&b| if b < 0x80 { b as char } else { '?' }).collect();
        return Err(format!("Invalid header {ascii}"));
    }
    let mut pos = 8;
    let mut gnu_names: Vec<u8> = Vec::new();
    fs::create_dir_all(root).map_err(|e| io_err("Failed to create directory", root, e))?;
    loop {
        if pos % 2 != 0 {
            if pos >= buf.len() {
                break;
            }
            pos += 1;
        }
        if pos >= buf.len() {
            break;
        }
        if buf.len() - pos < 58 {
            return Err("Truncated ar archive".to_string());
        }
        if buf.len() - pos < 60 {
            return Err(format!("Failed to read entry trailer. Occurred at byte: {}", buf.len()));
        }
        if &buf[pos + 58..pos + 60] != b"`\n" {
            return Err(format!("Invalid entry trailer. not read the content? Occurred at byte: {}", pos + 60));
        }
        let h = &buf[pos..pos + 60];
        let field = |a: usize, b: usize| String::from_utf8_lossy(&h[a..b]).trim().to_string();
        let mut name = field(0, 16);
        let ar_num = |a: usize, b: usize, radix: u32, ty: &str| -> Result<i64, String> {
            let t: String = h[a..b].iter().map(|&c| if c < 0x80 { c as char } else { '?' }).collect();
            let t = t.trim_matches(|c: char| c <= ' ');
            if t.is_empty() {
                return Ok(0);
            }
            i64::from_str_radix(t, radix).map_err(|_| format!("Unable to parse {ty} from string value: {t}"))
        };
        ar_num(16, 28, 10, "long")?;
        ar_num(28, 34, 10, "int")?;
        ar_num(34, 40, 10, "int")?;
        let mode = ar_num(40, 48, 8, "int")? as u32;
        let mut size: usize = ar_num(48, 58, 10, "long")?.max(0) as usize;
        let mut dstart = pos + 60;
        // A truncated data section is silently cut short (the reader just hits end of file).
        let size_full = size;
        if dstart + size > buf.len() {
            size = buf.len().saturating_sub(dstart);
        }
        pos = dstart + size_full;
        if name == "//" {
            gnu_names = buf[dstart..dstart + size].to_vec();
            continue;
        }
        if name == "/" || name == "/SYM64/" || name.starts_with("__.SYMDEF") {
            continue;
        }
        if let Some(n) = name.strip_prefix("#1/") {
            let n: usize = n.trim().parse().map_err(|_| format!("For input string: \"{}\"", n.trim()))?;
            if dstart + n > buf.len() {
                return Err("null".to_string());
            }
            if n > size {
                return Err("Invalid BSD ar name length".to_string());
            }
            name = cstr(&buf[dstart..dstart + n]);
            if name.starts_with("__.SYMDEF") {
                // Bazel fails on BSD symbol-table members ("__.SYMDEF SORTED")
                return Err("Invalid file path".to_string());
            }
            dstart += n;
            size -= n;
        } else if let Some(off) = name.strip_prefix('/') {
            if let Ok(off) = off.parse::<usize>() {
                let rest = gnu_names.get(off..).ok_or("Invalid GNU ar name offset")?;
                let end = rest.iter().position(|&c| c == b'\n').unwrap_or(rest.len());
                name = String::from_utf8_lossy(&rest[..end]).trim_end_matches('/').to_string();
            }
        } else if let Some(n) = name.strip_suffix('/') {
            name = n.to_string();
        }
        let name = opts.rename_files.get(&name).cloned().unwrap_or(name);
        let segs = normalize(&name);
        if segs.is_empty() || segs[0] == ".." {
            return Err(format!("Failed to extract {}, path is escaping the destination directory", name));
        }
        let out = join_rel(root, &segs);
        if let Some(p) = out.parent() {
            fs::create_dir_all(p).map_err(|e| io_err("Failed to create directory", p, e))?;
        }
        fs::write(&out, &buf[dstart..dstart + size]).map_err(|e| io_err("Failed to write", &out, e))?;
        fs::set_permissions(&out, fs::Permissions::from_mode((mode & 0o777) | 0o400))
            .map_err(|e| io_err("Failed to chmod", &out, e))?;
    }
    Ok(())
}

// ===========================================================================
// Public entry point
// ===========================================================================

/// Extract `archive` into `dest` following Bazel's decompressor semantics.
pub fn extract(archive: &Path, dest: &Path, opts: &ExtractOptions) -> Result<(), String> {
    if !opts.strip_prefix.is_empty() && opts.strip_components != 0 {
        return Err("Only one of 'strip_prefix' or 'strip_components' can be set".to_string());
    }
    let kind = match &opts.archive_type {
        Some(t) if !t.is_empty() => kind_for_type(t)?,
        _ => detect_kind(archive)?,
    };
    let add = normalize(&opts.add_prefix);
    if add.first().map(|s| s == "..").unwrap_or(false) {
        return Err(format!("add_prefix '{}' escapes the destination directory", opts.add_prefix));
    }
    let root = join_rel(dest, &add);
    let raw = fs::read(archive).map_err(|e| io_err("Error reading archive", archive, e))?;
    match kind {
        ArchiveKind::Zip => extract_zip(&raw, archive, &root, opts),
        ArchiveKind::Tar => extract_tar(&raw, archive, &root, opts),
        ArchiveKind::TarGz => {
            let tar = tar_stream_or_error(gunzip(&raw)).map_err(|e| format!("{}: {}", archive.display(), e))?;
            drop(raw);
            extract_tar(&tar, archive, &root, opts)
        }
        ArchiveKind::Gz => {
            let (data, fname) = gunzip_with_header(&raw).map_err(|e| format!("{}: {}", archive.display(), e))?;
            extract_single_file(data, fname, ".gz", archive, &root, opts)
        }
        ArchiveKind::TarXz => {
            let tar = tar_stream_or_error(crate::codecs::xz_decompress(&raw)).map_err(|e| format!("{}: {}", archive.display(), e))?;
            drop(raw);
            extract_tar(&tar, archive, &root, opts)
        }
        ArchiveKind::Xz => {
            let data = crate::codecs::xz_decompress(&raw).map_err(|e| format!("{}: {}", archive.display(), e))?;
            extract_single_file(data, None, ".xz", archive, &root, opts)
        }
        ArchiveKind::TarZst => {
            let tar = tar_stream_or_error(crate::codecs::zstd_decompress(&raw)).map_err(|e| format!("{}: {}", archive.display(), e))?;
            drop(raw);
            extract_tar(&tar, archive, &root, opts)
        }
        ArchiveKind::Zst => {
            let data = crate::codecs::zstd_decompress(&raw).map_err(|e| format!("{}: {}", archive.display(), e))?;
            extract_single_file(data, None, ".zst", archive, &root, opts)
        }
        ArchiveKind::TarBz2 => {
            let tar = tar_stream_or_error(crate::codecs::bunzip2(&raw)).map_err(|e| format!("{}: {}", archive.display(), e))?;
            drop(raw);
            extract_tar(&tar, archive, &root, opts)
        }
        ArchiveKind::Bz2 => {
            let data = crate::codecs::bunzip2(&raw).map_err(|e| format!("{}: {}", archive.display(), e))?;
            extract_single_file(data, None, ".bz2", archive, &root, opts)
        }
        ArchiveKind::SevenZ => crate::codecs::extract_7z(&raw, archive, &root, opts),
        ArchiveKind::Ar => extract_ar(&raw, archive, &root, opts),
        k => Err(unsupported(k)),
    }
}

fn extract_single_file(
    data: Vec<u8>,
    header_name: Option<String>,
    suffix: &str,
    archive: &Path,
    root: &Path,
    opts: &ExtractOptions,
) -> Result<(), String> {
    let name = if let Some(n) = header_name {
        n
    } else {
        let base = archive.file_name().unwrap().to_string_lossy().to_string();
        base.strip_suffix(suffix).unwrap_or(&base).to_string()
    };
    let name = opts.rename_files.get(&name).cloned().unwrap_or(name);
    let segs = normalize(&name);
    if segs.is_empty() || segs[0] == ".." {
        return Err(format!("Failed to extract {}, path is escaping the destination directory", name));
    }
    let out = join_rel(root, &segs);
    if let Some(p) = out.parent() {
        fs::create_dir_all(p).map_err(|e| io_err("Failed to create directory", p, e))?;
    }
    fs::write(&out, data).map_err(|e| io_err("Failed to write", &out, e))?;
    fs::set_permissions(&out, fs::Permissions::from_mode(0o644))
        .map_err(|e| io_err("Failed to chmod", &out, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn tmpdir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("zel-archive-test-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn have(cmd: &str) -> bool {
        Command::new(cmd).arg("--help").output().is_ok()
    }

    // --- tiny builders for tar ---
    fn tar_header(name: &str, typeflag: u8, size: usize, mode: u32, link: &str) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..name.len().min(100)].copy_from_slice(&name.as_bytes()[..name.len().min(100)]);
        h[100..108].copy_from_slice(format!("{:07o}\0", mode).as_bytes());
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", size).as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = typeflag;
        h[157..157 + link.len()].copy_from_slice(link.as_bytes());
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{:06o}\0 ", sum).as_bytes());
        h
    }
    fn tar_add(t: &mut Vec<u8>, name: &str, typeflag: u8, data: &[u8], mode: u32, link: &str) {
        t.extend(tar_header(name, typeflag, data.len(), mode, link));
        t.extend_from_slice(data);
        t.extend(vec![0u8; (512 - data.len() % 512) % 512]);
    }
    fn tar_end(t: &mut Vec<u8>) {
        t.extend(vec![0u8; 1024]);
    }

    // --- stored-block gzip/zip builders ---
    fn deflate_stored(data: &[u8]) -> Vec<u8> {
        let mut o = Vec::new();
        let chunks: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(65535).collect() };
        for (i, c) in chunks.iter().enumerate() {
            o.push(if i + 1 == chunks.len() { 1 } else { 0 });
            o.extend_from_slice(&(c.len() as u16).to_le_bytes());
            o.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
            o.extend_from_slice(c);
        }
        o
    }
    fn gzip_stored(data: &[u8]) -> Vec<u8> {
        let mut o = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
        o.extend(deflate_stored(data));
        o.extend_from_slice(&crc32(data).to_le_bytes());
        o.extend_from_slice(&(data.len() as u32).to_le_bytes());
        o
    }
    struct ZE<'a> {
        name: &'a str,
        data: &'a [u8],
        attr: u32,
        deflate: bool,
    }
    fn build_zip(entries: &[ZE]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for e in entries {
            let off = out.len() as u32;
            let (method, body) = if e.deflate { (8u16, deflate_stored(e.data)) } else { (0u16, e.data.to_vec()) };
            let crc = crc32(e.data);
            out.extend_from_slice(&[0x50, 0x4b, 3, 4, 20, 0, 0, 0]);
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0, 0, 0, 0]);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(e.name.as_bytes());
            out.extend_from_slice(&body);
            cd.extend_from_slice(&[0x50, 0x4b, 1, 2, 20, 3, 20, 0, 0, 0]);
            cd.extend_from_slice(&method.to_le_bytes());
            cd.extend_from_slice(&[0, 0, 0, 0]);
            cd.extend_from_slice(&crc.to_le_bytes());
            cd.extend_from_slice(&(body.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
            cd.extend_from_slice(&e.attr.to_le_bytes());
            cd.extend_from_slice(&off.to_le_bytes());
            cd.extend_from_slice(e.name.as_bytes());
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&cd);
        out.extend_from_slice(&[0x50, 0x4b, 5, 6, 0, 0, 0, 0]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    // --- known vectors (generated with gzip(1) / zlib) ---
    // gzip -9 of b"hello hello hello hello\n" (fixed Huffman with back-reference)
    fn hello_gz() -> Vec<u8> {
        let out = Command::new("sh")
            .arg("-c")
            .arg("printf 'hello hello hello hello\\n' | gzip -9n")
            .output();
        match out {
            Ok(o) if o.status.success() => o.stdout,
            _ => Vec::new(),
        }
    }

    #[test]
    fn crc32_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn inflate_stored_and_errors() {
        let (o, used) = inflate(&deflate_stored(b"abc")).unwrap();
        assert_eq!(o, b"abc");
        assert_eq!(used, 8);
        assert!(inflate(&[]).is_err());
        assert!(inflate(&[0x07]).is_err()); // bad block type 3
        assert!(inflate(&[0x01, 0x03, 0x00, 0x00, 0x00]).is_err()); // nlen mismatch
        assert!(inflate(&[0x01, 0x03, 0x00, 0xfc, 0xff, b'a']).is_err()); // truncated
    }

    #[test]
    fn inflate_fixed_huffman() {
        // zlib raw deflate of "a" * 10, fixed huffman: 4b 4c 84 01 00
        let (o, _) = inflate(&[0x4b, 0x4c, 0x84, 0x01, 0x00]).unwrap();
        assert_eq!(o, b"aaaaaaaaaa");
        // inflate_limit
        assert!(inflate_limit(&[0x4b, 0x4c, 0x84, 0x01, 0x00], 5).is_err());
    }

    #[test]
    fn inflate_garbage_does_not_panic() {
        let mut seed = 12345u32;
        for len in 0..200 {
            let mut v = Vec::new();
            for _ in 0..len {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                v.push((seed >> 24) as u8);
            }
            let _ = inflate(&v);
            let _ = gunzip(&v);
            let _ = parse_tar(&v);
            let _ = parse_zip(&v, Path::new("fuzz.zip"));
        }
    }

    #[test]
    fn gzip_roundtrip_system() {
        let gz = hello_gz();
        if gz.is_empty() {
            return;
        }
        assert_eq!(gunzip(&gz).unwrap(), b"hello hello hello hello\n");
        let mut bad = gz.clone();
        let n = bad.len();
        bad[n - 8] ^= 0xff;
        assert!(gunzip(&bad).unwrap_err().contains("CRC"));
    }

    #[test]
    fn gzip_header_flags_and_multimember() {
        // FNAME + FCOMMENT + FEXTRA + FHCRC
        let mut g = vec![0x1f, 0x8b, 8, 2 | 4 | 8 | 16, 0, 0, 0, 0, 0, 3];
        g.extend_from_slice(&[2, 0, 0xAA, 0xBB]); // extra
        g.extend_from_slice(b"name\0");
        g.extend_from_slice(b"comment\0");
        g.extend_from_slice(&[0, 0]); // hcrc
        g.extend(deflate_stored(b"xyz"));
        g.extend_from_slice(&crc32(b"xyz").to_le_bytes());
        g.extend_from_slice(&3u32.to_le_bytes());
        g.extend(gzip_stored(b"123"));
        assert_eq!(gunzip(&g).unwrap(), b"xyz123");
        assert!(gunzip(b"not gzip at all, really not").is_err());
        assert!(gunzip(&g[..12]).is_err());
    }

    #[test]
    fn tar_extract_basic() {
        let d = tmpdir("tar-basic");
        let mut t = Vec::new();
        tar_add(&mut t, "pkg/", b'5', b"", 0o755, "");
        tar_add(&mut t, "pkg/a.txt", b'0', b"hello", 0o644, "");
        tar_add(&mut t, "pkg/bin/run.sh", b'0', b"#!/bin/sh\n", 0o755, "");
        tar_add(&mut t, "pkg/link", b'2', b"", 0o777, "a.txt");
        tar_add(&mut t, "pkg/hard", b'1', b"", 0o644, "pkg/a.txt");
        tar_end(&mut t);
        fs::write(d.join("x.tar"), &t).unwrap();
        extract(&d.join("x.tar"), &d.join("out"), &ExtractOptions::default()).unwrap();
        assert_eq!(fs::read(d.join("out/pkg/a.txt")).unwrap(), b"hello");
        let m = fs::metadata(d.join("out/pkg/bin/run.sh")).unwrap().permissions().mode();
        assert_eq!(m & 0o111, 0o111);
        assert_eq!(fs::read_link(d.join("out/pkg/link")).unwrap(), PathBuf::from("a.txt"));
        assert_eq!(fs::read(d.join("out/pkg/link")).unwrap(), b"hello");
        assert_eq!(fs::read(d.join("out/pkg/hard")).unwrap(), b"hello");
    }

    #[test]
    fn tar_gz_strip_prefix_add_prefix_rename() {
        let d = tmpdir("tar-gz-strip");
        let mut t = Vec::new();
        tar_add(&mut t, "top-1.0/", b'5', b"", 0o755, "");
        tar_add(&mut t, "top-1.0/src/x.c", b'0', b"int x;", 0o644, "");
        tar_add(&mut t, "other/y", b'0', b"y", 0o644, "");
        tar_add(&mut t, "top-1.0/lnk", b'2', b"", 0o777, "/top-1.0/src/x.c");
        tar_end(&mut t);
        fs::write(d.join("x.tar.gz"), gzip_stored(&t)).unwrap();
        let mut o = ExtractOptions::default();
        o.strip_prefix = "top-1.0".into();
        o.add_prefix = "sub".into();
        extract(&d.join("x.tar.gz"), &d.join("out"), &o).unwrap();
        assert_eq!(fs::read(d.join("out/sub/src/x.c")).unwrap(), b"int x;");
        assert!(!d.join("out/sub/other").exists());
        assert!(!d.join("out/sub/y").exists());
        // absolute symlink is re-rooted with prefix stripped
        let l = fs::read_link(d.join("out/sub/lnk")).unwrap();
        assert_eq!(l, d.join("out/sub/src/x.c"));

        let mut o = ExtractOptions::default();
        o.rename_files.insert("other/y".into(), "renamed/z".into());
        extract(&d.join("x.tar.gz"), &d.join("out2"), &o).unwrap();
        assert_eq!(fs::read(d.join("out2/renamed/z")).unwrap(), b"y");
        assert!(!d.join("out2/other").exists());
    }

    #[test]
    fn strip_components() {
        let d = tmpdir("strip-comp");
        let mut t = Vec::new();
        tar_add(&mut t, "a/b/c.txt", b'0', b"c", 0o644, "");
        tar_add(&mut t, "a/top.txt", b'0', b"t", 0o644, "");
        tar_end(&mut t);
        fs::write(d.join("x.tar"), &t).unwrap();
        let mut o = ExtractOptions::default();
        o.strip_components = 1;
        extract(&d.join("x.tar"), &d.join("out"), &o).unwrap();
        assert_eq!(fs::read(d.join("out/b/c.txt")).unwrap(), b"c");
        assert_eq!(fs::read(d.join("out/top.txt")).unwrap(), b"t");
        o.strip_prefix = "a".into();
        assert!(extract(&d.join("x.tar"), &d.join("out3"), &o).unwrap_err().contains("Only one"));
    }

    #[test]
    fn prefix_not_found_message() {
        let d = tmpdir("prefix-nf");
        let mut t = Vec::new();
        tar_add(&mut t, "alpha/f", b'0', b"1", 0o644, "");
        tar_add(&mut t, "beta/g", b'0', b"1", 0o644, "");
        tar_add(&mut t, "rootfile", b'0', b"1", 0o644, "");
        tar_end(&mut t);
        fs::write(d.join("x.tar"), &t).unwrap();
        let mut o = ExtractOptions::default();
        o.strip_prefix = "nope".into();
        let e = extract(&d.join("x.tar"), &d.join("out"), &o).unwrap_err();
        assert_eq!(
            e,
            "Prefix \"nope\" was given, but not found in the archive. Here are possible prefixes for this archive: \"alpha\", \"beta\"."
        );
        let mut t = Vec::new();
        tar_add(&mut t, "rootfile", b'0', b"1", 0o644, "");
        tar_end(&mut t);
        fs::write(d.join("y.tar"), &t).unwrap();
        let e = extract(&d.join("y.tar"), &d.join("out"), &o).unwrap_err();
        assert!(e.contains("We could not find any directory in this archive"));
    }

    #[test]
    fn path_traversal_rejected() {
        let d = tmpdir("traversal");
        let mut t = Vec::new();
        tar_add(&mut t, "../evil", b'0', b"x", 0o644, "");
        tar_end(&mut t);
        fs::write(d.join("x.tar"), &t).unwrap();
        let e = extract(&d.join("x.tar"), &d.join("out"), &ExtractOptions::default()).unwrap_err();
        assert!(e.contains("escaping the destination directory"), "{}", e);
        assert!(!d.join("evil").exists());

        let mut t = Vec::new();
        tar_add(&mut t, "a/l", b'2', b"", 0o777, "../../etc/passwd");
        tar_end(&mut t);
        fs::write(d.join("y.tar"), &t).unwrap();
        // Tar symlinks to outside extraction root extract like real Bazel and do not error.
        extract(&d.join("y.tar"), &d.join("out2"), &ExtractOptions::default()).unwrap();
        assert!(fs::read_link(d.join("out2/a/l")).is_ok());

        // `a/../../b` normalises to ../b
        let mut t = Vec::new();
        tar_add(&mut t, "a/../../b", b'0', b"x", 0o644, "");
        tar_end(&mut t);
        fs::write(d.join("z.tar"), &t).unwrap();
        assert!(extract(&d.join("z.tar"), &d.join("out3"), &ExtractOptions::default()).is_err());

        // absolute entry names are made relative
        let mut t = Vec::new();
        tar_add(&mut t, "/abs/f", b'0', b"x", 0o644, "");
        tar_end(&mut t);
        fs::write(d.join("w.tar"), &t).unwrap();
        extract(&d.join("w.tar"), &d.join("out4"), &ExtractOptions::default()).unwrap();
        assert!(d.join("out4/abs/f").exists());
    }

    #[test]
    fn tar_pax_and_gnu_longname() {
        let d = tmpdir("tar-pax");
        let long = format!("dir/{}", "n".repeat(150));
        let mut t = Vec::new();
        // GNU long name
        let mut ln = long.clone().into_bytes();
        ln.push(0);
        tar_add(&mut t, "././@LongLink", b'L', &ln, 0o644, "");
        tar_add(&mut t, "truncated", b'0', b"gnu", 0o644, "");
        // pax path
        let rec = format!("path=pax/{}\n", "p".repeat(120));
        let full = format!("{} {}", rec.len() + 3 + 1 + 0, rec);
        // length includes digits of itself: compute properly
        let mut n = rec.len() + 2;
        loop {
            let s = format!("{} {}", n, rec);
            if s.len() == n {
                break;
            }
            n = s.len();
        }
        let _ = full;
        let rec_full = format!("{} {}", n, rec);
        tar_add(&mut t, "PaxHeaders/x", b'x', rec_full.as_bytes(), 0o644, "");
        tar_add(&mut t, "short", b'0', b"pax", 0o644, "");
        tar_end(&mut t);
        fs::write(d.join("x.tar"), &t).unwrap();
        extract(&d.join("x.tar"), &d.join("out"), &ExtractOptions::default()).unwrap();
        assert_eq!(fs::read(d.join("out").join(&long)).unwrap(), b"gnu");
        assert_eq!(fs::read(d.join(format!("out/pax/{}", "p".repeat(120)))).unwrap(), b"pax");
    }

    #[test]
    fn tar_bad_input() {
        let d = tmpdir("tar-bad");
        fs::write(d.join("x.tar"), vec![b'x'; 1024]).unwrap();
        let e = extract(&d.join("x.tar"), &d.join("out"), &ExtractOptions::default()).unwrap_err();
        assert!(e.contains("checksum") || e.contains("Invalid number"), "{}", e);
        // truncated data
        let mut t = Vec::new();
        t.extend(tar_header("f", b'0', 5000, 0o644, ""));
        t.extend(vec![0u8; 100]);
        fs::write(d.join("y.tar"), &t).unwrap();
        assert!(extract(&d.join("y.tar"), &d.join("out"), &ExtractOptions::default())
            .unwrap_err()
            .contains("Unexpected end"));
        // missing file
        assert!(extract(&d.join("nope.tar"), &d.join("out"), &ExtractOptions::default()).is_err());
    }

    #[test]
    fn zip_extract_basic() {
        let d = tmpdir("zip-basic");
        let z = build_zip(&[
            ZE { name: "root/", data: b"", attr: 0o040755 << 16, deflate: false },
            ZE { name: "root/a.txt", data: b"aaa", attr: 0o100644 << 16, deflate: true },
            ZE { name: "root/run", data: b"#!/bin/sh", attr: 0o100755 << 16, deflate: false },
            ZE { name: "root/ln", data: b"a.txt", attr: 0o120777 << 16, deflate: false },
            ZE { name: "other/z", data: b"z", attr: 0o100644 << 16, deflate: false },
        ]);
        fs::write(d.join("x.zip"), &z).unwrap();
        let mut o = ExtractOptions::default();
        o.strip_prefix = "root".into();
        extract(&d.join("x.zip"), &d.join("out"), &o).unwrap();
        assert_eq!(fs::read(d.join("out/a.txt")).unwrap(), b"aaa");
        assert!(fs::metadata(d.join("out/run")).unwrap().permissions().mode() & 0o100 != 0);
        assert_eq!(fs::read_link(d.join("out/ln")).unwrap(), PathBuf::from("a.txt"));
        assert!(!d.join("out/other").exists());
        // jar with forced type from odd name
        fs::write(d.join("blob.dat"), &z).unwrap();
        let mut o = ExtractOptions::default();
        o.archive_type = Some("jar".into());
        extract(&d.join("blob.dat"), &d.join("out2"), &o).unwrap();
        assert!(d.join("out2/other/z").exists());
        // prefix error lists all
        let mut o = ExtractOptions::default();
        o.strip_prefix = "x".into();
        let e = extract(&d.join("x.zip"), &d.join("out3"), &o).unwrap_err();
        assert!(e.contains("\"other\", \"root\"."), "{}", e);
    }

    #[test]
    fn zip_errors() {
        let d = tmpdir("zip-err");
        fs::write(d.join("x.zip"), b"hello world, not a zip file at all").unwrap();
        assert!(extract(&d.join("x.zip"), &d.join("o"), &ExtractOptions::default()).is_err());
        let mut z = build_zip(&[ZE { name: "f", data: b"data", attr: 0, deflate: false }]);
        z[30 + 1 + 1] ^= 0xff; // corrupt data (offset 30 + name len 1 = data start 31)
        fs::write(d.join("y.zip"), &z).unwrap();
        // Java's ZipReader neither verifies CRCs nor sizes of stored entries, so a corrupt payload extracts.
        assert!(extract(&d.join("y.zip"), &d.join("o"), &ExtractOptions::default()).is_ok());
        let z = build_zip(&[ZE { name: "../f", data: b"data", attr: 0, deflate: false }]);
        fs::write(d.join("z.zip"), &z).unwrap();
        assert!(extract(&d.join("z.zip"), &d.join("o"), &ExtractOptions::default())
            .unwrap_err()
            .contains("escaping"));
        // default perms when no unix attrs: readable
        let z = build_zip(&[ZE { name: "plain", data: b"p", attr: 0, deflate: false }]);
        fs::write(d.join("w.zip"), &z).unwrap();
        extract(&d.join("w.zip"), &d.join("o2"), &ExtractOptions::default()).unwrap();
        assert_eq!(fs::metadata(d.join("o2/plain")).unwrap().permissions().mode() & 0o777, 0o644);
    }

    #[test]
    fn zip_system_zip_with_real_deflate() {
        if !have("zip") || !have("unzip") {
            return;
        }
        let d = tmpdir("zip-sys");
        fs::create_dir_all(d.join("src/pkg")).unwrap();
        let text = "the quick brown fox jumps over the lazy dog\n".repeat(500);
        fs::write(d.join("src/pkg/big.txt"), &text).unwrap();
        fs::write(d.join("src/pkg/tool.sh"), "#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(d.join("src/pkg/tool.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("big.txt", d.join("src/pkg/sym")).unwrap();
        let st = Command::new("zip")
            .current_dir(d.join("src"))
            .args(["-q", "-r", "-y", "../a.zip", "pkg"])
            .status()
            .unwrap();
        assert!(st.success());
        let mut o = ExtractOptions::default();
        o.strip_prefix = "pkg".into();
        extract(&d.join("a.zip"), &d.join("out"), &o).unwrap();
        assert_eq!(fs::read_to_string(d.join("out/big.txt")).unwrap(), text);
        assert!(fs::metadata(d.join("out/tool.sh")).unwrap().permissions().mode() & 0o111 != 0);
        assert_eq!(fs::read_link(d.join("out/sym")).unwrap(), PathBuf::from("big.txt"));
    }

    #[test]
    fn tar_system_tar_gz_with_real_deflate() {
        if !have("gzip") {
            return;
        }
        let d = tmpdir("tar-sys");
        fs::create_dir_all(d.join("src/proj/sub")).unwrap();
        let text = "lorem ipsum dolor sit amet ".repeat(4000);
        fs::write(d.join("src/proj/sub/big"), &text).unwrap();
        fs::write(d.join("src/proj/exe"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(d.join("src/proj/exe"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("sub/big", d.join("src/proj/l")).unwrap();
        let st = Command::new("tar")
            .current_dir(d.join("src"))
            .args(["-czf", "../a.tar.gz", "proj"])
            .status();
        if !matches!(st, Ok(s) if s.success()) {
            return;
        }
        let mut o = ExtractOptions::default();
        o.strip_prefix = "proj".into();
        extract(&d.join("a.tar.gz"), &d.join("out"), &o).unwrap();
        assert_eq!(fs::read_to_string(d.join("out/sub/big")).unwrap(), text);
        assert!(fs::metadata(d.join("out/exe")).unwrap().permissions().mode() & 0o111 != 0);
        assert_eq!(fs::read_link(d.join("out/l")).unwrap(), PathBuf::from("sub/big"));
        // also with tgz name and a pax-format tar
        let st = Command::new("tar")
            .current_dir(d.join("src"))
            .args(["--format=pax", "-czf", "../b.tgz", "proj"])
            .status();
        if matches!(st, Ok(s) if s.success()) {
            extract(&d.join("b.tgz"), &d.join("out2"), &o).unwrap();
            assert_eq!(fs::read_to_string(d.join("out2/sub/big")).unwrap(), text);
        }
    }

    #[test]
    fn ar_extract() {
        let d = tmpdir("ar");
        let mut a = b"!<arch>\n".to_vec();
        let hdr = |name: &str, size: usize| {
            format!("{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", name, 0, 0, 0, "100644", size).into_bytes()
        };
        a.extend(hdr("debian-binary/", 4));
        a.extend_from_slice(b"2.0\n");
        a.extend(hdr("data.txt/", 3));
        a.extend_from_slice(b"abc\n"); // odd size padded
        fs::write(d.join("x.deb"), &a).unwrap();
        extract(&d.join("x.deb"), &d.join("out"), &ExtractOptions::default()).unwrap();
        assert_eq!(fs::read(d.join("out/debian-binary")).unwrap(), b"2.0\n");
        assert_eq!(fs::read(d.join("out/data.txt")).unwrap(), b"abc");
        fs::write(d.join("bad.ar"), b"junk").unwrap();
        assert!(extract(&d.join("bad.ar"), &d.join("o"), &ExtractOptions::default()).is_err());
    }

    #[test]
    fn bare_gz() {
        let d = tmpdir("gz");
        fs::write(d.join("file.txt.gz"), gzip_stored(b"payload")).unwrap();
        extract(&d.join("file.txt.gz"), &d.join("out"), &ExtractOptions::default()).unwrap();
        assert_eq!(fs::read(d.join("out/file.txt")).unwrap(), b"payload");

        // Gzip with FNAME in header
        let mut gz_fname = vec![0x1fu8, 0x8b, 0x08, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03];
        gz_fname.extend_from_slice(b"custom_name.txt\0");
        gz_fname.extend_from_slice(&[0x01, 0x05, 0x00, 0xfa, 0xff, b'h', b'e', b'l', b'l', b'o']);
        gz_fname.extend_from_slice(&crc32(b"hello").to_le_bytes());
        gz_fname.extend_from_slice(&(5u32).to_le_bytes());
        fs::write(d.join("single.gz"), &gz_fname).unwrap();
        extract(&d.join("single.gz"), &d.join("out_fname"), &ExtractOptions::default()).unwrap();
        assert_eq!(fs::read(d.join("out_fname/custom_name.txt")).unwrap(), b"hello");
    }

    #[test]
    fn deflate64_and_special_tar_entries() {
        // Test inflate64 on stored block
        let stored = [0x01, 0x05, 0x00, 0xfa, 0xff, b'w', b'o', b'r', b'l', b'd'];
        let (out, _) = inflate64(&stored).unwrap();
        assert_eq!(out, b"world");

        // Test tar with fifo (typeflag b'6') and char device (typeflag b'3')
        let d = tmpdir("special_tar");
        let mut t = Vec::new();
        tar_add(&mut t, "fifo_file", b'6', b"", 0o644, "");
        tar_add(&mut t, "chardev_file", b'3', b"", 0o644, "");
        tar_end(&mut t);
        fs::write(d.join("special.tar"), &t).unwrap();
        extract(&d.join("special.tar"), &d.join("out"), &ExtractOptions::default()).unwrap();
        assert!(d.join("out/fifo_file").is_file());
        assert!(d.join("out/chardev_file").is_file());
        assert_eq!(fs::read(d.join("out/fifo_file")).unwrap().len(), 0);
        assert_eq!(fs::read(d.join("out/chardev_file")).unwrap().len(), 0);
    }

    #[test]
    fn type_detection_and_errors() {
        assert_eq!(detect_kind(Path::new("/a/b.tar.gz")).unwrap(), ArchiveKind::TarGz);
        assert_eq!(detect_kind(Path::new("b.tgz")).unwrap(), ArchiveKind::TarGz);
        assert_eq!(detect_kind(Path::new("b.whl")).unwrap(), ArchiveKind::Zip);
        assert_eq!(detect_kind(Path::new("b.gz")).unwrap(), ArchiveKind::Gz);
        assert_eq!(detect_kind(Path::new("b.deb")).unwrap(), ArchiveKind::Ar);
        assert_eq!(detect_kind(Path::new("b.tar.xz")).unwrap(), ArchiveKind::TarXz);
        assert_eq!(detect_kind(Path::new("b.tzst")).unwrap(), ArchiveKind::TarZst);
        let e = detect_kind(Path::new("/x/foo.rar")).unwrap_err();
        assert!(e.starts_with("Expected a file with a .zip, .jar,"), "{}", e);
        assert!(e.ends_with(" or .br suffix (got /x/foo.rar)"), "{}", e);
        let e = kind_for_type("rar").unwrap_err();
        assert!(e.starts_with("No decompressor found for type rar. Available types are: zip, jar,"));
        assert_eq!(kind_for_type("nupkg").unwrap(), ArchiveKind::Zip);
    }

    #[test]
    fn unsupported_codecs() {
        let d = tmpdir("unsup");
        for n in ["a.tar.br", "a.br"] {
            fs::write(d.join(n), b"x").unwrap();
            let e = extract(&d.join(n), &d.join("o"), &ExtractOptions::default()).unwrap_err();
            assert!(e.starts_with("Unsupported archive format"), "{}: {}", n, e);
        }
        let mut o = ExtractOptions::default();
        o.archive_type = Some("tar.br".into());
        fs::write(d.join("blob"), b"x").unwrap();
        assert!(extract(&d.join("blob"), &d.join("o"), &o).unwrap_err().starts_with("Unsupported"));
        o.archive_type = Some("bogus".into());
        assert!(extract(&d.join("blob"), &d.join("o"), &o).unwrap_err().starts_with("No decompressor"));
    }

    #[test]
    fn bz2_and_tar_bz2_roundtrip() {
        if !have("bzip2") {
            return;
        }
        let d = tmpdir("bz2-test");
        let payload = b"hello bzip2 compressed payload with multiple repeats aaaaa bbbbb ccccc\n";
        let out = Command::new("sh")
            .arg("-c")
            .arg("printf 'hello bzip2 compressed payload with multiple repeats aaaaa bbbbb ccccc\\n' | bzip2 -c")
            .output()
            .unwrap();
        assert!(out.status.success());
        let bz2_data = out.stdout;

        let decompressed = crate::codecs::bunzip2(&bz2_data).unwrap();
        assert_eq!(decompressed, payload);

        fs::write(d.join("data.txt.bz2"), &bz2_data).unwrap();
        extract(&d.join("data.txt.bz2"), &d.join("out_single"), &ExtractOptions::default()).unwrap();
        assert_eq!(fs::read(d.join("out_single/data.txt")).unwrap(), payload);
    }

    #[test]
    fn xz_and_tar_xz_roundtrip() {
        if !have("xz") {
            return;
        }
        let d = tmpdir("xz-test");
        let payload = b"hello xz compressed payload with some repetitive strings 123456789 123456789\n";
        let out = Command::new("sh")
            .arg("-c")
            .arg("printf 'hello xz compressed payload with some repetitive strings 123456789 123456789\\n' | xz -c")
            .output()
            .unwrap();
        assert!(out.status.success());
        let xz_data = out.stdout;

        let decompressed = crate::codecs::xz_decompress(&xz_data).unwrap();
        assert_eq!(decompressed, payload);

        fs::write(d.join("data.txt.xz"), &xz_data).unwrap();
        extract(&d.join("data.txt.xz"), &d.join("out_single"), &ExtractOptions::default()).unwrap();
        assert_eq!(fs::read(d.join("out_single/data.txt")).unwrap(), payload);
    }

    #[test]
    fn zstd_and_tar_zst_roundtrip() {
        if !have("zstd") {
            return;
        }
        let d = tmpdir("zst-test");
        let payload = b"hello zstd raw block test\n";
        // Create an uncompressed zstd frame (or zstd -c)
        let out = Command::new("sh")
            .arg("-c")
            .arg("printf 'hello zstd raw block test\\n' | zstd -c")
            .output()
            .unwrap();
        if out.status.success() {
            let zst_data = out.stdout;
            let decompressed = crate::codecs::zstd_decompress(&zst_data).unwrap();
            assert_eq!(decompressed, payload);

            fs::write(d.join("data.txt.zst"), &zst_data).unwrap();
            extract(&d.join("data.txt.zst"), &d.join("out_single"), &ExtractOptions::default()).unwrap();
            assert_eq!(fs::read(d.join("out_single/data.txt")).unwrap(), payload);
        }
    }

    #[test]
    fn sevenz_extract_roundtrip() {
        if !have("7z") && !have("7za") {
            return;
        }
        let d = tmpdir("7z-test");
        let bin = if have("7z") { "7z" } else { "7za" };
        let src_file = d.join("test.txt");
        fs::write(&src_file, b"content inside 7z").unwrap();
        let archive_file = d.join("test.7z");
        let st = Command::new(bin)
            .arg("a")
            .arg(&archive_file)
            .arg(&src_file)
            .output()
            .unwrap();
        if st.status.success() {
            extract(&archive_file, &d.join("out_7z"), &ExtractOptions::default()).unwrap();
            assert_eq!(fs::read(d.join("out_7z/test.txt")).unwrap(), b"content inside 7z");
        }
    }

    #[test]
    fn symlink_not_written_through() {
        // a symlink entry followed by a file below it must not write outside root
        let d = tmpdir("symlink-through");
        let mut t = Vec::new();
        tar_add(&mut t, "l", b'2', b"", 0o777, "sub");
        tar_add(&mut t, "l/f", b'0', b"data", 0o644, "");
        tar_end(&mut t);
        fs::write(d.join("x.tar"), &t).unwrap();
        // symlinks are created last, so `l` becomes a directory first -> conflict error
        let r = extract(&d.join("x.tar"), &d.join("out"), &ExtractOptions::default());
        assert!(r.is_err());
        assert!(!d.join("out/sub").exists());
    }

    #[test]
    fn normalize_paths() {
        assert_eq!(normalize("a/./b//c/"), vec!["a", "b", "c"]);
        assert_eq!(normalize("/a/b"), vec!["a", "b"]);
        assert_eq!(normalize("a/../b"), vec!["b"]);
        assert_eq!(normalize("../a"), vec!["..", "a"]);
        assert!(normalize("").is_empty());
    }
}

/// Name of a Bazel-style scratch directory (`temp<random digits>`) inside a repository directory.
pub fn java_temp_dir_name() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64;
    format!("temp{}", 1_000_000_000_000_000_000u64 + (n.wrapping_mul(6364136223846793005) % 8_000_000_000_000_000_000u64))
}

/// Frames an extraction error the way Bazel reports a failed `ctx.download_and_extract` call made by
/// `http.bzl:203:45` (`_http_archive_impl`): located traceback, `Error in download_and_extract: ...`
/// and the `java.io.IOException: Error extracting <archive> to <dir>: <msg>` text.
pub fn bazel_http_archive_extract_failure(output_base: &Path, canonical: &str, archive: &Path, err: &str) -> String {
    let prefix = format!("{}: ", archive.display());
    let msg = err.strip_prefix(&prefix).unwrap_or(err);
    let body = if msg.starts_with("Expected a file with a ") || msg.starts_with("No decompressor found for type ") {
        msg.to_string()
    } else {
        let dir = archive.parent().map(|p| p.display().to_string()).unwrap_or_default();
        format!("java.io.IOException: Error extracting {} to {}: {}", archive.display(), dir, java_message(msg))
    };
    frame_native_failure(output_base, canonical, "http_archive", &body)
}

/// Frames an error raised through `frames` of Bazel's repo `.bzl` files (`(file, line, column, function, source
/// line)`, outermost first); `call` is the builtin named in the final `Error in <call>: <msg>` line.
pub fn frame_repo_bzl_failure(output_base: &Path, canonical: &str, frames: &[(&str, u32, u32, &str, &str)], call: &str, msg: &str) -> String {
    let base = output_base.join("external/bazel_tools/tools/build_defs/repo");
    let last = frames.last().expect("at least one frame");
    let mut tb = String::new();
    for (file, line, col, func, snippet) in frames {
        tb.push_str(&format!("\tFile \"{}\", line {line}, column {col}, in {func}\n\t\t{snippet}\n", base.join(file).display()));
    }
    let located = format!("{}:{}:{}: Traceback (most recent call last):\n{tb}Error in {call}: {msg}\n", base.join(last.0).display(), last.1, last.2);
    crate::fetch::frame_traceback_error(&located, canonical).unwrap_or_else(|| msg.to_string())
}

/// Frames a `fail(msg)` raised from `http.bzl` (frames are `(line, column, function, source line)`, outermost first).
pub fn frame_http_fail(output_base: &Path, canonical: &str, frames: &[(u32, u32, &str, &str)], msg: &str) -> String {
    let http = output_base.join("external/bazel_tools/tools/build_defs/repo/http.bzl").display().to_string();
    let last = frames.last().expect("at least one frame");
    let mut tb = String::new();
    for (line, col, func, snippet) in frames {
        tb.push_str(&format!("\tFile \"{http}\", line {line}, column {col}, in {func}\n\t\t{snippet}\n"));
    }
    let located = format!("{http}:{}:{}: Traceback (most recent call last):\n{tb}Error in fail: {msg}\n", last.0, last.1);
    crate::fetch::frame_traceback_error(&located, canonical).unwrap_or_else(|| msg.to_string())
}

/// Frames `body` (the text after `Error in <call>: `) as the Starlark traceback Bazel 9.3.2 prints when the
/// `http.bzl` implementation of `rule` (`http_archive`, `http_file` or `http_jar`) fails inside `ctx.download*`.
pub fn frame_native_failure(output_base: &Path, canonical: &str, rule: &str, body: &str) -> String {
    let (line, col, func, snippet, call) = match rule {
        "http_file" => (255, 33, "_http_file_impl", "download_info = ctx.download(", "download"),
        "http_jar" => (292, 33, "_http_jar_impl", "download_info = ctx.download(", "download"),
        _ => (203, 45, "_http_archive_impl", "download_info = ctx.download_and_extract(", "download_and_extract"),
    };
    let http = output_base.join("external/bazel_tools/tools/build_defs/repo/http.bzl").display().to_string();
    let located = format!(
        "{http}:{line}:{col}: Traceback (most recent call last):\n\tFile \"{http}\", line {line}, column {col}, in {func}\n\t\t{snippet}\nError in {call}: {body}\n"
    );
    crate::fetch::frame_traceback_error(&located, canonical).unwrap_or_else(|| body.to_string())
}

/// Maps Zel's codec error text onto the Java exception message Bazel would report.
pub fn java_message(msg: &str) -> String {
    const NULL: &str = "null";
    let exact: &[(&str, &str)] = &[
        // gzip (commons-compress GzipCompressorInputStream)
        ("Not in gzip format", "Input is not in the .gz format"),
        ("Invalid gzip header: reserved flags set", "Reserved flags are set in the .gz header"),
        ("Gzip CRC32 mismatch", "Gzip-compressed data is corrupt (CRC32 error)"),
        ("Gzip ISIZE mismatch", "Gzip-compressed data is corrupt(uncompressed size mismatch)"),
        ("Unexpected end of gzip header", NULL),
        ("Unexpected end of deflate stream", NULL),
        ("Unexpected end of gzip trailer", NULL),
        // bzip2
        ("Not a bzip2 archive (missing BZh header)", "Stream is not in the BZip2 format"),
        ("Unexpected end of bzip2 stream", "Unexpected end of stream"),
        ("Ran out of selectors during run", "Unexpected end of stream"),
        ("Huffman length out of range (1..20)", "Corrupted input, zvec value negative"),
        ("bzip2 stream CRC mismatch", "BZip2 CRC error"),
        // tar (commons-compress TarArchiveInputStream)
        ("Bad tar header checksum: not a valid tar archive", "Corrupted TAR archive."),
        ("Unexpected end of tar archive", "Truncated TAR archive"),
        // xz (xz-java via XZCompressorInputStream)
        ("Not an XZ archive (missing header magic)", "Input is not in the XZ format"),
        ("XZ stream header CRC32 mismatch", "XZ Stream Header is corrupt"),
        ("XZ block header CRC32 mismatch", "XZ Block Header is corrupt"),
        ("XZ block CRC32 check failed", "Integrity check (CRC32) does not match"),
        ("XZ block CRC64 check failed", "Integrity check (CRC64) does not match"),
        ("XZ block SHA-256 check failed", "Integrity check (SHA-256) does not match"),
        ("XZ stream header truncated", NULL),
        ("XZ Stream Footer CRC32 mismatch", "XZ Stream Footer is corrupt"),
        ("XZ bad Stream Footer magic", "XZ Stream Footer is corrupt"),
        ("XZ Stream Footer truncated", NULL),
        ("XZ index truncated", NULL),
        ("XZ stream truncated (missing index)", NULL),
        ("XZ varint truncated", NULL),
        ("XZ varint not minimal", "Compressed data is corrupt"),
        ("Zstd content checksum truncated", "Truncated source"),
        ("Zstd skippable frame truncated", "Truncated source"),
        ("Zstd reserved block type", "Data corruption detected"),
        ("Selector index out of range", "Unexpected end of stream"),
        ("orig_ptr out of range in bzip2 block", "Stream corrupted"),
        ("XZ block header truncated", NULL),
        ("XZ block truncated", NULL),
        ("XZ input is empty", NULL),
        ("LZMA2 data truncated", NULL),
        ("LZMA2 data corrupt (chunk not fully consumed)", "Compressed data is corrupt"),
        // zstd (zstd-jni)
        ("Not a zstd archive (bad magic number)", "Unknown frame descriptor"),
        ("Zstd: unknown frame magic", "Unknown frame descriptor"),
        ("Zstd frame header truncated", "Truncated source"),
        ("Truncated block data", "Truncated source"),
        ("Zstd frame header has reserved bit set", "Unsupported frame parameter"),
        ("Zstd content checksum mismatch", "Restored data doesn't match checksum"),
        // 7z (commons-compress SevenZFile)
        ("not a 7z archive (bad magic)", "Bad 7z signature"),
        ("7z start header CRC mismatch", "Checksum verification failed"),
        ("7z header CRC mismatch", "NextHeader CRC-32 mismatch"),
    ];
    for (k, v) in exact {
        if msg == *k {
            return v.to_string();
        }
    }
    if let Some(m) = msg.strip_prefix("Unsupported gzip compression method ") {
        return format!("Unsupported compression method {m} in the .gz header");
    }
    if msg.starts_with("Invalid deflate stream") {
        return "Gzip-compressed data is corrupt".to_string();
    }
    if msg.starts_with("Invalid number in tar header") || msg.starts_with("Bad tar header") {
        return "Corrupted TAR archive.".to_string();
    }
    if msg.starts_with("Invalid bzip2 block size") {
        return "BZip2 block size is invalid".to_string();
    }
    if msg.starts_with("Bad bzip2 block magic") {
        return "Bad block header".to_string();
    }
    if msg.starts_with("bzip2 block CRC mismatch") {
        return "BZip2 CRC error".to_string();
    }
    if msg.starts_with("LZMA ") || msg.starts_with("LZMA2 ") {
        return "Compressed data is corrupt".to_string();
    }
    if msg.starts_with("Invalid zstd match offset") || msg.starts_with("Zstd Huffman") || msg.starts_with("Zstd sequence") {
        return "Data corruption detected".to_string();
    }
    msg.to_string()
}

#[cfg(test)]
mod corrupt_tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("zel-corrupt-test-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }


    #[test]
    fn java_error_texts_and_zip_eocd_rule() {
        assert_eq!(java_message("Gzip CRC32 mismatch"), "Gzip-compressed data is corrupt (CRC32 error)");
        assert_eq!(java_message("Unexpected end of deflate stream"), "null");
        assert_eq!(java_message("Unsupported gzip compression method 12"), "Unsupported compression method 12 in the .gz header");
        let d = tmpdir("zip-tail");
        let mut z = minimal_zip();
        fs::write(d.join("a.zip"), &z).unwrap();
        assert!(extract(&d.join("a.zip"), &d.join("o1"), &ExtractOptions::default()).is_ok());
        z.push(0);
        fs::write(d.join("b.zip"), &z).unwrap();
        let e = extract(&d.join("b.zip"), &d.join("o2"), &ExtractOptions::default()).unwrap_err();
        assert!(e.contains("is malformed. It does not contain an end of central directory record."), "{e}");
        // 7z: too short for the start header -> Java's EOFException (message null)
        fs::write(d.join("c.7z"), b"7z\xbc\xaf\x27\x1c\0\x04\0\0\0\0").unwrap();
        let e = extract(&d.join("c.7z"), &d.join("o3"), &ExtractOptions::default()).unwrap_err();
        assert!(e.ends_with(": null"), "{e}");
    }
}

#[cfg(test)]
fn minimal_zip() -> Vec<u8> {
    // one stored entry "f" = "data"
    let mut z = Vec::new();
    z.extend_from_slice(&[0x50, 0x4b, 3, 4, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend_from_slice(&crc32(b"data").to_le_bytes());
    z.extend_from_slice(&4u32.to_le_bytes());
    z.extend_from_slice(&4u32.to_le_bytes());
    z.extend_from_slice(&[1, 0, 0, 0]);
    z.extend_from_slice(b"f");
    z.extend_from_slice(b"data");
    let cd = z.len();
    z.extend_from_slice(&[0x50, 0x4b, 1, 2, 20, 3, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend_from_slice(&crc32(b"data").to_le_bytes());
    z.extend_from_slice(&4u32.to_le_bytes());
    z.extend_from_slice(&4u32.to_le_bytes());
    z.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend_from_slice(&(0o100644u32 << 16).to_le_bytes());
    z.extend_from_slice(&0u32.to_le_bytes());
    z.extend_from_slice(b"f");
    let cd_size = z.len() - cd;
    z.extend_from_slice(&[0x50, 0x4b, 5, 6, 0, 0, 0, 0, 1, 0, 1, 0]);
    z.extend_from_slice(&(cd_size as u32).to_le_bytes());
    z.extend_from_slice(&(cd as u32).to_le_bytes());
    z.extend_from_slice(&[0, 0]);
    z
}
