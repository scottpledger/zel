#!/bin/bash
echo "shard ${TEST_SHARD_INDEX:-x}/${TEST_TOTAL_SHARDS:-x}"
[ -n "$TEST_SHARD_STATUS_FILE" ] && touch "$TEST_SHARD_STATUS_FILE"
exit 0
