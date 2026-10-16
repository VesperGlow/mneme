#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
podman build --platform linux/amd64 -t ghcr.io/vesperglow/mneme:latest -t localhost/mneme:0.1.0 -f Containerfile .
podman build --platform linux/amd64 -t ghcr.io/vesperglow/mneme-models:latest -t localhost/mneme-models:0.1.0 -f services/models/Containerfile .
