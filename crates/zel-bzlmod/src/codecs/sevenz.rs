//! Pure-Rust 7-Zip (7z) archive extractor.
//!
//! - Start header / next header CRC verification, plain and encoded headers
//! - Full coder graph (bind pairs, packed streams) per folder
//! - Coders: Copy, LZMA, LZMA2, BZip2, Deflate, Delta, BCJ x86/PPC/IA64/ARM/ARMT/SPARC
//! - CRC32 verification of every folder and every file (substream)
//! - Empty streams / empty files / directories
//!
//! Matching Bazel's `SevenZDecompressor` (Apache commons-compress), every non-directory entry
//! is written as a regular file (symlink entries become regular files holding the link target).

use std::path::Path;

use super::lzma::{lzma2_decode_into, lzma_decompress, parse_props_byte};
use super::xz::{bcj_decode, delta_decode, Bcj};
use crate::archive::{crc32, extract_entries_7z, ExtractOptions, SevenZEntry};

const SIGNATURE: &[u8; 6] = b"7z\xBC\xAF\x27\x1C";

const K_END: u8 = 0x00;
const K_HEADER: u8 = 0x01;
const K_ARCHIVE_PROPERTIES: u8 = 0x02;
const K_ADDITIONAL_STREAMS_INFO: u8 = 0x03;
const K_MAIN_STREAMS_INFO: u8 = 0x04;
const K_FILES_INFO: u8 = 0x05;
const K_PACK_INFO: u8 = 0x06;
const K_UNPACK_INFO: u8 = 0x07;
const K_SUB_STREAMS_INFO: u8 = 0x08;
const K_SIZE: u8 = 0x09;
const K_CRC: u8 = 0x0A;
const K_FOLDER: u8 = 0x0B;
const K_CODERS_UNPACK_SIZE: u8 = 0x0C;
const K_NUM_UNPACK_STREAM: u8 = 0x0D;
const K_EMPTY_STREAM: u8 = 0x0E;
const K_EMPTY_FILE: u8 = 0x0F;
const K_ANTI: u8 = 0x10;
const K_NAME: u8 = 0x11;
const K_WIN_ATTRIBUTES: u8 = 0x15;
const K_ENCODED_HEADER: u8 = 0x17;

const MAX_UNPACK: u64 = 1 << 40;

struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn new(d: &'a [u8]) -> Self {
        Self { d, p: 0 }
    }
    fn left(&self) -> usize {
        self.d.len() - self.p
    }
    fn u8(&mut self) -> Result<u8, String> {
        let b = *self.d.get(self.p).ok_or("7z header truncated")?;
        self.p += 1;
        Ok(b)
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        if n > self.left() {
            return Err("7z header truncated".into());
        }
        let s = &self.d[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    /// 7z variable-length number.
    fn number(&mut self) -> Result<u64, String> {
        let first = self.u8()?;
        let mut mask = 0x80u8;
        let mut value = 0u64;
        for i in 0..8 {
            if first & mask == 0 {
                value |= ((first & (mask - 1)) as u64) << (8 * i);
                return Ok(value);
            }
            value |= (self.u8()? as u64) << (8 * i);
            mask >>= 1;
        }
        Ok(value)
    }
    /// A count that must be plausible given the remaining header bytes.
    fn count(&mut self) -> Result<usize, String> {
        let n = self.number()?;
        if n > (1 << 28) {
            return Err("7z header has implausible item count".into());
        }
        Ok(n as usize)
    }
    fn bit_vector(&mut self, n: usize) -> Result<Vec<bool>, String> {
        let bytes = self.bytes((n + 7) / 8)?;
        Ok((0..n).map(|i| bytes[i / 8] & (0x80 >> (i % 8)) != 0).collect())
    }
    fn optional_bit_vector(&mut self, n: usize) -> Result<Vec<bool>, String> {
        let all = self.u8()?;
        if all != 0 {
            Ok(vec![true; n])
        } else {
            self.bit_vector(n)
        }
    }
    fn digests(&mut self, n: usize) -> Result<Vec<Option<u32>>, String> {
        let defined = self.optional_bit_vector(n)?;
        let mut v = Vec::with_capacity(n);
        for d in defined {
            v.push(if d { Some(self.u32()?) } else { None });
        }
        Ok(v)
    }
}

fn to_usize(v: u64) -> Result<usize, String> {
    if v > MAX_UNPACK {
        return Err("7z size too large".into());
    }
    usize::try_from(v).map_err(|_| "7z size too large".to_string())
}

#[derive(Debug, Clone)]
struct Coder {
    id: u64,
    nin: usize,
    nout: usize,
    props: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
struct Folder {
    coders: Vec<Coder>,
    /// (in_index, out_index)
    bind_pairs: Vec<(usize, usize)>,
    /// in-stream indices fed by pack streams, in pack stream order
    packed: Vec<usize>,
    unpack_sizes: Vec<u64>,
    crc: Option<u32>,
    first_pack: usize,
    sub_sizes: Vec<u64>,
    sub_crcs: Vec<Option<u32>>,
    have_sub: bool,
}

impl Folder {
    fn total_in(&self) -> usize {
        self.coders.iter().map(|c| c.nin).sum()
    }
    fn total_out(&self) -> usize {
        self.coders.iter().map(|c| c.nout).sum()
    }
    fn main_out(&self) -> Result<usize, String> {
        (0..self.total_out())
            .find(|o| !self.bind_pairs.iter().any(|b| b.1 == *o))
            .ok_or_else(|| "7z folder has no main output stream".to_string())
    }
    fn unpack_size(&self) -> Result<u64, String> {
        Ok(self.unpack_sizes[self.main_out()?])
    }
}

#[derive(Debug, Default)]
struct StreamsInfo {
    pack_pos: u64,
    pack_sizes: Vec<u64>,
    folders: Vec<Folder>,
}

fn read_streams_info(r: &mut Rd) -> Result<StreamsInfo, String> {
    let mut si = StreamsInfo::default();
    loop {
        let id = r.u8()?;
        match id {
            K_END => break,
            K_PACK_INFO => {
                si.pack_pos = r.number()?;
                let n = r.count()?;
                loop {
                    match r.u8()? {
                        K_END => break,
                        K_SIZE => {
                            si.pack_sizes.clear();
                            for _ in 0..n {
                                si.pack_sizes.push(r.number()?);
                            }
                        }
                        K_CRC => {
                            r.digests(n)?;
                        }
                        other => return Err(format!("7z unexpected pack info property 0x{other:02x}")),
                    }
                }
                if si.pack_sizes.len() != n {
                    return Err("7z pack info missing sizes".into());
                }
            }
            K_UNPACK_INFO => read_unpack_info(r, &mut si)?,
            K_SUB_STREAMS_INFO => read_substreams(r, &mut si)?,
            other => return Err(format!("7z unexpected streams info property 0x{other:02x}")),
        }
    }
    // Defaults when there is no SubStreamsInfo.
    let mut pack = 0usize;
    for f in &mut si.folders {
        f.first_pack = pack;
        pack += f.packed.len();
        if !f.have_sub {
            f.sub_sizes = vec![f.unpack_size()?];
            f.sub_crcs = vec![f.crc];
        }
    }
    if pack > si.pack_sizes.len() {
        return Err("7z folders reference more pack streams than exist".into());
    }
    Ok(si)
}

fn read_unpack_info(r: &mut Rd, si: &mut StreamsInfo) -> Result<(), String> {
    if r.u8()? != K_FOLDER {
        return Err("7z unpack info: folder expected".into());
    }
    let nf = r.count()?;
    if r.u8()? != 0 {
        return Err("7z external folder definitions are not supported".into());
    }
    let mut folders = Vec::with_capacity(nf.min(1 << 16));
    for _ in 0..nf {
        let ncoders = r.count()?;
        if ncoders == 0 || ncoders > 32 {
            return Err("7z folder has invalid coder count".into());
        }
        let mut f = Folder::default();
        for _ in 0..ncoders {
            let flags = r.u8()?;
            if flags & 0x80 != 0 {
                return Err("7z alternative coder methods are not supported".into());
            }
            let idsize = (flags & 0x0F) as usize;
            if idsize > 8 {
                return Err("7z coder id too long".into());
            }
            let mut id = 0u64;
            for &b in r.bytes(idsize)? {
                id = (id << 8) | b as u64;
            }
            let (nin, nout) = if flags & 0x10 != 0 { (r.count()?, r.count()?) } else { (1, 1) };
            if nin > 32 || nout > 32 || nout == 0 {
                return Err("7z coder has invalid stream counts".into());
            }
            let props = if flags & 0x20 != 0 {
                let n = r.count()?;
                r.bytes(n)?.to_vec()
            } else {
                Vec::new()
            };
            f.coders.push(Coder { id, nin, nout, props });
        }
        let (tin, tout) = (f.total_in(), f.total_out());
        if tout == 0 {
            return Err("7z folder has no outputs".into());
        }
        let nbp = tout - 1;
        if tin < nbp {
            return Err("7z folder is invalid".into());
        }
        for _ in 0..nbp {
            let i = r.count()?;
            let o = r.count()?;
            if i >= tin || o >= tout {
                return Err("7z bind pair out of range".into());
            }
            f.bind_pairs.push((i, o));
        }
        let npacked = tin - nbp;
        if npacked == 1 {
            let idx = (0..tin).find(|i| !f.bind_pairs.iter().any(|b| b.0 == *i)).ok_or("7z folder has no packed stream")?;
            f.packed.push(idx);
        } else {
            for _ in 0..npacked {
                let i = r.count()?;
                if i >= tin {
                    return Err("7z packed stream index out of range".into());
                }
                f.packed.push(i);
            }
        }
        folders.push(f);
    }
    if r.u8()? != K_CODERS_UNPACK_SIZE {
        return Err("7z unpack info: coder unpack sizes expected".into());
    }
    for f in &mut folders {
        for _ in 0..f.total_out() {
            let n = r.number()?;
            f.unpack_sizes.push(n);
        }
    }
    loop {
        match r.u8()? {
            K_END => break,
            K_CRC => {
                let d = r.digests(folders.len())?;
                for (f, c) in folders.iter_mut().zip(d) {
                    f.crc = c;
                }
            }
            other => return Err(format!("7z unexpected unpack info property 0x{other:02x}")),
        }
    }
    si.folders = folders;
    Ok(())
}

fn read_substreams(r: &mut Rd, si: &mut StreamsInfo) -> Result<(), String> {
    let nf = si.folders.len();
    let mut counts = vec![1usize; nf];
    let mut id = r.u8()?;
    if id == K_NUM_UNPACK_STREAM {
        for c in counts.iter_mut() {
            *c = r.count()?;
        }
        id = r.u8()?;
    }
    let mut sizes: Vec<Vec<u64>> = Vec::with_capacity(nf);
    let have_sizes = id == K_SIZE;
    for (i, f) in si.folders.iter().enumerate() {
        let total = f.unpack_size()?;
        let mut v = Vec::new();
        if counts[i] > 0 {
            if have_sizes {
                let mut sum = 0u64;
                for _ in 0..counts[i] - 1 {
                    let s = r.number()?;
                    sum = sum.checked_add(s).ok_or("7z substream sizes overflow")?;
                    v.push(s);
                }
                if sum > total {
                    return Err("7z substream sizes exceed folder size".into());
                }
                v.push(total - sum);
            } else {
                if counts[i] != 1 {
                    return Err("7z substream sizes missing".into());
                }
                v.push(total);
            }
        }
        sizes.push(v);
    }
    if have_sizes {
        id = r.u8()?;
    }
    // Which streams need an explicit digest?
    let mut crcs: Vec<Vec<Option<u32>>> = Vec::with_capacity(nf);
    let mut need = 0usize;
    for (i, f) in si.folders.iter().enumerate() {
        if counts[i] == 1 && f.crc.is_some() {
            crcs.push(vec![f.crc]);
        } else {
            crcs.push(vec![None; counts[i]]);
            need += counts[i];
        }
    }
    loop {
        match id {
            K_END => break,
            K_CRC => {
                let d = r.digests(need)?;
                let mut it = d.into_iter();
                for (i, f) in si.folders.iter().enumerate() {
                    if !(counts[i] == 1 && f.crc.is_some()) {
                        for slot in crcs[i].iter_mut() {
                            *slot = it.next().unwrap();
                        }
                    }
                }
            }
            other => return Err(format!("7z unexpected substreams property 0x{other:02x}")),
        }
        id = r.u8()?;
    }
    for (i, f) in si.folders.iter_mut().enumerate() {
        f.sub_sizes = std::mem::take(&mut sizes[i]);
        f.sub_crcs = std::mem::take(&mut crcs[i]);
        f.have_sub = true;
    }
    Ok(())
}

#[derive(Debug, Default, Clone)]
struct FileRec {
    name: String,
    has_stream: bool,
    is_dir: bool,
    anti: bool,
    attrs: Option<u32>,
}

fn read_files_info(r: &mut Rd) -> Result<Vec<FileRec>, String> {
    let n = r.count()?;
    let mut empty_stream = vec![false; n];
    let mut empty_file: Vec<bool> = Vec::new();
    let mut anti: Vec<bool> = Vec::new();
    let mut names: Vec<String> = vec![String::new(); n];
    let mut attrs: Vec<Option<u32>> = vec![None; n];
    let mut n_empty = 0usize;
    loop {
        let id = r.u8()?;
        if id == K_END {
            break;
        }
        let size = r.number()?;
        if size > r.left() as u64 {
            return Err("7z files info truncated".into());
        }
        let mut p = Rd::new(r.bytes(size as usize)?);
        match id {
            K_EMPTY_STREAM => {
                empty_stream = p.bit_vector(n)?;
                n_empty = empty_stream.iter().filter(|b| **b).count();
            }
            K_EMPTY_FILE => empty_file = p.bit_vector(n_empty)?,
            K_ANTI => anti = p.bit_vector(n_empty)?,
            K_NAME => {
                if p.u8()? != 0 {
                    return Err("7z external names are not supported".into());
                }
                let rest = p.bytes(p.left())?;
                if rest.len() % 2 != 0 {
                    return Err("7z names are malformed".into());
                }
                let mut units: Vec<u16> = Vec::new();
                let mut idx = 0usize;
                for ch in rest.chunks_exact(2) {
                    let u = u16::from_le_bytes([ch[0], ch[1]]);
                    if u == 0 {
                        if idx >= n {
                            return Err("7z has too many names".into());
                        }
                        names[idx] = String::from_utf16_lossy(&units);
                        units.clear();
                        idx += 1;
                    } else {
                        units.push(u);
                    }
                }
                if idx != n {
                    return Err("7z has too few names".into());
                }
            }
            K_WIN_ATTRIBUTES => {
                let defined = p.optional_bit_vector(n)?;
                if p.u8()? != 0 {
                    return Err("7z external attributes are not supported".into());
                }
                for (i, d) in defined.iter().enumerate() {
                    if *d {
                        attrs[i] = Some(p.u32()?);
                    }
                }
            }
            _ => {} // times, start pos, dummy, ...
        }
    }
    let mut recs = Vec::with_capacity(n);
    let mut empty_idx = 0usize;
    for i in 0..n {
        let mut rec = FileRec { name: names[i].clone(), has_stream: !empty_stream[i], attrs: attrs[i], ..Default::default() };
        if empty_stream[i] {
            let is_empty_file = empty_file.get(empty_idx).copied().unwrap_or(false);
            rec.is_dir = !is_empty_file;
            rec.anti = anti.get(empty_idx).copied().unwrap_or(false);
            empty_idx += 1;
        }
        recs.push(rec);
    }
    Ok(recs)
}

// ---------------------------------------------------------------------------
// Folder decoding
// ---------------------------------------------------------------------------

fn bcj_kind(id: u64) -> Option<Bcj> {
    Some(match id {
        0x03030103 | 0x04 => Bcj::X86,
        0x03030205 | 0x05 => Bcj::Ppc,
        0x03030401 | 0x06 => Bcj::Ia64,
        0x03030501 | 0x07 => Bcj::Arm,
        0x03030701 | 0x08 => Bcj::ArmThumb,
        0x03030805 | 0x09 => Bcj::Sparc,
        _ => return None,
    })
}

fn run_coder(c: &Coder, mut inputs: Vec<Vec<u8>>, out_size: u64) -> Result<Vec<u8>, String> {
    let out_size = to_usize(out_size)?;
    if let Some(kind) = bcj_kind(c.id) {
        if inputs.len() != 1 {
            return Err("7z BCJ coder needs one input".into());
        }
        let start = match c.props.len() {
            0 => 0,
            4 => u32::from_le_bytes(c.props[..4].try_into().unwrap()),
            _ => return Err("7z BCJ coder has invalid properties".into()),
        };
        let mut b = inputs.pop().unwrap();
        if b.len() < out_size {
            return Err("7z filter input shorter than expected".into());
        }
        b.truncate(out_size);
        bcj_decode(kind, &mut b, start);
        return Ok(b);
    }
    if inputs.len() != 1 {
        return Err(format!("7z coder 0x{:x} with {} inputs is not supported", c.id, inputs.len()));
    }
    let input = inputs.pop().unwrap();
    let out = match c.id {
        0x00 => {
            let mut b = input;
            if b.len() < out_size {
                return Err("7z copy coder input shorter than expected".into());
            }
            b.truncate(out_size);
            b
        }
        0x21 => {
            if c.props.len() != 1 || c.props[0] > 40 {
                return Err("7z LZMA2 coder has invalid properties".into());
            }
            let mut out = Vec::with_capacity(out_size.min(1 << 28));
            lzma2_decode_into(&input, &mut out, Some(out_size))?;
            out
        }
        0x030101 => {
            if c.props.len() < 5 {
                return Err("7z LZMA coder has invalid properties".into());
            }
            let (lc, lp, pb) = parse_props_byte(c.props[0])?;
            lzma_decompress(&input, out_size, lc, lp, pb)?
        }
        0x03 => {
            if c.props.len() != 1 {
                return Err("7z delta coder has invalid properties".into());
            }
            let mut b = input;
            if b.len() < out_size {
                return Err("7z filter input shorter than expected".into());
            }
            b.truncate(out_size);
            delta_decode(&mut b, c.props[0] as usize + 1);
            b
        }
        0x040108 => crate::archive::inflate_limit(&input, out_size).map(|(v, _)| v)?,
        0x040202 => super::bz2::bunzip2(&input)?,
        0x06F10701 => return Err("7z archive is encrypted (AES), which is not supported".into()),
        0x030401 => return Err("7z PPMd coder is not supported".into()),
        0x040109 => crate::archive::inflate64_limit(&input, out_size).map(|(v, _)| v)?,
        0x0303011B => return Err("7z BCJ2 coder is not supported".into()),
        other => return Err(format!("Unsupported 7z coder method 0x{other:x}")),
    };
    if out.len() != out_size {
        return Err(format!("7z coder 0x{:x} produced {} bytes, expected {}", c.id, out.len(), out_size));
    }
    Ok(out)
}

struct FolderCtx<'a> {
    f: &'a Folder,
    packs: Vec<&'a [u8]>,
    depth: usize,
}

impl<'a> FolderCtx<'a> {
    fn get_out(&mut self, out_idx: usize) -> Result<Vec<u8>, String> {
        self.depth += 1;
        if self.depth > 64 {
            return Err("7z folder coder graph is too deep".into());
        }
        // locate coder owning out stream
        let (mut ci, mut obase, mut ibase) = (0usize, 0usize, 0usize);
        loop {
            let c = self.f.coders.get(ci).ok_or("7z coder graph is invalid")?;
            if out_idx < obase + c.nout {
                break;
            }
            obase += c.nout;
            ibase += c.nin;
            ci += 1;
        }
        let coder = &self.f.coders[ci];
        if coder.nout != 1 {
            return Err("7z coders with several outputs are not supported".into());
        }
        let mut inputs = Vec::new();
        for k in 0..coder.nin {
            let gi = ibase + k;
            if let Some(bp) = self.f.bind_pairs.iter().find(|b| b.0 == gi) {
                let o = bp.1;
                inputs.push(self.get_out(o)?);
            } else {
                let pi = self.f.packed.iter().position(|p| *p == gi).ok_or("7z coder input is not connected")?;
                inputs.push(self.packs[pi].to_vec());
            }
        }
        let r = run_coder(coder, inputs, self.f.unpack_sizes[out_idx]);
        self.depth -= 1;
        r
    }
}

fn decode_folder(raw: &[u8], si: &StreamsInfo, fi: usize) -> Result<Vec<u8>, String> {
    let f = &si.folders[fi];
    let mut offset: u64 = 32 + si.pack_pos;
    for s in &si.pack_sizes[..f.first_pack] {
        offset = offset.checked_add(*s).ok_or("7z pack sizes overflow")?;
    }
    let mut packs = Vec::new();
    for k in 0..f.packed.len() {
        let sz = si.pack_sizes[f.first_pack + k];
        let end = offset.checked_add(sz).ok_or("7z pack sizes overflow")?;
        if end > raw.len() as u64 {
            return Err("7z archive truncated (pack stream beyond end of file)".into());
        }
        packs.push(&raw[offset as usize..end as usize]);
        offset = end;
    }
    let main = f.main_out()?;
    let mut ctx = FolderCtx { f, packs, depth: 0 };
    let out = ctx.get_out(main)?;
    if let Some(c) = f.crc {
        if crc32(&out) != c {
            return Err("7z CRC mismatch in folder data (archive is corrupt)".into());
        }
    }
    Ok(out)
}

fn fix_first_pack(si: &mut StreamsInfo) {
    let mut pack = 0usize;
    for f in &mut si.folders {
        f.first_pack = pack;
        pack += f.packed.len();
    }
}

pub fn extract_7z(raw: &[u8], archive: &Path, root: &Path, opts: &ExtractOptions) -> Result<(), String> {
    let err = |m: String| format!("{}: {}", archive.display(), m);
    // commons-compress SevenZFile.readHeaders
    if raw.len() < 12 {
        return Err(err("null".into()));
    }
    if &raw[..6] != SIGNATURE {
        return Err(err("Bad 7z signature".into()));
    }
    if raw[6] != 0 {
        return Err(err(format!("Unsupported 7z version ({},{})", raw[6] as i8, raw[7] as i8)));
    }
    let start_crc = u32::from_le_bytes(raw[8..12].try_into().unwrap());
    if start_crc == 0 {
        if raw.len() < 32 {
            return Err(err("null".into()));
        }
        if raw[12..32].iter().all(|&b| b == 0) {
            return Err(err("archive seems to be invalid.\nYou may want to retry and enable the tryToRecoverBrokenArchives if the archive could be a multi-volume archive that has been closed prematurely.".into()));
        }
    }
    if raw.len() < 28 {
        return Err(err("null".into()));
    }
    {
        let off = i64::from_le_bytes(raw[12..20].try_into().unwrap());
        let size = i64::from_le_bytes(raw[20..28].try_into().unwrap());
        if off < 0 || off.saturating_add(32) > raw.len() as i64 {
            return Err(err("nextHeaderOffset is out of bounds".into()));
        }
        let end = off.wrapping_add(size);
        if end < off || end.saturating_add(32) > raw.len() as i64 {
            return Err(err("nextHeaderSize is out of bounds".into()));
        }
    }
    if crc32(&raw[12..32]) != start_crc {
        return Err(err("Checksum verification failed".into()));
    }
    let nh_off = u64::from_le_bytes(raw[12..20].try_into().unwrap());
    let nh_size = u64::from_le_bytes(raw[20..28].try_into().unwrap());
    let nh_crc = u32::from_le_bytes(raw[28..32].try_into().unwrap());

    let (recs, data, entries_info) = if nh_size == 0 {
        (Vec::new(), Vec::new(), Vec::new())
    } else {
        let start = 32u64.checked_add(nh_off).ok_or_else(|| err("bad 7z header offset".into()))?;
        let end = start.checked_add(nh_size).ok_or_else(|| err("bad 7z header size".into()))?;
        if end > raw.len() as u64 {
            return Err(err("7z archive truncated (header beyond end of file)".into()));
        }
        let mut header: Vec<u8> = raw[start as usize..end as usize].to_vec();
        if crc32(&header) != nh_crc {
            return Err(err("NextHeader CRC-32 mismatch".into()));
        }
        let mut rounds = 0;
        loop {
            let mut r = Rd::new(&header);
            match r.u8().map_err(&err)? {
                K_ENCODED_HEADER => {
                    rounds += 1;
                    if rounds > 4 {
                        return Err(err("7z header is nested too deeply".into()));
                    }
                    let mut si = read_streams_info(&mut r).map_err(&err)?;
                    fix_first_pack(&mut si);
                    if si.folders.is_empty() {
                        return Err(err("7z encoded header has no folder".into()));
                    }
                    header = decode_folder(raw, &si, 0).map_err(&err)?;
                }
                K_HEADER => break,
                other => return Err(err(format!("unexpected 7z header id 0x{other:02x}"))),
            }
        }
        let mut r = Rd::new(&header);
        r.u8().map_err(&err)?;
        let mut si = StreamsInfo::default();
        let mut recs = Vec::new();
        loop {
            match r.u8().map_err(&err)? {
                K_END => break,
                K_ARCHIVE_PROPERTIES => loop {
                    if r.u8().map_err(&err)? == K_END {
                        break;
                    }
                    let n = r.number().map_err(&err)? as usize;
                    r.bytes(n).map_err(&err)?;
                },
                K_ADDITIONAL_STREAMS_INFO => {
                    read_streams_info(&mut r).map_err(&err)?;
                }
                K_MAIN_STREAMS_INFO => si = read_streams_info(&mut r).map_err(&err)?,
                K_FILES_INFO => recs = read_files_info(&mut r).map_err(&err)?,
                other => return Err(err(format!("unexpected 7z header property 0x{other:02x}"))),
            }
        }
        fix_first_pack(&mut si);
        // Decode folders and map substreams to files.
        let mut data: Vec<u8> = Vec::new();
        let mut spans: Vec<(usize, usize, Option<u32>)> = Vec::new(); // (start,len,crc) per substream
        for fi in 0..si.folders.len() {
            let f = &si.folders[fi];
            if f.sub_sizes.is_empty() {
                continue;
            }
            let buf = decode_folder(raw, &si, fi).map_err(&err)?;
            let mut off = 0usize;
            for (k, sz) in f.sub_sizes.iter().enumerate() {
                let sz = to_usize(*sz).map_err(&err)?;
                if off + sz > buf.len() {
                    return Err(err("7z substream exceeds folder data".into()));
                }
                spans.push((data.len() + off, sz, f.sub_crcs.get(k).copied().flatten()));
                off += sz;
            }
            data.extend_from_slice(&buf);
        }
        let with_stream = recs.iter().filter(|r| r.has_stream).count();
        if with_stream != spans.len() {
            return Err(err("7z archive is inconsistent (file/stream count mismatch)".into()));
        }
        (recs, data, spans)
    };

    let mut entries = Vec::new();
    let mut si_idx = 0usize;
    for rec in recs {
        if rec.anti {
            continue;
        }
        let mut e = SevenZEntry { name: rec.name.clone(), is_dir: rec.is_dir, mode: None, start: 0, len: 0 };
        if rec.has_stream {
            let (s, l, c) = entries_info[si_idx];
            si_idx += 1;
            if let Some(c) = c {
                if crc32(&data[s..s + l]) != c {
                    return Err(err(format!("7z CRC mismatch for entry {} (archive is corrupt)", rec.name)));
                }
            }
            e.start = s;
            e.len = l;
        }
        entries.push(e);
    }
    extract_entries_7z(&data, entries, archive, root, opts)
}
