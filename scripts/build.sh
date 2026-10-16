#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mneme_image_tag="${MNEME_IMAGE_TAG:-latest}"
if [[ ! "$mneme_image_tag" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]]; then
    echo 'MNEME_IMAGE_TAG 必须是合法的容器镜像标签。' >&2
    exit 1
fi
podman build --platform linux/amd64 -t "ghcr.io/vesperglow/mneme-api:$mneme_image_tag" -t localhost/mneme:0.1.0 -f Containerfile .
podman build --platform linux/amd64 -t "ghcr.io/vesperglow/mneme-models:$mneme_image_tag" -t localhost/mneme-models:0.1.0 -f services/models/Containerfile .
