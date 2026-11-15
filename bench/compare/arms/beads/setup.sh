#!/usr/bin/env bash
# `bd init` writes the Claude Code integration by default (SessionStart hook
# running `bd prime`, a CLAUDE.md section, AGENTS.md).
set -euo pipefail
cd "$REPO"
echo "BENCH_STEP command bd init"
bd init --quiet
