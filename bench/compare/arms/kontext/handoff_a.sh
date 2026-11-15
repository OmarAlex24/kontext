#!/usr/bin/env bash
# A makes an owner token for B (shown once).
set -euo pipefail
cd "$REPO"
echo "BENCH_STEP command kontext token create --role owner --person owner"
out=$(KONTEXT_ACTOR=human:owner kontext token create --role owner --person owner)
tok=$(printf '%s\n' "$out" | sed -n 's/^ *KONTEXT_TOKEN=\(.*\)$/\1/p' | head -1)
[ -n "$tok" ] || { echo "no token in the output of token create" >&2; exit 1; }
echo "BENCH_SECRET KONTEXT_TOKEN=$tok"
