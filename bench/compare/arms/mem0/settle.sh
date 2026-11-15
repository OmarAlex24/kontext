#!/usr/bin/env bash
# Wait until the plugin has no flush in flight (design §2.3), at most 10 min.
# Reads the plugin's own evidence.sqlite3; prints a JSON line with what it saw.
set -uo pipefail
exec python3 /opt/bench/arms/mem0/evidence.py settle --timeout "${BENCH_SETTLE_TIMEOUT:-600}"
