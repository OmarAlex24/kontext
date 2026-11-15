#!/usr/bin/env bash
# Export this run's memories into BENCH_OUT, then delete them (design §6.1).
# In dry runs it only reads the local evidence.
set -uo pipefail
cd "$REPO" || exit 1
python3 /opt/bench/arms/mem0/evidence.py export --out "$BENCH_OUT/mem0"
if [ "${BENCH_DRY:-0}" != "1" ]; then
    python3 /opt/bench/arms/mem0/evidence.py forget
fi
