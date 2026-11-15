#!/usr/bin/env bash
# Wait for the distills the hooks started (design §2.3).
set -uo pipefail
cd "$REPO" || exit 1
kontext --json distill --wait --timeout 600s
