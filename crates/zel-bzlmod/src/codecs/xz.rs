//! Pure-Rust XZ decompressor (container format, LZMA2, BCJ / delta filters, CRC32/CRC64/SHA-256).

use super::lzma::lzma2_decode_into;
use crate::archive::crc32;
use crate::hash::sha256;

const HEADER_MAGIC: &[u8; 6] = b"\xFD7zXZ\x00";

fn read_varint(data: &[u8], pos: &mut usize) -> Result<u64, String> {
    let mut val = 0u64;
    for i in 0..9 {
        if *pos >= data.len() {
            return Err("XZ varint truncated".into());
        }
        let b = data[*pos];
        *pos += 1;
        val |= ((b & 0x7F) as u64) << (7 * i);
        if b & 0x80 == 0 {
            if b == 0 && i > 0 {
                return Err("XZ varint not minimal".into());
            }
            return Ok(val);
        }
    }
    Err("XZ varint overflow".into())
}

fn crc64_table() -> &'static [u64; 256] {
    static T: std::sync::OnceLock<[u64; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let poly = 0xC96C5795D7870F42u64;
        let mut t = [0u64; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u64;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ poly } else { c >> 1 };
            }
            *e = c;
        }
        t
    })
}

pub fn crc64(data: &[u8]) -> u64 {
    let t = crc64_table();
    let mut crc = !0u64;
    for &b in data {
        crc = t[((crc ^ b as u64) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

// ---------------------------------------------------------------------------
// Branch/call/jump (BCJ) filters (decoders), following xz-embedded.
// `start` is the start offset property (normally 0).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bcj {
    X86,
    Ppc,
    Ia64,
    Arm,
    ArmThumb,
    Sparc,
}

fn x86_test_msbyte(b: u8) -> bool {
    b == 0x00 || b == 0xFF
}

fn bcj_x86(buf: &mut [u8], start: u32) {
    const ALLOWED: [bool; 8] = [true, true, true, false, true, false, false, false];
    const BITNUM: [usize; 8] = [0, 1, 2, 2, 3, 3, 3, 3];
    if buf.len() <= 4 {
        return;
    }
    let size = buf.len() - 4;
    let mut prev_pos: usize = usize::MAX;
    let mut prev_mask: u32 = 0;
    let mut i = 0usize;
    while i < size {
        if buf[i] & 0xFE != 0xE8 {
            i += 1;
            continue;
        }
        let d = i.wrapping_sub(prev_pos);
        if d > 3 {
            prev_mask = 0;
        } else {
            prev_mask = (prev_mask << (d - 1)) & 7;
            if prev_mask != 0 {
                let b = buf[i + 4 - BITNUM[prev_mask as usize]];
                if !ALLOWED[prev_mask as usize] || x86_test_msbyte(b) {
                    prev_pos = i;
                    prev_mask = (prev_mask << 1) | 1;
                    i += 1;
                    continue;
                }
            }
        }
        prev_pos = i;
        if x86_test_msbyte(buf[i + 4]) {
            let mut src = u32::from_le_bytes([buf[i + 1], buf[i + 2], buf[i + 3], buf[i + 4]]);
            let mut dest;
            loop {
                dest = src.wrapping_sub(start.wrapping_add(i as u32).wrapping_add(5));
                if prev_mask == 0 {
                    break;
                }
                let j = BITNUM[prev_mask as usize] as u32 * 8;
                let b = (dest >> (24 - j)) as u8;
                if !x86_test_msbyte(b) {
                    break;
                }
                src = dest ^ ((1u32 << (32 - j)) - 1);
            }
            dest &= 0x01FF_FFFF;
            dest |= 0u32.wrapping_sub(dest & 0x0100_0000);
            buf[i + 1..i + 5].copy_from_slice(&dest.to_le_bytes());
            i += 5;
        } else {
            prev_mask = (prev_mask << 1) | 1;
            i += 1;
        }
    }
}

fn bcj_ppc(buf: &mut [u8], start: u32) {
    let mut i = 0usize;
    while i + 4 <= buf.len() {
        if buf[i] >> 2 == 0x12 && buf[i + 3] & 3 == 1 {
            let src = u32::from_be_bytes([buf[i] & 3, buf[i + 1], buf[i + 2], buf[i + 3] & !3]);
            let dest = src.wrapping_sub(start.wrapping_add(i as u32));
            buf[i] = 0x48 | ((dest >> 24) & 0x03) as u8;
            buf[i + 1] = (dest >> 16) as u8;
            buf[i + 2] = (dest >> 8) as u8;
            buf[i + 3] = (buf[i + 3] & 0x03) | (dest as u8 & !3);
        }
        i += 4;
    }
}

fn bcj_ia64(buf: &mut [u8], start: u32) {
    const BRANCH: [u8; 32] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 6, 6, 0, 0, 7, 7, 4, 4, 0, 0, 4, 4, 0, 0,
    ];
    let mut i = 0usize;
    while i + 16 <= buf.len() {
        let mask = BRANCH[(buf[i] & 0x1F) as usize];
        let mut bit_pos = 5u32;
        for slot in 0..3 {
            if (mask >> slot) & 1 != 0 {
                let byte_pos = (bit_pos >> 3) as usize;
                let bit_res = bit_pos & 7;
                let mut instr: u64 = 0;
                for j in 0..6 {
                    instr |= (buf[i + j + byte_pos] as u64) << (8 * j);
                }
                let mut norm = instr >> bit_res;
                if (norm >> 37) & 0xF == 0x5 && (norm >> 9) & 0x7 == 0 {
                    let mut addr = ((norm >> 13) & 0x0F_FFFF) as u32;
                    addr |= (((norm >> 36) & 1) as u32) << 20;
                    addr <<= 4;
                    addr = addr.wrapping_sub(start.wrapping_add(i as u32));
                    addr >>= 4;
                    norm &= !(0x8F_FFFFu64 << 13);
                    norm |= ((addr & 0x0F_FFFF) as u64) << 13;
                    norm |= ((addr & 0x10_0000) as u64) << (36 - 20);
                    instr &= (1u64 << bit_res) - 1;
                    instr |= norm << bit_res;
                    for j in 0..6 {
                        buf[i + j + byte_pos] = (instr >> (8 * j)) as u8;
                    }
                }
            }
            bit_pos += 41;
        }
        i += 16;
    }
}

fn bcj_arm(buf: &mut [u8], start: u32) {
    let mut i = 0usize;
    while i + 4 <= buf.len() {
        if buf[i + 3] == 0xEB {
            let mut src = u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], 0]);
            src <<= 2;
            let mut dest = src.wrapping_sub(start.wrapping_add(i as u32).wrapping_add(8));
            dest >>= 2;
            buf[i] = dest as u8;
            buf[i + 1] = (dest >> 8) as u8;
            buf[i + 2] = (dest >> 16) as u8;
        }
        i += 4;
    }
}

fn bcj_armthumb(buf: &mut [u8], start: u32) {
    let mut i = 0usize;
    while i + 4 <= buf.len() {
        if buf[i + 1] & 0xF8 == 0xF0 && buf[i + 3] & 0xF8 == 0xF8 {
            let mut src = (((buf[i + 1] & 7) as u32) << 19)
                | ((buf[i] as u32) << 11)
                | (((buf[i + 3] & 7) as u32) << 8)
                | buf[i + 2] as u32;
            src <<= 1;
            let mut dest = src.wrapping_sub(start.wrapping_add(i as u32).wrapping_add(4));
            dest >>= 1;
            buf[i + 1] = 0xF0 | ((dest >> 19) & 7) as u8;
            buf[i] = (dest >> 11) as u8;
            buf[i + 3] = 0xF8 | ((dest >> 8) & 7) as u8;
            buf[i + 2] = dest as u8;
            i += 2;
        }
        i += 2;
    }
}

fn bcj_sparc(buf: &mut [u8], start: u32) {
    let mut i = 0usize;
    while i + 4 <= buf.len() {
        if (buf[i] == 0x40 && buf[i + 1] & 0xC0 == 0x00) || (buf[i] == 0x7F && buf[i + 1] & 0xC0 == 0xC0) {
            let mut src = u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
            src <<= 2;
            let mut dest = src.wrapping_sub(start.wrapping_add(i as u32));
            dest >>= 2;
            dest = ((0u32.wrapping_sub((dest >> 22) & 1) << 22) & 0x3FFF_FFFF) | (dest & 0x3F_FFFF) | 0x4000_0000;
            buf[i..i + 4].copy_from_slice(&dest.to_be_bytes());
        }
        i += 4;
    }
}

/// Decodes a BCJ filter in place over the whole (single-shot) buffer.
pub fn bcj_decode(kind: Bcj, buf: &mut [u8], start: u32) {
    match kind {
        Bcj::X86 => bcj_x86(buf, start),
        Bcj::Ppc => bcj_ppc(buf, start),
        Bcj::Ia64 => bcj_ia64(buf, start),
        Bcj::Arm => bcj_arm(buf, start),
        Bcj::ArmThumb => bcj_armthumb(buf, start),
        Bcj::Sparc => bcj_sparc(buf, start),
    }
}

pub fn bcj_x86_decode(buf: &mut [u8], start: u32) {
    bcj_decode(Bcj::X86, buf, start);
}

pub fn delta_decode(data: &mut [u8], dist: usize) {
    if dist == 0 {
        return;
    }
    for i in dist..data.len() {
        data[i] = data[i].wrapping_add(data[i - dist]);
    }
}

#[derive(Debug)]
enum Filter {
    Delta(usize),
    Bcj(Bcj, u32),
    Lzma2,
}

fn parse_filter(id: u64, props: &[u8]) -> Result<Filter, String> {
    let bcj_start = |props: &[u8]| -> Result<u32, String> {
        match props.len() {
            0 => Ok(0),
            4 => Ok(u32::from_le_bytes(props.try_into().unwrap())),
            _ => Err("XZ unsupported BCJ filter properties".to_string()),
        }
    };
    Ok(match id {
        0x21 => {
            if props.len() != 1 || props[0] > 40 {
                return Err("XZ invalid LZMA2 properties".into());
            }
            Filter::Lzma2
        }
        0x03 => {
            if props.len() != 1 {
                return Err("XZ invalid delta filter properties".into());
            }
            Filter::Delta(props[0] as usize + 1)
        }
        0x04 => Filter::Bcj(Bcj::X86, bcj_start(props)?),
        0x05 => Filter::Bcj(Bcj::Ppc, bcj_start(props)?),
        0x06 => Filter::Bcj(Bcj::Ia64, bcj_start(props)?),
        0x07 => Filter::Bcj(Bcj::Arm, bcj_start(props)?),
        0x08 => Filter::Bcj(Bcj::ArmThumb, bcj_start(props)?),
        0x09 => Filter::Bcj(Bcj::Sparc, bcj_start(props)?),
        other => return Err(format!("XZ unsupported filter id 0x{other:x}")),
    })
}

fn check_size(check_type: u8) -> usize {
    match check_type {
        0 => 0,
        1..=3 => 4,
        4..=6 => 8,
        7..=9 => 16,
        10..=12 => 32,
        _ => 64,
    }
}

pub fn xz_decompress(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    match xz_into(input, &mut output) {
        Ok(()) => Ok(output),
        Err(e) => {
            super::stash_partial(output);
            Err(e)
        }
    }
}

fn xz_into(input: &[u8], output: &mut Vec<u8>) -> Result<(), String> {
    let mut pos = 0usize;
    let mut first = true;

    loop {
        // Stream padding (zero bytes, multiple of 4) between concatenated streams.
        if !first {
            let pad_start = pos;
            while pos < input.len() && input[pos] == 0 {
                pos += 1;
            }
            if pos == input.len() {
                if (pos - pad_start) % 4 != 0 {
                    return Err("XZ stream padding is not a multiple of four bytes".into());
                }
                break;
            }
            if (pos - pad_start) % 4 != 0 {
                return Err("XZ stream padding is not a multiple of four bytes".into());
            }
        }
        first = false;

        if input.len() - pos < 12 {
            return Err(if pos == 0 && input.is_empty() { "XZ input is empty".into() } else { "XZ stream header truncated".to_string() });
        }
        if &input[pos..pos + 6] != HEADER_MAGIC {
            return Err("Not an XZ archive (missing header magic)".into());
        }
        let stream_flags = [input[pos + 6], input[pos + 7]];
        if u32::from_le_bytes(input[pos + 8..pos + 12].try_into().unwrap()) != crc32(&stream_flags) {
            return Err("XZ stream header CRC32 mismatch".into());
        }
        if stream_flags[0] != 0 || stream_flags[1] & 0xF0 != 0 {
            return Err("Unsupported XZ stream flags".into());
        }
        let check_type = stream_flags[1] & 0x0F;
        let csize = check_size(check_type);
        pos += 12;

        let mut records: Vec<(u64, u64)> = Vec::new();

        loop {
            if pos >= input.len() {
                return Err("XZ stream truncated (missing index)".into());
            }
            if input[pos] == 0 {
                break; // index indicator
            }
            let bh_size = (input[pos] as usize + 1) * 4;
            if pos + bh_size > input.len() {
                return Err("XZ block header truncated".into());
            }
            let bh = &input[pos..pos + bh_size];
            if u32::from_le_bytes(bh[bh_size - 4..].try_into().unwrap()) != crc32(&bh[..bh_size - 4]) {
                return Err("XZ block header CRC32 mismatch".into());
            }
            let flags = bh[1];
            if flags & 0x3C != 0 {
                return Err("XZ block header uses unsupported flags".into());
            }
            let nfilters = (flags & 3) as usize + 1;
            let mut hp = 2usize;
            let hdr_end = bh_size - 4;
            let mut sub = &bh[..hdr_end];
            let comp_size = if flags & 0x40 != 0 { Some(read_varint(sub, &mut hp)?) } else { None };
            let uncomp_size = if flags & 0x80 != 0 { Some(read_varint(sub, &mut hp)?) } else { None };
            let mut filters = Vec::new();
            for _ in 0..nfilters {
                let id = read_varint(sub, &mut hp)?;
                let psize = read_varint(sub, &mut hp)? as usize;
                if psize > sub.len() - hp.min(sub.len()) {
                    return Err("XZ block header truncated".into());
                }
                filters.push(parse_filter(id, &sub[hp..hp + psize])?);
                hp += psize;
            }
            sub = &sub[hp..];
            if sub.iter().any(|&b| b != 0) {
                return Err("XZ block header padding is not zero".into());
            }
            if !matches!(filters.last(), Some(Filter::Lzma2)) || filters[..filters.len() - 1].iter().any(|f| matches!(f, Filter::Lzma2)) {
                return Err("XZ filter chain must end with LZMA2".into());
            }

            pos += bh_size;
            let data_start = pos;
            let out_start = output.len();
            let avail = &input[data_start..];
            let consumed = lzma2_decode_into(avail, output, uncomp_size.map(|v| v as usize))?;
            if let Some(cs) = comp_size {
                if cs != consumed as u64 {
                    return Err("XZ block compressed size mismatch".into());
                }
            }
            let block = &mut output[out_start..];
            for f in filters[..filters.len() - 1].iter().rev() {
                match f {
                    Filter::Delta(d) => delta_decode(block, *d),
                    Filter::Bcj(k, s) => bcj_decode(*k, block, *s),
                    Filter::Lzma2 => unreachable!(),
                }
            }
            let ulen = block.len() as u64;
            pos = data_start + consumed;
            let pad = (4 - consumed % 4) % 4;
            if pos + pad + csize > input.len() {
                return Err("XZ block truncated".into());
            }
            if input[pos..pos + pad].iter().any(|&b| b != 0) {
                return Err("XZ block padding is not zero".into());
            }
            pos += pad;
            let cb = &input[pos..pos + csize];
            match check_type {
                1 => {
                    if u32::from_le_bytes(cb.try_into().unwrap()) != crc32(block) {
                        return Err("XZ block CRC32 check failed".into());
                    }
                }
                4 => {
                    if u64::from_le_bytes(cb.try_into().unwrap()) != crc64(block) {
                        return Err("XZ block CRC64 check failed".into());
                    }
                }
                10 => {
                    let expected: [u8; 32] = cb.try_into().unwrap();
                    if sha256(block) != expected {
                        return Err("XZ block SHA-256 check failed".into());
                    }
                }
                _ => {}
            }
            pos += csize;
            let unpadded = bh_size as u64 + consumed as u64 + csize as u64;
            records.push((unpadded, ulen));
        }

        // Index
        let index_start = pos;
        pos += 1;
        let nrec = read_varint(input, &mut pos)?;
        if nrec != records.len() as u64 {
            return Err("XZ index record count mismatch".into());
        }
        for r in &records {
            let u = read_varint(input, &mut pos)?;
            let l = read_varint(input, &mut pos)?;
            if (u, l) != *r {
                return Err("XZ index does not match decoded blocks".into());
            }
        }
        let pad = (4 - (pos - index_start) % 4) % 4;
        if pos + pad + 4 > input.len() {
            return Err("XZ index truncated".into());
        }
        if input[pos..pos + pad].iter().any(|&b| b != 0) {
            return Err("XZ index padding is not zero".into());
        }
        pos += pad;
        if u32::from_le_bytes(input[pos..pos + 4].try_into().unwrap()) != crc32(&input[index_start..pos]) {
            return Err("XZ Index CRC32 mismatch".into());
        }
        pos += 4;
        let index_size = pos - index_start;

        if pos + 12 > input.len() {
            return Err("XZ Stream Footer truncated".into());
        }
        let footer = &input[pos..pos + 12];
        if &footer[10..12] != b"YZ" {
            return Err("XZ bad Stream Footer magic".into());
        }
        if u32::from_le_bytes(footer[0..4].try_into().unwrap()) != crc32(&footer[4..10]) {
            return Err("XZ Stream Footer CRC32 mismatch".into());
        }
        let backward = (u32::from_le_bytes(footer[4..8].try_into().unwrap()) as usize + 1) * 4;
        if backward != index_size {
            return Err("XZ backward size mismatch".into());
        }
        if footer[8..10] != stream_flags {
            return Err("XZ Stream Footer flags mismatch Stream Header flags".into());
        }
        pos += 12;
        // Bazel reads a single XZ stream (XZCompressorInputStream without concatenation): bytes after the
        // first stream's footer are never looked at.
        break;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc64() {
        assert_eq!(crc64(b"123456789"), 0x995DC9BBDF1939FA);
    }
}
