#!/bin/bash
echo "shard $TEST_SHARD_INDEX of $TEST_TOTAL_SHARDS"; [ -n "$TEST_SHARD_STATUS_FILE" ] && touch "$TEST_SHARD_STATUS_FILE"; exit 0
