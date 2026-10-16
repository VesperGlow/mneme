#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
umask 077
mneme_image_tag="${MNEME_IMAGE_TAG:-latest}"
if [[ ! "$mneme_image_tag" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]]; then
    echo 'MNEME_IMAGE_TAG 必须是合法的容器镜像标签。' >&2
    exit 1
fi
python3 - <<'PY'
import os
from pathlib import Path

source = Path('.env')
if not source.exists():
    raise SystemExit('请先复制 .env.example 为 .env 并填写真实密钥与模型。')
source.chmod(0o600)
values = {}
allowed = {'MNEME_API_TOKEN', 'MNEME_LLM_API_KEY', 'MNEME_NEO4J_PASSWORD', 'MNEME_LLM_BASE_URL', 'MNEME_LLM_MODEL'}
for number, line in enumerate(source.read_text().splitlines(), 1):
    line = line.strip()
    if not line or line.startswith('#'):
        continue
    key, separator, value = line.partition('=')
    if not separator or key not in allowed or not value or any(c.isspace() or c in '\\"\'' for c in value):
        raise SystemExit(f'.env 第 {number} 行无效：使用无引号、无空格的 KEY=VALUE。')
    if key in values:
        raise SystemExit(f'.env 重复键：{key}')
    if value.startswith('replace-') or 'api.example.com' in value:
        raise SystemExit(f'请替换 {key} 的占位值。')
    values[key] = value
for key in ('MNEME_API_TOKEN', 'MNEME_LLM_API_KEY', 'MNEME_NEO4J_PASSWORD'):
    if key not in values:
        raise SystemExit(f'缺少 {key}')
if len(values['MNEME_API_TOKEN']) < 32 or len(values['MNEME_NEO4J_PASSWORD']) < 16:
    raise SystemExit('API Token 至少 32 字符，Neo4j 密码至少 16 字符。')
dest = Path.home() / '.config/mneme'
dest.mkdir(parents=True, exist_ok=True, mode=0o700)
os.chmod(dest, 0o700)
for name, text in {
    'app.env': '\n'.join(f'{k}={v}' for k, v in values.items()) + '\n',
    'neo4j.env': f"NEO4J_AUTH=neo4j/{values['MNEME_NEO4J_PASSWORD']}\n",
}.items():
    path = dest / name
    path.write_text(text)
    path.chmod(0o600)
config = dest / 'config.toml'
if not config.exists():
    config.write_text(Path('config/default.toml').read_text())
    config.chmod(0o600)
print(f'配置已写入 {dest}；已有 config.toml 会保留。')
PY
mkdir -p "$HOME/.config/containers/systemd" "$HOME/.config/systemd/user"
cp deploy/quadlet/* "$HOME/.config/containers/systemd/"
sed -i "s|^Image=ghcr.io/vesperglow/mneme:latest$|Image=ghcr.io/vesperglow/mneme:$mneme_image_tag|" "$HOME/.config/containers/systemd/mneme-api.container"
sed -i "s|^Image=ghcr.io/vesperglow/mneme-models:latest$|Image=ghcr.io/vesperglow/mneme-models:$mneme_image_tag|" "$HOME/.config/containers/systemd/mneme-models.container"
cp deploy/systemd/mneme.target "$HOME/.config/systemd/user/"
systemctl --user daemon-reload
systemctl --user enable --now mneme.target
systemctl --user --no-pager status mneme.target
