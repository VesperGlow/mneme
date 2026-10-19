#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/images.sh"
mneme_select_services "$@"
for mneme_service in "${mneme_services[@]}"; do
    podman pull --platform linux/amd64 "ghcr.io/vesperglow/mneme-$mneme_service:$mneme_image_tag"
done
