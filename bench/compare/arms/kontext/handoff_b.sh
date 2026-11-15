#!/usr/bin/env bash
# B joins the project with A's token and installs the hooks in its clone.
# Needs KONTEXT_TOKEN (from handoff_a, by the runner) and KONTEXT_SERVER_URL.
set -euo pipefail
cd "$REPO"
echo "BENCH_STEP manual carry the token from A to B"
echo "BENCH_STEP command KONTEXT_TOKEN=… kontext clone --server <sidecar> bench"
KONTEXT_ACTOR=human:owner kontext clone --server "$KONTEXT_SERVER_URL" bench
echo "BENCH_STEP command kontext install"
KONTEXT_ACTOR=human:owner kontext install
python3 /opt/bench/arms/kontext/distill_command.py
