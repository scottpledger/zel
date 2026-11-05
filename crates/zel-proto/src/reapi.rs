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

//! Full Remote Execution API v2 message set (build.bazel.remote.execution.v2), plus the
//! `google.bytestream`, `google.longrunning` and `google.rpc` messages used by Bazel's remote
//! client. Field numbers follow `remote_execution.proto` in `third_party/remoteapis`.
//!
//! Messages are declared with the [`message!`] macro, which derives a proto3 encoder/decoder
//! from a `number name: type` field list. Default-valued scalar fields are omitted on encode
//! (canonical proto3 serialization, which is what Bazel's Java protobuf emits and therefore what
//! Action/Command digests are computed over). Unknown fields are skipped on decode.

use crate::{wire, DecodeError};

/// A proto message that can be (de)serialized.
pub trait Message: Default + Sized {
    fn encode(&self, buf: &mut Vec<u8>);
    fn merge_field(
        &mut self,
        num: u32,
        wire_type: u32,
        buf: &[u8],
        pos: &mut usize,
    ) -> Result<(), DecodeError>;

    fn encode_to_vec(&self) -> Vec<u8> {
        let mut b = Vec::new();
        self.encode(&mut b);
        b
    }

    fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let mut msg = Self::default();
        let mut pos = 0;
        while pos < buf.len() {
            let (f, w) = wire::decode_tag(buf, &mut pos)?;
            msg.merge_field(f, w, buf, &mut pos)?;
        }
        Ok(msg)
    }
}

/// A protobuf field type usable inside [`message!`].
pub trait Field {
    fn encode_field(&self, num: u32, buf: &mut Vec<u8>);
    fn decode_field(
        &mut self,
        wire_type: u32,
        buf: &[u8],
        pos: &mut usize,
    ) -> Result<(), DecodeError>;
}

fn bad_wire(wt: u32) -> DecodeError {
    DecodeError(format!("unexpected wire type {wt}"))
}

impl Field for String {
    fn encode_field(&self, num: u32, buf: &mut Vec<u8>) {
        wire::encode_string(num, self, buf);
    }
    fn decode_field(&mut self, wt: u32, buf: &[u8], pos: &mut usize) -> Result<(), DecodeError> {
        if wt != wire::WIRE_TYPE_LENGTH_DELIMITED {
            return Err(bad_wire(wt));
        }
        *self = wire::decode_string(buf, pos)?;
        Ok(())
    }
}
impl Field for Vec<u8> {
    fn encode_field(&self, num: u32, buf: &mut Vec<u8>) {
        wire::encode_bytes(num, self, buf);
    }
    fn decode_field(&mut self, wt: u32, buf: &[u8], pos: &mut usize) -> Result<(), DecodeError> {
        if wt != wire::WIRE_TYPE_LENGTH_DELIMITED {
            return Err(bad_wire(wt));
        }
        *self = wire::decode_bytes(buf, pos)?.to_vec();
        Ok(())
    }
}
impl Field for bool {
    fn encode_field(&self, num: u32, buf: &mut Vec<u8>) {
        wire::encode_bool(num, *self, buf);
    }
    fn decode_field(&mut self, wt: u32, buf: &[u8], pos: &mut usize) -> Result<(), DecodeError> {
        if wt != wire::WIRE_TYPE_VARINT {
            return Err(bad_wire(wt));
        }
        *self = wire::decode_varint(buf, pos)? != 0;
        Ok(())
    }
}
macro_rules! int_field {
    ($t:ty, $enc:ident) => {
        impl Field for $t {
            fn encode_field(&self, num: u32, buf: &mut Vec<u8>) {
                wire::$enc(num, *self as _, buf);
            }
            fn decode_field(
                &mut self,
                wt: u32,
                buf: &[u8],
                pos: &mut usize,
            ) -> Result<(), DecodeError> {
                if wt != wire::WIRE_TYPE_VARINT {
                    return Err(bad_wire(wt));
                }
                *self = wire::decode_varint(buf, pos)? as $t;
                Ok(())
            }
        }
    };
}
int_field!(i32, encode_int32);
int_field!(i64, encode_int64);
int_field!(u32, encode_int64);
int_field!(u64, encode_int64);

impl<T: Message> Field for Option<T> {
    fn encode_field(&self, num: u32, buf: &mut Vec<u8>) {
        if let Some(m) = self {
            wire::encode_bytes_always(num, &m.encode_to_vec(), buf);
        }
    }
    fn decode_field(&mut self, wt: u32, buf: &[u8], pos: &mut usize) -> Result<(), DecodeError> {
        if wt != wire::WIRE_TYPE_LENGTH_DELIMITED {
            return Err(bad_wire(wt));
        }
        *self = Some(T::decode(wire::decode_bytes(buf, pos)?)?);
        Ok(())
    }
}
impl<T: Message> Field for Vec<T> {
    fn encode_field(&self, num: u32, buf: &mut Vec<u8>) {
        for m in self {
            wire::encode_bytes_always(num, &m.encode_to_vec(), buf);
        }
    }
    fn decode_field(&mut self, wt: u32, buf: &[u8], pos: &mut usize) -> Result<(), DecodeError> {
        if wt != wire::WIRE_TYPE_LENGTH_DELIMITED {
            return Err(bad_wire(wt));
        }
        self.push(T::decode(wire::decode_bytes(buf, pos)?)?);
        Ok(())
    }
}
impl Field for Vec<String> {
    fn encode_field(&self, num: u32, buf: &mut Vec<u8>) {
        for s in self {
            wire::encode_bytes_always(num, s.as_bytes(), buf);
        }
    }
    fn decode_field(&mut self, wt: u32, buf: &[u8], pos: &mut usize) -> Result<(), DecodeError> {
        if wt != wire::WIRE_TYPE_LENGTH_DELIMITED {
            return Err(bad_wire(wt));
        }
        self.push(wire::decode_string(buf, pos)?);
        Ok(())
    }
}
/// Repeated enum (packed on encode; packed or unpacked accepted on decode).
impl Field for Vec<i32> {
    fn encode_field(&self, num: u32, buf: &mut Vec<u8>) {
        if self.is_empty() {
            return;
        }
        let mut inner = Vec::new();
        for v in self {
            wire::encode_varint(*v as i64 as u64, &mut inner);
        }
        wire::encode_bytes_always(num, &inner, buf);
    }
    fn decode_field(&mut self, wt: u32, buf: &[u8], pos: &mut usize) -> Result<(), DecodeError> {
        match wt {
            wire::WIRE_TYPE_VARINT => self.push(wire::decode_varint(buf, pos)? as i32),
            wire::WIRE_TYPE_LENGTH_DELIMITED => {
                let inner = wire::decode_bytes(buf, pos)?;
                let mut p = 0;
                while p < inner.len() {
                    self.push(wire::decode_varint(inner, &mut p)? as i32);
                }
            }
            other => return Err(bad_wire(other)),
        }
        Ok(())
    }
}

/// Declares a proto message struct with `Message` impl.
#[macro_export]
macro_rules! message {
    ($(#[$m:meta])* $name:ident { $($num:literal $f:ident : $t:ty),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Default, PartialEq)]
        pub struct $name { $(pub $f: $t),* }
        impl $crate::reapi::Message for $name {
            #[allow(unused_variables)]
            fn encode(&self, buf: &mut Vec<u8>) {
                $( $crate::reapi::Field::encode_field(&self.$f, $num, buf); )*
            }
            #[allow(unused_variables)]
            fn merge_field(&mut self, num: u32, wt: u32, buf: &[u8], pos: &mut usize)
                -> Result<(), $crate::DecodeError> {
                match num {
                    $( $num => $crate::reapi::Field::decode_field(&mut self.$f, wt, buf, pos), )*
                    _ => $crate::wire::skip_field(wt, buf, pos),
                }
            }
        }
    };
}

message!(#[derive(Eq, Hash, PartialOrd, Ord)] Digest { 1 hash: String, 2 size_bytes: i64 });
message!(Timestamp { 1 seconds: i64, 2 nanos: i32 });
message!(Duration { 1 seconds: i64, 2 nanos: i32 });
message!(UInt32Value { 1 value: u32 });
message!(
    /// `google.protobuf.Any`.
    AnyMsg { 1 type_url: String, 2 value: Vec<u8> }
);
message!(
    /// `google.rpc.Status`.
    RpcStatus { 1 code: i32, 2 message: String, 3 details: Vec<AnyMsg> }
);

message!(NodeProperty { 1 name: String, 2 value: String });
message!(NodeProperties {
    1 properties: Vec<NodeProperty>,
    2 mtime: Option<Timestamp>,
    3 unix_mode: Option<UInt32Value>,
});
message!(FileNode {
    1 name: String, 2 digest: Option<Digest>, 4 is_executable: bool,
    6 node_properties: Option<NodeProperties>,
});
message!(DirectoryNode { 1 name: String, 2 digest: Option<Digest> });
message!(SymlinkNode { 1 name: String, 2 target: String, 4 node_properties: Option<NodeProperties> });
message!(Directory {
    1 files: Vec<FileNode>, 2 directories: Vec<DirectoryNode>, 3 symlinks: Vec<SymlinkNode>,
    5 node_properties: Option<NodeProperties>,
});
message!(PlatformProperty { 1 name: String, 2 value: String });
message!(Platform { 1 properties: Vec<PlatformProperty> });
message!(EnvironmentVariable { 1 name: String, 2 value: String });
message!(Command {
    1 arguments: Vec<String>,
    2 environment_variables: Vec<EnvironmentVariable>,
    3 output_files: Vec<String>,
    4 output_directories: Vec<String>,
    5 platform: Option<Platform>,
    6 working_directory: String,
    7 output_paths: Vec<String>,
    8 output_node_properties: Vec<String>,
});
message!(Action {
    1 command_digest: Option<Digest>,
    2 input_root_digest: Option<Digest>,
    6 timeout: Option<Duration>,
    7 do_not_cache: bool,
    9 salt: Vec<u8>,
    10 platform: Option<Platform>,
});
message!(
    /// Bazel's `CacheSalt` (`src/main/protobuf/cache_salt.proto`).
    CacheSalt { 1 may_be_executed_remotely: bool, 2 workspace: String }
);
message!(ExecutedActionMetadata {
    1 worker: String,
    2 queued_timestamp: Option<Timestamp>,
    3 worker_start_timestamp: Option<Timestamp>,
    4 worker_completed_timestamp: Option<Timestamp>,
    5 input_fetch_start_timestamp: Option<Timestamp>,
    6 input_fetch_completed_timestamp: Option<Timestamp>,
    7 execution_start_timestamp: Option<Timestamp>,
    8 execution_completed_timestamp: Option<Timestamp>,
    9 output_upload_start_timestamp: Option<Timestamp>,
    10 output_upload_completed_timestamp: Option<Timestamp>,
});
message!(OutputFile {
    1 path: String, 2 digest: Option<Digest>, 4 is_executable: bool, 5 contents: Vec<u8>,
    7 node_properties: Option<NodeProperties>,
});
message!(OutputSymlink { 1 path: String, 2 target: String, 4 node_properties: Option<NodeProperties> });
message!(OutputDirectory {
    1 path: String, 3 tree_digest: Option<Digest>, 4 is_topologically_sorted: bool,
    5 root_directory_digest: Option<Digest>,
});
message!(Tree { 1 root: Option<Directory>, 2 children: Vec<Directory> });
message!(ActionResult {
    2 output_files: Vec<OutputFile>,
    10 output_file_symlinks: Vec<OutputSymlink>,
    12 output_symlinks: Vec<OutputSymlink>,
    3 output_directories: Vec<OutputDirectory>,
    11 output_directory_symlinks: Vec<OutputSymlink>,
    4 exit_code: i32,
    5 stdout_raw: Vec<u8>,
    6 stdout_digest: Option<Digest>,
    7 stderr_raw: Vec<u8>,
    8 stderr_digest: Option<Digest>,
    9 execution_metadata: Option<ExecutedActionMetadata>,
});

message!(ExecutionPolicy { 1 priority: i32 });
message!(ResultsCachePolicy { 1 priority: i32 });
message!(ExecuteRequest {
    1 instance_name: String, 3 skip_cache_lookup: bool, 6 action_digest: Option<Digest>,
    7 execution_policy: Option<ExecutionPolicy>, 8 results_cache_policy: Option<ResultsCachePolicy>,
    9 digest_function: i32,
});
message!(LogFile { 1 digest: Option<Digest>, 2 human_readable: bool });
message!(ExecuteResponse {
    1 result: Option<ActionResult>, 2 cached_result: bool, 3 status: Option<RpcStatus>,
    5 message: String,
});
message!(ExecuteOperationMetadata {
    1 stage: i32, 2 action_digest: Option<Digest>, 3 stdout_stream_name: String,
    4 stderr_stream_name: String,
});
message!(WaitExecutionRequest { 1 name: String });
message!(
    /// `google.longrunning.Operation` (result is a oneof of `error` / `response`).
    Operation {
    1 name: String, 2 metadata: Option<AnyMsg>, 3 done: bool, 4 error: Option<RpcStatus>,
    5 response: Option<AnyMsg>,
});
message!(GetActionResultRequest {
    1 instance_name: String, 2 action_digest: Option<Digest>, 3 inline_stdout: bool,
    4 inline_stderr: bool, 5 inline_output_files: Vec<String>, 6 digest_function: i32,
});
message!(UpdateActionResultRequest {
    1 instance_name: String, 2 action_digest: Option<Digest>, 3 action_result: Option<ActionResult>,
    4 results_cache_policy: Option<ResultsCachePolicy>, 5 digest_function: i32,
});
message!(FindMissingBlobsRequest {
    1 instance_name: String, 2 blob_digests: Vec<Digest>, 3 digest_function: i32,
});
message!(FindMissingBlobsResponse { 2 missing_blob_digests: Vec<Digest> });
message!(BatchUpdateBlobsRequestItem { 1 digest: Option<Digest>, 2 data: Vec<u8>, 3 compressor: i32 });
message!(BatchUpdateBlobsRequest {
    1 instance_name: String, 2 requests: Vec<BatchUpdateBlobsRequestItem>, 5 digest_function: i32,
});
message!(BatchUpdateBlobsResponseItem { 1 digest: Option<Digest>, 2 status: Option<RpcStatus> });
message!(BatchUpdateBlobsResponse { 1 responses: Vec<BatchUpdateBlobsResponseItem> });
message!(BatchReadBlobsRequest {
    1 instance_name: String, 2 digests: Vec<Digest>, 3 acceptable_compressors: Vec<i32>,
    4 digest_function: i32,
});
message!(BatchReadBlobsResponseItem {
    1 digest: Option<Digest>, 2 data: Vec<u8>, 4 compressor: i32, 3 status: Option<RpcStatus>,
});
message!(BatchReadBlobsResponse { 1 responses: Vec<BatchReadBlobsResponseItem> });
message!(GetTreeRequest {
    1 instance_name: String, 2 root_digest: Option<Digest>, 3 page_size: i32, 4 page_token: String,
    5 digest_function: i32,
});
message!(GetTreeResponse { 1 directories: Vec<Directory>, 2 next_page_token: String });
message!(GetCapabilitiesRequest { 1 instance_name: String });
message!(SemVer { 1 major: i32, 2 minor: i32, 3 patch: i32, 4 prerelease: String });
message!(ActionCacheUpdateCapabilities { 1 update_enabled: bool });
message!(CacheCapabilities {
    1 digest_functions: Vec<i32>,
    2 action_cache_update_capabilities: Option<ActionCacheUpdateCapabilities>,
    4 max_batch_total_size_bytes: i64,
    5 symlink_absolute_path_strategy: i32,
    6 supported_compressors: Vec<i32>,
    7 supported_batch_update_compressors: Vec<i32>,
});
message!(ExecutionCapabilities {
    1 digest_function: i32, 2 exec_enabled: bool, 4 supported_node_properties: Vec<String>,
    5 digest_functions: Vec<i32>,
});
message!(ServerCapabilities {
    1 cache_capabilities: Option<CacheCapabilities>,
    2 execution_capabilities: Option<ExecutionCapabilities>,
    3 deprecated_api_version: Option<SemVer>,
    4 low_api_version: Option<SemVer>,
    5 high_api_version: Option<SemVer>,
});
message!(ToolDetails { 1 tool_name: String, 2 tool_version: String });
message!(RequestMetadata {
    1 tool_details: Option<ToolDetails>, 2 action_id: String, 3 tool_invocation_id: String,
    4 correlated_invocations_id: String, 5 action_mnemonic: String, 6 target_id: String,
    7 configuration_id: String,
});

message!(
    /// `google.rpc.PreconditionFailure.Violation`.
    PreconditionViolation { 1 r#type: String, 2 subject: String, 3 description: String }
);
message!(
    /// `google.rpc.PreconditionFailure`.
    PreconditionFailure { 1 violations: Vec<PreconditionViolation> }
);

// google.bytestream
message!(ReadRequest { 1 resource_name: String, 2 read_offset: i64, 3 read_limit: i64 });
message!(ReadResponse { 10 data: Vec<u8> });
message!(WriteRequest {
    1 resource_name: String, 2 write_offset: i64, 3 finish_write: bool, 10 data: Vec<u8>,
});
message!(WriteResponse { 1 committed_size: i64 });
message!(QueryWriteStatusRequest { 1 resource_name: String });
message!(QueryWriteStatusResponse { 1 committed_size: i64, 2 complete: bool });

/// `DigestFunction.Value`.
pub mod digest_function {
    pub const UNKNOWN: i32 = 0;
    pub const SHA256: i32 = 1;
    pub const SHA1: i32 = 2;
    pub const MD5: i32 = 3;
    pub const VSO: i32 = 4;
    pub const SHA384: i32 = 5;
    pub const SHA512: i32 = 6;
    pub const MURMUR3: i32 = 7;
    pub const SHA256TREE: i32 = 8;
    pub const BLAKE3: i32 = 9;
}

/// `ExecutionStage.Value`.
pub mod execution_stage {
    pub const UNKNOWN: i32 = 0;
    pub const CACHE_CHECK: i32 = 1;
    pub const QUEUED: i32 = 2;
    pub const EXECUTING: i32 = 3;
    pub const COMPLETED: i32 = 4;
}

/// `Compressor.Value`.
pub mod compressor {
    pub const IDENTITY: i32 = 0;
    pub const ZSTD: i32 = 1;
    pub const DEFLATE: i32 = 2;
    pub const BROTLI: i32 = 3;
}

/// `google.rpc.Code` / gRPC status codes.
pub mod code {
    pub const OK: i32 = 0;
    pub const CANCELLED: i32 = 1;
    pub const UNKNOWN: i32 = 2;
    pub const INVALID_ARGUMENT: i32 = 3;
    pub const DEADLINE_EXCEEDED: i32 = 4;
    pub const NOT_FOUND: i32 = 5;
    pub const ALREADY_EXISTS: i32 = 6;
    pub const PERMISSION_DENIED: i32 = 7;
    pub const RESOURCE_EXHAUSTED: i32 = 8;
    pub const FAILED_PRECONDITION: i32 = 9;
    pub const ABORTED: i32 = 10;
    pub const OUT_OF_RANGE: i32 = 11;
    pub const UNIMPLEMENTED: i32 = 12;
    pub const INTERNAL: i32 = 13;
    pub const UNAVAILABLE: i32 = 14;
    pub const DATA_LOSS: i32 = 15;
    pub const UNAUTHENTICATED: i32 = 16;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_roundtrip_and_canonical_bytes() {
        let d = Digest { hash: "ab".into(), size_bytes: 3 };
        let b = d.encode_to_vec();
        assert_eq!(b, vec![0x0a, 2, b'a', b'b', 0x10, 3]);
        assert_eq!(Digest::decode(&b).unwrap(), d);
        assert!(Digest::default().encode_to_vec().is_empty());
    }

    #[test]
    fn nested_and_repeated() {
        let cmd = Command {
            arguments: vec!["a".into(), "b".into()],
            environment_variables: vec![EnvironmentVariable { name: "X".into(), value: "1".into() }],
            output_paths: vec!["o".into()],
            platform: Some(Platform {
                properties: vec![PlatformProperty { name: "k".into(), value: "v".into() }],
            }),
            ..Default::default()
        };
        assert_eq!(Command::decode(&cmd.encode_to_vec()).unwrap(), cmd);
    }

    #[test]
    fn packed_and_unpacked_enums_decode() {
        let c = CacheCapabilities { digest_functions: vec![1, 9], ..Default::default() };
        let packed = c.encode_to_vec();
        assert_eq!(packed, vec![0x0a, 2, 1, 9]);
        assert_eq!(CacheCapabilities::decode(&packed).unwrap(), c);
        assert_eq!(CacheCapabilities::decode(&[0x08, 1, 0x08, 9]).unwrap(), c);
    }

    #[test]
    fn unknown_fields_skipped_and_action_result() {
        let mut ar = ActionResult { exit_code: 3, stdout_raw: b"hi".to_vec(), ..Default::default() };
        ar.output_files.push(OutputFile {
            path: "p".into(),
            digest: Some(Digest { hash: "h".into(), size_bytes: 1 }),
            is_executable: true,
            ..Default::default()
        });
        let mut bytes = ar.encode_to_vec();
        bytes.extend_from_slice(&[0xf8, 0x06, 0x01]); // unknown field 111 varint
        assert_eq!(ActionResult::decode(&bytes).unwrap(), ar);
    }
}
