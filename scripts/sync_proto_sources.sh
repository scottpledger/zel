#!/bin/bash
# Re-derives crates/zel-proto/protos/** from a Bazel source checkout (default ~/Development/bazel,
# which must be the 9.3.2 tag) and regenerates crates/zel-proto/src/embedded.rs.
# google/protobuf/wkt.proto and google/rpc/status.proto and google/devtools/build/v1/bes.proto are
# not Bazel sources (googleapis / protobuf WKT subsets) and are left untouched.
set -euo pipefail
BZL="${1:-$HOME/Development/bazel}"
P="$(cd "$(dirname "$0")/.." && pwd)/crates/zel-proto"
cp "$BZL/src/main/java/com/google/devtools/build/lib/buildeventstream/proto/build_event_stream.proto" "$P/protos/bep/build_event_stream.proto"
for f in $(cd "$P/protos/src" && find . -name '*.proto' | sed 's|^\./||'); do
  if [ -f "$BZL/src/$f" ]; then cp "$BZL/src/$f" "$P/protos/src/$f"
  else echo "removing $f (absent from $BZL)"; rm -f "$P/protos/src/$f"; fi
done
{
  echo "// Generated list of embedded .proto sources (see protos/; regenerate with scripts/sync_proto_sources.sh)."
  echo "pub const SOURCES: &[(&str, &str)] = &["
  (cd "$P/protos" && find . -name '*.proto' | sed 's|^\./||' | sort) | while read -r f; do
    echo "    (\"$f\", include_str!(\"../protos/$f\")),"
  done
  echo "];"
} > "$P/src/embedded.rs"
