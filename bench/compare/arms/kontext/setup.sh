#!/usr/bin/env bash
# kontext v0.2.0, server path (design §1.2). Needs KONTEXT_SERVER_URL (the
# sidecar) and KONTEXT_ADMIN_SECRET (the sidecar's, for init --server only).
set -euo pipefail
cd "$REPO"
export KONTEXT_ACTOR=human:owner
echo "BENCH_STEP command kontext init bench --server <sidecar>"
kontext init bench --server "$KONTEXT_SERVER_URL"
echo "BENCH_STEP command kontext install"
kontext install
python3 /opt/bench/arms/kontext/distill_command.py
