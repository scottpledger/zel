//! Pure-Rust LZMA and LZMA2 decompressor.
//!
//! Re-implementation of the LZMA SDK (`LzmaDec` / `Lzma2Dec`) algorithm.  The decoder writes
//! straight into the caller's output `Vec<u8>`; the output buffer *is* the dictionary.  Match
//! distances are validated against the number of bytes produced since the last dictionary
//! reset (`dict_start`), not per chunk.

const NUM_STATES: usize = 12;
const NUM_LEN_TO_POS_STATES: usize = 4;
const PROB_INIT: u16 = 1024;
const TOP: u32 = 1 << 24;

struct Rc<'a> {
    data: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl<'a> Rc<'a> {
    fn new(data: &'a [u8]) -> Result<Self, String> {
        if data.len() < 5 {
            return Err("LZMA data truncated (range coder init)".into());
        }
        if data[0] != 0 {
            return Err("LZMA data corrupt (range coder first byte not zero)".into());
        }
        let code = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
        Ok(Self { data, pos: 5, range: 0xFFFF_FFFF, code })
    }

    #[inline(always)]
    fn normalize(&mut self) {
        if self.range < TOP {
            self.range <<= 8;
            let b = if self.pos < self.data.len() { self.data[self.pos] } else { 0 };
            self.pos += 1;
            self.code = (self.code << 8) | b as u32;
        }
    }

    #[inline(always)]
    fn bit(&mut self, prob: &mut u16) -> u32 {
        self.normalize();
        let p = *prob as u32;
        let bound = (self.range >> 11) * p;
        if self.code < bound {
            self.range = bound;
            *prob = (p + ((2048 - p) >> 5)) as u16;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *prob = (p - (p >> 5)) as u16;
            1
        }
    }

    #[inline(always)]
    fn tree(&mut self, probs: &mut [u16], bits: u32) -> usize {
        let mut m = 1usize;
        for _ in 0..bits {
            m = (m << 1) | self.bit(&mut probs[m]) as usize;
        }
        m - (1 << bits)
    }

    #[inline(always)]
    fn rev_tree(&mut self, probs: &mut [u16], bits: u32) -> usize {
        let mut m = 1usize;
        let mut sym = 0usize;
        for i in 0..bits {
            let b = self.bit(&mut probs[m]) as usize;
            m = (m << 1) | b;
            sym |= b << i;
        }
        sym
    }

    #[inline(always)]
    fn direct(&mut self, count: u32) -> u32 {
        let mut res = 0u32;
        for _ in 0..count {
            self.normalize();
            self.range >>= 1;
            if self.code >= self.range {
                self.code -= self.range;
                res = (res << 1) | 1;
            } else {
                res <<= 1;
            }
        }
        res
    }

    fn overrun(&self) -> bool {
        self.pos > self.data.len()
    }
}

struct LenDec {
    choice: u16,
    choice2: u16,
    low: [[u16; 8]; 16],
    mid: [[u16; 8]; 16],
    high: [u16; 256],
}

impl LenDec {
    fn new() -> Self {
        Self { choice: PROB_INIT, choice2: PROB_INIT, low: [[PROB_INIT; 8]; 16], mid: [[PROB_INIT; 8]; 16], high: [PROB_INIT; 256] }
    }
    #[inline(always)]
    fn decode(&mut self, rc: &mut Rc, pos_state: usize) -> usize {
        if rc.bit(&mut self.choice) == 0 {
            rc.tree(&mut self.low[pos_state], 3)
        } else if rc.bit(&mut self.choice2) == 0 {
            8 + rc.tree(&mut self.mid[pos_state], 3)
        } else {
            16 + rc.tree(&mut self.high, 8)
        }
    }
}

/// LZMA decoder state (probabilities, state machine and repeat distances).
pub struct LzmaDecoder {
    lc: u32,
    lp: u32,
    pb: u32,
    is_match: [[u16; 16]; NUM_STATES],
    is_rep: [u16; NUM_STATES],
    is_rep_g0: [u16; NUM_STATES],
    is_rep_g1: [u16; NUM_STATES],
    is_rep_g2: [u16; NUM_STATES],
    is_rep0_long: [[u16; 16]; NUM_STATES],
    pos_slot: [[u16; 64]; NUM_LEN_TO_POS_STATES],
    pos_special: [u16; 115],
    align: [u16; 16],
    len_dec: LenDec,
    rep_len_dec: LenDec,
    lit: Vec<u16>,
    reps: [usize; 4],
    state: usize,
}

/// Parses an LZMA properties byte into (lc, lp, pb).
pub fn parse_props_byte(b: u8) -> Result<(u32, u32, u32), String> {
    if b >= 9 * 5 * 5 {
        return Err("LZMA properties byte invalid".into());
    }
    let b = b as u32;
    Ok((b % 9, (b / 9) % 5, b / 45))
}

impl LzmaDecoder {
    pub fn new(lc: u32, lp: u32, pb: u32) -> Self {
        Self {
            lc,
            lp,
            pb,
            is_match: [[PROB_INIT; 16]; NUM_STATES],
            is_rep: [PROB_INIT; NUM_STATES],
            is_rep_g0: [PROB_INIT; NUM_STATES],
            is_rep_g1: [PROB_INIT; NUM_STATES],
            is_rep_g2: [PROB_INIT; NUM_STATES],
            is_rep0_long: [[PROB_INIT; 16]; NUM_STATES],
            pos_slot: [[PROB_INIT; 64]; NUM_LEN_TO_POS_STATES],
            pos_special: [PROB_INIT; 115],
            align: [PROB_INIT; 16],
            len_dec: LenDec::new(),
            rep_len_dec: LenDec::new(),
            lit: vec![PROB_INIT; 0x300usize << (lc + lp)],
            reps: [1; 4],
            state: 0,
        }
    }

    /// Resets probabilities, state and repeat distances (optionally with new properties).
    pub fn reset(&mut self, lc: u32, lp: u32, pb: u32) {
        *self = Self::new(lc, lp, pb);
    }

    /// Decodes until `limit` bytes are in `out` (or an end marker is found).
    /// `dict_start` is the index in `out` of the last dictionary reset.
    /// Returns Ok(true) when an end marker was seen.
    /// If `strict_len` a match crossing `limit` is an error, otherwise it is truncated.
    fn decode(
        &mut self,
        rc: &mut Rc,
        out: &mut Vec<u8>,
        dict_start: usize,
        limit: usize,
        strict_len: bool,
    ) -> Result<bool, String> {
        let pb_mask = (1usize << self.pb) - 1;
        let lp_mask = (1usize << self.lp) - 1;
        let lc = self.lc;
        let mut state = self.state;
        let mut reps = self.reps;
        let mut end_marker = false;

        while out.len() < limit {
            if rc.overrun() {
                return Err("LZMA data truncated".into());
            }
            let pos = out.len() - dict_start;
            let pos_state = pos & pb_mask;

            if rc.bit(&mut self.is_match[state][pos_state]) == 0 {
                let prev = if pos > 0 { out[out.len() - 1] as usize } else { 0 };
                let base = 0x300 * (((pos & lp_mask) << lc) + (prev >> (8 - lc)));
                let probs = &mut self.lit[base..base + 0x300];
                let mut sym = 1usize;
                if state >= 7 {
                    // reps[0] <= pos is guaranteed by earlier validation
                    let mut match_byte = out[out.len() - reps[0]] as usize;
                    let mut offs = 0x100usize;
                    while sym < 0x100 {
                        match_byte <<= 1;
                        let match_bit = match_byte & offs;
                        let bit = rc.bit(&mut probs[offs + match_bit + sym]) as usize;
                        sym = (sym << 1) | bit;
                        offs &= if bit != 0 { match_bit } else { !match_bit };
                    }
                } else {
                    while sym < 0x100 {
                        sym = (sym << 1) | rc.bit(&mut probs[sym]) as usize;
                    }
                }
                out.push(sym as u8);
                state = if state < 4 { 0 } else if state < 10 { state - 3 } else { state - 6 };
                continue;
            }

            let len;
            if rc.bit(&mut self.is_rep[state]) != 0 {
                if pos == 0 {
                    return Err("LZMA data corrupt (repeat match with empty dictionary)".into());
                }
                if rc.bit(&mut self.is_rep_g0[state]) == 0 {
                    if rc.bit(&mut self.is_rep0_long[state][pos_state]) == 0 {
                        // short rep
                        if reps[0] > pos {
                            return Err(format!("LZMA repeat offset {} > available bytes {}", reps[0], pos));
                        }
                        state = if state < 7 { 9 } else { 11 };
                        let b = out[out.len() - reps[0]];
                        out.push(b);
                        continue;
                    }
                } else {
                    let dist;
                    if rc.bit(&mut self.is_rep_g1[state]) == 0 {
                        dist = reps[1];
                    } else {
                        if rc.bit(&mut self.is_rep_g2[state]) == 0 {
                            dist = reps[2];
                        } else {
                            dist = reps[3];
                            reps[3] = reps[2];
                        }
                        reps[2] = reps[1];
                    }
                    reps[1] = reps[0];
                    reps[0] = dist;
                }
                len = 2 + self.rep_len_dec.decode(rc, pos_state);
                state = if state < 7 { 8 } else { 11 };
            } else {
                reps[3] = reps[2];
                reps[2] = reps[1];
                reps[1] = reps[0];
                len = 2 + self.len_dec.decode(rc, pos_state);
                state = if state < 7 { 7 } else { 10 };
                let len_state = (len - 2).min(NUM_LEN_TO_POS_STATES - 1);
                let slot = rc.tree(&mut self.pos_slot[len_state], 6);
                if slot < 4 {
                    reps[0] = slot + 1;
                } else {
                    let nd = ((slot >> 1) - 1) as u32;
                    let mut dist = (2 | (slot & 1)) << nd;
                    if slot < 14 {
                        let start = dist - slot;
                        dist += rc.rev_tree(&mut self.pos_special[start..], nd);
                    } else {
                        dist += (rc.direct(nd - 4) as usize) << 4;
                        dist += rc.rev_tree(&mut self.align, 4);
                    }
                    if dist == 0xFFFF_FFFF {
                        end_marker = true;
                        break;
                    }
                    reps[0] = dist + 1;
                }
            }

            if reps[0] > pos {
                return Err(format!("LZMA repeat offset {} > available bytes {}", reps[0], pos));
            }
            let remaining = limit - out.len();
            let mut copy = len;
            if copy > remaining {
                if strict_len {
                    return Err("LZMA data corrupt (match crosses chunk boundary)".into());
                }
                copy = remaining;
            }
            let dist = reps[0];
            let cur = out.len();
            if dist >= copy {
                out.extend_from_within(cur - dist..cur - dist + copy);
            } else {
                out.reserve(copy);
                for i in 0..copy {
                    let b = out[cur - dist + i];
                    out.push(b);
                }
            }
        }
        self.state = state;
        self.reps = reps;
        Ok(end_marker)
    }
}

/// Decompresses a raw LZMA stream (7z coder / .lzma payload) producing exactly `unpack_size` bytes.
pub fn lzma_decompress(data: &[u8], unpack_size: usize, lc: u32, lp: u32, pb: u32) -> Result<Vec<u8>, String> {
    if lc > 8 || lp > 4 || pb > 4 {
        return Err("LZMA properties out of range".into());
    }
    let mut dec = LzmaDecoder::new(lc, lp, pb);
    let mut out = Vec::with_capacity(unpack_size.min(1 << 28));
    if unpack_size == 0 {
        return Ok(out);
    }
    let mut rc = Rc::new(data)?;
    let marker = dec.decode(&mut rc, &mut out, 0, unpack_size, false)?;
    if rc.overrun() {
        return Err("LZMA data truncated".into());
    }
    if marker && out.len() != unpack_size {
        return Err("LZMA data ended before expected size".into());
    }
    Ok(out)
}

/// Decodes an LZMA2 stream, appending to `out`; the dictionary starts at the current end of `out`.
/// Returns the number of input bytes consumed (including the end marker byte).
/// With `expected_len`, the stream may end without an end marker once that many bytes are produced.
pub fn lzma2_decode_into(input: &[u8], out: &mut Vec<u8>, expected_len: Option<usize>) -> Result<usize, String> {
    let start_len = out.len();
    let mut dict_start = start_len;
    let mut need_dict_reset = true;
    let mut need_props = true;
    let mut dec: Option<LzmaDecoder> = None;
    let mut pos = 0usize;

    loop {
        if pos >= input.len() {
            return match expected_len {
                Some(n) if out.len() - start_len == n => Ok(pos),
                _ => Err("LZMA2 data truncated".into()),
            };
        }
        let control = input[pos];
        pos += 1;
        if control == 0 {
            if let Some(n) = expected_len {
                if out.len() - start_len != n {
                    return Err("LZMA2 stream size mismatch".into());
                }
            }
            return Ok(pos);
        }
        if control < 0x80 {
            if control > 2 {
                return Err("LZMA2 data corrupt (invalid control byte)".into());
            }
            if pos + 2 > input.len() {
                return Err("LZMA2 data truncated".into());
            }
            let size = (((input[pos] as usize) << 8) | input[pos + 1] as usize) + 1;
            pos += 2;
            if control == 1 {
                dict_start = out.len();
                need_dict_reset = false;
                need_props = true; // an LZMA chunk after a dict reset must reset state first
            } else if need_dict_reset {
                return Err("LZMA2 data corrupt (missing dictionary reset)".into());
            }
            if pos + size > input.len() {
                return Err("LZMA2 data truncated".into());
            }
            out.extend_from_slice(&input[pos..pos + size]);
            pos += size;
            continue;
        }

        if pos + 4 > input.len() {
            return Err("LZMA2 data truncated".into());
        }
        let unpack = ((((control & 0x1F) as usize) << 16) | ((input[pos] as usize) << 8) | input[pos + 1] as usize) + 1;
        let pack = (((input[pos + 2] as usize) << 8) | input[pos + 3] as usize) + 1;
        pos += 4;
        let mode = (control >> 5) & 3;
        if mode == 3 {
            dict_start = out.len();
            need_dict_reset = false;
        } else if need_dict_reset {
            return Err("LZMA2 data corrupt (missing dictionary reset)".into());
        }
        if mode >= 2 {
            if pos >= input.len() {
                return Err("LZMA2 data truncated".into());
            }
            let (lc, lp, pb) = parse_props_byte(input[pos])?;
            pos += 1;
            if lc + lp > 4 {
                return Err("LZMA2 data corrupt (lc + lp > 4)".into());
            }
            match dec.as_mut() {
                Some(d) => d.reset(lc, lp, pb),
                None => dec = Some(LzmaDecoder::new(lc, lp, pb)),
            }
            need_props = false;
        } else {
            if need_props {
                return Err("LZMA2 data corrupt (missing properties reset)".into());
            }
            if mode == 1 {
                let d = dec.as_mut().unwrap();
                let (lc, lp, pb) = (d.lc, d.lp, d.pb);
                d.reset(lc, lp, pb);
            }
        }
        let d = dec.as_mut().unwrap();
        if pos + pack > input.len() {
            return Err("LZMA2 data truncated".into());
        }
        let chunk = &input[pos..pos + pack];
        pos += pack;
        let limit = out.len() + unpack;
        out.reserve(unpack);
        let mut rc = Rc::new(chunk)?;
        let marker = d.decode(&mut rc, out, dict_start, limit, true)?;
        if marker || out.len() != limit {
            return Err("LZMA2 data corrupt (chunk ended early)".into());
        }
        rc.normalize();
        if rc.pos != chunk.len() || rc.code != 0 {
            return Err("LZMA2 data corrupt (chunk not fully consumed)".into());
        }
    }
}

pub fn lzma2_decompress(input: &[u8], expected_len: Option<usize>) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    lzma2_decode_into(input, &mut out, expected_len)?;
    Ok(out)
}
