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

//! Arbitrary-precision integer implementation (`BigInt`) with zero external dependencies.
//! Implements exact arithmetic, two's complement bitwise logic, and floored division/modulo
//! matching Starlark and Python specifications.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Arbitrary-precision integer with sign and base-2^32 magnitude limbs.
/// Limbs are ordered least-significant first.
/// Canonical representation: zero has `is_negative = false` and `limbs.is_empty()`.
#[derive(Clone, Eq)]
pub struct BigInt {
    is_negative: bool,
    limbs: Vec<u32>,
}

impl BigInt {
    pub const ZERO: Self = Self {
        is_negative: false,
        limbs: Vec::new(),
    };

    pub const ONE: Self = Self {
        is_negative: false,
        limbs: Vec::new(), // will be normalized from from_u64
    };

    pub fn zero() -> Self {
        Self {
            is_negative: false,
            limbs: Vec::new(),
        }
    }

    pub fn one() -> Self {
        Self::from_u64(1)
    }

    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    pub fn is_negative(&self) -> bool {
        self.is_negative
    }

    pub fn signum(&self) -> i32 {
        if self.is_zero() {
            0
        } else if self.is_negative {
            -1
        } else {
            1
        }
    }

    fn normalize(&mut self) {
        while let Some(&0) = self.limbs.last() {
            self.limbs.pop();
        }
        if self.limbs.is_empty() {
            self.is_negative = false;
        }
    }

    pub fn from_i64(val: i64) -> Self {
        if val == 0 {
            return Self::zero();
        }
        let is_negative = val < 0;
        let mag = if val == i64::MIN {
            (i64::MAX as u64) + 1
        } else {
            val.unsigned_abs()
        };
        let mut res = Self::from_u64(mag);
        res.is_negative = is_negative;
        res
    }

    pub fn from_u64(val: u64) -> Self {
        let mut limbs = Vec::new();
        let low = val as u32;
        let high = (val >> 32) as u32;
        if low != 0 || high != 0 {
            limbs.push(low);
        }
        if high != 0 {
            limbs.push(high);
        }
        Self {
            is_negative: false,
            limbs,
        }
    }

    pub fn to_i64(&self) -> Option<i64> {
        match self.limbs.len() {
            0 => Some(0),
            1 => {
                let v = self.limbs[0] as i64;
                Some(if self.is_negative { -v } else { v })
            }
            2 => {
                let mag = (self.limbs[0] as u64) | ((self.limbs[1] as u64) << 32);
                if self.is_negative {
                    if mag == (1u64 << 63) {
                        Some(i64::MIN)
                    } else if mag < (1u64 << 63) {
                        Some(-(mag as i64))
                    } else {
                        None
                    }
                } else if mag <= (i64::MAX as u64) {
                    Some(mag as i64)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn to_u64(&self) -> Option<u64> {
        if self.is_negative {
            return None;
        }
        match self.limbs.len() {
            0 => Some(0),
            1 => Some(self.limbs[0] as u64),
            2 => Some((self.limbs[0] as u64) | ((self.limbs[1] as u64) << 32)),
            _ => None,
        }
    }

    pub fn to_f64(&self) -> f64 {
        if self.is_zero() {
            return 0.0;
        }
        let mut acc = 0.0f64;
        let base = 4294967296.0f64; // 2^32
        for &limb in self.limbs.iter().rev() {
            acc = acc * base + (limb as f64);
        }
        if self.is_negative {
            -acc
        } else {
            acc
        }
    }

    pub fn cmp_mag(&self, other: &Self) -> Ordering {
        if self.limbs.len() != other.limbs.len() {
            return self.limbs.len().cmp(&other.limbs.len());
        }
        for (&a, &b) in self.limbs.iter().rev().zip(other.limbs.iter().rev()) {
            if a != b {
                return a.cmp(&b);
            }
        }
        Ordering::Equal
    }

    pub fn abs(&self) -> Self {
        Self {
            is_negative: false,
            limbs: self.limbs.clone(),
        }
    }

    fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let max_len = a.len().max(b.len());
        let mut res = Vec::with_capacity(max_len + 1);
        let mut carry: u64 = 0;
        for i in 0..max_len {
            let ai = a.get(i).copied().unwrap_or(0) as u64;
            let bi = b.get(i).copied().unwrap_or(0) as u64;
            let sum = ai + bi + carry;
            res.push(sum as u32);
            carry = sum >> 32;
        }
        if carry != 0 {
            res.push(carry as u32);
        }
        res
    }

    /// Precondition: a >= b in magnitude
    fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut res = Vec::with_capacity(a.len());
        let mut borrow: i64 = 0;
        for i in 0..a.len() {
            let ai = a[i] as i64;
            let bi = b.get(i).copied().unwrap_or(0) as i64;
            let diff = ai - bi - borrow;
            if diff < 0 {
                res.push((diff + 0x1_0000_0000) as u32);
                borrow = 1;
            } else {
                res.push(diff as u32);
                borrow = 0;
            }
        }
        while let Some(&0) = res.last() {
            res.pop();
        }
        res
    }

    fn mul_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        if a.is_empty() || b.is_empty() {
            return Vec::new();
        }
        let mut res = vec![0u32; a.len() + b.len()];
        for (i, &ai) in a.iter().enumerate() {
            let mut carry: u64 = 0;
            let ai = ai as u64;
            for (j, &bj) in b.iter().enumerate() {
                let cur = (res[i + j] as u64) + ai * (bj as u64) + carry;
                res[i + j] = cur as u32;
                carry = cur >> 32;
            }
            let mut k = i + b.len();
            while carry != 0 {
                let cur = (res[k] as u64) + carry;
                res[k] = cur as u32;
                carry = cur >> 32;
                k += 1;
            }
        }
        while let Some(&0) = res.last() {
            res.pop();
        }
        res
    }

    /// Bit-by-bit division for magnitude.
    fn div_rem_mag(a: &Self, b: &Self) -> (Self, Self) {
        if b.is_zero() {
            panic!("division by zero");
        }
        if a.cmp_mag(b) == Ordering::Less {
            return (Self::zero(), a.clone());
        }
        // Single limb fast path
        if b.limbs.len() == 1 {
            let b0 = b.limbs[0] as u64;
            let mut rem: u64 = 0;
            let mut q_limbs = vec![0u32; a.limbs.len()];
            for (i, &ai) in a.limbs.iter().enumerate().rev() {
                let cur = (rem << 32) | (ai as u64);
                q_limbs[i] = (cur / b0) as u32;
                rem = cur % b0;
            }
            let mut q = Self {
                is_negative: false,
                limbs: q_limbs,
            };
            q.normalize();
            let mut r = Self::from_u64(rem);
            r.normalize();
            return (q, r);
        }

        // Bitwise long division
        let num_bits = a.bit_length();
        let mut rem = Self::zero();
        let mut quot = vec![0u32; a.limbs.len()];

        for bit_idx in (0..num_bits).rev() {
            rem.shl_one_bit();
            if a.get_bit(bit_idx) {
                rem.set_bit_zero();
            }
            if rem.cmp_mag(b) != Ordering::Less {
                rem = rem.sub_mag_self(b);
                let limb_idx = bit_idx / 32;
                let bit_in_limb = bit_idx % 32;
                quot[limb_idx] |= 1 << bit_in_limb;
            }
        }

        let mut q = Self {
            is_negative: false,
            limbs: quot,
        };
        q.normalize();
        rem.normalize();
        (q, rem)
    }

    fn bit_length(&self) -> usize {
        if self.limbs.is_empty() {
            return 0;
        }
        let top = *self.limbs.last().unwrap();
        (self.limbs.len() - 1) * 32 + (32 - top.leading_zeros() as usize)
    }

    fn get_bit(&self, bit: usize) -> bool {
        let limb_idx = bit / 32;
        let bit_in_limb = bit % 32;
        if limb_idx < self.limbs.len() {
            (self.limbs[limb_idx] & (1 << bit_in_limb)) != 0
        } else {
            false
        }
    }

    fn shl_one_bit(&mut self) {
        let mut carry = 0u32;
        for limb in &mut self.limbs {
            let next_carry = *limb >> 31;
            *limb = (*limb << 1) | carry;
            carry = next_carry;
        }
        if carry != 0 {
            self.limbs.push(carry);
        }
    }

    fn set_bit_zero(&mut self) {
        if self.limbs.is_empty() {
            self.limbs.push(1);
        } else {
            self.limbs[0] |= 1;
        }
    }

    fn sub_mag_self(&self, other: &Self) -> Self {
        Self {
            is_negative: false,
            limbs: Self::sub_mag(&self.limbs, &other.limbs),
        }
    }

    pub fn add(&self, other: &Self) -> Self {
        if self.is_zero() {
            return other.clone();
        }
        if other.is_zero() {
            return self.clone();
        }
        if self.is_negative == other.is_negative {
            Self {
                is_negative: self.is_negative,
                limbs: Self::add_mag(&self.limbs, &other.limbs),
            }
        } else {
            match self.cmp_mag(other) {
                Ordering::Equal => Self::zero(),
                Ordering::Greater => {
                    let mut res = Self {
                        is_negative: self.is_negative,
                        limbs: Self::sub_mag(&self.limbs, &other.limbs),
                    };
                    res.normalize();
                    res
                }
                Ordering::Less => {
                    let mut res = Self {
                        is_negative: other.is_negative,
                        limbs: Self::sub_mag(&other.limbs, &self.limbs),
                    };
                    res.normalize();
                    res
                }
            }
        }
    }

    pub fn sub(&self, other: &Self) -> Self {
        self.add(&other.neg())
    }

    pub fn neg(&self) -> Self {
        if self.is_zero() {
            Self::zero()
        } else {
            Self {
                is_negative: !self.is_negative,
                limbs: self.limbs.clone(),
            }
        }
    }

    pub fn mul(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Self::zero();
        }
        let limbs = Self::mul_mag(&self.limbs, &other.limbs);
        Self {
            is_negative: self.is_negative ^ other.is_negative,
            limbs,
        }
    }

    /// Mathematical floored division matching Python / Starlark: `a // b`.
    pub fn floored_div(&self, other: &Self) -> Result<Self, &'static str> {
        if other.is_zero() {
            return Err("integer division by zero");
        }
        let (q_mag, r_mag) = Self::div_rem_mag(self, other);
        let same_sign = self.is_negative == other.is_negative;
        if same_sign {
            Ok(q_mag)
        } else if r_mag.is_zero() {
            Ok(q_mag.neg())
        } else {
            // Negative quotient with non-zero remainder floors down: -q_mag - 1
            Ok(q_mag.neg().sub(&Self::from_u64(1)))
        }
    }

    /// Mathematical floored modulo matching Python / Starlark: `a % b`.
    /// `a % b == a - (a // b) * b`. Sign matches `b` (or 0).
    pub fn floored_mod(&self, other: &Self) -> Result<Self, &'static str> {
        if other.is_zero() {
            return Err("integer modulo by zero");
        }
        let q = self.floored_div(other)?;
        let prod = q.mul(other);
        Ok(self.sub(&prod))
    }

    /// Two's complement bitwise NOT: `~x = -x - 1`.
    pub fn bit_not(&self) -> Self {
        self.neg().sub(&Self::from_u64(1))
    }

    /// Two's complement bitwise AND: `x & y`.
    pub fn bit_and(&self, other: &Self) -> Self {
        match (self.is_negative, other.is_negative) {
            (false, false) => {
                let len = self.limbs.len().min(other.limbs.len());
                let mut res = Vec::with_capacity(len);
                for i in 0..len {
                    res.push(self.limbs[i] & other.limbs[i]);
                }
                let mut r = Self {
                    is_negative: false,
                    limbs: res,
                };
                r.normalize();
                r
            }
            (true, true) => {
                // ~ ( (~x) | (~y) )
                let nx = self.bit_not();
                let ny = other.bit_not();
                nx.bit_or(&ny).bit_not()
            }
            (false, true) => {
                // x & ~ny
                let ny = other.bit_not();
                let mut res = Vec::with_capacity(self.limbs.len());
                for i in 0..self.limbs.len() {
                    let ny_limb = ny.limbs.get(i).copied().unwrap_or(0);
                    res.push(self.limbs[i] & !ny_limb);
                }
                let mut r = Self {
                    is_negative: false,
                    limbs: res,
                };
                r.normalize();
                r
            }
            (true, false) => other.bit_and(self),
        }
    }

    /// Two's complement bitwise OR: `x | y`.
    pub fn bit_or(&self, other: &Self) -> Self {
        match (self.is_negative, other.is_negative) {
            (false, false) => {
                let len = self.limbs.len().max(other.limbs.len());
                let mut res = Vec::with_capacity(len);
                for i in 0..len {
                    let a = self.limbs.get(i).copied().unwrap_or(0);
                    let b = other.limbs.get(i).copied().unwrap_or(0);
                    res.push(a | b);
                }
                let mut r = Self {
                    is_negative: false,
                    limbs: res,
                };
                r.normalize();
                r
            }
            _ => {
                // ~ ( (~x) & (~y) )
                let nx = self.bit_not();
                let ny = other.bit_not();
                nx.bit_and(&ny).bit_not()
            }
        }
    }

    /// Two's complement bitwise XOR: `x ^ y`.
    pub fn bit_xor(&self, other: &Self) -> Self {
        match (self.is_negative, other.is_negative) {
            (false, false) => {
                let len = self.limbs.len().max(other.limbs.len());
                let mut res = Vec::with_capacity(len);
                for i in 0..len {
                    let a = self.limbs.get(i).copied().unwrap_or(0);
                    let b = other.limbs.get(i).copied().unwrap_or(0);
                    res.push(a ^ b);
                }
                let mut r = Self {
                    is_negative: false,
                    limbs: res,
                };
                r.normalize();
                r
            }
            (true, true) => {
                // (~x) ^ (~y)
                let nx = self.bit_not();
                let ny = other.bit_not();
                nx.bit_xor(&ny)
            }
            (true, false) => {
                // ~ ( (~x) ^ y )
                let nx = self.bit_not();
                nx.bit_xor(other).bit_not()
            }
            (false, true) => other.bit_xor(self),
        }
    }

    /// Left shift: `x << shift`.
    pub fn shl(&self, shift: i64) -> Result<Self, String> {
        if shift < 0 {
            return Err(format!("negative shift count: {shift}"));
        }
        if shift >= 512 {
            return Err(format!("shift count too large: {shift}"));
        }
        if self.is_zero() || shift == 0 {
            return Ok(self.clone());
        }
        let shift = shift as usize;
        let limb_shift = shift / 32;
        let bit_shift = shift % 32;

        let mut res = vec![0u32; limb_shift];
        let mut carry = 0u32;
        for &limb in &self.limbs {
            let low = if bit_shift == 0 {
                limb
            } else {
                (limb << bit_shift) | carry
            };
            res.push(low);
            carry = if bit_shift == 0 {
                0
            } else {
                limb >> (32 - bit_shift)
            };
        }
        if carry != 0 {
            res.push(carry);
        }
        let mut r = Self {
            is_negative: self.is_negative,
            limbs: res,
        };
        r.normalize();
        Ok(r)
    }

    /// Right shift: `x >> shift`.
    pub fn shr(&self, shift: i64) -> Result<Self, String> {
        if shift < 0 {
            return Err(format!("negative shift count: {shift}"));
        }
        if self.is_zero() || shift == 0 {
            return Ok(self.clone());
        }
        if self.is_negative {
            // Python right shift for negative integers: ~((~self) >> shift)
            let nx = self.bit_not();
            let shifted = nx.shr(shift)?;
            return Ok(shifted.bit_not());
        }
        let shift = shift as usize;
        let limb_shift = shift / 32;
        let bit_shift = shift % 32;

        if limb_shift >= self.limbs.len() {
            return Ok(Self::zero());
        }

        let mut res = Vec::new();
        let remaining = &self.limbs[limb_shift..];
        let mut carry = 0u32;
        for (i, &limb) in remaining.iter().enumerate().rev() {
            let high = if bit_shift == 0 {
                limb
            } else {
                (limb >> bit_shift) | (carry << (32 - bit_shift))
            };
            res.push(high);
            carry = limb;
            if i == 0 && bit_shift != 0 {
                // Done
            }
        }
        res.reverse();
        let mut r = Self {
            is_negative: false,
            limbs: res,
        };
        r.normalize();
        Ok(r)
    }

    pub fn from_str_radix(s: &str, radix: u32) -> Result<Self, String> {
        if !(2..=36).contains(&radix) {
            return Err(format!("invalid radix: {radix}"));
        }
        let s = s.trim();
        if s.is_empty() {
            return Err("empty string".to_string());
        }
        let (is_negative, s) = if let Some(stripped) = s.strip_prefix('-') {
            (true, stripped)
        } else if let Some(stripped) = s.strip_prefix('+') {
            (false, stripped)
        } else {
            (false, s)
        };
        let s = match radix {
            16 => s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s),
            8 => s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")).unwrap_or(s),
            2 => s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")).unwrap_or(s),
            _ => s,
        };
        if s.is_empty() {
            return Err("invalid integer literal".to_string());
        }

        let mut acc = Self::zero();
        let r = Self::from_u64(radix as u64);
        for c in s.chars() {
            let digit = c
                .to_digit(radix)
                .ok_or_else(|| format!("invalid digit '{c}' for radix {radix}"))?;
            acc = acc.mul(&r).add(&Self::from_u64(digit as u64));
        }
        acc.is_negative = is_negative;
        acc.normalize();
        Ok(acc)
    }

    pub fn from_str_auto(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let (sign, s) = if let Some(rest) = s.strip_prefix('-') {
            ("-", rest)
        } else if let Some(rest) = s.strip_prefix('+') {
            ("", rest)
        } else {
            ("", s)
        };

        if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            let mut val = Self::from_str_radix(rest, 16)?;
            if sign == "-" {
                val = val.neg();
            }
            Ok(val)
        } else if let Some(rest) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
            let mut val = Self::from_str_radix(rest, 8)?;
            if sign == "-" {
                val = val.neg();
            }
            Ok(val)
        } else if let Some(rest) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
            let mut val = Self::from_str_radix(rest, 2)?;
            if sign == "-" {
                val = val.neg();
            }
            Ok(val)
        } else {
            Self::from_str_radix(&format!("{sign}{s}"), 10)
        }
    }

    pub fn to_str_radix(&self, radix: u32) -> String {
        if self.is_zero() {
            return "0".to_string();
        }
        if radix == 10 {
            return self.to_string();
        }
        if radix == 16 {
            return self.format_hex(false);
        }
        // General radix conversion using div_rem_mag
        let mut cur = self.abs();
        let r = Self::from_u64(radix as u64);
        let mut digits = Vec::new();
        let chars = b"0123456789abcdefghijklmnopqrstuvwxyz";
        while !cur.is_zero() {
            let (q, rem) = Self::div_rem_mag(&cur, &r);
            let d = rem.to_u64().unwrap_or(0) as usize;
            digits.push(chars[d] as char);
            cur = q;
        }
        if self.is_negative {
            digits.push('-');
        }
        digits.into_iter().rev().collect()
    }

    fn format_hex(&self, uppercase: bool) -> String {
        if self.is_zero() {
            return "0".to_string();
        }
        let hex_chars = if uppercase {
            b"0123456789ABCDEF"
        } else {
            b"0123456789abcdef"
        };
        let mut s = String::new();
        if self.is_negative {
            s.push('-');
        }
        let mut leading = true;
        for &limb in self.limbs.iter().rev() {
            for shift in (0..8).rev() {
                let nibble = ((limb >> (shift * 4)) & 0xF) as usize;
                if leading && nibble == 0 {
                    continue;
                }
                leading = false;
                s.push(hex_chars[nibble] as char);
            }
        }
        if s.is_empty() || s == "-" {
            "0".to_string()
        } else {
            s
        }
    }

    fn format_octal(&self) -> String {
        self.to_str_radix(8)
    }

    fn format_binary(&self) -> String {
        self.to_str_radix(2)
    }
}

impl Default for BigInt {
    fn default() -> Self {
        Self::zero()
    }
}

impl PartialEq for BigInt {
    fn eq(&self, other: &Self) -> bool {
        self.is_negative == other.is_negative && self.limbs == other.limbs
    }
}

impl Hash for BigInt {
    fn hash<H: Hasher>(&self, state: &mut H) {
        if self.is_zero() {
            0i64.hash(state);
        } else if let Some(i) = self.to_i64() {
            i.hash(state);
        } else {
            self.is_negative.hash(state);
            self.limbs.hash(state);
        }
    }
}

impl PartialOrd for BigInt {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BigInt {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.is_negative != other.is_negative {
            if self.is_negative {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        } else if self.is_negative {
            other.cmp_mag(self)
        } else {
            self.cmp_mag(other)
        }
    }
}

impl fmt::Debug for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl fmt::Display for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_zero() {
            return f.write_str("0");
        }
        let mut cur = self.abs();
        let base = Self::from_u64(1_000_000_000);
        let mut chunks = Vec::new();
        while !cur.is_zero() {
            let (q, rem) = Self::div_rem_mag(&cur, &base);
            chunks.push(rem.to_u64().unwrap_or(0));
            cur = q;
        }
        if self.is_negative {
            f.write_str("-")?;
        }
        let first = chunks.pop().unwrap();
        write!(f, "{first}")?;
        for chunk in chunks.into_iter().rev() {
            write!(f, "{chunk:09}")?;
        }
        Ok(())
    }
}

impl fmt::LowerHex for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format_hex(false))
    }
}

impl fmt::UpperHex for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format_hex(true))
    }
}

impl fmt::Octal for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format_octal())
    }
}

impl fmt::Binary for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format_binary())
    }
}

impl From<i64> for BigInt {
    fn from(v: i64) -> Self {
        Self::from_i64(v)
    }
}

impl From<u64> for BigInt {
    fn from(v: u64) -> Self {
        Self::from_u64(v)
    }
}

impl From<i32> for BigInt {
    fn from(v: i32) -> Self {
        Self::from_i64(v as i64)
    }
}

impl From<u32> for BigInt {
    fn from(v: u32) -> Self {
        Self::from_u64(v as u64)
    }
}

impl From<usize> for BigInt {
    fn from(v: usize) -> Self {
        Self::from_u64(v as u64)
    }
}

impl PartialEq<i64> for BigInt {
    fn eq(&self, other: &i64) -> bool {
        self.to_i64() == Some(*other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bigint_basic_arithmetic() {
        let a = BigInt::from_i64(100);
        let b = BigInt::from_i64(7);
        assert_eq!(a.add(&b), BigInt::from_i64(107));
        assert_eq!(a.sub(&b), BigInt::from_i64(93));
        assert_eq!(a.mul(&b), BigInt::from_i64(700));
        assert_eq!(a.floored_div(&b).unwrap(), BigInt::from_i64(14));
        assert_eq!(a.floored_mod(&b).unwrap(), BigInt::from_i64(2));

        // Negative floored div/mod
        let a = BigInt::from_i64(100);
        let b = BigInt::from_i64(-7);
        assert_eq!(a.floored_div(&b).unwrap(), BigInt::from_i64(-15));
        assert_eq!(a.floored_mod(&b).unwrap(), BigInt::from_i64(-5));

        let a = BigInt::from_i64(-100);
        let b = BigInt::from_i64(7);
        assert_eq!(a.floored_div(&b).unwrap(), BigInt::from_i64(-15));
        assert_eq!(a.floored_mod(&b).unwrap(), BigInt::from_i64(5));

        let a = BigInt::from_i64(-100);
        let b = BigInt::from_i64(-7);
        assert_eq!(a.floored_div(&b).unwrap(), BigInt::from_i64(14));
        assert_eq!(a.floored_mod(&b).unwrap(), BigInt::from_i64(-2));
    }

    #[test]
    fn test_bigint_powers_of_two() {
        let one = BigInt::one();
        let big = one.shl(128).unwrap();
        let big_minus_one = one.shl(127).unwrap();
        assert_eq!(big.floored_div(&big_minus_one).unwrap(), BigInt::from_i64(2));
        assert_eq!(format!("{big}"), "340282366920938463463374607431768211456");
        assert_eq!(format!("{:x}", big), "100000000000000000000000000000000");
    }

    #[test]
    fn test_bigint_bitwise() {
        let a = BigInt::from_i64(8);
        let b = BigInt::from_i64(3);
        assert_eq!(a.bit_or(&b), BigInt::from_i64(11));
        assert_eq!(a.bit_and(&b), BigInt::from_i64(0));
        assert_eq!(a.bit_xor(&b), BigInt::from_i64(11));

        let maxint = BigInt::from_i64(0x7fffffff);
        let minint = BigInt::from_i64(-0x80000000);
        let neg_one = BigInt::from_i64(-1);

        assert_eq!(minint.bit_and(&neg_one), minint);
        assert_eq!(maxint.bit_and(&neg_one), maxint);
        assert_eq!(minint.bit_or(&neg_one), neg_one);
        assert_eq!(maxint.bit_or(&neg_one), neg_one);
        assert_eq!(minint.bit_xor(&neg_one), maxint);
        assert_eq!(maxint.bit_xor(&neg_one), minint);
        assert_eq!(minint.bit_not(), maxint);
        assert_eq!(maxint.bit_not(), minint);

        // Shifts
        assert_eq!(BigInt::from_i64(2).shr(1).unwrap(), BigInt::from_i64(1));
        assert_eq!(minint.shr(9999).unwrap(), BigInt::from_i64(-1));
        assert_eq!(maxint.shr(9999).unwrap(), BigInt::from_i64(0));
        assert_eq!(minint.shr(31).unwrap(), BigInt::from_i64(-1));
        assert_eq!(minint.shr(30).unwrap(), BigInt::from_i64(-2));
        assert_eq!(BigInt::from_i64(-10).shr(1000).unwrap(), BigInt::from_i64(-1));
    }

    #[test]
    fn test_bigint_string_roundtrip() {
        let s = "3539537889086624823140625";
        let val = BigInt::from_str_radix(s, 10).unwrap();
        assert_eq!(val.to_string(), s);

        let hex_s = "0x100000000";
        let hex_val = BigInt::from_str_auto(hex_s).unwrap();
        assert_eq!(hex_val, BigInt::from_i64(1).shl(32).unwrap());
    }
}
