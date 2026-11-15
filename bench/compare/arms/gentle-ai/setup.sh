#!/usr/bin/env bash
# gentle-ai and Engram are installed globally in the image (`gentle-ai install
# --agent claude-code --preset full-gentleman`); nothing per repo.
set -euo pipefail
gentle-ai version
engram version
