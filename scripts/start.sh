#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
python3 scripts/configure.py
bash scripts/pull.sh
bash scripts/install.sh
