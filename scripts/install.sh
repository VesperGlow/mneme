#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
umask 077
mneme_image_tag="${MNEME_IMAGE_TAG:-latest}"
if [[ ! "$mneme_image_tag" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]]; then
    echo 'MNEME_IMAGE_TAG 必须是合法的容器镜像标签。' >&2
    exit 1
fi
python3 scripts/configure.py
mkdir -p "$HOME/.config/containers/systemd" "$HOME/.config/systemd/user"
cp deploy/quadlet/* "$HOME/.config/containers/systemd/"
sed -i "s|^Image=ghcr.io/vesperglow/mneme-api:latest$|Image=ghcr.io/vesperglow/mneme-api:$mneme_image_tag|" "$HOME/.config/containers/systemd/mneme-api.container"
sed -i "s|^Image=ghcr.io/vesperglow/mneme-models:latest$|Image=ghcr.io/vesperglow/mneme-models:$mneme_image_tag|" "$HOME/.config/containers/systemd/mneme-models.container"
cp deploy/systemd/mneme.target "$HOME/.config/systemd/user/"
systemctl --user daemon-reload
systemctl --user enable --now mneme.target
systemctl --user --no-pager status mneme.target
