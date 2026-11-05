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

//! `DigestHashFunction`: the hash functions Bazel registers for `--digest_function`
//! (`SHA-1`, `SHA-256`, `SHA-384`, `SHA-512` in `DigestHashFunction`, `BLAKE3` in
//! `BazelHashFunctions`), each with its alternative spellings, plus from-scratch
//! implementations of SHA-1, SHA-384/512 and BLAKE3 (SHA-256 comes from `zel_util`).

use zel_util::{sha256, to_hex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub enum DigestFunction {
    Sha1,
    #[default]
    Sha256,
    Sha384,
    Sha512,
    Blake3,
}

impl DigestFunction {
    pub const ALL: [DigestFunction; 5] = [
        DigestFunction::Sha1,
        DigestFunction::Sha256,
        DigestFunction::Sha384,
        DigestFunction::Sha512,
        DigestFunction::Blake3,
    ];

    /// Canonical name (`toString()`).
    pub fn name(self) -> &'static str {
        match self {
            DigestFunction::Sha1 => "SHA-1",
            DigestFunction::Sha256 => "SHA-256",
            DigestFunction::Sha384 => "SHA-384",
            DigestFunction::Sha512 => "SHA-512",
            DigestFunction::Blake3 => "BLAKE3",
        }
    }

    fn names(self) -> &'static [&'static str] {
        match self {
            DigestFunction::Sha1 => &["SHA-1", "SHA1", "sha1"],
            DigestFunction::Sha256 => &["SHA-256", "SHA256", "sha256"],
            DigestFunction::Sha384 => &["SHA-384", "SHA384", "sha384"],
            DigestFunction::Sha512 => &["SHA-512", "SHA512", "sha512"],
            DigestFunction::Blake3 => &["BLAKE3"],
        }
    }

    /// `DigestFunctionConverter.convert` (case-insensitive match against all registered names).
    pub fn parse(input: &str) -> Result<DigestFunction, String> {
        for f in Self::ALL {
            if f.names().iter().any(|n| n.eq_ignore_ascii_case(input)) {
                return Ok(f);
            }
        }
        let mut names: Vec<&str> = Self::ALL.iter().map(|f| f.name()).collect();
        names.sort();
        Err(format!(
            "'{input}' is not a valid hash function. Possible values are: {}",
            names.join(", ")
        ))
    }

    pub fn digest_length(self) -> usize {
        match self {
            DigestFunction::Sha1 => 20,
            DigestFunction::Sha256 | DigestFunction::Blake3 => 32,
            DigestFunction::Sha384 => 48,
            DigestFunction::Sha512 => 64,
        }
    }

    pub fn hash(self, data: &[u8]) -> Vec<u8> {
        match self {
            DigestFunction::Sha1 => sha1(data).to_vec(),
            DigestFunction::Sha256 => sha256::digest(data).to_vec(),
            DigestFunction::Sha384 => sha384(data).to_vec(),
            DigestFunction::Sha512 => sha512(data).to_vec(),
            DigestFunction::Blake3 => blake3(data).to_vec(),
        }
    }

    pub fn hex(self, data: &[u8]) -> String {
        to_hex(&self.hash(data))
    }

    /// Digest of a file's contents.
    pub fn hex_file(self, path: &std::path::Path) -> std::io::Result<String> {
        Ok(self.hex(&std::fs::read(path)?))
    }
}

/// The server-wide `--digest_function` startup option (set per command from the client's startup
/// options; `Sha256` by default).
static SERVER_DIGEST: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

pub fn set_server_digest_function(f: DigestFunction) {
    let v = DigestFunction::ALL.iter().position(|x| *x == f).unwrap_or(1) as u8;
    SERVER_DIGEST.store(v, std::sync::atomic::Ordering::SeqCst);
}

pub fn server_digest_function() -> DigestFunction {
    DigestFunction::ALL[SERVER_DIGEST.load(std::sync::atomic::Ordering::SeqCst) as usize % DigestFunction::ALL.len()]
}

/// Extracts `--digest_function=<value>` from raw startup options (last one wins) and validates it.
pub fn digest_function_from_startup_options(opts: &[String]) -> Result<DigestFunction, String> {
    let mut f = DigestFunction::default();
    let mut it = opts.iter().peekable();
    while let Some(o) = it.next() {
        if let Some(v) = o.strip_prefix("--digest_function=") {
            f = DigestFunction::parse(v)?;
        } else if o == "--digest_function" {
            if let Some(v) = it.next() {
                f = DigestFunction::parse(v)?;
            }
        }
    }
    Ok(f)
}

// ------------------------------------------------------------------------------- SHA-1

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

// ------------------------------------------------------------------------ SHA-384 / SHA-512

const K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc, 0x3956c25bf348b538,
    0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118, 0xd807aa98a3030242, 0x12835b0145706fbe,
    0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2, 0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235,
    0xc19bf174cf692694, 0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5, 0x983e5152ee66dfab,
    0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4, 0xc6e00bf33da88fc2, 0xd5a79147930aa725,
    0x06ca6351e003826f, 0x142929670a0e6e70, 0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df, 0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30, 0xd192e819d6ef5218,
    0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8, 0x19a4c116b8d2d0c8, 0x1e376c085141ab53,
    0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8, 0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3, 0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b, 0xca273eceea26619c,
    0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178, 0x06f067aa72176fba, 0x0a637dc5a2c898a6,
    0x113f9804bef90dae, 0x1b710b35131c471b, 0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c, 0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

fn sha512_core(data: &[u8], init: [u64; 8]) -> [u64; 8] {
    let mut h = init;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 128 != 112 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u128) * 8).to_be_bytes());
    for chunk in msg.chunks(128) {
        let mut w = [0u64; 80];
        for i in 0..16 {
            let mut b = [0u8; 8];
            b.copy_from_slice(&chunk[8 * i..8 * i + 8]);
            w[i] = u64::from_be_bytes(b);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..80 {
            let s1 = v[4].rotate_right(14) ^ v[4].rotate_right(18) ^ v[4].rotate_right(41);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K512[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(28) ^ v[0].rotate_right(34) ^ v[0].rotate_right(39);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h
}

pub fn sha512(data: &[u8]) -> [u8; 64] {
    let h = sha512_core(
        data,
        [
            0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1, 0x510e527fade682d1,
            0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
        ],
    );
    let mut out = [0u8; 64];
    for (i, v) in h.iter().enumerate() {
        out[8 * i..8 * i + 8].copy_from_slice(&v.to_be_bytes());
    }
    out
}

pub fn sha384(data: &[u8]) -> [u8; 48] {
    let h = sha512_core(
        data,
        [
            0xcbbb9d5dc1059ed8, 0x629a292a367cd507, 0x9159015a3070dd17, 0x152fecd8f70e5939, 0x67332667ffc00b31,
            0x8eb44a8768581511, 0xdb0c2e0d64f98fa7, 0x47b5481dbefa4fa4,
        ],
    );
    let mut out = [0u8; 48];
    for i in 0..6 {
        out[8 * i..8 * i + 8].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

// ------------------------------------------------------------------------------- BLAKE3

const B3_IV: [u32; 8] = [
    0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
];
const B3_PERM: [usize; 16] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];
const CHUNK_START: u32 = 1;
const CHUNK_END: u32 = 2;
const PARENT: u32 = 4;
const ROOT: u32 = 8;

#[inline(always)]
fn g(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, mx: u32, my: u32) {
    s[a] = s[a].wrapping_add(s[b]).wrapping_add(mx);
    s[d] = (s[d] ^ s[a]).rotate_right(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_right(12);
    s[a] = s[a].wrapping_add(s[b]).wrapping_add(my);
    s[d] = (s[d] ^ s[a]).rotate_right(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_right(7);
}

fn b3_round(s: &mut [u32; 16], m: &[u32; 16]) {
    g(s, 0, 4, 8, 12, m[0], m[1]);
    g(s, 1, 5, 9, 13, m[2], m[3]);
    g(s, 2, 6, 10, 14, m[4], m[5]);
    g(s, 3, 7, 11, 15, m[6], m[7]);
    g(s, 0, 5, 10, 15, m[8], m[9]);
    g(s, 1, 6, 11, 12, m[10], m[11]);
    g(s, 2, 7, 8, 13, m[12], m[13]);
    g(s, 3, 4, 9, 14, m[14], m[15]);
}

fn b3_compress(cv: &[u32; 8], block: &[u32; 16], counter: u64, block_len: u32, flags: u32) -> [u32; 16] {
    let mut s = [
        cv[0], cv[1], cv[2], cv[3], cv[4], cv[5], cv[6], cv[7], B3_IV[0], B3_IV[1], B3_IV[2], B3_IV[3],
        counter as u32, (counter >> 32) as u32, block_len, flags,
    ];
    let mut m = *block;
    for r in 0..7 {
        b3_round(&mut s, &m);
        if r < 6 {
            let mut p = [0u32; 16];
            for i in 0..16 {
                p[i] = m[B3_PERM[i]];
            }
            m = p;
        }
    }
    for i in 0..8 {
        s[i] ^= s[i + 8];
        s[i + 8] ^= cv[i];
    }
    s
}

fn words_from_block(b: &[u8; 64]) -> [u32; 16] {
    let mut w = [0u32; 16];
    for i in 0..16 {
        w[i] = u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
    }
    w
}

struct B3Output {
    input_cv: [u32; 8],
    block_words: [u32; 16],
    counter: u64,
    block_len: u32,
    flags: u32,
}

impl B3Output {
    fn chaining_value(&self) -> [u32; 8] {
        let o = b3_compress(&self.input_cv, &self.block_words, self.counter, self.block_len, self.flags);
        let mut cv = [0u32; 8];
        cv.copy_from_slice(&o[..8]);
        cv
    }
    fn root_bytes(&self) -> [u8; 32] {
        let o = b3_compress(&self.input_cv, &self.block_words, 0, self.block_len, self.flags | ROOT);
        let mut out = [0u8; 32];
        for i in 0..8 {
            out[4 * i..4 * i + 4].copy_from_slice(&o[i].to_le_bytes());
        }
        out
    }
}

struct ChunkState {
    cv: [u32; 8],
    counter: u64,
    block: [u8; 64],
    block_len: usize,
    blocks_compressed: u32,
}

impl ChunkState {
    fn new(counter: u64) -> Self {
        Self { cv: B3_IV, counter, block: [0; 64], block_len: 0, blocks_compressed: 0 }
    }
    fn len(&self) -> usize {
        64 * self.blocks_compressed as usize + self.block_len
    }
    fn start_flag(&self) -> u32 {
        if self.blocks_compressed == 0 {
            CHUNK_START
        } else {
            0
        }
    }
    fn update(&mut self, mut input: &[u8]) {
        while !input.is_empty() {
            if self.block_len == 64 {
                let w = words_from_block(&self.block);
                let o = b3_compress(&self.cv, &w, self.counter, 64, self.start_flag());
                self.cv.copy_from_slice(&o[..8]);
                self.blocks_compressed += 1;
                self.block = [0; 64];
                self.block_len = 0;
            }
            let take = (64 - self.block_len).min(input.len());
            self.block[self.block_len..self.block_len + take].copy_from_slice(&input[..take]);
            self.block_len += take;
            input = &input[take..];
        }
    }
    fn output(&self) -> B3Output {
        B3Output {
            input_cv: self.cv,
            block_words: words_from_block(&self.block),
            counter: self.counter,
            block_len: self.block_len as u32,
            flags: self.start_flag() | CHUNK_END,
        }
    }
}

fn parent_output(left: [u32; 8], right: [u32; 8]) -> B3Output {
    let mut w = [0u32; 16];
    w[..8].copy_from_slice(&left);
    w[8..].copy_from_slice(&right);
    B3Output { input_cv: B3_IV, block_words: w, counter: 0, block_len: 64, flags: PARENT }
}

/// BLAKE3 (default 256-bit output, unkeyed).
pub fn blake3(data: &[u8]) -> [u8; 32] {
    let mut chunk = ChunkState::new(0);
    let mut stack: Vec<[u32; 8]> = Vec::new();
    let mut input = data;
    while !input.is_empty() {
        if chunk.len() == 1024 {
            let mut cv = chunk.output().chaining_value();
            let mut total = chunk.counter + 1;
            while total & 1 == 0 {
                cv = parent_output(stack.pop().unwrap(), cv).chaining_value();
                total >>= 1;
            }
            stack.push(cv);
            chunk = ChunkState::new(chunk.counter + 1);
        }
        let take = (1024 - chunk.len()).min(input.len());
        chunk.update(&input[..take]);
        input = &input[take..];
    }
    let mut out = chunk.output();
    while let Some(left) = stack.pop() {
        out = parent_output(left, out.chaining_value());
    }
    out.root_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_vectors() {
        assert_eq!(to_hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(to_hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            to_hex(&sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn sha2_vectors() {
        assert_eq!(
            to_hex(&sha512(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            to_hex(&sha384(b"abc")),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(
            to_hex(&sha512(b"")),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
    }

    #[test]
    fn blake3_vectors() {
        assert_eq!(to_hex(&blake3(b"")), "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
        assert_eq!(to_hex(&blake3(b"abc")), "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85");
    }

    #[test]
    fn blake3_multi_chunk_matches_reference_vectors() {
        // Official test vectors use input bytes i % 251.
        let input = |n: usize| -> Vec<u8> { (0..n).map(|i| (i % 251) as u8).collect() };
        assert_eq!(to_hex(&blake3(&input(1))), "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213");
        assert_eq!(to_hex(&blake3(&input(1023))), "10108970eeda3eb932baac1428c7a2163b0e924c9a9e25b35bba72b28f70bd11");
        assert_eq!(to_hex(&blake3(&input(1024))), "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7");
        assert_eq!(to_hex(&blake3(&input(1025))), "d00278ae47eb27b34faecf67b4fe263f82d5412916c1ffd97c8cb7fb814b8444");
        assert_eq!(to_hex(&blake3(&input(2048))), "e776b6028c7cd22a4d0ba182a8bf62205d2ef576467e838ed6f2529b85fba24a");
        assert_eq!(to_hex(&blake3(&input(31744))), "62b6960e1a44bcc1eb1a611a8d6235b6b4b78f32e7abc4fb4c6cdcce94895c47");
    }

    #[test]
    fn converter_matches_bazel() {
        assert_eq!(DigestFunction::parse("sha256").unwrap(), DigestFunction::Sha256);
        assert_eq!(DigestFunction::parse("SHA-1").unwrap(), DigestFunction::Sha1);
        assert_eq!(DigestFunction::parse("blake3").unwrap(), DigestFunction::Blake3);
        assert_eq!(DigestFunction::parse("Sha512").unwrap(), DigestFunction::Sha512);
        assert_eq!(
            DigestFunction::parse("MD5").unwrap_err(),
            "'MD5' is not a valid hash function. Possible values are: BLAKE3, SHA-1, SHA-256, SHA-384, SHA-512"
        );
        assert_eq!(DigestFunction::default(), DigestFunction::Sha256);
        for f in DigestFunction::ALL {
            assert_eq!(f.hash(b"x").len(), f.digest_length());
        }
    }
}
