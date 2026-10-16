#!/usr/bin/env bash
set -euo pipefail
mneme_image_tag="${MNEME_IMAGE_TAG:-latest}"
if [[ ! "$mneme_image_tag" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]]; then
    echo 'MNEME_IMAGE_TAG 必须是合法的容器镜像标签。' >&2
    exit 1
fi
podman pull --platform linux/amd64 "ghcr.io/vesperglow/mneme:$mneme_image_tag"
podman pull --platform linux/amd64 "ghcr.io/vesperglow/mneme-models:$mneme_image_tag"
