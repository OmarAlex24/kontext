#!/usr/bin/env bash
# Writes .claude/skills/openspec-* and .claude/commands/opsx/ in the repo.
set -euo pipefail
cd "$REPO"
echo "BENCH_STEP command openspec init --tools claude"
openspec init --tools claude
