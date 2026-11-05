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

//! Digest functions (`--digest_function`), mirroring Bazel's `DigestUtil`.

use zel_proto::reapi::{digest_function, Action, Digest, Message};
use zel_util::{md5, sha256, to_hex};

/// Supported content digest functions. Bazel additionally supports SHA-1 and BLAKE3, which are
/// not implemented here (no external crates); `parse` reports them as unsupported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DigestFn {
    #[default]
    Sha256,
    Md5,
}

impl DigestFn {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_uppercase().as_str() {
            "SHA256" | "" => Ok(Self::Sha256),
            "MD5" => Ok(Self::Md5),
            "SHA1" | "BLAKE3" | "SHA384" | "SHA512" => Err(format!(
                "--digest_function={s} is not supported by this build (supported: SHA256, MD5)"
            )),
            other => Err(format!("Unknown digest function: {other}")),
        }
    }

    pub fn hash(&self, data: &[u8]) -> String {
        match self {
            Self::Sha256 => sha256::hex_digest(data),
            Self::Md5 => to_hex(&md5::digest(data)),
        }
    }

    pub fn digest(&self, data: &[u8]) -> Digest {
        Digest { hash: self.hash(data), size_bytes: data.len() as i64 }
    }

    /// Expected hex length.
    pub fn hex_len(&self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Md5 => 32,
        }
    }

    /// `DigestFunction.Value` to put in requests. Bazel leaves the field unset for SHA-256.
    pub fn proto_value(&self) -> i32 {
        match self {
            Self::Sha256 => digest_function::UNKNOWN,
            Self::Md5 => digest_function::MD5,
        }
    }

    /// Full enum value (for capability matching).
    pub fn enum_value(&self) -> i32 {
        match self {
            Self::Sha256 => digest_function::SHA256,
            Self::Md5 => digest_function::MD5,
        }
    }

    /// Lowercased name used for ByteStream resource names (omitted for SHA-256) and for the
    /// disk cache subdirectory (omitted for the "old style" SHA-256 layout).
    pub fn resource_name(&self) -> Option<&'static str> {
        match self {
            Self::Sha256 => None,
            Self::Md5 => Some("md5"),
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
    fn sha256_empty_digest() {
        let d = DigestFn::Sha256.digest(b"");
        assert_eq!(d.hash, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(d.size_bytes, 0);
        assert_eq!(DigestFn::Md5.digest(b"").hash, "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn parse_digest_function() {
        assert_eq!(DigestFn::parse("sha256").unwrap(), DigestFn::Sha256);
        assert_eq!(DigestFn::parse("MD5").unwrap(), DigestFn::Md5);
        assert!(DigestFn::parse("blake3").unwrap_err().contains("not supported"));
        assert!(DigestFn::parse("nope").is_err());
    }
}
