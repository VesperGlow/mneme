#!/usr/bin/env python3
"""Prepare persistent local credentials without executing the .env as shell code."""
import argparse
import os
from pathlib import Path
import secrets
import tempfile
from urllib.parse import urlsplit, urlunsplit

ALLOWED = {
    'MNEME_CONSOLE_PASSWORD', 'MNEME_CONSOLE_SESSION_DAYS', 'MNEME_CONSOLE_SECURE_COOKIE',
    'MNEME_API_TOKEN', 'MNEME_LLM_API_KEY', 'MNEME_NEO4J_PASSWORD',
    'MNEME_LLM_BASE_URL', 'MNEME_LLM_MODEL', 'MNEME_QQ_APP_ID',
    'MNEME_QQ_APP_SECRET', 'MNEME_QQ_SANDBOX', 'MNEME_QQ_LISTEN',
    'MNEME_TOOLS_TOKEN', 'MNEME_TOOLS_ENABLED', 'MNEME_TOOLS_URL',
    'MNEME_TOOLS_MAX_CALLS', 'MNEME_TOOLS_TURN_TIMEOUT_SECONDS',
}


def read_env(path):
    values = {}
    for number, line in enumerate(path.read_text().splitlines(), 1):
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        key, separator, value = line.partition('=')
        if not separator or key not in ALLOWED or any(c.isspace() or c in '\\"\'' for c in value):
            raise ValueError(f'.env 第 {number} 行无效：使用无引号、无空格的 KEY=VALUE。')
        if key in values:
            raise ValueError(f'.env 重复键：{key}')
        if value.startswith('replace-') or 'api.example.com' in value:
            raise ValueError(f'请替换 {key} 的占位值。')
        values[key] = value
    return values


def write_private(path, text):
    fd, temporary = tempfile.mkstemp(prefix='.' + path.name, dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as output:
            output.write(text)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def configure(source, dest):
    if not source.exists():
        raise ValueError('请先复制 .env.example 为 .env，填写 LLM API Key、URL 和 QQ AppID、AppSecret。')
    source.chmod(0o600)
    values = read_env(source)
    for key in ('MNEME_LLM_API_KEY', 'MNEME_LLM_BASE_URL'):
        if not values.get(key):
            raise ValueError(f'缺少 {key}')
    url = urlsplit(values['MNEME_LLM_BASE_URL'])
    if url.scheme not in ('http', 'https') or not url.hostname or url.username or url.password or url.query or url.fragment:
        raise ValueError('LLM URL 必须是不含密钥、查询参数或片段的 http(s) URL。')
    path = url.path.rstrip('/')
    if path.endswith('/chat/completions'):
        path = path[:-len('/chat/completions')]
    values['MNEME_LLM_BASE_URL'] = urlunsplit((url.scheme, url.netloc, path or '/v1', '', ''))
    appid, secret = values.get('MNEME_QQ_APP_ID', ''), values.get('MNEME_QQ_APP_SECRET', '')
    if bool(appid) != bool(secret):
        raise ValueError('QQ AppID 和 AppSecret 必须同时填写或同时留空。')
    if appid and (not appid.isascii() or not appid.isdecimal() or len(appid) > 32 or not 8 <= len(secret) <= 256 or not secret.isascii()):
        raise ValueError('QQ AppID 必须为数字，AppSecret 为 8–256 个 ASCII 字符。')
    if values.get('MNEME_QQ_SANDBOX', 'false') not in ('true', 'false'):
        raise ValueError('MNEME_QQ_SANDBOX 必须为 true 或 false。')
    dest.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(dest, 0o700)
    old = read_env(dest / 'app.env') if (dest / 'app.env').exists() else {}
    generated = {}
    for key in ('MNEME_API_TOKEN', 'MNEME_NEO4J_PASSWORD', 'MNEME_TOOLS_TOKEN', 'MNEME_CONSOLE_PASSWORD'):
        if not values.get(key):
            values[key] = old.get(key) or secrets.token_urlsafe(32)
            generated[key] = values[key]
    if len(values['MNEME_API_TOKEN']) < 32 or len(values['MNEME_NEO4J_PASSWORD']) < 16:
        raise ValueError('API Token 至少 32 字符，Neo4j 密码至少 16 字符。')
    if len(values['MNEME_TOOLS_TOKEN']) < 32 or values['MNEME_TOOLS_TOKEN'] == values['MNEME_API_TOKEN']:
        raise ValueError('工具服务需要与 API Token 不同的至少 32 字符凭证。')
    values['MNEME_TOOLS_ENABLED'] = values.get('MNEME_TOOLS_ENABLED', old.get('MNEME_TOOLS_ENABLED', 'false'))
    if values['MNEME_TOOLS_ENABLED'] not in ('true', 'false'):
        raise ValueError('MNEME_TOOLS_ENABLED 必须为 true 或 false。')
    for key, default, maximum in [('MNEME_TOOLS_MAX_CALLS', '200', 200),
                                  ('MNEME_TOOLS_TURN_TIMEOUT_SECONDS', '1800', 3600)]:
        value = values.get(key, old.get(key, default))
        minimum = 10 if key.endswith('SECONDS') else 1
        if not value.isascii() or not value.isdecimal() or not minimum <= int(value) <= maximum:
            raise ValueError(f'{key} 必须为 {minimum}–{maximum} 的整数。')
        values[key] = value
    if not 10 <= len(values['MNEME_CONSOLE_PASSWORD']) <= 128:
        raise ValueError('WebUI 密码必须为 10–128 字符。')
    if values.get('MNEME_CONSOLE_SECURE_COOKIE', 'false') not in ('true', 'false'):
        raise ValueError('MNEME_CONSOLE_SECURE_COOKIE 必须为 true 或 false。')
    days = values.get('MNEME_CONSOLE_SESSION_DAYS', '30')
    if not days.isascii() or not days.isdecimal() or not 1 <= int(days) <= 365:
        raise ValueError('MNEME_CONSOLE_SESSION_DAYS 必须为 1–365 的整数。')
    if generated:
        lines = source.read_text().splitlines()
        for key, value in generated.items():
            replaced = False
            for i, line in enumerate(lines):
                if line.strip().startswith(key + '='):
                    lines[i] = f'{key}={value}'
                    replaced = True
            if not replaced:
                lines.append(f'{key}={value}')
        write_private(source, '\n'.join(lines) + '\n')
    values['MNEME_LLM_MODEL'] = values.get('MNEME_LLM_MODEL') or 'auto'
    write_private(dest / 'app.env', ''.join(f'{key}={value}\n' for key, value in values.items() if value))
    write_private(dest / 'tools.env', f"MNEME_TOOLS_TOKEN={values['MNEME_TOOLS_TOKEN']}\n")
    write_private(dest / 'neo4j.env', f"NEO4J_AUTH=neo4j/{values['MNEME_NEO4J_PASSWORD']}\n")
    write_private(dest / 'console-password', values['MNEME_CONSOLE_PASSWORD'] + '\n')
    write_private(dest / 'console-token', values['MNEME_API_TOKEN'] + '\n')
    config = dest / 'config.toml'
    if not config.exists():
        write_private(config, (Path(__file__).resolve().parent.parent / 'config/default.toml').read_text())
    return values


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--env-file', type=Path, default=Path('.env'))
    parser.add_argument('--config-dir', type=Path, default=Path.home() / '.config/mneme')
    args = parser.parse_args()
    try:
        configure(args.env_file, args.config_dir)
    except (ValueError, OSError) as error:
        raise SystemExit(str(error)) from None
    print(f'配置已写入 {args.config_dir}，已保留已有密钥和人格配置。')
    print(f'WebUI 登录密码：本机文件 {args.config_dir / "console-password"}（0600）。')
