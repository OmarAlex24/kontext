#!/usr/bin/env bash
# Push the Dolt database to the git remote (refs/dolt/data).
set -euo pipefail
cd "$REPO"
echo "BENCH_STEP command bd dolt push"
bd dolt push
