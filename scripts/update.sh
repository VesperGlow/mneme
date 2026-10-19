#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/images.sh
mneme_select_services "$@"
if [ ! -f "$mneme_quadlet_dir/mneme.pod" ]; then
    echo '首次安装请先运行 scripts/start.sh。' >&2; exit 1
fi
# Pull everything selected before changing any service definition or restarting.
bash scripts/pull.sh "${mneme_services[@]}"
bash scripts/install.sh "${mneme_services[@]}"
mneme_units=()
for mneme_service in "${mneme_services[@]}"; do mneme_units+=("mneme-$mneme_service.service"); done
systemctl --user restart "${mneme_units[@]}"
systemctl --user --no-pager status "${mneme_units[@]}"
