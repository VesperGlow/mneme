#!/usr/bin/env python3
"""Explicitly authorized, budget-limited test relay. Key stays in process memory.

Run in a terminal; enter a disposable key at the hidden prompt. Only the fixed
official DeepSeek endpoint is allowed. No production configuration is loaded.
The ledger contains usage/cost/timing only, never credentials or message text.
"""
import argparse
import getpass
import json
import math
import os
from pathlib import Path
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import urllib.error
import urllib.request

ENDPOINT = "https://api.deepseek.com/v1/chat/completions"
MODEL = "deepseek-flash"
# Official CNY prices per million tokens, checked 2026-10-18. Reserve peak
# cache-miss prices even on weekends. Actual off-peak charges are lower.
INPUT_PEAK, OUTPUT_PEAK, CACHE_PEAK = 2.0, 8.0, 0.04


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class Budget:
    def __init__(self, limit, path, resume=False):
        self.limit, self.path = limit, path
        self.lock = threading.Lock()
        self.reserved = self.spent = 0.0
        self.rows = []
        if resume:
            previous = json.loads(path.read_text())
            if (previous['provider'] != 'official-deepseek-test-relay'
                    or previous['model'] != MODEL or previous['endpoint'] != ENDPOINT
                    or previous['budget_cny'] != limit):
                raise ValueError('Budget identity or limit changed')
            amounts = [previous['charged_upper_cny'], previous['reserved_cny']]
            self.rows = previous['requests']
            amounts.extend(row['charged_upper_cny'] for row in self.rows)
            if not all(isinstance(v, (int, float)) and math.isfinite(v) and v >= -1e-9 for v in amounts):
                raise ValueError('Invalid persisted budget amounts')
            # A process may die after sending a request but before its usage
            # arrives. Carry that entire reservation as spent, never release it.
            carried = max(0.0, previous['reserved_cny'])
            self.spent = max(previous['charged_upper_cny'],
                             sum(row['charged_upper_cny'] for row in self.rows)) + carried
            if carried > 1e-9:
                self.rows.append({'status': None, 'usage': None, 'usage_known': False,
                                  'charged_upper_cny': carried, 'reserved_upper_cny': carried,
                                  'at': time.time(), 'recovered_unfinished_reservation': True})

    def snapshot(self):
        return {"provider": "official-deepseek-test-relay", "model": MODEL,
                "pid": os.getpid(), "budget_cny": self.limit,
                "charged_upper_cny": self.spent, "reserved_cny": self.reserved,
                "requests": self.rows, "endpoint": ENDPOINT,
                "pricing_source": "https://api-docs.deepseek.com/zh-cn/quick_start/pricing/",
                "accounting": "peak cache-miss upper bound; unknown usage retains full reservation"}

    def save(self):
        self.path.with_suffix('.tmp').write_text(json.dumps(self.snapshot(), indent=2))
        self.path.with_suffix('.tmp').replace(self.path)

    def reserve(self, body):
        # UTF-8 byte length bounds ordinary text tokens. Add generous framing
        # overhead per message; multimodal/tools are prohibited in this relay.
        prompt_bound = len(json.dumps(body, ensure_ascii=False).encode()) + 1024 + 128*len(body['messages'])
        amount = (prompt_bound*INPUT_PEAK + body['max_tokens']*OUTPUT_PEAK)/1e6 * 1.25
        with self.lock:
            if self.spent + self.reserved + amount > self.limit:
                return None
            self.reserved += amount
            self.save()
        return amount

    def finish(self, amount, status, usage, began):
        known = isinstance(usage, dict) and 'prompt_tokens' in usage and 'completion_tokens' in usage
        charge = (usage['prompt_tokens']*INPUT_PEAK + usage['completion_tokens']*OUTPUT_PEAK)/1e6 if known else amount
        with self.lock:
            self.reserved -= amount
            self.spent += charge
            self.rows.append({"status": status, "usage": usage, "ms": (time.monotonic()-began)*1000,
                              "charged_upper_cny": charge, "reserved_upper_cny": amount,
                              "usage_known": known, "at": time.time()})
            self.save()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--budget-cny', type=float, required=True)
    parser.add_argument('--ledger', type=Path, required=True)
    parser.add_argument('--resume', action='store_true', help='Continue the SAME budget, conservatively charging unfinished reservations')
    parser.add_argument('--thinking', action='store_true', help='Enable bounded low-effort thinking for the dedicated real usage test only')
    args = parser.parse_args()
    if not 0 < args.budget_cny <= 5:
        parser.error('This authorized evaluation is capped at 5 CNY')
    if args.resume:
        if not args.ledger.exists():
            parser.error('Cannot resume a missing budget ledger')
        previous_pid = json.loads(args.ledger.read_text())['pid']
        try:
            os.kill(previous_pid, 0)
        except ProcessLookupError:
            pass
        else:
            parser.error('Previous relay process is still alive; refusing concurrent budget owners')
    elif args.ledger.exists():
        parser.error('Refusing to reset an existing budget ledger; use a new ledger only with a fresh budget')
    args.ledger.parent.mkdir(parents=True, exist_ok=True)
    budget = Budget(args.budget_cny, args.ledger, resume=args.resume)
    key = getpass.getpass('Disposable DeepSeek key (hidden): ').strip()
    if not key.startswith('sk-') or any(c.isspace() for c in key):
        raise SystemExit('Invalid key format')
    # Free discovery call, fixed official host. Do not print server error text.
    req = urllib.request.Request('https://api.deepseek.com/v1/models', headers={'Authorization': 'Bearer '+key})
    try:
        with urllib.request.build_opener(NoRedirect).open(req, timeout=30) as response:
            catalog = json.load(response)
        if MODEL not in {m['id'] for m in catalog['data']}:
            raise SystemExit('Authorized model is absent from the official model catalog')
    except urllib.error.HTTPError as error:
        raise SystemExit(f'Official model discovery failed: HTTP {error.code}') from None
    budget.save()

    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *args):
            pass

        def reply(self, status, value):
            data = json.dumps(value).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if self.path not in ('/health', '/metrics'):
                return self.reply(404, {})
            with budget.lock:
                self.reply(200, {**budget.snapshot(), 'thinking':args.thinking})

        def do_POST(self):
            if self.path != '/v1/chat/completions':
                return self.reply(404, {})
            size = int(self.headers.get('Content-Length', 0))
            if not 0 < size <= 128*1024:
                return self.reply(413, {'error': 'test request size limit'})
            try:
                body = json.loads(self.rfile.read(size))
                assert body['model'] in ('mneme-eval-local', 'mneme-eval-qwen3-0.6b')
                assert isinstance(body['messages'], list) and 0 < len(body['messages']) <= 32
                assert all(isinstance(m.get('content'), str) for m in body['messages'])
                assert not body.get('tools') and not body.get('tool_choice')
                assert isinstance(body['max_tokens'], int) and 0 < body['max_tokens'] <= 8000
                assert body.get('response_format', {}).get('type', 'text') in ('text', 'json_object')
            except (ValueError, KeyError, TypeError, AssertionError):
                return self.reply(400, {'error': 'outside authorized text-only test protocol'})
            body['model'] = MODEL
            body['thinking'] = {'type': 'enabled' if args.thinking else 'disabled'}
            if args.thinking:
                body['reasoning_effort'] = 'low'
            if body.get('stream'):
                body['stream_options'] = {'include_usage': True}
            amount = budget.reserve(body)
            if amount is None:
                return self.reply(429, {'error': 'authorized evaluation budget exhausted'})
            usage, status, began = None, 502, time.monotonic()
            request = urllib.request.Request(ENDPOINT, data=json.dumps(body, ensure_ascii=False).encode(),
                headers={'Content-Type': 'application/json', 'Authorization': 'Bearer '+key})
            try:
                # Redirects must never carry the credential to another host.
                with urllib.request.build_opener(NoRedirect).open(request, timeout=90) as response:
                    status = response.status
                    if body.get('stream'):
                        self.send_response(status)
                        self.send_header('Content-Type', 'text/event-stream')
                        self.send_header('Connection', 'close')
                        self.end_headers()
                        connected = True
                        for line in response:
                            if line.startswith(b'data: ') and not line.startswith(b'data: [DONE]'):
                                chunk = json.loads(line[6:])
                                usage = chunk.get('usage') or usage
                            if connected:
                                try:
                                    self.wfile.write(line)
                                    self.wfile.flush()
                                except (BrokenPipeError, ConnectionResetError):
                                    connected = False
                        self.close_connection = True
                    else:
                        value = json.load(response)
                        usage = value.get('usage')
                        self.reply(status, value)
            except urllib.error.HTTPError as error:
                status = error.code
                self.reply(status, {'error': f'official provider HTTP {status}'})
            except (OSError, ValueError):
                self.close_connection = True
            finally:
                budget.finish(amount, status, usage, began)

    print(f'Authorized isolated relay ready on 127.0.0.1:18091; cap {args.budget_cny:g} CNY; thinking={args.thinking}', flush=True)
    server = ThreadingHTTPServer(('127.0.0.1', 18091), Handler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
        print('Relay stopped; budget ledger saved without credentials', flush=True)


if __name__ == '__main__':
    main()
