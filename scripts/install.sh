#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
umask 077
source scripts/images.sh
mneme_select_services "$@"
if [ "$#" -gt 0 ] && [ ! -f "$mneme_quadlet_dir/mneme.pod" ]; then
    echo '首次安装请先运行不带服务参数的 scripts/start.sh。' >&2; exit 1
fi
python3 scripts/configure.py
mkdir -p "$mneme_quadlet_dir" "$mneme_systemd_dir"
if [ "$#" = 0 ]; then
    cp deploy/quadlet/* "$mneme_quadlet_dir/"
    cp deploy/systemd/mneme.target "$mneme_systemd_dir/"
    cp deploy/systemd/mneme-backup.service deploy/systemd/mneme-backup.timer "$mneme_systemd_dir/"
    mkdir -p "$HOME/.local/lib/mneme"
    cp scripts/backup.py scripts/backup-scheduled.py "$HOME/.local/lib/mneme/"
fi
for mneme_service in "${mneme_services[@]}"; do
    cp "deploy/quadlet/mneme-$mneme_service.container" "$mneme_quadlet_dir/"
    sed -i "s|^Image=ghcr.io/vesperglow/mneme-$mneme_service:latest$|Image=ghcr.io/vesperglow/mneme-$mneme_service:$mneme_image_tag|" "$mneme_quadlet_dir/mneme-$mneme_service.container"
    if [ "$mneme_service" = tools ]; then cp deploy/quadlet/mneme-tools-seccomp.json "$mneme_quadlet_dir/"; fi
done
systemctl --user daemon-reload
if [ "$#" = 0 ]; then
    systemctl --user enable --now mneme.target
    systemctl --user enable --now mneme-backup.timer
    systemctl --user --no-pager status mneme.target
fi
