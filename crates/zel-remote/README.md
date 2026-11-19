# zel-remote

Remote caching / execution client, implemented from scratch with **no external crates**.

| Layer | Module | Status |
|---|---|---|
| HTTP/2 (RFC 9113): frames, SETTINGS, stream multiplexing, connection+stream flow control (both directions), PING/GOAWAY/RST_STREAM, CONTINUATION | `http2` | done, h2c (prior knowledge) |
| HPACK (RFC 7541): static+dynamic tables, Huffman enc/dec | `hpack` | done, validated against the RFC's own test vectors (C.1–C.5) |
| gRPC: pooled channel (`--remote_max_connections`), deadlines (`grpc-timeout`), metadata, trailers, unary/client-/server-streaming, `RemoteRetrier`-style retries | `grpc` | done |
| REAPI v2 client: Capabilities, CAS (FindMissingBlobs, BatchUpdateBlobs, BatchReadBlobs, GetTree), ByteStream Read/Write, ActionCache Get/Update, Execution Execute/WaitExecution (+Operations, stream-resume) | `reapi_client`, `zel-proto::reapi` | done |
| Merkle trees (sorted nodes, symlinks, node properties, empty dirs, digest-only inputs) and `Tree` flattening | `merkle` | done |
| `--disk_cache` (Bazel layout `[<fn>/]{ac,cas}/<2 hex>/<hash>`, tmp+rename, mtime refresh, AC integrity check) | `disk_cache` | done |
| Combined cache + remote execution + local fallback behind a `SpawnRunner` trait | `spawn_runner` | done |
| Flags: `--remote_cache --remote_executor --remote_instance_name --disk_cache --remote_upload_local_results --remote_accept_cached --remote_download_outputs=all\|minimal\|toplevel (+legacy shorthands) --remote_header --remote_cache_header --remote_exec_header --remote_timeout --remote_retries --remote_retry_max_delay --remote_max_connections --remote_local_fallback --remote_default_exec_properties --digest_function --credential_helper --remote_proxy=unix:...` | `options`, `credentials` | done |
| In-process fake REAPI server (real gRPC/HTTP/2 over in-memory pipe, TCP or unix socket) | `fake_server` | done (used by the tests) |

## Not supported (deliberately, with explicit errors)

* **TLS** — `grpcs://`, `https://`, `--tls_enabled`: a TLS stack cannot be written without crates; these are rejected with an
  explanatory error. Use `grpc://` or `unix:` (e.g. behind a local TLS-terminating proxy).
* `--google_credentials` (needs an OAuth2 token exchange over HTTPS) — rejected when a remote cache is configured. Use
  `--remote_header=Authorization=Bearer …` or `--credential_helper`.
* **Compression** — `--remote_cache_compression` (zstd) and gRPC message compression: no pure-Rust implementation here; the flag
  produces a warning and blobs go uncompressed. Compressed gRPC replies are rejected.
* HTTP/1.1 REST caches (`--remote_cache=http://…`), SHA-1/BLAKE3 digest functions (`--digest_function` accepts SHA256 and MD5),
  `--remote_download_regex`, native OAuth, `--remote_grpc_log`, circuit breaker, lease extension, cache eviction retries.
* `--watchfs` has no native file-watcher backend; see zel-skyframe.
