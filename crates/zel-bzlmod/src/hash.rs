//! Pure Rust hashing (MD5, SHA-1, SHA-256, SHA-384, SHA-512), hex/base64 helpers and
//! Subresource Integrity style [`Checksum`] parsing/verification.

use std::fmt;

// ---------------------------------------------------------------------------
// Hex helpers
// ---------------------------------------------------------------------------

/// Lowercase hex encoding.
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}

/// Decode a hex string (either case). Returns `None` on odd length or bad digit.
pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    let val = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(b.len() / 2);
    for pair in b.chunks(2) {
        out.push((val(pair[0])? << 4) | val(pair[1])?);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Base64
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 encoding with padding.
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | (*chunk.get(2).unwrap_or(&0) as u32);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Base64 decode; accepts standard or URL-safe alphabet, with or without padding.
pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return Err(format!("invalid base64 character '{}'", c as char)),
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if s.len() % 4 == 1 {
        return Err("invalid base64 length".to_string());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Digests
// ---------------------------------------------------------------------------

/// Hash algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Algorithm {
    Md5,
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Algorithm {
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Md5 => "md5",
            Algorithm::Sha1 => "sha1",
            Algorithm::Sha256 => "sha256",
            Algorithm::Sha384 => "sha384",
            Algorithm::Sha512 => "sha512",
        }
    }
    pub fn digest_len(self) -> usize {
        match self {
            Algorithm::Md5 => 16,
            Algorithm::Sha1 => 20,
            Algorithm::Sha256 => 32,
            Algorithm::Sha384 => 48,
            Algorithm::Sha512 => 64,
        }
    }
    pub fn from_name(s: &str) -> Option<Algorithm> {
        match s.to_ascii_lowercase().as_str() {
            "md5" => Some(Algorithm::Md5),
            "sha1" | "sha-1" => Some(Algorithm::Sha1),
            "sha256" | "sha-256" => Some(Algorithm::Sha256),
            "sha384" | "sha-384" => Some(Algorithm::Sha384),
            "sha512" | "sha-512" => Some(Algorithm::Sha512),
            _ => None,
        }
    }
    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Algorithm::Md5 => md5(data).to_vec(),
            Algorithm::Sha1 => sha1(data).to_vec(),
            Algorithm::Sha256 => sha256(data).to_vec(),
            Algorithm::Sha384 => sha384(data).to_vec(),
            Algorithm::Sha512 => sha512(data).to_vec(),
        }
    }
}

fn pad_message(data: &[u8], block: usize, big_endian: bool) -> Vec<u8> {
    let len_bytes = if block == 128 { 16 } else { 8 };
    let mut m = data.to_vec();
    m.push(0x80);
    while m.len() % block != block - len_bytes {
        m.push(0);
    }
    let bitlen = (data.len() as u128).wrapping_mul(8);
    if big_endian {
        if len_bytes == 16 {
            m.extend_from_slice(&bitlen.to_be_bytes());
        } else {
            m.extend_from_slice(&(bitlen as u64).to_be_bytes());
        }
    } else {
        m.extend_from_slice(&(bitlen as u64).to_le_bytes());
    }
    m
}

pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6,
        10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32)
        .collect();
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in pad_message(data, 64, false).chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (mut f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, v) in [a0, b0, c0, d0].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    for chunk in pad_message(data, 64, true).chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i / 20 {
                0 => ((b & c) | (!b & d), 0x5A827999),
                1 => (b ^ c ^ d, 0x6ED9EBA1),
                2 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
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
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    for chunk in pad_message(data, 64, true).chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
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
    let mut out = [0u8; 32];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

const K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

fn sha512_core(data: &[u8], init: [u64; 8]) -> [u64; 8] {
    let mut h = init;
    for chunk in pad_message(data, 128, true).chunks(128) {
        let mut w = [0u64; 80];
        for i in 0..16 {
            let mut b = [0u8; 8];
            b.copy_from_slice(&chunk[i * 8..i * 8 + 8]);
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
            0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
            0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
        ],
    );
    let mut out = [0u8; 64];
    for (i, x) in h.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&x.to_be_bytes());
    }
    out
}

pub fn sha384(data: &[u8]) -> [u8; 48] {
    let h = sha512_core(
        data,
        [
            0xcbbb9d5dc1059ed8, 0x629a292a367cd507, 0x9159015a3070dd17, 0x152fecd8f70e5939,
            0x67332667ffc00b31, 0x8eb44a8768581511, 0xdb0c2e0d64f98fa7, 0x47b5481dbefa4fa4,
        ],
    );
    let mut out = [0u8; 48];
    for i in 0..6 {
        out[i * 8..i * 8 + 8].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    to_hex(&sha256(data))
}
pub fn sha1_hex(data: &[u8]) -> String {
    to_hex(&sha1(data))
}
pub fn md5_hex(data: &[u8]) -> String {
    to_hex(&md5(data))
}

// ---------------------------------------------------------------------------
// Checksum
// ---------------------------------------------------------------------------

/// An expected checksum: algorithm + raw digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checksum {
    pub algorithm: Algorithm,
    pub digest: Vec<u8>,
}

impl Checksum {
    /// Parse a Subresource Integrity string (`sha256-<base64>`, also sha1/sha384/sha512).
    /// Only the first token is considered if several are space separated.
    pub fn parse_integrity(s: &str) -> Result<Checksum, String> {
        let s = s.trim();
        let tok = s.split_whitespace().next().unwrap_or("");
        let unsupported = || format!("Unsupported checksum algorithm: '{}' (expected SHA-1, SHA-256, SHA-384, or SHA-512)", s);
        let (algo, b64) = tok.split_once('-').ok_or_else(unsupported)?;
        let (algorithm, key_name) = match algo {
            "sha1" => (Algorithm::Sha1, "SHA-1"),
            "sha256" => (Algorithm::Sha256, "SHA-256"),
            "sha384" => (Algorithm::Sha384, "SHA-384"),
            "sha512" => (Algorithm::Sha512, "SHA-512"),
            _ => return Err(unsupported()),
        };
        let digest = base64_decode(b64).map_err(|_| format!("Invalid base64 '{}'", b64))?;
        if digest.len() != algorithm.digest_len() {
            return Err(format!("Invalid {} SRI checksum '{}'", key_name, s));
        }
        Ok(Checksum { algorithm, digest })
    }

    /// Parse a hex digest. If `algorithm` is None, it's inferred from length (40 -> sha1,
    /// 64 -> sha256, 96 -> sha384, 128 -> sha512).
    pub fn parse_hex(s: &str, algorithm: Option<Algorithm>) -> Result<Checksum, String> {
        let t = s.trim();
        let digest = from_hex(t).ok_or_else(|| format!("Invalid hex checksum '{}'", s))?;
        let algorithm = match algorithm {
            Some(a) => a,
            None => match digest.len() {
                20 => Algorithm::Sha1,
                32 => Algorithm::Sha256,
                48 => Algorithm::Sha384,
                64 => Algorithm::Sha512,
                n => return Err(format!("Invalid hex checksum '{}': unexpected length {}", s, n * 2)),
            },
        };
        if digest.len() != algorithm.digest_len() {
            return Err(format!(
                "Invalid {} checksum '{}': expected {} hex characters",
                algorithm.name(),
                s,
                algorithm.digest_len() * 2
            ));
        }
        Ok(Checksum { algorithm, digest })
    }

    /// Parse a sha256 hex string.
    pub fn sha256_hex(s: &str) -> Result<Checksum, String> {
        Checksum::parse_hex(s, Some(Algorithm::Sha256))
    }

    /// Parse either an integrity string or a bare hex digest.
    pub fn parse(s: &str) -> Result<Checksum, String> {
        if s.contains('-') {
            Checksum::parse_integrity(s)
        } else {
            Checksum::parse_hex(s, None)
        }
    }

    pub fn hex(&self) -> String {
        to_hex(&self.digest)
    }

    pub fn to_integrity(&self) -> String {
        format!("{}-{}", self.algorithm.name(), base64_encode(&self.digest))
    }

    pub fn matches(&self, data: &[u8]) -> bool {
        self.algorithm.digest(data) == self.digest
    }

    /// Verify `data`; the error mimics Bazel's checksum mismatch message.
    pub fn verify(&self, data: &[u8]) -> Result<(), String> {
        let actual = self.algorithm.digest(data);
        if actual == self.digest {
            Ok(())
        } else {
            Err(format!(
                "Checksum was {} but wanted {}",
                to_hex(&actual),
                self.hex()
            ))
        }
    }
}

impl fmt::Display for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_integrity())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn sha1_vectors() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn sha256_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let million = vec![b'a'; 1_000_000];
        assert_eq!(
            sha256_hex(&million),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn sha384_512_vectors() {
        assert_eq!(
            to_hex(&sha384(b"abc")),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(
            to_hex(&sha512(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            to_hex(&sha512(b"")),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
    }

    #[test]
    fn padding_boundaries() {
        // lengths around block boundaries should not panic and be deterministic
        for n in [55usize, 56, 63, 64, 65, 111, 112, 119, 120, 127, 128, 129] {
            let d = vec![7u8; n];
            assert_eq!(sha256(&d), sha256(&d));
            assert_eq!(sha512(&d).len(), 64);
            assert_eq!(md5(&d).len(), 16);
        }
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(to_hex(&[0, 255, 16]), "00ff10");
        assert_eq!(from_hex("00FF10"), Some(vec![0, 255, 16]));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
    }

    #[test]
    fn base64_roundtrip() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(base64_decode("Zg").unwrap(), b"f");
        assert_eq!(base64_decode("Zm9vYg").unwrap(), b"foob");
        assert!(base64_decode("Z$").is_err());
        assert!(base64_decode("Z").is_err());
        for n in 0..20 {
            let d: Vec<u8> = (0..n).map(|i| (i * 37) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&d)).unwrap(), d);
        }
    }

    #[test]
    fn integrity_parse_and_verify() {
        let c = Checksum::parse_integrity("sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=").unwrap();
        assert_eq!(c.algorithm, Algorithm::Sha256);
        assert_eq!(c.hex(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(c.verify(b"abc").is_ok());
        let err = c.verify(b"abd").unwrap_err();
        assert!(err.contains("Checksum was") && err.contains("but wanted"));
        // no padding tolerated
        assert!(Checksum::parse_integrity("sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0").is_ok());
        assert_eq!(c.to_integrity(), "sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=");
    }

    #[test]
    fn integrity_all_algorithms() {
        for a in [Algorithm::Sha1, Algorithm::Sha256, Algorithm::Sha384, Algorithm::Sha512] {
            let s = format!("{}-{}", a.name(), base64_encode(&a.digest(b"hello")));
            let c = Checksum::parse_integrity(&s).unwrap();
            assert!(c.matches(b"hello"));
            assert!(!c.matches(b"hellO"));
            assert_eq!(Checksum::parse(&s).unwrap(), c);
        }
    }

    #[test]
    fn integrity_errors() {
        assert!(Checksum::parse_integrity("").is_err());
        assert!(Checksum::parse_integrity("sha256").is_err());
        assert!(Checksum::parse_integrity("md5-AAAA").is_err());
        assert!(Checksum::parse_integrity("sha256-AAAA").is_err());
        assert!(Checksum::parse_integrity("sha256-!!!!").is_err());
    }

    #[test]
    fn hex_parse() {
        let h = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let c = Checksum::parse(h).unwrap();
        assert_eq!(c.algorithm, Algorithm::Sha256);
        assert!(c.matches(b"abc"));
        assert!(Checksum::sha256_hex(&h.to_uppercase()).is_ok());
        assert!(Checksum::sha256_hex("abcd").is_err());
        assert!(Checksum::parse("xyz").is_err());
        assert_eq!(Checksum::parse(&sha1_hex(b"a")).unwrap().algorithm, Algorithm::Sha1);
    }
}

/// Port of Bazel's `Fingerprint` (SHA-256 over protobuf-coded values), as used by
/// `DigestWriter.computePredeclaredInputHash` and `RepoRecordedInput.Dirents`.
#[derive(Default)]
pub struct Fingerprint {
    buf: Vec<u8>,
}

impl Fingerprint {
    pub fn new() -> Self {
        Self::default()
    }

    fn varint(&mut self, mut v: u64) {
        loop {
            if v < 0x80 {
                self.buf.push(v as u8);
                return;
            }
            self.buf.push((v & 0x7f) as u8 | 0x80);
            v >>= 7;
        }
    }

    /// `addInt`: `CodedOutputStream.writeInt32NoTag` (negative values are sign-extended to 64 bits).
    pub fn add_int(&mut self, x: i32) -> &mut Self {
        self.varint(x as i64 as u64);
        self
    }

    pub fn add_bytes(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }

    pub fn add_boolean(&mut self, b: bool) -> &mut Self {
        self.buf.push(b as u8);
        self
    }

    /// `addString`: length-prefixed internal (Latin-1 / raw UTF-8) bytes.
    pub fn add_string(&mut self, s: &str) -> &mut Self {
        self.varint(s.len() as u64);
        self.buf.extend_from_slice(s.as_bytes());
        self
    }

    pub fn add_nullable_string(&mut self, s: Option<&str>) -> &mut Self {
        match s {
            None => self.add_boolean(false),
            Some(s) => {
                self.add_boolean(true);
                self.add_string(s)
            }
        }
    }

    pub fn add_strings<S: AsRef<str>>(&mut self, items: &[S]) -> &mut Self {
        self.add_int(items.len() as i32);
        for i in items {
            self.add_string(i.as_ref());
        }
        self
    }

    pub fn digest(&self) -> [u8; 32] {
        sha256(&self.buf)
    }

    pub fn hex_digest(&self) -> String {
        to_hex(&self.digest())
    }
}
