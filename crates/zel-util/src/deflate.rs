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

//! Pure-Rust DEFLATE (RFC 1951) / gzip (RFC 1952): an LZ77 hash-chain compressor emitting
//! fixed-Huffman blocks, and a complete inflater (stored, fixed and dynamic blocks).

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// CRC-32 (IEEE 802.3) as used by gzip.
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    });
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn bits(&mut self, v: u32, count: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Huffman codes are sent most-significant bit first.
    fn code(&mut self, code: u32, len: u32) {
        let mut rev = 0;
        for i in 0..len {
            if code & (1 << i) != 0 {
                rev |= 1 << (len - 1 - i);
            }
        }
        self.bits(rev, len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

fn write_lit(w: &mut BitWriter, sym: u32) {
    match sym {
        0..=143 => w.code(0x30 + sym, 8),
        144..=255 => w.code(0x190 + sym - 144, 9),
        256..=279 => w.code(sym - 256, 7),
        _ => w.code(0xC0 + sym - 280, 8),
    }
}

/// Raw DEFLATE stream using LZ77 + fixed Huffman coding.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter { out: Vec::with_capacity(data.len() / 3 + 16), acc: 0, n: 0 };
    w.bits(1, 1); // BFINAL
    w.bits(1, 2); // fixed Huffman
    const HBITS: usize = 15;
    let mut head = vec![usize::MAX; 1 << HBITS];
    let mut prev = vec![usize::MAX; data.len()];
    let hash = |i: usize| -> usize {
        let v = (data[i] as u32) << 16 | (data[i + 1] as u32) << 8 | data[i + 2] as u32;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HBITS as u32)) as usize
    };
    let mut i = 0;
    while i < data.len() {
        let mut best_len = 0;
        let mut best_dist = 0;
        if i + 3 <= data.len() {
            let h = hash(i);
            let mut cand = head[h];
            let mut chain = 64;
            while cand != usize::MAX && chain > 0 && i - cand <= 32768 {
                let max = (data.len() - i).min(258);
                let mut l = 0;
                while l < max && data[cand + l] == data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - cand;
                    if l == max {
                        break;
                    }
                }
                cand = prev[cand];
                chain -= 1;
            }
            prev[i] = head[h];
            head[h] = i;
        }
        if best_len >= 3 {
            let li = LEN_BASE.iter().rposition(|&b| b as usize <= best_len).unwrap();
            write_lit(&mut w, 257 + li as u32);
            w.bits((best_len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
            let di = DIST_BASE.iter().rposition(|&b| b as usize <= best_dist).unwrap();
            w.code(di as u32, 5);
            w.bits((best_dist - DIST_BASE[di] as usize) as u32, DIST_EXTRA[di] as u32);
            // Insert skipped positions into the hash chains.
            for k in 1..best_len {
                let p = i + k;
                if p + 3 <= data.len() {
                    let h = hash(p);
                    prev[p] = head[h];
                    head[h] = p;
                }
            }
            i += best_len;
        } else {
            write_lit(&mut w, data[i] as u32);
            i += 1;
        }
    }
    write_lit(&mut w, 256);
    w.finish()
}

/// gzip container around [`deflate`] (what `java.util.zip.GZIPOutputStream` produces).
pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    out.extend(deflate(data));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
}

impl<'a> BitReader<'a> {
    fn bits(&mut self, count: u32) -> Result<u32, String> {
        while self.n < count {
            let b = *self.data.get(self.pos).ok_or("unexpected end of deflate stream")?;
            self.pos += 1;
            self.acc |= (b as u32) << self.n;
            self.n += 8;
        }
        let v = if count == 32 { self.acc } else { self.acc & ((1u32 << count) - 1) };
        self.acc = if count == 32 { 0 } else { self.acc >> count };
        self.n -= count;
        Ok(v)
    }

    fn align(&mut self) {
        self.acc = 0;
        self.n = 0;
    }
}

struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Self {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..16 {
            offs[i] = offs[i - 1] + counts[i - 1];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Self { counts, symbols }
    }

    fn decode(&self, r: &mut BitReader) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= r.bits(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("invalid Huffman code".into())
    }
}

/// Inflates a raw DEFLATE stream; returns the data and the number of input bytes consumed.
pub fn inflate(data: &[u8]) -> Result<(Vec<u8>, usize), String> {
    let mut r = BitReader { data, pos: 0, acc: 0, n: 0 };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = r.bits(1)? == 1;
        match r.bits(2)? {
            0 => {
                r.align();
                if r.pos + 4 > data.len() {
                    return Err("truncated stored block".into());
                }
                let len = u16::from_le_bytes([data[r.pos], data[r.pos + 1]]) as usize;
                r.pos += 4;
                if r.pos + len > data.len() {
                    return Err("truncated stored block".into());
                }
                out.extend_from_slice(&data[r.pos..r.pos + len]);
                r.pos += len;
            }
            t @ (1 | 2) => {
                let (lit, dist) = if t == 1 {
                    let mut l = [0u8; 288];
                    for (i, v) in l.iter_mut().enumerate() {
                        *v = match i {
                            0..=143 => 8,
                            144..=255 => 9,
                            256..=279 => 7,
                            _ => 8,
                        };
                    }
                    (Huffman::new(&l), Huffman::new(&[5u8; 30]))
                } else {
                    let hlit = r.bits(5)? as usize + 257;
                    let hdist = r.bits(5)? as usize + 1;
                    let hclen = r.bits(4)? as usize + 4;
                    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                    let mut cl = [0u8; 19];
                    for &o in ORDER.iter().take(hclen) {
                        cl[o] = r.bits(3)? as u8;
                    }
                    let clh = Huffman::new(&cl);
                    let mut lengths = vec![0u8; hlit + hdist];
                    let mut i = 0;
                    while i < hlit + hdist {
                        let sym = clh.decode(&mut r)?;
                        match sym {
                            0..=15 => {
                                lengths[i] = sym as u8;
                                i += 1;
                            }
                            16 => {
                                if i == 0 {
                                    return Err("repeat with no previous length".into());
                                }
                                let prev = lengths[i - 1];
                                for _ in 0..3 + r.bits(2)? {
                                    if i < lengths.len() {
                                        lengths[i] = prev;
                                        i += 1;
                                    }
                                }
                            }
                            17 => i += 3 + r.bits(3)? as usize,
                            _ => i += 11 + r.bits(7)? as usize,
                        }
                    }
                    lengths.truncate(hlit + hdist);
                    (Huffman::new(&lengths[..hlit]), Huffman::new(&lengths[hlit..]))
                };
                loop {
                    let sym = lit.decode(&mut r)? as usize;
                    match sym {
                        0..=255 => out.push(sym as u8),
                        256 => break,
                        257..=285 => {
                            let s = sym - 257;
                            let len = LEN_BASE[s] as usize + r.bits(LEN_EXTRA[s] as u32)? as usize;
                            let ds = dist.decode(&mut r)? as usize;
                            if ds >= 30 {
                                return Err("bad distance symbol".into());
                            }
                            let d = DIST_BASE[ds] as usize + r.bits(DIST_EXTRA[ds] as u32)? as usize;
                            if d > out.len() {
                                return Err("distance too far back".into());
                            }
                            for _ in 0..len {
                                let b = out[out.len() - d];
                                out.push(b);
                            }
                        }
                        _ => return Err("bad literal/length symbol".into()),
                    }
                }
            }
            _ => return Err("invalid deflate block type".into()),
        }
        if last {
            break;
        }
    }
    Ok((out, r.pos))
}

/// Decompresses a gzip stream (single member), verifying CRC and length.
pub fn gunzip(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
        return Err("not in GZIP format".into());
    }
    let flags = data[3];
    let mut p = 10;
    if flags & 4 != 0 {
        let xlen = u16::from_le_bytes([data[p], data[p + 1]]) as usize;
        p += 2 + xlen;
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            while *data.get(p).ok_or("truncated gzip header")? != 0 {
                p += 1;
            }
            p += 1;
        }
    }
    if flags & 2 != 0 {
        p += 2;
    }
    let (out, used) = inflate(data.get(p..).ok_or("truncated gzip")?)?;
    let t = p + used;
    if data.len() < t + 8 {
        return Err("truncated gzip trailer".into());
    }
    let crc = u32::from_le_bytes(data[t..t + 4].try_into().unwrap());
    let isize = u32::from_le_bytes(data[t + 4..t + 8].try_into().unwrap());
    if crc != crc32(&out) || isize != out.len() as u32 {
        return Err("gzip CRC/length mismatch".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn roundtrip_various() {
        let mut big = Vec::new();
        for i in 0..5000 {
            big.extend_from_slice(format!("{{\"name\":\"event {}\",\"ph\":\"X\",\"ts\":{}}},\n", i % 37, i * 13).as_bytes());
        }
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            b"a".to_vec(),
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec(),
            (0..=255u8).collect(),
            big.clone(),
        ];
        for c in cases {
            let z = gzip(&c);
            assert_eq!(gunzip(&z).unwrap(), c);
        }
        let z = gzip(&big);
        assert!(z.len() * 4 < big.len(), "poor ratio {} -> {}", big.len(), z.len());
    }

    #[test]
    fn inflates_stored_block() {
        // Stored block: "hello"
        let stored = [1u8, 5, 0, 0xfa, 0xff, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(inflate(&stored).unwrap().0, b"hello");
    }

    #[test]
    fn interoperates_with_system_gzip_when_available() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let data: Vec<u8> = (0..20000).map(|i| (i * 7 % 251) as u8).chain(b"hello hello hello".iter().copied()).collect();
        // Our output must be readable by gzip -d.
        let Ok(mut child) = Command::new("gzip").arg("-dc").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn() else {
            return;
        };
        let z = gzip(&data);
        child.stdin.take().unwrap().write_all(&z).unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.stdout, data);
        // And we must read gzip -9's dynamic-Huffman output.
        let Ok(mut c2) = Command::new("gzip").arg("-9c").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn() else {
            return;
        };
        let mut stdin = c2.stdin.take().unwrap();
        let d2 = data.clone();
        let t = std::thread::spawn(move || {
            stdin.write_all(&d2).unwrap();
        });
        let out = c2.wait_with_output().unwrap();
        t.join().unwrap();
        assert_eq!(gunzip(&out.stdout).unwrap(), data);
    }
}
