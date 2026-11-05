//! Pure-Rust Bzip2 (bunzip2) decompressor.
//!
//! Implements RFC / standard bzip2 decompression:
//! - Block header (0x314159265359)
//! - Invertible Burrows-Wheeler Transform (BWT)
//! - Move-to-Front (MTF) + RLE2 decoding
//! - Multiple Huffman tables with canonical codes
//! - RLE1 expansion (run of 4 consecutive identical bytes)
//! - CRC-32 validation

struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn read_bit(&mut self) -> Result<u32, String> {
        let byte_idx = self.bit_pos >> 3;
        if byte_idx >= self.data.len() {
            return Err("Unexpected end of bzip2 stream".into());
        }
        let bit_offset = 7 - (self.bit_pos & 7);
        self.bit_pos += 1;
        Ok(((self.data[byte_idx] >> bit_offset) & 1) as u32)
    }

    fn read_bits(&mut self, n: usize) -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        if n > 32 {
            return Err("Cannot read more than 32 bits at once".into());
        }
        let mut val = 0u32;
        for _ in 0..n {
            val = (val << 1) | self.read_bit()?;
        }
        Ok(val)
    }

    fn read_u8(&mut self) -> Result<u8, String> {
        self.read_bits(8).map(|v| v as u8)
    }
}

/// CRC-32 for bzip2 (polynomial 0x04C11DB7, init 0xFFFFFFFF, inverted).
fn bzip2_crc32(prev: u32, buf: &[u8]) -> u32 {
    let mut crc = prev;
    for &b in buf {
        crc = (crc << 8) ^ CRC_TABLE[(((crc >> 24) ^ (b as u32)) & 0xFF) as usize];
    }
    crc
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let poly = 0x04C11DB7u32;
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            c = if (c & 0x80000000) != 0 {
                (c << 1) ^ poly
            } else {
                c << 1
            };
            j += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// A canonical Huffman decoder for bzip2.
struct CanonicalHuffman {
    limit: [i32; 24],
    base: [i32; 24],
    perm: [usize; 258],
    min_len: usize,
    max_len: usize,
}

impl CanonicalHuffman {
    fn new(lengths: &[u8], alphabet_size: usize) -> Result<Self, String> {
        let mut min_len = 32;
        let mut max_len = 0;
        for &l in &lengths[..alphabet_size] {
            let l = l as usize;
            if l > max_len {
                max_len = l;
            }
            if l > 0 && l < min_len {
                min_len = l;
            }
        }
        if alphabet_size == 0 || max_len == 0 {
            return Ok(Self {
                limit: [0; 24],
                base: [0; 24],
                perm: [0; 258],
                min_len: 0,
                max_len: 0,
            });
        }
        if max_len > 20 {
            return Err("Bzip2 Huffman code length > 20".into());
        }

        let mut limit = [0i32; 24];
        let mut base = [0i32; 24];
        let mut perm = [0usize; 258];

        let mut pp = 0;
        for i in min_len..=max_len {
            for j in 0..alphabet_size {
                if lengths[j] as usize == i {
                    perm[pp] = j;
                    pp += 1;
                }
            }
        }

        let mut base_v = 0i32;
        for i in min_len..=max_len {
            base[i] = base_v;
            let mut count = 0i32;
            for j in 0..alphabet_size {
                if lengths[j] as usize == i {
                    count += 1;
                }
            }
            limit[i] = base_v + count - 1;
            base_v = (base_v + count) << 1;
        }

        Ok(Self {
            limit,
            base,
            perm,
            min_len,
            max_len,
        })
    }

    fn decode(&self, reader: &mut BitReader) -> Result<usize, String> {
        if self.max_len == 0 {
            return Ok(0);
        }
        let mut code = reader.read_bits(self.min_len)? as i32;
        let mut len = self.min_len;
        let mut perm_offset = 0usize;
        while len <= self.max_len {
            if code <= self.limit[len] {
                let idx = perm_offset + (code - self.base[len]) as usize;
                if idx < self.perm.len() {
                    return Ok(self.perm[idx]);
                }
                return Err("Bzip2 Huffman index out of range".into());
            }
            perm_offset += (self.limit[len] - self.base[len] + 1) as usize;
            len += 1;
            if len <= self.max_len {
                code = (code << 1) | (reader.read_bit()? as i32);
            }
        }
        Err("Bzip2 Huffman decode error: code length exceeded max".into())
    }
}

/// Decompresses one or more concatenated bzip2 streams from `input`.
/// Trailing bytes that do not start another stream are ignored (like bzip2 does).
pub fn bunzip2(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    loop {
        let used = match bunzip2_stream(&input[pos..], &mut out) {
            Ok(u) => u,
            Err(e) => {
                super::stash_partial(out);
                return Err(e);
            }
        };
        pos += used;
        if pos >= input.len() {
            break;
        }
        if input.len() - pos < 3 || &input[pos..pos + 3] != b"BZh" {
            super::stash_partial(out);
            return Err("Garbage after a valid BZip2 stream".into());
        }
    }
    Ok(out)
}

/// Decompresses one bzip2 stream; returns the number of input bytes consumed.
fn bunzip2_stream(input: &[u8], decompressed: &mut Vec<u8>) -> Result<usize, String> {
    if input.len() == 3 && &input[..3] == b"BZh" {
        return Err("Invalid bzip2 block size: \u{ffff}".into());
    }
    if input.len() < 4 || &input[..3] != b"BZh" {
        return Err("Not a bzip2 archive (missing BZh header)".into());
    }
    let block_size_char = input[3];
    if !(b'1'..=b'9').contains(&block_size_char) {
        return Err(format!("Invalid bzip2 block size: {}", block_size_char as char));
    }

    let mut reader = BitReader::new(&input[4..]);
    let max_block = (block_size_char - b'0') as usize * 100_000;
    let mut stream_crc = 0u32;

    loop {
        let magic_high = reader.read_bits(24)?;
        let magic_low = reader.read_bits(24)?;
        let magic = ((magic_high as u64) << 24) | (magic_low as u64);

        if magic == 0x177245385090 {
            // End of stream marker
            let expected_stream_crc = reader.read_bits(32)?;
            if expected_stream_crc != stream_crc {
                return Err("bzip2 stream CRC mismatch".into());
            }
            break;
        }

        if magic != 0x314159265359 {
            return Err(format!("Bad bzip2 block magic: 0x{:012x}", magic));
        }

        let block_crc = reader.read_bits(32)?;
        let randomised = reader.read_bit()? != 0;
        if randomised {
            return Err("Randomised bzip2 blocks are not supported".into());
        }

        let orig_ptr = reader.read_bits(24)? as usize;

        // In-use bitmap
        let in_use_16 = reader.read_bits(16)?;
        let mut in_use = [false; 256];
        let mut alphabet_size = 0usize;
        for i in 0..16 {
            if (in_use_16 & (1 << (15 - i))) != 0 {
                let in_use_byte = reader.read_bits(16)?;
                for j in 0..16 {
                    if (in_use_byte & (1 << (15 - j))) != 0 {
                        in_use[i * 16 + j] = true;
                        alphabet_size += 1;
                    }
                }
            }
        }
        if alphabet_size == 0 {
            return Err("Empty bzip2 alphabet".into());
        }
        let total_symbols = alphabet_size + 2; // RUNA, RUNB, symbols..., EOB

        // Huffman tables (2 to 6)
        let num_tables = reader.read_bits(3)? as usize;
        if !(2..=6).contains(&num_tables) {
            return Err(format!("Invalid number of bzip2 Huffman tables: {num_tables}"));
        }

        let num_selectors = reader.read_bits(15)? as usize;
        if num_selectors == 0 {
            return Err("Zero selectors in bzip2 block".into());
        }

        // Read selectors (MTF encoded)
        let mut selector_mtf: Vec<usize> = (0..num_tables).collect();
        let mut selectors = Vec::with_capacity(num_selectors);
        for _ in 0..num_selectors {
            let mut count = 0;
            while reader.read_bit()? != 0 {
                count += 1;
                if count >= num_tables {
                    return Err("Selector index out of range".into());
                }
            }
            let sel = selector_mtf.remove(count);
            selector_mtf.insert(0, sel);
            selectors.push(sel);
        }

        // Table code lengths
        let mut tables = Vec::with_capacity(num_tables);
        for _ in 0..num_tables {
            let mut current_len = reader.read_bits(5)? as i32;
            let mut lengths = vec![0u8; total_symbols];
            for sym in 0..total_symbols {
                while reader.read_bit()? != 0 {
                    if reader.read_bit()? != 0 {
                        current_len -= 1;
                    } else {
                        current_len += 1;
                    }
                }
                if !(1..=20).contains(&current_len) {
                    return Err("Huffman length out of range (1..20)".into());
                }
                lengths[sym] = current_len as u8;
            }
            tables.push(CanonicalHuffman::new(&lengths, total_symbols)?);
        }

        // Decode symbols into BWT buffer
        // Build initial MTF table
        let mut mtf = Vec::with_capacity(alphabet_size);
        for i in 0..256 {
            if in_use[i] {
                mtf.push(i as u8);
            }
        }

        let mut bwt = Vec::new();
        let mut group_idx = 0;
        let mut selector_idx = 0;

        loop {
            if group_idx == 0 {
                if selector_idx >= selectors.len() {
                    return Err("Ran out of selectors in bzip2 block".into());
                }
                group_idx = 50;
            }
            group_idx -= 1;
            let current_table = &tables[selectors[selector_idx]];
            if group_idx == 0 {
                selector_idx += 1;
            }

            let sym = current_table.decode(&mut reader)?;

            if sym == 0 || sym == 1 {
                // RUNA or RUNB
                let mut run_len = 0usize;
                let mut run_shift = 0usize;
                let mut cur_sym = sym;
                loop {
                    run_len += (cur_sym + 1) << run_shift;
                    run_shift += 1;

                    if group_idx == 0 {
                        if selector_idx >= selectors.len() {
                            return Err("Ran out of selectors during run".into());
                        }
                        group_idx = 50;
                    }
                    group_idx -= 1;
                    let table = &tables[selectors[selector_idx]];
                    if group_idx == 0 {
                        selector_idx += 1;
                    }

                    cur_sym = table.decode(&mut reader)?;
                    if cur_sym != 0 && cur_sym != 1 {
                        break;
                    }
                }
                let byte = mtf[0];
                if run_len > max_block || bwt.len() + run_len > max_block {
                    return Err("bzip2 block exceeds declared block size".into());
                }
                bwt.resize(bwt.len() + run_len, byte);

                if cur_sym == alphabet_size + 1 {
                    // EOB
                    break;
                }

                // cur_sym is in 2..=alphabet_size
                let mtf_idx = cur_sym - 1;
                if mtf_idx >= mtf.len() || bwt.len() >= max_block {
                    return Err("bzip2 data is corrupt".into());
                }
                let b = mtf.remove(mtf_idx);
                mtf.insert(0, b);
                bwt.push(b);
            } else if sym == alphabet_size + 1 {
                // EOB
                break;
            } else {
                let mtf_idx = sym - 1;
                if mtf_idx >= mtf.len() || bwt.len() >= max_block {
                    return Err("bzip2 data is corrupt".into());
                }
                let b = mtf.remove(mtf_idx);
                mtf.insert(0, b);
                bwt.push(b);
            }
        }

        if orig_ptr >= bwt.len() && !bwt.is_empty() {
            return Err("orig_ptr out of range in bzip2 block".into());
        }

        // Inverse Burrows-Wheeler Transform
        let n = bwt.len();
        let mut decoded_block = Vec::with_capacity(n);
        if n > 0 {
            let mut count = [0usize; 256];
            for &b in &bwt {
                count[b as usize] += 1;
            }
            let mut base = [0usize; 256];
            let mut sum = 0usize;
            for i in 0..256 {
                base[i] = sum;
                sum += count[i];
            }
            let mut tt = vec![0usize; n];
            for (i, &b) in bwt.iter().enumerate() {
                let b = b as usize;
                tt[base[b]] = i;
                base[b] += 1;
            }

            let mut t_pos = tt[orig_ptr];
            for _ in 0..n {
                decoded_block.push(bwt[t_pos]);
                t_pos = tt[t_pos];
            }
        }

        // RLE1 decode (sequences of 4 identical bytes)
        let mut block_out = Vec::with_capacity(decoded_block.len());
        let mut i = 0;
        while i < decoded_block.len() {
            if i + 4 <= decoded_block.len()
                && decoded_block[i] == decoded_block[i + 1]
                && decoded_block[i] == decoded_block[i + 2]
                && decoded_block[i] == decoded_block[i + 3]
            {
                let b = decoded_block[i];
                block_out.extend_from_slice(&[b, b, b, b]);
                i += 4;
                if i < decoded_block.len() {
                    let repeat = decoded_block[i] as usize;
                    block_out.resize(block_out.len() + repeat, b);
                    i += 1;
                }
            } else {
                block_out.push(decoded_block[i]);
                i += 1;
            }
        }

        // Verify block CRC
        let computed_crc = !bzip2_crc32(!0, &block_out);
        if computed_crc != block_crc {
            // Java emits the block's bytes before the CRC check fires.
            decompressed.extend_from_slice(&block_out);
            return Err(format!(
                "bzip2 block CRC mismatch: calculated 0x{:08x}, expected 0x{:08x}",
                computed_crc, block_crc
            ));
        }

        stream_crc = ((stream_crc << 1) | (stream_crc >> 31)) ^ computed_crc;
        decompressed.extend_from_slice(&block_out);
    }

    Ok(4 + (reader.bit_pos + 7) / 8)
}
