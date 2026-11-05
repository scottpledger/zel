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

//! HPACK (RFC 7541) header compression: static + dynamic tables, integer and string literal
//! coding, Huffman decoding/encoding. The HPACK Huffman code is a canonical code; the table is
//! reconstructed from the per-symbol code lengths of RFC 7541 Appendix B (and validated against
//! the RFC's own test vectors in the unit tests).

use std::collections::VecDeque;

pub type Header = (String, String);

/// RFC 7541 Appendix B code lengths for symbols 0..=256 (256 = EOS).
const HUFF_LENS: [u8; 257] = [
    13, 23, 28, 28, 28, 28, 28, 28, 28, 24, 30, 28, 28, 30, 28, 28, 28, 28, 28, 28, 28, 28, 30, 28,
    28, 28, 28, 28, 28, 28, 28, 28, // 0..31
    6, 10, 10, 12, 13, 6, 8, 11, 10, 10, 8, 11, 8, 6, 6, 6, 5, 5, 5, 6, 6, 6, 6, 6, 6, 6, 7, 8, 15,
    6, 12, 10, // 32..63
    13, 6, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 8, 7, 8, 13, 19, 13,
    14, 6, // 64..95
    15, 5, 6, 5, 6, 5, 6, 6, 6, 5, 7, 7, 6, 6, 6, 5, 6, 7, 6, 5, 5, 6, 7, 7, 7, 7, 7, 15, 11, 14,
    13, 28, // 96..127
    20, 22, 20, 20, 22, 22, 22, 23, 22, 23, 23, 23, 23, 23, 24, 23, 24, 24, 22, 23, 24, 23, 23, 23,
    23, 21, 22, 23, 22, 23, 23, 24, // 128..159
    22, 21, 20, 22, 22, 23, 23, 21, 23, 22, 22, 24, 21, 22, 23, 23, 21, 21, 22, 21, 23, 22, 23, 23,
    20, 22, 22, 22, 23, 22, 22, 23, // 160..191
    26, 26, 20, 19, 22, 23, 22, 25, 26, 26, 26, 27, 27, 26, 24, 25, 19, 21, 26, 27, 27, 26, 27, 24,
    21, 21, 26, 26, 28, 27, 27, 27, // 192..223
    20, 24, 20, 21, 22, 21, 21, 23, 22, 22, 25, 25, 24, 24, 26, 23, 26, 27, 26, 26, 27, 27, 27, 27,
    27, 28, 27, 27, 27, 27, 27, 26, // 224..255
    30, // EOS
];

struct HuffTable {
    codes: [u32; 257],
    // Canonical decoding tables indexed by length.
    first_code: [u32; 31],
    first_index: [usize; 31],
    count: [u32; 31],
    sorted_syms: Vec<u16>,
}

fn huff_table() -> &'static HuffTable {
    use std::sync::OnceLock;
    static T: OnceLock<HuffTable> = OnceLock::new();
    T.get_or_init(|| {
        let mut count = [0u32; 31];
        for &l in HUFF_LENS.iter() {
            count[l as usize] += 1;
        }
        let mut first_code = [0u32; 31];
        let mut first_index = [0usize; 31];
        let mut code = 0u32;
        let mut idx = 0usize;
        for l in 1..31 {
            code = (code + count[l - 1]) << 1;
            if l == 1 {
                code = 0;
            }
            first_code[l] = code;
            first_index[l] = idx;
            idx += count[l] as usize;
        }
        let mut sorted_syms: Vec<u16> = (0..257u16).collect();
        sorted_syms.sort_by_key(|&s| (HUFF_LENS[s as usize], s));
        let mut codes = [0u32; 257];
        let mut next = first_code;
        for &s in &sorted_syms {
            let l = HUFF_LENS[s as usize] as usize;
            codes[s as usize] = next[l];
            next[l] += 1;
        }
        HuffTable { codes, first_code, first_index, count, sorted_syms }
    })
}

/// Huffman-encodes `data` (padding with the most significant bits of EOS).
pub fn huffman_encode(data: &[u8]) -> Vec<u8> {
    let t = huff_table();
    let mut out = Vec::new();
    let mut acc: u64 = 0;
    let mut bits = 0u32;
    for &b in data {
        let l = HUFF_LENS[b as usize] as u32;
        acc = (acc << l) | t.codes[b as usize] as u64;
        bits += l;
        while bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
        acc &= (1u64 << bits) - 1;
    }
    if bits > 0 {
        out.push(((acc << (8 - bits)) | ((1u64 << (8 - bits)) - 1)) as u8);
    }
    out
}

/// Huffman-decodes `data`; errors on EOS, over-long padding, or non-1 padding.
pub fn huffman_decode(data: &[u8]) -> Result<Vec<u8>, String> {
    let t = huff_table();
    let mut out = Vec::with_capacity(data.len() * 2);
    let mut code = 0u32;
    let mut len = 0usize;
    for &byte in data {
        for i in (0..8).rev() {
            code = (code << 1) | ((byte >> i) & 1) as u32;
            len += 1;
            if len > 30 {
                return Err("invalid huffman code".into());
            }
            if t.count[len] > 0 && code >= t.first_code[len] && code - t.first_code[len] < t.count[len]
            {
                let sym = t.sorted_syms[t.first_index[len] + (code - t.first_code[len]) as usize];
                if sym == 256 {
                    return Err("EOS in huffman string".into());
                }
                out.push(sym as u8);
                code = 0;
                len = 0;
            }
        }
    }
    if len > 7 || (len > 0 && code != (1u32 << len) - 1) {
        return Err("invalid huffman padding".into());
    }
    Ok(out)
}

const STATIC_TABLE: [(&str, &str); 61] = [
    (":authority", ""),
    (":method", "GET"),
    (":method", "POST"),
    (":path", "/"),
    (":path", "/index.html"),
    (":scheme", "http"),
    (":scheme", "https"),
    (":status", "200"),
    (":status", "204"),
    (":status", "206"),
    (":status", "304"),
    (":status", "400"),
    (":status", "404"),
    (":status", "500"),
    ("accept-charset", ""),
    ("accept-encoding", "gzip, deflate"),
    ("accept-language", ""),
    ("accept-ranges", ""),
    ("accept", ""),
    ("access-control-allow-origin", ""),
    ("age", ""),
    ("allow", ""),
    ("authorization", ""),
    ("cache-control", ""),
    ("content-disposition", ""),
    ("content-encoding", ""),
    ("content-language", ""),
    ("content-length", ""),
    ("content-location", ""),
    ("content-range", ""),
    ("content-type", ""),
    ("cookie", ""),
    ("date", ""),
    ("etag", ""),
    ("expect", ""),
    ("expires", ""),
    ("from", ""),
    ("host", ""),
    ("if-match", ""),
    ("if-modified-since", ""),
    ("if-none-match", ""),
    ("if-range", ""),
    ("if-unmodified-since", ""),
    ("last-modified", ""),
    ("link", ""),
    ("location", ""),
    ("max-forwards", ""),
    ("proxy-authenticate", ""),
    ("proxy-authorization", ""),
    ("range", ""),
    ("referer", ""),
    ("refresh", ""),
    ("retry-after", ""),
    ("server", ""),
    ("set-cookie", ""),
    ("strict-transport-security", ""),
    ("transfer-encoding", ""),
    ("user-agent", ""),
    ("vary", ""),
    ("via", ""),
    ("www-authenticate", ""),
];

fn encode_int(value: usize, prefix_bits: u8, first_byte_flags: u8, out: &mut Vec<u8>) {
    let max = (1usize << prefix_bits) - 1;
    if value < max {
        out.push(first_byte_flags | value as u8);
        return;
    }
    out.push(first_byte_flags | max as u8);
    let mut rem = value - max;
    while rem >= 128 {
        out.push((rem % 128) as u8 | 0x80);
        rem /= 128;
    }
    out.push(rem as u8);
}

fn decode_int(buf: &[u8], pos: &mut usize, prefix_bits: u8) -> Result<usize, String> {
    let max = (1usize << prefix_bits) - 1;
    let first = *buf.get(*pos).ok_or("truncated hpack integer")? as usize;
    *pos += 1;
    let mut value = first & max;
    if value < max {
        return Ok(value);
    }
    let mut shift = 0u32;
    loop {
        let b = *buf.get(*pos).ok_or("truncated hpack integer")? as usize;
        *pos += 1;
        if shift > 28 {
            return Err("hpack integer overflow".into());
        }
        value += (b & 0x7f) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            return Ok(value);
        }
    }
}

fn encode_string(s: &str, out: &mut Vec<u8>) {
    let h = huffman_encode(s.as_bytes());
    if h.len() < s.len() {
        encode_int(h.len(), 7, 0x80, out);
        out.extend_from_slice(&h);
    } else {
        encode_int(s.len(), 7, 0, out);
        out.extend_from_slice(s.as_bytes());
    }
}

fn decode_string(buf: &[u8], pos: &mut usize) -> Result<String, String> {
    let huff = buf.get(*pos).ok_or("truncated hpack string")? & 0x80 != 0;
    let len = decode_int(buf, pos, 7)?;
    if *pos + len > buf.len() {
        return Err("truncated hpack string".into());
    }
    let raw = &buf[*pos..*pos + len];
    *pos += len;
    let bytes = if huff { huffman_decode(raw)? } else { raw.to_vec() };
    // Header values may legally carry arbitrary octets; keep them lossless-ish.
    Ok(String::from_utf8(bytes.clone())
        .unwrap_or_else(|_| bytes.iter().map(|&b| b as char).collect()))
}

#[derive(Debug)]
struct DynTable {
    entries: VecDeque<Header>,
    size: usize,
    max_size: usize,
}

impl DynTable {
    fn new(max_size: usize) -> Self {
        Self { entries: VecDeque::new(), size: 0, max_size }
    }
    fn entry_size(h: &Header) -> usize {
        h.0.len() + h.1.len() + 32
    }
    fn evict(&mut self) {
        while self.size > self.max_size {
            match self.entries.pop_back() {
                Some(e) => self.size -= Self::entry_size(&e),
                None => break,
            }
        }
    }
    fn insert(&mut self, h: Header) {
        self.size += Self::entry_size(&h);
        self.entries.push_front(h);
        self.evict();
    }
    fn set_max(&mut self, n: usize) {
        self.max_size = n;
        self.evict();
    }
}

fn lookup(dt: &DynTable, index: usize) -> Result<Header, String> {
    if index == 0 {
        return Err("hpack index 0".into());
    }
    if index <= 61 {
        let (n, v) = STATIC_TABLE[index - 1];
        return Ok((n.to_string(), v.to_string()));
    }
    dt.entries.get(index - 62).cloned().ok_or_else(|| format!("hpack index {index} out of range"))
}

/// HPACK decoder (one per connection direction; header blocks must be decoded in order).
#[derive(Debug)]
pub struct Decoder {
    table: DynTable,
    /// Upper bound the peer may set via dynamic table size updates (our SETTINGS_HEADER_TABLE_SIZE).
    max_allowed: usize,
}

impl Decoder {
    pub fn new(max_table_size: usize) -> Self {
        Self { table: DynTable::new(max_table_size), max_allowed: max_table_size }
    }

    pub fn decode(&mut self, buf: &[u8]) -> Result<Vec<Header>, String> {
        let mut pos = 0;
        let mut out = Vec::new();
        while pos < buf.len() {
            let b = buf[pos];
            if b & 0x80 != 0 {
                let idx = decode_int(buf, &mut pos, 7)?;
                out.push(lookup(&self.table, idx)?);
            } else if b & 0xc0 == 0x40 {
                let idx = decode_int(buf, &mut pos, 6)?;
                let name = if idx == 0 {
                    decode_string(buf, &mut pos)?
                } else {
                    lookup(&self.table, idx)?.0
                };
                let value = decode_string(buf, &mut pos)?;
                self.table.insert((name.clone(), value.clone()));
                out.push((name, value));
            } else if b & 0xe0 == 0x20 {
                let n = decode_int(buf, &mut pos, 5)?;
                if n > self.max_allowed {
                    return Err("hpack table size update exceeds limit".into());
                }
                self.table.set_max(n);
            } else {
                // Literal without indexing (0000) or never indexed (0001): 4-bit prefix.
                let idx = decode_int(buf, &mut pos, 4)?;
                let name = if idx == 0 {
                    decode_string(buf, &mut pos)?
                } else {
                    lookup(&self.table, idx)?.0
                };
                let value = decode_string(buf, &mut pos)?;
                out.push((name, value));
            }
        }
        Ok(out)
    }
}

/// HPACK encoder. Uses the static table for full/name matches and incremental indexing for
/// non-sensitive headers so repeated requests on a connection compress to a few bytes.
#[derive(Debug)]
pub struct Encoder {
    table: DynTable,
    pending_size_update: Option<usize>,
}

impl Encoder {
    pub fn new() -> Self {
        Self { table: DynTable::new(4096), pending_size_update: None }
    }

    /// Applies the peer's SETTINGS_HEADER_TABLE_SIZE.
    pub fn set_peer_max_table_size(&mut self, n: usize) {
        let n = n.min(4096);
        if n != self.table.max_size {
            self.table.set_max(n);
            self.pending_size_update = Some(n);
        }
    }

    pub fn encode(&mut self, headers: &[Header]) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(n) = self.pending_size_update.take() {
            encode_int(n, 5, 0x20, &mut out);
        }
        for h in headers {
            let sensitive = h.0 == "authorization" || h.0 == "cookie" || h.0 == "proxy-authorization";
            let mut full = None;
            let mut name_idx = None;
            for (i, (n, v)) in STATIC_TABLE.iter().enumerate() {
                if *n == h.0 {
                    if name_idx.is_none() {
                        name_idx = Some(i + 1);
                    }
                    if *v == h.1 {
                        full = Some(i + 1);
                        break;
                    }
                }
            }
            if full.is_none() {
                for (i, e) in self.table.entries.iter().enumerate() {
                    if e.0 == h.0 {
                        if name_idx.is_none() {
                            name_idx = Some(i + 62);
                        }
                        if e.1 == h.1 {
                            full = Some(i + 62);
                            break;
                        }
                    }
                }
            }
            if let Some(i) = full {
                encode_int(i, 7, 0x80, &mut out);
                continue;
            }
            let (prefix, flags, index_it) =
                if sensitive { (4u8, 0x10u8, false) } else { (6u8, 0x40u8, true) };
            match name_idx {
                Some(i) => encode_int(i, prefix, flags, &mut out),
                None => {
                    out.push(flags);
                    encode_string(&h.0, &mut out);
                }
            }
            encode_string(&h.1, &mut out);
            if index_it {
                self.table.insert(h.clone());
            }
        }
        out
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn huffman_code_is_complete() {
        // Kraft equality: sum of 2^-len == 1 for a complete prefix code.
        let sum: u64 = HUFF_LENS.iter().map(|&l| 1u64 << (30 - l)).sum();
        assert_eq!(sum, 1u64 << 30);
        // Spot-check canonical codes from RFC 7541 Appendix B.
        let t = huff_table();
        assert_eq!(t.codes[b'0' as usize], 0x0);
        assert_eq!(t.codes[b' ' as usize], 0x14);
        assert_eq!(t.codes[b'a' as usize], 0x3);
        assert_eq!(t.codes[b'%' as usize], 0x15);
        assert_eq!(t.codes[b'!' as usize], 0x3f8);
        assert_eq!(t.codes[0], 0x1ff8);
        assert_eq!(t.codes[256], 0x3fffffff);
        assert_eq!(t.codes[255], 0x3ffffee);
    }

    #[test]
    fn huffman_rfc_vectors() {
        let cases: [(&str, &str); 6] = [
            ("www.example.com", "f1e3 c2e5 f23a 6ba0 ab90 f4ff"),
            ("no-cache", "a8eb 1064 9cbf"),
            ("custom-key", "25a8 49e9 5ba9 7d7f"),
            ("custom-value", "25a8 49e9 5bb8 e8b4 bf"),
            ("302", "6402"),
            ("Mon, 21 Oct 2013 20:13:21 GMT", "d07a be94 1054 d444 a820 0595 040b 8166 e082 a62d 1bff"),
        ];
        for (plain, hex) in cases {
            let enc = unhex(hex);
            assert_eq!(huffman_encode(plain.as_bytes()), enc, "encode {plain}");
            assert_eq!(huffman_decode(&enc).unwrap(), plain.as_bytes(), "decode {plain}");
        }
        assert_eq!(
            huffman_decode(&unhex("9d29 ad17 1863 c78f 0b97 c8e9 ae82 ae43 d3")).unwrap(),
            b"https://www.example.com"
        );
    }

    #[test]
    fn huffman_roundtrip_all_bytes() {
        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(huffman_decode(&huffman_encode(&all)).unwrap(), all);
    }

    #[test]
    fn rfc_c3_requests_without_huffman() {
        let mut d = Decoder::new(4096);
        let h = d.decode(&unhex("8286 8441 0f77 7777 2e65 7861 6d70 6c65 2e63 6f6d")).unwrap();
        assert_eq!(
            h,
            vec![
                (":method".into(), "GET".into()),
                (":scheme".into(), "http".into()),
                (":path".into(), "/".into()),
                (":authority".into(), "www.example.com".into()),
            ]
        );
        let h = d.decode(&unhex("8286 84be 5808 6e6f 2d63 6163 6865")).unwrap();
        assert_eq!(h[3], (":authority".into(), "www.example.com".into()));
        assert_eq!(h[4], ("cache-control".into(), "no-cache".into()));
    }

    #[test]
    fn rfc_c4_requests_with_huffman_and_dynamic_table() {
        let mut d = Decoder::new(4096);
        let h = d.decode(&unhex("8286 8441 8cf1 e3c2 e5f2 3a6b a0ab 90f4 ff")).unwrap();
        assert_eq!(h[3], (":authority".into(), "www.example.com".into()));
        let h = d.decode(&unhex("8286 84be 5886 a8eb 1064 9cbf")).unwrap();
        assert_eq!(h[4], ("cache-control".into(), "no-cache".into()));
        let h = d
            .decode(&unhex("8287 85bf 4088 25a8 49e9 5ba9 7d7f 8925 a849 e95b b8e8 b4bf"))
            .unwrap();
        assert_eq!(h[4], ("custom-key".into(), "custom-value".into()));
    }

    #[test]
    fn dynamic_table_eviction_rfc_c5() {
        // RFC 7541 C.5: responses with a 256-byte dynamic table.
        let mut d = Decoder::new(256);
        let h = d
            .decode(&unhex(
                "4803 3330 3258 0770 7269 7661 7465 611d 4d6f 6e2c 2032 3120 4f63 7420 3230 3133 2032 303a 3133 3a32 3120 474d 546e 1768 7474 7073 3a2f 2f77 7777 2e65 7861 6d70 6c65 2e63 6f6d",
            ))
            .unwrap();
        assert_eq!(h[0], (":status".into(), "302".into()));
        assert_eq!(h[3], ("location".into(), "https://www.example.com".into()));
        assert_eq!(d.table.size, 222);
        let h = d.decode(&unhex("4803 3330 37c1 c0bf")).unwrap();
        assert_eq!(h[0], (":status".into(), "307".into()));
        assert_eq!(d.table.entries.len(), 4);
    }

    #[test]
    fn encoder_decoder_roundtrip() {
        let mut e = Encoder::new();
        let mut d = Decoder::new(4096);
        let hs: Vec<Header> = vec![
            (":method".into(), "POST".into()),
            (":scheme".into(), "http".into()),
            (":path".into(), "/build.bazel.remote.execution.v2.ContentAddressableStorage/FindMissingBlobs".into()),
            (":authority".into(), "localhost:8980".into()),
            ("content-type".into(), "application/grpc".into()),
            ("te".into(), "trailers".into()),
            ("authorization".into(), "Bearer secret".into()),
            ("x-custom".into(), "v".into()),
        ];
        for _ in 0..3 {
            let b = e.encode(&hs);
            assert_eq!(d.decode(&b).unwrap(), hs);
        }
        let first = Encoder::new().encode(&hs).len();
        let second = e.encode(&hs).len();
        assert!(second < first, "dynamic table should shrink repeat requests");
    }

    #[test]
    fn integer_coding_rfc_c1() {
        let mut o = Vec::new();
        encode_int(10, 5, 0, &mut o);
        assert_eq!(o, [0x0a]);
        o.clear();
        encode_int(1337, 5, 0, &mut o);
        assert_eq!(o, [0x1f, 0x9a, 0x0a]);
        let mut p = 0;
        assert_eq!(decode_int(&o, &mut p, 5).unwrap(), 1337);
        o.clear();
        encode_int(42, 8, 0, &mut o);
        assert_eq!(o, [0x2a]);
    }
}
