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

//! Core Bazel utilities (`ExitCode`, `DetailedExitCode`, `Os`, `Clock`, `Md5`, `Sha256`).

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zel_proto::failure_details::FailureDetail;

/// The Bazel release whose behavior Zel emulates (the spec is the Bazel 9.3.2 source tree). Everything
/// that reports or checks "the Bazel version" (`version`, `info release`, `native.bazel_version`,
/// `bazel_compatibility`, BEP `build_tool_version`, ...) derives from this one constant. The single
/// documented deviation is the product name (`zel`); an explicit `ZEL_EMBED_LABEL`/`BAZEL_EMBED_LABEL`
/// replaces the label exactly like Bazel's `--embed_label`-style build stamping.
pub const BAZEL_COMPAT_VERSION: &str = "9.3.2";
/// `BlazeVersionInfo.getReleaseName()` of [`BAZEL_COMPAT_VERSION`].
pub const BAZEL_COMPAT_RELEASE_NAME: &str = "release 9.3.2";

pub mod action_env;
pub mod classfile;
pub mod deflate;
pub mod file_type;
pub mod ijar;
pub mod jar;
pub mod java_regex;
pub mod json;
pub mod param_escaper;
pub mod resource_converter;
pub mod singlejar;
pub mod terminal;
pub mod zip;
pub mod zstd;

/// Exit codes shared by Bazel client and server, matching `ExitCode.java`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExitCode {
    pub code: i32,
    pub name: &'static str,
    pub infrastructure_failure: bool,
}

impl ExitCode {
    pub const SUCCESS: Self = Self::new(0, "SUCCESS", false);
    pub const BUILD_FAILURE: Self = Self::new(1, "BUILD_FAILURE", false);
    pub const PARSING_FAILURE: Self = Self::new(1, "PARSING_FAILURE", false);
    pub const COMMAND_LINE_ERROR: Self = Self::new(2, "COMMAND_LINE_ERROR", false);
    pub const TESTS_FAILED: Self = Self::new(3, "TESTS_FAILED", false);
    pub const PARTIAL_ANALYSIS_FAILURE: Self = Self::new(3, "PARTIAL_ANALYSIS_FAILURE", false);
    pub const NO_TESTS_FOUND: Self = Self::new(4, "NO_TESTS_FOUND", false);
    pub const RUN_FAILURE: Self = Self::new(6, "RUN_FAILURE", false);
    pub const ANALYSIS_FAILURE: Self = Self::new(7, "ANALYSIS_FAILURE", false);
    pub const INTERRUPTED: Self = Self::new(8, "INTERRUPTED", false);
    pub const LOCK_HELD_NOBLOCK_FOR_LOCK: Self = Self::new(9, "LOCK_HELD_NOBLOCK_FOR_LOCK", false);
    pub const REMOTE_ENVIRONMENTAL_ERROR: Self = Self::new(32, "REMOTE_ENVIRONMENTAL_ERROR", true);
    pub const OOM_ERROR: Self = Self::new(33, "OOM_ERROR", true);
    pub const REMOTE_ERROR: Self = Self::new(34, "REMOTE_ERROR", true);
    pub const LOCAL_ENVIRONMENTAL_ERROR: Self = Self::new(36, "LOCAL_ENVIRONMENTAL_ERROR", true);
    pub const BLAZE_INTERNAL_ERROR: Self = Self::new(37, "BLAZE_INTERNAL_ERROR", true);
    pub const PUBLISH_ERROR: Self = Self::new(38, "PUBLISH_ERROR", true);
    pub const REMOTE_CACHE_EVICTED: Self = Self::new(39, "REMOTE_CACHE_EVICTED", true);
    /// `TRANSIENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR` in `ExitCode.java` (name `PUBLISH_ERROR`).
    pub const TRANSIENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR: Self = Self::PUBLISH_ERROR;
    pub const PERSISTENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR: Self =
        Self::new(45, "PERSISTENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR", false);
    pub const EXTERNAL_DEPS_ERROR: Self = Self::new(48, "EXTERNAL_DEPS_ERROR", false);

    pub const fn new(code: i32, name: &'static str, infrastructure_failure: bool) -> Self {
        Self {
            code,
            name,
            infrastructure_failure,
        }
    }

    pub const fn numeric_exit_code(&self) -> i32 {
        self.code
    }

    pub const fn is_infrastructure_failure(&self) -> bool {
        self.infrastructure_failure
    }

    /// Every *registered* exit code (`ExitCode.values()`); excludes `PARSING_FAILURE` and
    /// `PARTIAL_ANALYSIS_FAILURE`, which duplicate numeric codes and are unregistered in Java.
    pub fn values() -> &'static [ExitCode] {
        &[
            Self::SUCCESS,
            Self::BUILD_FAILURE,
            Self::COMMAND_LINE_ERROR,
            Self::TESTS_FAILED,
            Self::NO_TESTS_FOUND,
            Self::RUN_FAILURE,
            Self::ANALYSIS_FAILURE,
            Self::INTERRUPTED,
            Self::LOCK_HELD_NOBLOCK_FOR_LOCK,
            Self::REMOTE_ENVIRONMENTAL_ERROR,
            Self::OOM_ERROR,
            Self::REMOTE_ERROR,
            Self::LOCAL_ENVIRONMENTAL_ERROR,
            Self::BLAZE_INTERNAL_ERROR,
            Self::TRANSIENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR,
            Self::REMOTE_CACHE_EVICTED,
            Self::PERSISTENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR,
            Self::EXTERNAL_DEPS_ERROR,
        ]
    }

    /// `ExitCode.forCode(int)`: only registered codes are returned.
    pub fn for_code(code: i32) -> Option<ExitCode> {
        Self::values().iter().copied().find(|c| c.code == code)
    }

    /// Maps a numeric code to the best-known `ExitCode`, falling back to an anonymous one.
    pub fn from_numeric(code: i32) -> ExitCode {
        Self::for_code(code).unwrap_or(ExitCode::new(code, "UNKNOWN", false))
    }
}

impl fmt::Display for ExitCode {
    /// Java's `ExitCode.toString()` is the bare name.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// Combines an [`ExitCode`] with an optional [`FailureDetail`] proto message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailedExitCode {
    pub exit_code: ExitCode,
    pub failure_detail: Option<FailureDetail>,
}

impl DetailedExitCode {
    pub const fn success() -> Self {
        Self {
            exit_code: ExitCode::SUCCESS,
            failure_detail: None,
        }
    }

    pub fn of(exit_code: ExitCode, failure_detail: FailureDetail) -> Self {
        Self {
            exit_code,
            failure_detail: Some(failure_detail),
        }
    }

    pub fn from_exit_code(exit_code: ExitCode) -> Self {
        Self {
            exit_code,
            failure_detail: None,
        }
    }

    pub fn is_success(&self) -> bool {
        self.exit_code == ExitCode::SUCCESS
    }

    /// `DetailedExitCode.of(FailureDetail)`: derives the exit code from the detail's metadata.
    /// Returns `None` if the detail has no category/known code.
    pub fn of_detail(failure_detail: FailureDetail) -> Option<Self> {
        let n = failure_detail.exit_code()?;
        Some(Self {
            exit_code: ExitCode::from_numeric(n),
            failure_detail: Some(failure_detail),
        })
    }

    /// Priority per `DetailedExitCodeComparator.getPriority`: infrastructure (4) > ordinary (2) >
    /// null (0), +1 when a failure detail is attached.
    pub fn priority(code: Option<&DetailedExitCode>) -> i32 {
        match code {
            None => 0,
            Some(c) => {
                (if c.exit_code.infrastructure_failure {
                    4
                } else {
                    2
                }) + i32::from(c.failure_detail.is_some())
            }
        }
    }

    /// `chooseMoreImportantWithFirstIfTie`.
    pub fn choose_more_important(
        first: Option<DetailedExitCode>,
        second: Option<DetailedExitCode>,
    ) -> Option<DetailedExitCode> {
        if Self::priority(first.as_ref()) >= Self::priority(second.as_ref()) {
            first
        } else {
            second
        }
    }

    /// Serialized `FailureDetail` proto for `--failure_detail_out` / `failure_detail.rawproto`.
    pub fn failure_detail_rawproto(&self) -> Option<Vec<u8>> {
        self.failure_detail
            .as_ref()
            .map(FailureDetail::encode_to_vec)
    }
}

/// Host operating system enumeration matching `com.google.devtools.build.lib.util.OS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    Darwin,
    Linux,
    Windows,
    FreeBsd,
    OpenBsd,
    Unknown,
}

impl Os {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Darwin
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "freebsd") {
            Self::FreeBsd
        } else if cfg!(target_os = "openbsd") {
            Self::OpenBsd
        } else {
            Self::Unknown
        }
    }

    pub const fn canonical_name(&self) -> &'static str {
        match self {
            Self::Darwin => "osx",
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::FreeBsd => "freebsd",
            Self::OpenBsd => "openbsd",
            Self::Unknown => "unknown",
        }
    }
}

/// Abstract clock interface matching `com.google.devtools.build.lib.clock.Clock`.
pub trait Clock: Send + Sync {
    fn current_time_millis(&self) -> u64;
    fn nano_time(&self) -> u128;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct JavaClock;

impl Clock for JavaClock {
    fn current_time_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis() as u64
    }

    fn nano_time(&self) -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos()
    }
}

/// RFC 1321 MD5 implementation (used by Bazel to compute `$OUTPUT_BASE` directory names
/// from the workspace directory path and for `Fingerprint`).
#[allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]
pub mod md5 {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];

    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    pub fn digest(data: &[u8]) -> [u8; 16] {
        let mut a0: u32 = 0x67452301;
        let mut b0: u32 = 0xefcdab89;
        let mut c0: u32 = 0x98badcfe;
        let mut d0: u32 = 0x10325476;

        let bit_len = (data.len() as u64).wrapping_mul(8);
        let mut msg = data.to_vec();
        msg.push(0x80);
        while (msg.len() % 64) != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_le_bytes());

        for chunk in msg.chunks_exact(64) {
            let mut m = [0u32; 16];
            for (i, word) in chunk.chunks_exact(4).enumerate() {
                m[i] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            }

            let mut a = a0;
            let mut b = b0;
            let mut c = c0;
            let mut d = d0;

            for i in 0..64 {
                let (f, g) = match i {
                    0..=15 => ((b & c) | ((!b) & d), i),
                    16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                    32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                    _ => (c ^ (b | (!d)), (7 * i) % 16),
                };
                let temp = d;
                d = c;
                c = b;
                b = b.wrapping_add(
                    a.wrapping_add(f)
                        .wrapping_add(K[i])
                        .wrapping_add(m[g])
                        .rotate_left(S[i]),
                );
                a = temp;
            }

            a0 = a0.wrapping_add(a);
            b0 = b0.wrapping_add(b);
            c0 = c0.wrapping_add(c);
            d0 = d0.wrapping_add(d);
        }

        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&a0.to_le_bytes());
        out[4..8].copy_from_slice(&b0.to_le_bytes());
        out[8..12].copy_from_slice(&c0.to_le_bytes());
        out[12..16].copy_from_slice(&d0.to_le_bytes());
        out
    }

    pub fn hex_digest(data: &[u8]) -> String {
        super::to_hex(&digest(data))
    }
}

/// FIPS 180-4 SHA-1 implementation for `DigestHashFunction.SHA1`.
#[allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]
pub mod sha1 {
    pub fn digest(data: &[u8]) -> [u8; 20] {
        let mut h0: u32 = 0x67452301;
        let mut h1: u32 = 0xefcdab89;
        let mut h2: u32 = 0x98badcfe;
        let mut h3: u32 = 0x10325476;
        let mut h4: u32 = 0xc3d2e1f0;

        let bit_len = (data.len() as u64).wrapping_mul(8);
        let mut msg = data.to_vec();
        msg.push(0x80);
        while (msg.len() % 64) != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());

        for chunk in msg.chunks_exact(64) {
            let mut w = [0u32; 80];
            for (i, word) in chunk.chunks_exact(4).enumerate() {
                w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
            }
            for i in 16..80 {
                w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
            }

            let mut a = h0;
            let mut b = h1;
            let mut c = h2;
            let mut d = h3;
            let mut e = h4;

            for (i, &wi) in w.iter().enumerate() {
                let (f, k) = match i {
                    0..=19 => ((b & c) | ((!b) & d), 0x5a827999u32),
                    20..=39 => (b ^ c ^ d, 0x6ed9eba1u32),
                    40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdcu32),
                    _ => (b ^ c ^ d, 0xca62c1d6u32),
                };
                let temp = a
                    .rotate_left(5)
                    .wrapping_add(f)
                    .wrapping_add(e)
                    .wrapping_add(k)
                    .wrapping_add(wi);
                e = d;
                d = c;
                c = b.rotate_left(30);
                b = a;
                a = temp;
            }

            h0 = h0.wrapping_add(a);
            h1 = h1.wrapping_add(b);
            h2 = h2.wrapping_add(c);
            h3 = h3.wrapping_add(d);
            h4 = h4.wrapping_add(e);
        }

        let mut out = [0u8; 20];
        out[0..4].copy_from_slice(&h0.to_be_bytes());
        out[4..8].copy_from_slice(&h1.to_be_bytes());
        out[8..12].copy_from_slice(&h2.to_be_bytes());
        out[12..16].copy_from_slice(&h3.to_be_bytes());
        out[16..20].copy_from_slice(&h4.to_be_bytes());
        out
    }

    pub fn hex_digest(data: &[u8]) -> String {
        super::to_hex(&digest(data))
    }
}

/// FIPS 180-4 SHA-256 implementation for content digests and action caching.
#[allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]
pub mod sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    const INIT: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    fn compress_block(h: &mut [u32; 8], chunk: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    /// Streaming SHA-256 hasher for large artifacts and logs without buffering whole files.
    #[derive(Clone)]
    pub struct Hasher {
        state: [u32; 8],
        buf: [u8; 64],
        buf_len: usize,
        total_len: u64,
    }

    impl Default for Hasher {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Hasher {
        pub fn new() -> Self {
            Self {
                state: INIT,
                buf: [0u8; 64],
                buf_len: 0,
                total_len: 0,
            }
        }

        pub fn update(&mut self, mut data: &[u8]) {
            self.total_len = self.total_len.wrapping_add(data.len() as u64);
            if self.buf_len > 0 {
                let take = (64 - self.buf_len).min(data.len());
                self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
                self.buf_len += take;
                data = &data[take..];
                if self.buf_len == 64 {
                    compress_block(&mut self.state, &self.buf);
                    self.buf_len = 0;
                }
            }
            while data.len() >= 64 {
                let block: &[u8; 64] = data[..64].try_into().unwrap();
                compress_block(&mut self.state, block);
                data = &data[64..];
            }
            if !data.is_empty() {
                self.buf[..data.len()].copy_from_slice(data);
                self.buf_len = data.len();
            }
        }

        pub fn finish(mut self) -> [u8; 32] {
            let bit_len = self.total_len.wrapping_mul(8);
            self.buf[self.buf_len] = 0x80;
            self.buf_len += 1;
            if self.buf_len > 56 {
                self.buf[self.buf_len..64].fill(0);
                compress_block(&mut self.state, &self.buf);
                self.buf_len = 0;
            }
            self.buf[self.buf_len..56].fill(0);
            self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
            compress_block(&mut self.state, &self.buf);

            let mut out = [0u8; 32];
            for (i, word) in self.state.iter().enumerate() {
                out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
            }
            out
        }

        pub fn finish_hex(self) -> String {
            super::to_hex(&self.finish())
        }
    }

    pub fn digest(data: &[u8]) -> [u8; 32] {
        let mut hasher = Hasher::new();
        hasher.update(data);
        hasher.finish()
    }

    pub fn hex_digest(data: &[u8]) -> String {
        super::to_hex(&digest(data))
    }

    /// Streams a regular file on disk in 64 KiB chunks, returning `(byte_length, sha256_hex)`.
    /// Rejects directories and non-regular files.
    pub fn hash_file(path: &std::path::Path) -> std::io::Result<(u64, String)> {
        use std::io::Read;
        let meta = std::fs::metadata(path)?;
        if !meta.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        let mut f = std::fs::File::open(path)?;
        let mut hasher = Hasher::new();
        let mut buf = [0u8; 65536];
        let mut total = 0u64;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        Ok((total, hasher.finish_hex()))
    }
}

pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

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

pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = ((c[0] as u32) << 16)
            | ((*c.get(1).unwrap_or(&0) as u32) << 8)
            | (*c.get(2).unwrap_or(&0) as u32);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

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

/// Generates a standard RFC 4122 version-4 UUID string (`xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`).
pub fn uuid_v4() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let mut seed = Vec::with_capacity(32);
    seed.extend_from_slice(&nanos.to_le_bytes());
    seed.extend_from_slice(&pid.to_le_bytes());
    seed.extend_from_slice(&seq.to_le_bytes());
    let mut b = sha256::digest(&seed);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{}-{}-{}-{}-{}",
        to_hex(&b[0..4]),
        to_hex(&b[4..6]),
        to_hex(&b[6..8]),
        to_hex(&b[8..10]),
        to_hex(&b[10..16]),
    )
}

/// FIPS 180-4 SHA-384 and SHA-512 implementations.
#[allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]
pub mod sha512 {
    const K512: [u64; 80] = [
        0x428a2f98d728ae22,
        0x7137449123ef65cd,
        0xb5c0fbcfec4d3b2f,
        0xe9b5dba58189dbbc,
        0x3956c25bf348b538,
        0x59f111f1b605d019,
        0x923f82a4af194f9b,
        0xab1c5ed5da6d8118,
        0xd807aa98a3030242,
        0x12835b0145706fbe,
        0x243185be4ee4b28c,
        0x550c7dc3d5ffb4e2,
        0x72be5d74f27b896f,
        0x80deb1fe3b1696b1,
        0x9bdc06a725c71235,
        0xc19bf174cf692694,
        0xe49b69c19ef14ad2,
        0xefbe4786384f25e3,
        0x0fc19dc68b8cd5b5,
        0x240ca1cc77ac9c65,
        0x2de92c6f592b0275,
        0x4a7484aa6ea6e483,
        0x5cb0a9dcbd41fbd4,
        0x76f988da831153b5,
        0x983e5152ee66dfab,
        0xa831c66d2db43210,
        0xb00327c898fb213f,
        0xbf597fc7beef0ee4,
        0xc6e00bf33da88fc2,
        0xd5a79147930aa725,
        0x06ca6351e003826f,
        0x142929670a0e6e70,
        0x27b70a8546d22ffc,
        0x2e1b21385c26c926,
        0x4d2c6dfc5ac42aed,
        0x53380d139d95b3df,
        0x650a73548baf63de,
        0x766a0abb3c77b2a8,
        0x81c2c92e47edaee6,
        0x92722c851482353b,
        0xa2bfe8a14cf10364,
        0xa81a664bbc423001,
        0xc24b8b70d0f89791,
        0xc76c51a30654be30,
        0xd192e819d6ef5218,
        0xd69906245565a910,
        0xf40e35855771202a,
        0x106aa07032bbd1b8,
        0x19a4c116b8d2d0c8,
        0x1e376c085141ab53,
        0x2748774cdf8eeb99,
        0x34b0bcb5e19b48a8,
        0x391c0cb3c5c95a63,
        0x4ed8aa4ae3418acb,
        0x5b9cca4f7763e373,
        0x682e6ff3d6b2b8a3,
        0x748f82ee5defb2fc,
        0x78a5636f43172f60,
        0x84c87814a1f0ab72,
        0x8cc702081a6439ec,
        0x90befffa23631e28,
        0xa4506cebde82bde9,
        0xbef9a3f7b2c67915,
        0xc67178f2e372532b,
        0xca273eceea26619c,
        0xd186b8c721c0c207,
        0xeada7dd6cde0eb1e,
        0xf57d4f7fee6ed178,
        0x06f067aa72176fba,
        0x0a637dc5a2c898a6,
        0x113f9804bef90dae,
        0x1b710b35131c471b,
        0x28db77f523047d84,
        0x32caab7b40c72493,
        0x3c9ebe0a15c9bebc,
        0x431d67c49c100d4c,
        0x4cc5d4becb3e42b6,
        0x597f299cfc657e2a,
        0x5fcb6fab3ad6faec,
        0x6c44198c4a475817,
    ];

    fn core(data: &[u8], mut h: [u64; 8]) -> [u64; 8] {
        let bitlen = (data.len() as u128).wrapping_mul(8);
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 128 != 112 {
            msg.push(0);
        }
        msg.extend_from_slice(&bitlen.to_be_bytes());
        for chunk in msg.chunks_exact(128) {
            let mut w = [0u64; 80];
            for i in 0..16 {
                w[i] = u64::from_be_bytes(chunk[i * 8..i * 8 + 8].try_into().unwrap());
            }
            for i in 16..80 {
                let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
                let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let mut v = h;
            for i in 0..80 {
                let s1 = v[4].rotate_right(14) ^ v[4].rotate_right(18) ^ v[4].rotate_right(41);
                let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
                let t1 = v[7]
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K512[i])
                    .wrapping_add(w[i]);
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

    pub fn digest(data: &[u8]) -> [u8; 64] {
        let h = core(
            data,
            [
                0x6a09e667f3bcc908,
                0xbb67ae8584caa73b,
                0x3c6ef372fe94f82b,
                0xa54ff53a5f1d36f1,
                0x510e527fade682d1,
                0x9b05688c2b3e6c1f,
                0x1f83d9abfb41bd6b,
                0x5be0cd19137e2179,
            ],
        );
        let mut out = [0u8; 64];
        for (i, x) in h.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&x.to_be_bytes());
        }
        out
    }

    pub fn digest384(data: &[u8]) -> [u8; 48] {
        let h = core(
            data,
            [
                0xcbbb9d5dc1059ed8,
                0x629a292a367cd507,
                0x9159015a3070dd17,
                0x152fecd8f70e5939,
                0x67332667ffc00b31,
                0x8eb44a8768581511,
                0xdb0c2e0d64f98fa7,
                0x47b5481dbefa4fa4,
            ],
        );
        let mut out = [0u8; 48];
        for (i, x) in h.iter().take(6).enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&x.to_be_bytes());
        }
        out
    }

    pub fn hex_digest(data: &[u8]) -> String {
        super::to_hex(&digest(data))
    }

    pub fn hex_digest384(data: &[u8]) -> String {
        super::to_hex(&digest384(data))
    }
}

/// Pure-Rust BLAKE3 implementation (256-bit default output).
#[allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]
pub mod blake3 {
    const IV: [u32; 8] = [
        0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB,
        0x5BE0CD19,
    ];
    const MSG_PERMUTATION: [usize; 16] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];
    const CHUNK_START: u32 = 1 << 0;
    const CHUNK_END: u32 = 1 << 1;
    const PARENT: u32 = 1 << 2;
    const ROOT: u32 = 1 << 3;

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

    fn round(s: &mut [u32; 16], m: &[u32; 16]) {
        g(s, 0, 4, 8, 12, m[0], m[1]);
        g(s, 1, 5, 9, 13, m[2], m[3]);
        g(s, 2, 6, 10, 14, m[4], m[5]);
        g(s, 3, 7, 11, 15, m[6], m[7]);
        g(s, 0, 5, 10, 15, m[8], m[9]);
        g(s, 1, 6, 11, 12, m[10], m[11]);
        g(s, 2, 7, 8, 13, m[12], m[13]);
        g(s, 3, 4, 9, 14, m[14], m[15]);
    }

    fn compress(
        cv: &[u32; 8],
        block: &[u8; 64],
        block_len: u32,
        counter: u64,
        flags: u32,
    ) -> [u32; 16] {
        let mut m = [0u32; 16];
        for i in 0..16 {
            m[i] = u32::from_le_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        let mut s = [
            cv[0],
            cv[1],
            cv[2],
            cv[3],
            cv[4],
            cv[5],
            cv[6],
            cv[7],
            IV[0],
            IV[1],
            IV[2],
            IV[3],
            counter as u32,
            (counter >> 32) as u32,
            block_len,
            flags,
        ];
        for r in 0..7 {
            round(&mut s, &m);
            if r < 6 {
                let mut next = [0u32; 16];
                for i in 0..16 {
                    next[i] = m[MSG_PERMUTATION[i]];
                }
                m = next;
            }
        }
        for i in 0..8 {
            s[i] ^= s[i + 8];
            s[i + 8] ^= cv[i];
        }
        s
    }

    fn first8(out: &[u32; 16]) -> [u32; 8] {
        out[..8].try_into().unwrap()
    }

    fn parent_block(left: &[u32; 8], right: &[u32; 8]) -> [u8; 64] {
        let mut b = [0u8; 64];
        for (i, &w) in left.iter().chain(right.iter()).enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        b
    }

    struct ChunkOutput {
        cv: [u32; 8],
        block: [u8; 64],
        block_len: u32,
        counter: u64,
        flags: u32,
    }

    impl ChunkOutput {
        fn chaining_value(&self) -> [u32; 8] {
            first8(&compress(
                &self.cv,
                &self.block,
                self.block_len,
                self.counter,
                self.flags,
            ))
        }

        fn root_bytes(&self) -> [u8; 32] {
            let words = compress(&self.cv, &self.block, self.block_len, 0, self.flags | ROOT);
            let mut out = [0u8; 32];
            for i in 0..8 {
                out[i * 4..i * 4 + 4].copy_from_slice(&words[i].to_le_bytes());
            }
            out
        }
    }

    fn chunk_output(chunk: &[u8], counter: u64) -> ChunkOutput {
        let mut cv = IV;
        if chunk.is_empty() {
            return ChunkOutput {
                cv,
                block: [0u8; 64],
                block_len: 0,
                counter,
                flags: CHUNK_START | CHUNK_END,
            };
        }
        let num_blocks = chunk.len().div_ceil(64);
        for (bi, blk) in chunk.chunks(64).enumerate() {
            let mut block = [0u8; 64];
            block[..blk.len()].copy_from_slice(blk);
            let mut flags = 0u32;
            if bi == 0 {
                flags |= CHUNK_START;
            }
            if bi + 1 == num_blocks {
                return ChunkOutput {
                    cv,
                    block,
                    block_len: blk.len() as u32,
                    counter,
                    flags: flags | CHUNK_END,
                };
            }
            cv = first8(&compress(&cv, &block, 64, counter, flags));
        }
        unreachable!()
    }

    pub fn digest(data: &[u8]) -> [u8; 32] {
        if data.len() <= 1024 {
            return chunk_output(data, 0).root_bytes();
        }
        let mut cv_stack: Vec<[u32; 8]> = Vec::new();
        let chunks: Vec<&[u8]> = data.chunks(1024).collect();
        for (idx, chunk) in chunks.iter().enumerate() {
            let is_last = idx + 1 == chunks.len();
            let co = chunk_output(chunk, idx as u64);
            if !is_last {
                let mut new_cv = co.chaining_value();
                let mut total_chunks = (idx + 1) as u64;
                while total_chunks & 1 == 0 {
                    let left = cv_stack.pop().unwrap();
                    let pb = parent_block(&left, &new_cv);
                    new_cv = first8(&compress(&IV, &pb, 64, 0, PARENT));
                    total_chunks >>= 1;
                }
                cv_stack.push(new_cv);
            } else {
                let mut cur = co;
                while let Some(left) = cv_stack.pop() {
                    let right = cur.chaining_value();
                    cur = ChunkOutput {
                        cv: IV,
                        block: parent_block(&left, &right),
                        block_len: 64,
                        counter: 0,
                        flags: PARENT,
                    };
                }
                return cur.root_bytes();
            }
        }
        unreachable!()
    }

    pub fn hex_digest(data: &[u8]) -> String {
        super::to_hex(&digest(data))
    }
}

/// Incremental fingerprint builder matching `com.google.devtools.build.lib.util.Fingerprint`.
#[derive(Debug, Clone, Default)]
pub struct Fingerprint {
    buffer: Vec<u8>,
}

impl Fingerprint {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    pub fn add_bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.buffer
            .extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        self.buffer.extend_from_slice(bytes);
        self
    }

    pub fn add_str(&mut self, s: &str) -> &mut Self {
        self.add_bytes(s.as_bytes())
    }

    pub fn add_bool(&mut self, b: bool) -> &mut Self {
        self.buffer.push(u8::from(b));
        self
    }

    pub fn add_i32(&mut self, v: i32) -> &mut Self {
        self.buffer.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn add_u64(&mut self, v: u64) -> &mut Self {
        self.buffer.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn add_strings<I, S>(&mut self, iter: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let items: Vec<S> = iter.into_iter().collect();
        self.add_i32(items.len() as i32);
        for item in items {
            self.add_str(item.as_ref());
        }
        self
    }

    pub fn digest_and_reset(&mut self) -> [u8; 32] {
        let d = sha256::digest(&self.buffer);
        self.buffer.clear();
        d
    }

    pub fn hex_digest_and_reset(&mut self) -> String {
        to_hex(&self.digest_and_reset())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md5_test_vectors() {
        assert_eq!(md5::hex_digest(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5::hex_digest(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5::hex_digest(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn test_sha256_test_vectors() {
        assert_eq!(
            sha256::hex_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256::hex_digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn test_sha1_test_vectors() {
        assert_eq!(
            sha1::hex_digest(b""),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            sha1::hex_digest(b"abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            sha1::hex_digest(b"The quick brown fox jumps over the lazy dog"),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
    }

    #[test]
    fn test_sha256_streaming_and_hash_file() {
        for len in [0usize, 1, 55, 56, 57, 63, 64, 65, 130_000] {
            let data: Vec<u8> = (0..len)
                .map(|i| (i.wrapping_mul(31) & 0xff) as u8)
                .collect();
            let mut h = sha256::Hasher::new();
            for chunk in data.chunks(17) {
                h.update(chunk);
            }
            assert_eq!(h.finish_hex(), sha256::hex_digest(&data), "len={len}");
        }
        let dir = std::env::temp_dir().join(format!("zel-sha256-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("payload.bin");
        let payload = b"streaming sha256 verification payload";
        std::fs::write(&file, payload).unwrap();
        let (sz, hex) = sha256::hash_file(&file).unwrap();
        assert_eq!(sz, payload.len() as u64);
        assert_eq!(hex, sha256::hex_digest(payload));
        assert!(sha256::hash_file(&dir).is_err());
        assert!(sha256::hash_file(&dir.join("missing.bin")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sha1_sha384_sha512_blake3_vectors() {
        assert_eq!(
            sha1::hex_digest(b"abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            sha512::hex_digest384(b"abc"),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(
            sha512::hex_digest(b"abc"),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        // Official BLAKE3 test vectors (len 0, 3, 65, 1025, 4096)
        let seq = |n: usize| -> Vec<u8> { (0..n).map(|i| (i % 251) as u8).collect() };
        assert_eq!(
            blake3::hex_digest(&seq(0)),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(
            blake3::hex_digest(&seq(3)),
            "e1be4d7a8ab5560aa4199eea339849ba8e293d55ca0a81006726d184519e647f"
        );
        assert_eq!(
            blake3::hex_digest(&seq(65)),
            "de1e5fa0be70df6d2be8fffd0e99ceaa8eb6e8c93a63f2d8d1c30ecb6b263dee"
        );
        assert_eq!(
            blake3::hex_digest(&seq(1025)),
            "d00278ae47eb27b34faecf67b4fe263f82d5412916c1ffd97c8cb7fb814b8444"
        );
        assert_eq!(
            blake3::hex_digest(&seq(2049)),
            "5f4d72f40d7a5f82b15ca2b2e44b1de3c2ef86c426c95c1af0b6879522563030"
        );
        assert_eq!(
            blake3::hex_digest(&seq(4096)),
            "015094013f57a5277b59d8475c0501042c0b642e531b0a1c8f58d2163229e969"
        );
        let u = uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
    }
}

#[cfg(test)]
mod exit_code_tests {
    use super::*;
    use zel_proto::failure_details::{Category, FailureDetail, InterruptedCode};

    #[test]
    fn registry_matches_java() {
        assert_eq!(ExitCode::values().len(), 18);
        assert_eq!(ExitCode::for_code(1), Some(ExitCode::BUILD_FAILURE));
        assert_eq!(ExitCode::for_code(3), Some(ExitCode::TESTS_FAILED));
        assert_eq!(ExitCode::for_code(38).unwrap().name, "PUBLISH_ERROR");
        assert_eq!(ExitCode::for_code(5), None);
        assert_eq!(ExitCode::BUILD_FAILURE.to_string(), "BUILD_FAILURE");
    }

    #[test]
    fn priority_ordering() {
        let infra = DetailedExitCode::from_exit_code(ExitCode::OOM_ERROR);
        let plain = DetailedExitCode::of(
            ExitCode::BUILD_FAILURE,
            FailureDetail::new("x", Category::Interrupted(InterruptedCode::Interrupted)),
        );
        assert_eq!(DetailedExitCode::priority(None), 0);
        assert_eq!(DetailedExitCode::priority(Some(&infra)), 4);
        assert_eq!(DetailedExitCode::priority(Some(&plain)), 3);
        let c = DetailedExitCode::choose_more_important(Some(plain.clone()), Some(infra.clone()));
        assert_eq!(c, Some(infra.clone()));
        // Ties keep the first.
        let a = DetailedExitCode::from_exit_code(ExitCode::BUILD_FAILURE);
        let b = DetailedExitCode::from_exit_code(ExitCode::TESTS_FAILED);
        assert_eq!(
            DetailedExitCode::choose_more_important(Some(a.clone()), Some(b)),
            Some(a)
        );
    }

    #[test]
    fn exit_code_from_detail_metadata() {
        let fd = FailureDetail::new("m", Category::named("spawn", "TIMEOUT").unwrap());
        let d = DetailedExitCode::of_detail(fd).unwrap();
        assert_eq!(d.exit_code.code, 1);
        let fd = FailureDetail::new("m", Category::Interrupted(InterruptedCode::Interrupted));
        assert_eq!(
            DetailedExitCode::of_detail(fd).unwrap().exit_code,
            ExitCode::INTERRUPTED
        );
        assert!(d.failure_detail_rawproto().is_some());
    }
}
