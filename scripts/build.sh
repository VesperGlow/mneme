#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
podman build -t localhost/mneme:0.1.0 -f Containerfile .
podman build -t localhost/mneme-models:0.1.0 -f services/models/Containerfile .
