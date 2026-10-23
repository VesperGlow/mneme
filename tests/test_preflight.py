"""Metric conventions and corpus coverage; the full runner remains an explicit opt-in."""
import importlib.util
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
AVAILABLE = all(importlib.util.find_spec(x) for x in ['fastapi', 'psutil', 'requests', 'tiktoken', 'uvicorn'])
if AVAILABLE:
    spec = importlib.util.spec_from_file_location('mneme_preflight', ROOT/'scripts/preflight/run.py')
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)


class CorpusTests(unittest.TestCase):
    def test_at_least_100_unique_groups_cover_all_requested_categories(self):
        cases = json.loads((ROOT/'tests/fixtures/preflight/dialogues.json').read_text())['cases']
        self.assertGreaterEqual(len(cases), 100)
        self.assertEqual(len(cases), len({c['id'] for c in cases}))
        self.assertEqual(len({c['category'] for c in cases}), 12)
        self.assertTrue(all(c['turns'] and 'expected_clauses' in c for c in cases))


@unittest.skipUnless(AVAILABLE, 'install the optional preflight runner dependencies')
class MetricTests(unittest.TestCase):
    def test_recall_denominator_and_standard_precision_at_five(self):
        score = runner.metric(['a', 'noise'], ['a', 'b'])
        self.assertEqual(score['recall_at_30'], .5)
        self.assertEqual(score['precision_at_5'], .2)
        self.assertEqual(score['returned_precision'], .5)

    def test_mrr_uses_first_relevant_rank(self):
        self.assertEqual(runner.metric(['noise', 'a', 'b'], ['a', 'b'])['mrr'], .5)

    def test_negative_queries_do_not_inflate_recall(self):
        self.assertIsNone(runner.metric([], [])['recall_at_30'])
        self.assertFalse(runner.metric([], [])['negative_contamination'])
        self.assertTrue(runner.metric(['noise'], [])['negative_contamination'])

    def test_first_stage_absence_is_a_miss(self):
        self.assertEqual(runner.metric(['noise']*30 + ['gold'], ['gold'])['recall_at_30'], 0)

    def test_percentile_nearest_rank_and_small_samples(self):
        self.assertEqual(runner.percentiles([4,1,2,3])['p50_ms'], 2)
        self.assertEqual(runner.percentiles([4,1,2,3])['p95_ms'], 4)
        self.assertIsNone(runner.percentiles([])['p95_ms'])

    def test_live_provider_is_not_silently_redirected(self):
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'provider.json'
            path.write_text(json.dumps({'base_url':'https://unapproved.example/v1','model':'deepseek-flash','key':'not-a-secret'}))
            path.chmod(0o600)
            with self.assertRaisesRegex(ValueError, 'not the authorized'):
                runner.Fixture('http://127.0.0.1:1', 'http://127.0.0.1:2', path, Path(directory))


@unittest.skipUnless(AVAILABLE, 'install the optional preflight runner dependencies')
class TcpFaultTests(unittest.TestCase):
    def test_reset_closes_live_keepalive_before_waiting_for_listener(self):
        import http.server
        import threading
        import time
        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def log_message(self, *args): pass
            def do_GET(self):
                self.send_response(200)
                self.send_header('Content-Length', '2')
                self.end_headers()
                self.wfile.write(b'ok')
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        proxy = runner.TcpProxy(f'http://127.0.0.1:{server.server_port}').start()
        session = runner.requests.Session()
        url = f'http://127.0.0.1:{proxy.port}/'
        try:
            self.assertEqual(session.get(url, timeout=2).status_code, 200)
            began = time.monotonic()
            proxy.suspend()
            self.assertLess(time.monotonic()-began, 2)
            with self.assertRaises(runner.requests.ConnectionError): session.get(url, timeout=1)
            proxy.resume()
            self.assertEqual(session.get(url, timeout=2).status_code, 200)
        finally:
            session.close(); proxy.stop(); server.shutdown(); server.server_close(); thread.join(2)


class VerdictTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location('mneme_preflight_report', ROOT/'scripts/preflight/report.py')
        cls.report = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.report)
        cls.limits = json.loads((ROOT/'scripts/preflight/acceptance.json').read_text())

    def test_missing_measurements_cannot_pass(self):
        gates = self.report.evaluate({}, {}, {}, self.limits)
        self.assertFalse(all(gates.values()))
        self.assertFalse(gates['real_quality_evidence'])
        self.assertFalse(gates['long_scales_complete'])

    def test_mixed_source_revisions_are_rejected(self):
        gates = self.report.evaluate({'source_sha256':'one'}, {'source_sha256':'two'}, {'source_sha256':'one'}, self.limits)
        self.assertFalse(gates['same_source_revision'])

    def test_runner_failure_is_not_implicitly_a_pass(self):
        gates = self.report.evaluate({'results':{'runner_failure':{'type':'TimeoutError'}}}, {}, {}, self.limits)
        self.assertFalse(gates['no_runner_failure'])
