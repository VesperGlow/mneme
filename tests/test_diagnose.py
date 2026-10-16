"""Probe real local HTTP fixtures and verify that diagnostic output never contains secrets."""
import importlib.util
import json
import os
from pathlib import Path
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('diagnose', Path(__file__).resolve().parents[1] / 'scripts/diagnose.py')
diagnose = importlib.util.module_from_spec(spec)
spec.loader.exec_module(diagnose)


class DiagnoseTest(unittest.TestCase):
    def setUp(self):
        self.requests = []
        self.mode = 'success'
        test = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                test.requests.append((self.path, dict(self.headers), None))
                if test.mode == 'redirect':
                    self.send_response(302)
                    self.send_header('Location', '/sensitive-target')
                    self.end_headers()
                    return
                self.respond({'data': [{'id': 'fixture-model'}]})

            def respond(self, body, status=200):
                self.send_response(status)
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                self.wfile.write(json.dumps(body).encode())

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                test.requests.append((self.path, dict(self.headers), body))
                if test.mode == 'denied':
                    self.respond({'error': {'code': 'invalid_api_key', 'message': 'never-print-key-or-private-text'},
                                  'code': 11244}, 403 if self.path == '/qq-token' else 401)
                elif self.path == '/qq-token':
                    self.respond({'access_token': 'never-print-access-token', 'expires_in': '7200'})
                else:
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                    self.end_headers()
                    self.wfile.write(b'data: {"choices":[{"delta":{"content":"never-print-private-response"},"finish_reason":null}]}\n\n')
                    if test.mode != 'truncated':
                        self.wfile.write(b'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n')

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        base = 'http://127.0.0.1:' + str(self.server.server_port)
        self.payload = {'env': {'MNEME_LLM_BASE_URL': base + '/v1/chat/completions',
            'MNEME_LLM_API_KEY': 'never-print-llm-key', 'MNEME_LLM_MODEL': 'fixture-model',
            'MNEME_QQ_APP_ID': '12345678', 'MNEME_QQ_APP_SECRET': 'never-print-app-secret'},
            'config': {'qq': {'token_url': base + '/qq-token'}}, 'chat': True}

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def probe(self):
        with patch.dict(os.environ):
            result = diagnose.run_probe(self.payload)
        self.assertNotIn('never-print', json.dumps(result))
        return result

    def test_successful_probes_use_actual_inputs_but_never_return_tokens_or_chat_text(self):
        report = self.probe()
        self.assertTrue(report['qq_token']['ok'])
        self.assertTrue(report['llm']['catalog']['ok'])
        self.assertTrue(report['llm']['chat']['ok'])
        self.assertTrue(report['llm']['chat']['done'])
        token_request = next(r for r in self.requests if r[0] == '/qq-token')
        self.assertEqual(token_request[2]['clientSecret'], 'never-print-app-secret')
        chat_request = next(r for r in self.requests if r[0] == '/v1/chat/completions')
        self.assertEqual(chat_request[1]['Authorization'], 'Bearer never-print-llm-key')
        self.assertEqual(chat_request[2]['model'], 'fixture-model')

    def test_http_rejections_report_status_and_safe_codes(self):
        self.mode = 'denied'
        report = self.probe()
        self.assertEqual(report['qq_token']['http_status'], 403)
        self.assertEqual(report['llm']['chat']['http_status'], 401)
        self.assertEqual(report['llm']['chat']['api_code'], 'invalid_api_key')

    def test_redirects_are_not_followed(self):
        self.mode = 'redirect'
        report = self.probe()
        self.assertFalse(report['llm']['catalog']['ok'])
        self.assertEqual(report['llm']['catalog']['http_status'], 302)
        self.assertFalse(any(r[0] == '/sensitive-target' for r in self.requests))

    def test_truncated_stream_is_not_reported_as_success(self):
        self.mode = 'truncated'
        report = self.probe()
        self.assertFalse(report['llm']['chat']['ok'])
        self.assertTrue(report['llm']['chat']['received_text'])
        self.assertFalse(report['llm']['chat']['done'])


if __name__ == '__main__':
    unittest.main()
