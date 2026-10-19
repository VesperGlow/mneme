#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/images.sh
mneme_select_services "$@"
for mneme_service in "${mneme_services[@]}"; do
    if [ "$mneme_service" = api ]; then mneme_containerfile=Containerfile; else mneme_containerfile="services/$mneme_service/Containerfile"; fi
    mneme_local_tags=()
    if [ "$mneme_service" = api ]; then mneme_local_tags=(-t localhost/mneme:0.1.0); fi
    if [ "$mneme_service" = models ]; then mneme_local_tags=(-t localhost/mneme-models:0.1.0); fi
    podman build --platform linux/amd64 "${mneme_local_tags[@]}" -t "ghcr.io/vesperglow/mneme-$mneme_service:$mneme_image_tag" -f "$mneme_containerfile" .
done
