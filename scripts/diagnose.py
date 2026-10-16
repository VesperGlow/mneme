#!/usr/bin/env python3
"""Probe the running Pod's outbound APIs without printing credentials or private responses."""
import argparse
import inspect
import json
import subprocess
import sys
import tomllib


def run_probe(payload):
    import concurrent.futures
    import errno
    import json
    import os
    import socket
    import ssl
    import urllib.error
    import urllib.request

    values, config = payload['env'], payload['config']
    proxy_keys = ('HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'NO_PROXY',
                  'http_proxy', 'https_proxy', 'all_proxy', 'no_proxy')
    for key in proxy_keys:
        os.environ.pop(key, None)
        if values.get(key):
            os.environ[key] = values[key]

    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            return None

    opener = urllib.request.build_opener(NoRedirect())

    def request(url, body=None, bearer=None, timeout=10):
        headers = {'Content-Type': 'application/json'}
        if bearer:
            headers['Authorization'] = 'Bearer ' + bearer
        data = None if body is None else json.dumps(body).encode()
        try:
            with opener.open(urllib.request.Request(url, data=data, headers=headers), timeout=timeout) as response:
                wire = response.read(131073)
                if len(wire) > 131072:
                    return {'ok': False, 'category': 'response_too_large'}, None
                result = {'ok': True, 'http_status': response.status}
                if 'text/event-stream' in response.headers.get('Content-Type', ''):
                    return result, wire.decode('utf-8')
                return result, json.loads(wire)
        except urllib.error.HTTPError as error:
            result = {'ok': False, 'category': 'http', 'http_status': error.code}
            try:
                value = json.loads(error.read(65536))
                detail = value.get('error')
                code = detail.get('code') if isinstance(detail, dict) else value.get('code')
                if isinstance(code, int) or code in ('invalid_api_key', 'model_not_found', 'insufficient_quota',
                                                     'rate_limit_exceeded', 'context_length_exceeded', 'unsupported_parameter'):
                    result['api_code'] = code
            except Exception:
                pass
            return result, None
        except Exception as error:
            reason = error.reason if isinstance(error, urllib.error.URLError) else error
            category = 'request'
            if isinstance(reason, (TimeoutError, socket.timeout)):
                category = 'timeout'
            elif isinstance(reason, socket.gaierror):
                category = 'dns'
            elif isinstance(reason, ssl.SSLError):
                category = 'tls'
            elif isinstance(reason, ConnectionRefusedError):
                category = 'connection_refused'
            elif isinstance(reason, OSError) and reason.errno in (errno.ENETUNREACH, errno.EHOSTUNREACH):
                category = 'network_unreachable'
            elif isinstance(reason, (ValueError, json.JSONDecodeError, UnicodeError)):
                category = 'invalid_response'
            return {'ok': False, 'category': category}, None

    def qq():
        if not values.get('MNEME_QQ_APP_ID'):
            return {'ok': True, 'enabled': False}
        result, data = request(config.get('qq', {}).get('token_url', 'https://api.bot.qq.com/app/getAppAccessToken'),
                               {'appId': values['MNEME_QQ_APP_ID'], 'clientSecret': values.get('MNEME_QQ_APP_SECRET', '')})
        if result['ok'] and (not isinstance(data, dict) or not data.get('access_token')):
            result.update(ok=False, category='token_missing')
            if isinstance(data, dict) and isinstance(data.get('code'), int):
                result['api_code'] = data['code']
        return result

    def llm():
        llm = config.get('llm', {})
        base = values.get('MNEME_LLM_BASE_URL') or llm.get('base_url', '')
        base = base.rstrip('/')
        if base.endswith('/chat/completions'):
            base = base[:-len('/chat/completions')]
        from urllib.parse import urlsplit
        if not urlsplit(base).path:
            base += '/v1'
        key = values.get('MNEME_LLM_API_KEY', '')
        model = values.get('MNEME_LLM_MODEL') or llm.get('model', 'auto')
        result = {}
        if model == 'auto':
            local, data = request('http://127.0.0.1:8080/v1/console/system', bearer=values.get('MNEME_API_TOKEN'), timeout=8)
            if local['ok'] and isinstance(data, dict):
                model = data.get('services', {}).get('llm', {}).get('model') or model
        catalog, data = request(base + '/models', bearer=key)
        result['catalog'] = catalog
        if model == 'auto' and catalog['ok'] and isinstance(data, dict):
            excluded = ('embed', 'rerank', 'whisper', 'tts', 'image', 'dall-e', 'moderation', 'transcrib')
            model = next((m['id'] for m in data.get('data', []) if isinstance(m, dict) and isinstance(m.get('id'), str)
                          and not any(kind in m['id'].lower() for kind in excluded)), 'auto')
        result['model_selected'] = model != 'auto'
        if payload.get('chat'):
            if model == 'auto':
                result['chat'] = {'ok': False, 'category': 'model_not_selected'}
            else:
                chat, data = request(base + '/chat/completions', {'model': model, 'temperature': 0.7,
                    'max_tokens': 64, 'stream': True, 'messages': [{'role': 'user', 'content': '请只回复 OK。'}]}, key, 45)
                if chat['ok']:
                    text, finished, done = False, False, False
                    try:
                        for line in data.splitlines() if isinstance(data, str) else []:
                            if not line.startswith('data:'):
                                continue
                            value = line[5:].strip()
                            if value == '[DONE]':
                                done = True
                                break
                            chunk = json.loads(value)
                            choice = (chunk.get('choices') or [{}])[0]
                            text |= bool(choice.get('delta', {}).get('content'))
                            finished |= choice.get('finish_reason') in ('stop', 'length')
                        chat.update(ok=text and finished and done, received_text=text, finished=finished, done=done)
                        if not chat['ok']:
                            chat['category'] = 'invalid_or_incomplete_stream'
                    except Exception:
                        chat.update(ok=False, category='invalid_stream')
                result['chat'] = chat
        return result

    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        llm_future, qq_future = pool.submit(llm), pool.submit(qq)
        return {'llm': llm_future.result(), 'qq_token': qq_future.result()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--api-container', default='mneme-api')
    parser.add_argument('--probe-container', default='mneme-models')
    parser.add_argument('--chat', action='store_true', help='Also request one short LLM completion (up to 64 tokens).')
    args = parser.parse_args()
    try:
        raw = subprocess.run(['podman', 'inspect', args.api_container], check=True, capture_output=True, text=True)
        environment = dict(entry.split('=', 1) for entry in json.loads(raw.stdout)[0]['Config']['Env'] if '=' in entry)
        config_path = environment.get('MNEME_CONFIG') or '/app/config/default.toml'
        allowed = ('MNEME_LLM_API_KEY', 'MNEME_LLM_BASE_URL', 'MNEME_LLM_MODEL', 'MNEME_API_TOKEN',
                   'MNEME_QQ_APP_ID', 'MNEME_QQ_APP_SECRET', 'HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'NO_PROXY',
                   'http_proxy', 'https_proxy', 'all_proxy', 'no_proxy')
        environment = {key: value for key, value in environment.items() if key in allowed}
        raw = subprocess.run(['podman', 'exec', args.api_container, 'cat', config_path],
                             check=True, capture_output=True, text=True)
        config = tomllib.loads(raw.stdout)
        payload = {'env': environment, 'config': {key: config.get(key, {}) for key in ('llm', 'qq')}, 'chat': args.chat}
        code = inspect.getsource(run_probe) + '\nimport json, sys\nprint(json.dumps(run_probe(json.load(sys.stdin)), ensure_ascii=False, indent=2))'
        child = subprocess.run(['podman', 'exec', '-i', args.probe_container, 'python', '-c', code],
                               input=json.dumps(payload), text=True, capture_output=True, timeout=90)
        if child.returncode:
            raise ValueError('probe process failed')
        print(json.dumps(json.loads(child.stdout), ensure_ascii=False, indent=2))
    except Exception:
        print(json.dumps({'ok': False, 'category': 'diagnostic_unavailable',
                          'hint': 'Check that the API and model containers are running, and the config mount exists.'}))
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
