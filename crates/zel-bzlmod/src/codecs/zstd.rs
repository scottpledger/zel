//! Pure-Rust Zstandard decompressor (RFC 8878).
//!
//! Frames are decoded into a single output buffer which doubles as the sliding window:
//! match offsets may reach back across block boundaries up to the window size (and no
//! further than the start of the current frame).  Supports raw / RLE / compressed blocks,
//! 1- and 4-stream Huffman literals (incl. treeless reuse of the previous table), FSE
//! predefined / RLE / compressed / repeat sequence tables, repeat-offset history,
//! skippable frames, concatenated frames and the xxh64 content checksum.

const MAGIC: u32 = 0xFD2F_B528;
const BLOCK_MAX: usize = 128 * 1024;

#[inline(always)]
fn mask(n: u32) -> u64 {
    if n >= 64 { u64::MAX } else { (1u64 << n) - 1 }
}

/// Backward bitstream (read from the last byte towards the first, MSB first).
struct Rev<'a> {
    d: &'a [u8],
    /// remaining valid bits; becomes negative when the stream was over-read.
    pos: i64,
}

impl<'a> Rev<'a> {
    fn new(d: &'a [u8]) -> Result<Self, String> {
        let last = *d.last().ok_or("Zstd bitstream is empty")?;
        if last == 0 {
            return Err("Zstd bitstream end mark missing".into());
        }
        let h = 7 - last.leading_zeros() as i64;
        Ok(Self { d, pos: (d.len() as i64 - 1) * 8 + h })
    }

    #[inline(always)]
    fn load(&self, idx: usize) -> u64 {
        if idx + 8 <= self.d.len() {
            u64::from_le_bytes(self.d[idx..idx + 8].try_into().unwrap())
        } else {
            let mut b = [0u8; 8];
            if idx < self.d.len() {
                let n = self.d.len() - idx;
                b[..n].copy_from_slice(&self.d[idx..]);
            }
            u64::from_le_bytes(b)
        }
    }

    /// Peeks `n` (<= 32) bits; bits below the start of the stream read as zero.
    #[inline(always)]
    fn peek(&self, n: u32) -> u64 {
        if n == 0 {
            return 0;
        }
        let start = self.pos - n as i64;
        if start >= 0 {
            (self.load((start >> 3) as usize) >> (start & 7)) & mask(n)
        } else if self.pos <= 0 {
            0
        } else {
            (self.load(0) & mask(self.pos as u32)) << (-start)
        }
    }

    #[inline(always)]
    fn read(&mut self, n: u32) -> u64 {
        let v = self.peek(n);
        self.pos -= n as i64;
        v
    }
}

// ---------------------------------------------------------------------------
// FSE
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Fse {
    al: u32,
    sym: Vec<u8>,
    nb: Vec<u8>,
    base: Vec<u16>,
}

impl Fse {
    fn rle(symbol: u8) -> Self {
        Fse { al: 0, sym: vec![symbol], nb: vec![0], base: vec![0] }
    }

    fn build(norm: &[i16], al: u32) -> Result<Self, String> {
        let size = 1usize << al;
        let mut sym = vec![0u8; size];
        let mut next = vec![0u32; norm.len()];
        let mut high = size as isize - 1;
        for (s, &c) in norm.iter().enumerate() {
            if c == -1 {
                sym[high as usize] = s as u8;
                high -= 1;
                next[s] = 1;
            } else {
                next[s] = c as u32;
            }
        }
        let step = (size >> 1) + (size >> 3) + 3;
        let m = size - 1;
        let mut pos = 0usize;
        for (s, &c) in norm.iter().enumerate() {
            for _ in 0..c.max(0) {
                sym[pos] = s as u8;
                pos = (pos + step) & m;
                while pos as isize > high {
                    pos = (pos + step) & m;
                }
            }
        }
        if pos != 0 {
            return Err("Zstd FSE table is corrupt".into());
        }
        let mut nb = vec![0u8; size];
        let mut base = vec![0u16; size];
        for u in 0..size {
            let s = sym[u] as usize;
            let n = next[s];
            next[s] += 1;
            let bits = al - (31 - n.leading_zeros());
            nb[u] = bits as u8;
            base[u] = ((n << bits) as usize - size) as u16;
        }
        Ok(Fse { al, sym, nb, base })
    }
}

/// Reads an FSE table description. Returns (table, bytes consumed).
fn fse_read_table(data: &[u8], max_sv: usize, max_al: u32) -> Result<(Fse, usize), String> {
    let mut bitpos = 0usize;
    let get = |bitpos: usize, n: u32| -> u32 {
        let idx = bitpos >> 3;
        let mut b = [0u8; 8];
        if idx < data.len() {
            let k = (data.len() - idx).min(8);
            b[..k].copy_from_slice(&data[idx..idx + k]);
        }
        ((u64::from_le_bytes(b) >> (bitpos & 7)) & mask(n)) as u32
    };
    let al = get(bitpos, 4) + 5;
    bitpos += 4;
    if al > max_al {
        return Err("Zstd FSE accuracy log too large".into());
    }
    let mut remaining: i32 = (1 << al) + 1;
    let mut threshold: i32 = 1 << al;
    let mut nbits = al + 1;
    let mut norm: Vec<i16> = Vec::new();
    let mut prev0 = false;
    while remaining > 1 && norm.len() <= max_sv {
        if prev0 {
            let mut n0 = norm.len();
            loop {
                let r = get(bitpos, 2);
                bitpos += 2;
                n0 += r as usize;
                if r != 3 {
                    break;
                }
                if n0 > max_sv + 1 {
                    return Err("Zstd FSE table is corrupt".into());
                }
            }
            if n0 > max_sv + 1 {
                return Err("Zstd FSE table is corrupt".into());
            }
            while norm.len() < n0 {
                norm.push(0);
            }
            if norm.len() > max_sv {
                break;
            }
        }
        let max = (2 * threshold - 1) - remaining;
        let low = get(bitpos, nbits - 1) as i32;
        let mut count;
        if low < max {
            count = low;
            bitpos += (nbits - 1) as usize;
        } else {
            count = get(bitpos, nbits) as i32 & (2 * threshold - 1);
            if count >= threshold {
                count -= max;
            }
            bitpos += nbits as usize;
        }
        count -= 1;
        remaining -= count.abs();
        norm.push(count as i16);
        prev0 = count == 0;
        while remaining < threshold {
            nbits -= 1;
            threshold >>= 1;
        }
    }
    if remaining != 1 {
        return Err("Zstd FSE table is corrupt".into());
    }
    let used = (bitpos + 7) / 8;
    if used > data.len() {
        return Err("Zstd FSE table truncated".into());
    }
    Ok((Fse::build(&norm, al)?, used))
}

fn predefined_ll() -> Fse {
    const N: [i16; 36] = [
        4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1, -1, -1, -1, -1,
    ];
    Fse::build(&N, 6).unwrap()
}
fn predefined_ml() -> Fse {
    const N: [i16; 53] = [
        1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
    ];
    Fse::build(&N, 6).unwrap()
}
fn predefined_of() -> Fse {
    const N: [i16; 29] = [1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1];
    Fse::build(&N, 5).unwrap()
}

const LL_BASE: [u32; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64, 128, 256, 512, 1024, 2048,
    4096, 8192, 16384, 32768, 65536,
];
const LL_BITS: [u8; 36] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];
const ML_BASE: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35,
    37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 131, 259, 515, 1027, 2051, 4099, 8195, 16387, 32771, 65539,
];
const ML_BITS: [u8; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 4,
    5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];

// ---------------------------------------------------------------------------
// Huffman
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Huf {
    max_bits: u32,
    /// entry = symbol | (nbits << 8)
    table: Vec<u16>,
}

impl Huf {
    fn from_weights(weights: &[u8]) -> Result<Self, String> {
        if weights.is_empty() || weights.len() > 255 {
            return Err("Zstd Huffman weights invalid".into());
        }
        let mut total: u32 = 0;
        for &w in weights {
            if w > 11 {
                return Err("Zstd Huffman weight too large".into());
            }
            if w > 0 {
                total += 1 << (w - 1);
            }
        }
        if total == 0 {
            return Err("Zstd Huffman weights invalid".into());
        }
        let max_bits = 32 - total.leading_zeros(); // floor(log2)+1
        if max_bits > 11 {
            return Err("Zstd Huffman max bits too large".into());
        }
        let rest = (1u32 << max_bits) - total;
        if !rest.is_power_of_two() {
            return Err("Zstd Huffman weights do not sum to a power of two".into());
        }
        let last = (rest.trailing_zeros() + 1) as u8;
        let mut w: Vec<u8> = weights.to_vec();
        w.push(last);
        let size = 1usize << max_bits;
        let mut table = vec![0u16; size];
        let mut pos = 0usize;
        for weight in 1..=max_bits as u8 {
            let run = 1usize << (weight - 1);
            let nb = max_bits + 1 - weight as u32;
            for (s, &sw) in w.iter().enumerate() {
                if sw == weight {
                    for e in &mut table[pos..pos + run] {
                        *e = s as u16 | ((nb as u16) << 8);
                    }
                    pos += run;
                }
            }
        }
        if pos != size {
            return Err("Zstd Huffman table is corrupt".into());
        }
        Ok(Huf { max_bits, table })
    }

    fn decode_stream(&self, data: &[u8], count: usize, out: &mut Vec<u8>) -> Result<(), String> {
        let mut r = Rev::new(data)?;
        out.reserve(count);
        for _ in 0..count {
            let e = self.table[r.peek(self.max_bits) as usize];
            r.pos -= (e >> 8) as i64;
            out.push(e as u8);
        }
        if r.pos != 0 {
            return Err("Zstd Huffman stream is corrupt".into());
        }
        Ok(())
    }
}

/// Reads a Huffman tree description. Returns (table, bytes consumed).
fn read_huf_tree(data: &[u8]) -> Result<(Huf, usize), String> {
    let hb = *data.first().ok_or("Zstd Huffman header truncated")? as usize;
    let mut weights: Vec<u8> = Vec::new();
    let used;
    if hb >= 128 {
        let n = hb - 127;
        let nbytes = (n + 1) / 2;
        if data.len() < 1 + nbytes {
            return Err("Zstd Huffman header truncated".into());
        }
        for i in 0..n {
            let b = data[1 + i / 2];
            weights.push(if i % 2 == 0 { b >> 4 } else { b & 15 });
        }
        used = 1 + nbytes;
    } else {
        if hb == 0 || data.len() < 1 + hb {
            return Err("Zstd Huffman header invalid".into());
        }
        let comp = &data[1..1 + hb];
        let (fse, tlen) = fse_read_table(comp, 255, 6)?;
        let mut r = Rev::new(&comp[tlen..])?;
        let mut s1 = r.read(fse.al) as usize;
        let mut s2 = r.read(fse.al) as usize;
        loop {
            if weights.len() > 255 {
                return Err("Zstd Huffman weights overflow".into());
            }
            weights.push(fse.sym[s1]);
            s1 = fse.base[s1] as usize + r.read(fse.nb[s1] as u32) as usize;
            if r.pos < 0 {
                weights.push(fse.sym[s2]);
                break;
            }
            weights.push(fse.sym[s2]);
            s2 = fse.base[s2] as usize + r.read(fse.nb[s2] as u32) as usize;
            if r.pos < 0 {
                weights.push(fse.sym[s1]);
                break;
            }
        }
        if weights.len() > 255 {
            return Err("Zstd Huffman weights overflow".into());
        }
        used = 1 + hb;
    }
    Ok((Huf::from_weights(&weights)?, used))
}

// ---------------------------------------------------------------------------
// Frame decoding
// ---------------------------------------------------------------------------

struct FrameState {
    huf: Option<Huf>,
    ll: Option<Fse>,
    of: Option<Fse>,
    ml: Option<Fse>,
    reps: [usize; 3],
}

fn decode_literals(
    blk: &[u8],
    st: &mut FrameState,
    lits: &mut Vec<u8>,
) -> Result<usize, String> {
    lits.clear();
    if blk.is_empty() {
        return Err("Zstd literals section truncated".into());
    }
    let b0 = blk[0];
    let ltype = b0 & 3;
    let sf = (b0 >> 2) & 3;
    match ltype {
        0 | 1 => {
            let (size, hdr) = match sf {
                0 | 2 => ((b0 >> 3) as usize, 1),
                1 => {
                    if blk.len() < 2 {
                        return Err("Zstd literals header truncated".into());
                    }
                    (((b0 >> 4) as usize) | ((blk[1] as usize) << 4), 2)
                }
                _ => {
                    if blk.len() < 3 {
                        return Err("Zstd literals header truncated".into());
                    }
                    (((b0 >> 4) as usize) | ((blk[1] as usize) << 4) | ((blk[2] as usize) << 12), 3)
                }
            };
            if size > BLOCK_MAX {
                return Err("Zstd literals too large".into());
            }
            if ltype == 0 {
                if blk.len() < hdr + size {
                    return Err("Zstd raw literals truncated".into());
                }
                lits.extend_from_slice(&blk[hdr..hdr + size]);
                Ok(hdr + size)
            } else {
                if blk.len() < hdr + 1 {
                    return Err("Zstd RLE literals truncated".into());
                }
                lits.resize(size, blk[hdr]);
                Ok(hdr + 1)
            }
        }
        _ => {
            let (hdr, streams, bits) = match sf {
                0 => (3usize, 1usize, 10u32),
                1 => (3, 4, 10),
                2 => (4, 4, 14),
                _ => (5, 4, 18),
            };
            if blk.len() < hdr {
                return Err("Zstd literals header truncated".into());
            }
            let mut v = 0u64;
            for i in 0..hdr {
                v |= (blk[i] as u64) << (8 * i);
            }
            v >>= 4;
            let regen = (v & mask(bits)) as usize;
            let comp = ((v >> bits) & mask(bits)) as usize;
            if regen > BLOCK_MAX {
                return Err("Zstd literals too large".into());
            }
            if blk.len() < hdr + comp {
                return Err("Zstd compressed literals truncated".into());
            }
            let body = &blk[hdr..hdr + comp];
            let mut off = 0usize;
            if ltype == 2 {
                let (h, used) = read_huf_tree(body)?;
                st.huf = Some(h);
                off = used;
            }
            let huf = st.huf.as_ref().ok_or("Zstd treeless literals without previous Huffman table")?;
            let data = &body[off..];
            if streams == 1 {
                huf.decode_stream(data, regen, lits)?;
            } else {
                if data.len() < 6 {
                    return Err("Zstd literal jump table truncated".into());
                }
                let s1 = u16::from_le_bytes([data[0], data[1]]) as usize;
                let s2 = u16::from_le_bytes([data[2], data[3]]) as usize;
                let s3 = u16::from_le_bytes([data[4], data[5]]) as usize;
                let rest = &data[6..];
                if s1 + s2 + s3 > rest.len() {
                    return Err("Zstd literal stream sizes invalid".into());
                }
                let seg = (regen + 3) / 4;
                if seg * 3 > regen {
                    return Err("Zstd literal stream sizes invalid".into());
                }
                huf.decode_stream(&rest[..s1], seg, lits)?;
                huf.decode_stream(&rest[s1..s1 + s2], seg, lits)?;
                huf.decode_stream(&rest[s1 + s2..s1 + s2 + s3], seg, lits)?;
                huf.decode_stream(&rest[s1 + s2 + s3..], regen - 3 * seg, lits)?;
            }
            Ok(hdr + comp)
        }
    }
}

fn read_seq_table(
    mode: u8,
    blk: &[u8],
    pos: &mut usize,
    prev: &Option<Fse>,
    predefined: fn() -> Fse,
    max_sv: usize,
    max_al: u32,
) -> Result<Fse, String> {
    match mode {
        0 => Ok(predefined()),
        1 => {
            let b = *blk.get(*pos).ok_or("Zstd sequence table truncated")?;
            *pos += 1;
            if b as usize > max_sv {
                return Err("Zstd RLE sequence symbol out of range".into());
            }
            Ok(Fse::rle(b))
        }
        2 => {
            let (t, used) = fse_read_table(&blk[(*pos).min(blk.len())..], max_sv, max_al)?;
            *pos += used;
            Ok(t)
        }
        _ => prev.clone().ok_or_else(|| "Zstd repeat sequence table without previous table".to_string()),
    }
}

fn decode_block(
    blk: &[u8],
    st: &mut FrameState,
    lits: &mut Vec<u8>,
    out: &mut Vec<u8>,
    frame_start: usize,
    window: u64,
) -> Result<(), String> {
    let block_out_start = out.len();
    let mut pos = decode_literals(blk, st, lits)?;
    if pos >= blk.len() {
        return Err("Zstd sequences section truncated".into());
    }
    let b0 = blk[pos] as usize;
    pos += 1;
    let nseq = if b0 == 0 {
        0
    } else if b0 < 128 {
        b0
    } else if b0 < 255 {
        if pos >= blk.len() {
            return Err("Zstd sequences header truncated".into());
        }
        let v = ((b0 - 128) << 8) + blk[pos] as usize;
        pos += 1;
        v
    } else {
        if pos + 2 > blk.len() {
            return Err("Zstd sequences header truncated".into());
        }
        let v = blk[pos] as usize + ((blk[pos + 1] as usize) << 8) + 0x7F00;
        pos += 2;
        v
    };
    if nseq == 0 {
        if pos != blk.len() {
            return Err("Zstd block has trailing data".into());
        }
        out.extend_from_slice(lits);
        return Ok(());
    }
    if pos >= blk.len() {
        return Err("Zstd sequences header truncated".into());
    }
    let modes = blk[pos];
    pos += 1;
    if modes & 3 != 0 {
        return Err("Zstd reserved sequence mode bits set".into());
    }
    let ll = read_seq_table(modes >> 6, blk, &mut pos, &st.ll, predefined_ll, 35, 9)?;
    let of = read_seq_table((modes >> 4) & 3, blk, &mut pos, &st.of, predefined_of, 31, 8)?;
    let ml = read_seq_table((modes >> 2) & 3, blk, &mut pos, &st.ml, predefined_ml, 52, 9)?;
    // For FSE tables the symbol range was validated while building; make sure sym tables
    // cannot index outside the base tables.
    if ll.sym.iter().any(|&s| s as usize >= LL_BASE.len())
        || ml.sym.iter().any(|&s| s as usize >= ML_BASE.len())
        || of.sym.iter().any(|&s| s > 31)
    {
        return Err("Zstd sequence symbol out of range".into());
    }
    let mut r = Rev::new(&blk[pos..])?;
    let mut s_ll = r.read(ll.al) as usize;
    let mut s_of = r.read(of.al) as usize;
    let mut s_ml = r.read(ml.al) as usize;

    let mut lit_pos = 0usize;
    let mut reps = st.reps;
    let out_limit = block_out_start + BLOCK_MAX;
    let avail_total = |out: &Vec<u8>| (out.len() - frame_start) as u64;

    for i in 0..nseq {
        let llc = ll.sym[s_ll] as usize;
        let mlc = ml.sym[s_ml] as usize;
        let ofc = of.sym[s_of] as u32;
        let off_val = (1u64 << ofc) + r.read(ofc);
        let mlen = ML_BASE[mlc] as usize + r.read(ML_BITS[mlc] as u32) as usize;
        let llen = LL_BASE[llc] as usize + r.read(LL_BITS[llc] as u32) as usize;
        if i + 1 < nseq {
            s_ll = ll.base[s_ll] as usize + r.read(ll.nb[s_ll] as u32) as usize;
            s_ml = ml.base[s_ml] as usize + r.read(ml.nb[s_ml] as u32) as usize;
            s_of = of.base[s_of] as usize + r.read(of.nb[s_of] as u32) as usize;
        }
        if r.pos < 0 {
            return Err("Zstd sequence bitstream overrun".into());
        }

        // Resolve offset.
        let offset: u64;
        if off_val > 3 {
            offset = off_val - 3;
            reps = [offset as usize, reps[0], reps[1]];
        } else {
            let idx = off_val as usize + if llen == 0 { 1 } else { 0 };
            match idx {
                1 => offset = reps[0] as u64,
                2 => {
                    offset = reps[1] as u64;
                    reps = [reps[1], reps[0], reps[2]];
                }
                3 => {
                    offset = reps[2] as u64;
                    reps = [reps[2], reps[0], reps[1]];
                }
                _ => {
                    let o = reps[0] as i64 - 1;
                    if o <= 0 {
                        return Err("Zstd invalid repeat offset".into());
                    }
                    offset = o as u64;
                    reps = [offset as usize, reps[0], reps[1]];
                }
            }
        }

        if llen > lits.len() - lit_pos {
            return Err("Zstd sequence literal length exceeds literals".into());
        }
        if out.len() + llen + mlen > out_limit {
            return Err("Zstd block output exceeds block size limit".into());
        }
        out.extend_from_slice(&lits[lit_pos..lit_pos + llen]);
        lit_pos += llen;

        if offset == 0 || offset > avail_total(out) || offset > window {
            return Err(format!("Invalid zstd match offset: {} (out len is {})", offset, avail_total(out)));
        }
        let offset = offset as usize;
        let cur = out.len();
        if offset >= mlen {
            out.extend_from_within(cur - offset..cur - offset + mlen);
        } else {
            out.reserve(mlen);
            for k in 0..mlen {
                let b = out[cur - offset + k];
                out.push(b);
            }
        }
    }
    if r.pos != 0 {
        return Err("Zstd sequence bitstream not fully consumed".into());
    }
    if out.len() + (lits.len() - lit_pos) > out_limit {
        return Err("Zstd block output exceeds block size limit".into());
    }
    out.extend_from_slice(&lits[lit_pos..]);
    st.reps = reps;
    st.ll = Some(ll);
    st.of = Some(of);
    st.ml = Some(ml);
    Ok(())
}

pub fn zstd_decompress(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    match zstd_into(input, &mut out) {
        Ok(()) => Ok(out),
        Err(e) => {
            super::stash_partial(out);
            Err(e)
        }
    }
}

fn zstd_into(input: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
    let mut pos = 0usize;
    let mut lits: Vec<u8> = Vec::new();
    let mut frames = 0usize;

    while pos < input.len() {
        if input.len() - pos < 4 {
            return Err(if frames > 0 { "Not a zstd archive (bad magic number)".into() } else { "Zstd frame header truncated".into() });
        }
        let magic = u32::from_le_bytes(input[pos..pos + 4].try_into().unwrap());
        if magic & 0xFFFF_FFF0 == 0x184D_2A50 {
            if input.len() - pos < 8 {
                return Err("Zstd skippable frame truncated".into());
            }
            let n = u32::from_le_bytes(input[pos + 4..pos + 8].try_into().unwrap()) as usize;
            if input.len() - pos - 8 < n {
                return Err("Zstd skippable frame truncated".into());
            }
            pos += 8 + n;
            frames += 1;
            continue;
        }
        if magic != MAGIC {
            return Err(if frames == 0 { "Not a zstd archive (bad magic number)".into() } else { "Zstd: unknown frame magic".to_string() });
        }
        pos += 4;
        let fd = *input.get(pos).ok_or("Zstd frame header truncated")?;
        pos += 1;
        let fcs_flag = fd >> 6;
        let single = fd & 0x20 != 0;
        if fd & 0x08 != 0 {
            return Err("Zstd frame header has reserved bit set".into());
        }
        let has_checksum = fd & 0x04 != 0;
        let dict_flag = fd & 3;
        let mut window: u64 = 0;
        if !single {
            let wd = *input.get(pos).ok_or("Zstd frame header truncated")?;
            pos += 1;
            let exp = (wd >> 3) as u32;
            let mant = (wd & 7) as u64;
            let base = 1u64 << (10 + exp);
            window = base + (base / 8) * mant;
        }
        let dict_len = match dict_flag {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 4,
        };
        if input.len() - pos < dict_len {
            return Err("Zstd frame header truncated".into());
        }
        let mut dict_id = 0u32;
        for i in 0..dict_len {
            dict_id |= (input[pos + i] as u32) << (8 * i);
        }
        pos += dict_len;
        if dict_id != 0 {
            return Err(format!("Zstd frame requires dictionary {dict_id}, which is not supported"));
        }
        let fcs_len = match fcs_flag {
            0 => if single { 1 } else { 0 },
            1 => 2,
            2 => 4,
            _ => 8,
        };
        if input.len() - pos < fcs_len {
            return Err("Zstd frame header truncated".into());
        }
        let mut fcs: Option<u64> = None;
        if fcs_len > 0 {
            let mut v = 0u64;
            for i in 0..fcs_len {
                v |= (input[pos + i] as u64) << (8 * i);
            }
            if fcs_len == 2 {
                v += 256;
            }
            fcs = Some(v);
        }
        pos += fcs_len;
        if single {
            window = fcs.unwrap_or(0);
        }

        let frame_start = out.len();
        if let Some(n) = fcs {
            out.reserve(n.min(1 << 28) as usize);
        }
        let mut st = FrameState { huf: None, ll: None, of: None, ml: None, reps: [1, 4, 8] };

        loop {
            if input.len() - pos < 3 {
                return Err("Truncated block data".into());
            }
            let h = input[pos] as u32 | ((input[pos + 1] as u32) << 8) | ((input[pos + 2] as u32) << 16);
            pos += 3;
            let last = h & 1 != 0;
            let btype = (h >> 1) & 3;
            let bsize = (h >> 3) as usize;
            match btype {
                0 => {
                    if bsize > BLOCK_MAX || input.len() - pos < bsize {
                        return Err("Truncated block data".into());
                    }
                    out.extend_from_slice(&input[pos..pos + bsize]);
                    pos += bsize;
                }
                1 => {
                    if bsize > BLOCK_MAX || input.len() - pos < 1 {
                        return Err("Truncated block data".into());
                    }
                    let b = input[pos];
                    pos += 1;
                    let n = out.len() + bsize;
                    out.resize(n, b);
                }
                2 => {
                    if bsize > BLOCK_MAX || input.len() - pos < bsize {
                        return Err("Truncated block data".into());
                    }
                    decode_block(&input[pos..pos + bsize], &mut st, &mut lits, out, frame_start, window)?;
                    pos += bsize;
                }
                _ => return Err("Zstd reserved block type".into()),
            }
            if last {
                break;
            }
        }
        if let Some(n) = fcs {
            if (out.len() - frame_start) as u64 != n {
                return Err("Zstd frame content size mismatch".into());
            }
        }
        if has_checksum {
            if input.len() - pos < 4 {
                return Err("Zstd content checksum truncated".into());
            }
            let expect = u32::from_le_bytes(input[pos..pos + 4].try_into().unwrap());
            pos += 4;
            if (xxh64(&out[frame_start..], 0) & 0xFFFF_FFFF) as u32 != expect {
                return Err("Zstd content checksum mismatch".into());
            }
        }
        frames += 1;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Content checksum: xxHash-64
// ---------------------------------------------------------------------------

pub fn xxh64(data: &[u8], seed: u64) -> u64 {
    let p1 = 0x9E3779B185EBCA87u64;
    let p2 = 0xC2B2AE3D27D4EB4Fu64;
    let p3 = 0x165667B19E3779F9u64;
    let p4 = 0x85EBCA77C2B2AE63u64;
    let p5 = 0x27D4EB2F165667C5u64;

    let round = |acc: u64, val: u64| acc.wrapping_add(val.wrapping_mul(p2)).rotate_left(31).wrapping_mul(p1);
    let merge_round = |acc: u64, val: u64| {
        let v = round(0, val);
        (acc ^ v).wrapping_mul(p1).wrapping_add(p4)
    };

    let len = data.len();
    let mut h64 = if len >= 32 {
        let mut v1 = seed.wrapping_add(p1).wrapping_add(p2);
        let mut v2 = seed.wrapping_add(p2);
        let mut v3 = seed;
        let mut v4 = seed.wrapping_sub(p1);

        for chunk in data[..len - len % 32].chunks_exact(32) {
            let u0 = u64::from_le_bytes(chunk[0..8].try_into().unwrap());
            let u1 = u64::from_le_bytes(chunk[8..16].try_into().unwrap());
            let u2 = u64::from_le_bytes(chunk[16..24].try_into().unwrap());
            let u3 = u64::from_le_bytes(chunk[24..32].try_into().unwrap());

            v1 = round(v1, u0);
            v2 = round(v2, u1);
            v3 = round(v3, u2);
            v4 = round(v4, u3);
        }

        let mut h = v1.rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
        h = merge_round(h, v1);
        h = merge_round(h, v2);
        h = merge_round(h, v3);
        h = merge_round(h, v4);
        h
    } else {
        seed.wrapping_add(p5)
    };

    h64 = h64.wrapping_add(len as u64);

    let mut i = len - len % 32;
    while i + 8 <= len {
        let u = u64::from_le_bytes(data[i..i + 8].try_into().unwrap());
        let k1 = round(0, u);
        h64 = (h64 ^ k1).rotate_left(27).wrapping_mul(p1).wrapping_add(p4);
        i += 8;
    }
    if i + 4 <= len {
        let u = u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as u64;
        h64 = (h64 ^ (u.wrapping_mul(p1))).rotate_left(23).wrapping_mul(p2).wrapping_add(p3);
        i += 4;
    }
    while i < len {
        let u = data[i] as u64;
        h64 = (h64 ^ (u.wrapping_mul(p5))).rotate_left(11).wrapping_mul(p1);
        i += 1;
    }

    h64 ^= h64 >> 33;
    h64 = h64.wrapping_mul(p2);
    h64 ^= h64 >> 29;
    h64 = h64.wrapping_mul(p3);
    h64 ^= h64 >> 32;

    h64
}
