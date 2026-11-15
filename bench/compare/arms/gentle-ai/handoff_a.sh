#!/usr/bin/env bash
# Engram Git Sync on A: export the new memories as a chunk, commit, push.
set -euo pipefail
cd "$REPO"
echo "BENCH_STEP command engram sync"
engram sync
echo "BENCH_STEP command git add .engram && git commit && git push"
git add -A .engram 2>/dev/null || true
git commit -q -m "engram sync" || echo "nothing new from engram"
git push -q origin HEAD
