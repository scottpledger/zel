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

//! Digest functions (`--digest_function`), mirroring Bazel's `DigestUtil` and
//! `DigestFunctionConverter`.

use zel_proto::reapi::{digest_function, Action, Digest, Message};
use zel_util::{blake3, md5, sha1, sha256, sha512, to_hex};

/// Supported content digest functions matching Bazel's `DigestFunctionConverter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DigestFn {
    #[default]
    Sha256,
    Sha1,
    Md5,
    Sha384,
    Sha512,
    Blake3,
}

impl DigestFn {
    pub fn parse(s: &str) -> Result<Self, String> {
        let normalized: String = s
            .chars()
            .filter(|c| *c != '-' && *c != '_')
            .collect::<String>()
            .to_ascii_uppercase();
        match normalized.as_str() {
            "SHA256" | "" => Ok(Self::Sha256),
            "SHA1" => Ok(Self::Sha1),
            "MD5" => Ok(Self::Md5),
            "SHA384" => Ok(Self::Sha384),
            "SHA512" => Ok(Self::Sha512),
            "BLAKE3" => Ok(Self::Blake3),
            _ => Err(format!("Unknown digest function: {s}")),
        }
    }

    pub fn from_proto(val: i32) -> Option<Self> {
        match val {
            digest_function::UNKNOWN | digest_function::SHA256 => Some(Self::Sha256),
            digest_function::SHA1 => Some(Self::Sha1),
            digest_function::MD5 => Some(Self::Md5),
            digest_function::SHA384 => Some(Self::Sha384),
            digest_function::SHA512 => Some(Self::Sha512),
            digest_function::BLAKE3 => Some(Self::Blake3),
            _ => None,
        }
    }

    pub fn hash(&self, data: &[u8]) -> String {
        match self {
            Self::Sha256 => sha256::hex_digest(data),
            Self::Sha1 => sha1::hex_digest(data),
            Self::Md5 => to_hex(&md5::digest(data)),
            Self::Sha384 => sha512::hex_digest384(data),
            Self::Sha512 => sha512::hex_digest(data),
            Self::Blake3 => blake3::hex_digest(data),
        }
    }

    pub fn digest(&self, data: &[u8]) -> Digest {
        Digest {
            hash: self.hash(data),
            size_bytes: data.len() as i64,
        }
    }

    /// Expected lowercase hex digest length.
    pub fn hex_len(&self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Sha1 => 40,
            Self::Md5 => 32,
            Self::Sha384 => 96,
            Self::Sha512 => 128,
            Self::Blake3 => 64,
        }
    }

    /// `DigestFunction.Value` to put in REAPI requests.
    /// Bazel 9.3.2 always sends the concrete `DigestFunction.Value` (e.g. `SHA256 = 1`).
    pub fn proto_value(&self) -> i32 {
        self.enum_value()
    }

    /// Concrete `DigestFunction.Value` enum constant.
    pub fn enum_value(&self) -> i32 {
        match self {
            Self::Sha256 => digest_function::SHA256,
            Self::Sha1 => digest_function::SHA1,
            Self::Md5 => digest_function::MD5,
            Self::Sha384 => digest_function::SHA384,
            Self::Sha512 => digest_function::SHA512,
            Self::Blake3 => digest_function::BLAKE3,
        }
    }

    /// Matches Bazel's `DigestUtil.isOldStyleDigestFunction()`: enum values 1..=7
    /// (`SHA256`, `SHA1`, `MD5`, `VSO`, `RIPEMD160`, `SHA384`, `SHA512`) omit the
    /// `{digest_function}/` segment in ByteStream resource names and `DiskCache` paths.
    pub fn is_old_style(&self) -> bool {
        let v = self.enum_value();
        (1..=7).contains(&v)
    }

    /// Lowercased name inserted into ByteStream resource names and `DiskCache` subdirectories
    /// for non-old-style digest functions (`BLAKE3` -> `Some("blake3")`; old-style -> `None`).
    pub fn resource_name(&self) -> Option<&'static str> {
        if self.is_old_style() {
            None
        } else {
            match self {
                Self::Blake3 => Some("blake3"),
                _ => None,
            }
        }
    }

    pub fn action_digest(&self, action: &Action) -> Digest {
        self.digest(&action.encode_to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_digests_and_old_style_rules() {
        let d = DigestFn::Sha256.digest(b"");
        assert_eq!(
            d.hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(d.size_bytes, 0);
        assert_eq!(DigestFn::Sha256.proto_value(), digest_function::SHA256);
        assert_eq!(DigestFn::Sha256.resource_name(), None);

        assert_eq!(
            DigestFn::Md5.digest(b"").hash,
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        // MD5 (enum 3 <= 7) is old-style in Bazel: resource_name is None!
        assert!(DigestFn::Md5.is_old_style());
        assert_eq!(DigestFn::Md5.resource_name(), None);

        assert!(DigestFn::Sha1.is_old_style());
        assert_eq!(DigestFn::Sha1.resource_name(), None);
        assert!(DigestFn::Sha384.is_old_style());
        assert!(DigestFn::Sha512.is_old_style());

        // BLAKE3 (enum 9 > 7) includes `blake3` in ByteStream resource names and DiskCache paths.
        assert!(!DigestFn::Blake3.is_old_style());
        assert_eq!(DigestFn::Blake3.resource_name(), Some("blake3"));
        assert_eq!(
            DigestFn::Blake3.digest(b"").hash,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn parse_all_supported_digest_functions() {
        assert_eq!(DigestFn::parse("sha256").unwrap(), DigestFn::Sha256);
        assert_eq!(DigestFn::parse("SHA-256").unwrap(), DigestFn::Sha256);
        assert_eq!(DigestFn::parse("MD5").unwrap(), DigestFn::Md5);
        assert_eq!(DigestFn::parse("sha1").unwrap(), DigestFn::Sha1);
        assert_eq!(DigestFn::parse("sha384").unwrap(), DigestFn::Sha384);
        assert_eq!(DigestFn::parse("sha512").unwrap(), DigestFn::Sha512);
        assert_eq!(DigestFn::parse("blake3").unwrap(), DigestFn::Blake3);
        assert!(DigestFn::parse("nope").is_err());
    }
}
