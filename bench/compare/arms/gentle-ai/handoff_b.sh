#!/usr/bin/env bash
# Engram Git Sync on B: the clone already has .engram/; import its chunks.
set -euo pipefail
cd "$REPO"
echo "BENCH_STEP command engram sync --import"
engram sync --import
