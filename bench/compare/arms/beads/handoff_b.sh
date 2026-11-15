#!/usr/bin/env bash
# Bootstrap the Dolt database of the fresh clone from the remote.
set -euo pipefail
cd "$REPO"
echo "BENCH_STEP command bd bootstrap"
bd bootstrap
