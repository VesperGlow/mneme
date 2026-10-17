"""HTTP failover, interrupted transfers and cache integrity before model loading."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import socket
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import unittest
from unittest.mock import patch


module_spec = importlib.util.spec_from_file_location(
    'model_artifacts', Path(__file__).resolve().parents[1] / 'services/models/artifacts.py')
artifacts = importlib.util.module_from_spec(module_spec)
module_spec.loader.exec_module(artifacts)


class ModelArtifactsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.cache = Path(self.temp.name)
        self.body = ('中文记忆模型。'.encode() * 20000)
        self.record = {'size': len(self.body), 'sha256': hashlib.sha256(self.body).hexdigest(),
                       'modelscope_revision': 'b' * 40}
        self.spec = {'repo': 'fixture/model', 'revision': 'a' * 40,
                     'modelscope_repo': 'fixture/mirror', 'files': {'model.safetensors': self.record}}
        self.modes = {'huggingface': 'good', 'modelscope': 'good'}
        self.requests = []
        sleeper = patch.object(artifacts.time, 'sleep')
        sleeper.start()
        self.addCleanup(sleeper.stop)
        test = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                source = self.path.split('/')[1]
                test.requests.append((source, dict(self.headers)))
                mode = test.modes[source]
                if mode == 'disconnect_once' and sum(s == source for s, _ in test.requests) == 1:
                    mode = 'truncated'
                if mode == 'failed':
                    self.send_response(503)
                    self.end_headers()
                    self.wfile.write(b'never-print-private-upstream-body-or-signed-query')
                    return
                start, stop = self.headers.get('Range', 'bytes=0-')[6:].split('-', 1)
                offset = int(start)
                end = min(int(stop), len(test.body) - 1) if stop else len(test.body) - 1
                if mode == 'ignore_range':
                    offset, end = 0, len(test.body) - 1
                body = test.body[offset:end + 1]
                if mode == 'corrupt':
                    body = b'x' * len(body)
                ranged = bool(self.headers.get('Range')) and mode != 'ignore_range'
                self.send_response(206 if ranged else 200)
                self.send_header('Content-Length', str(len(body)))
                if ranged:
                    start = 0 if mode == 'wrong_range' else offset
                    self.send_header('Content-Range', f'bytes {start}-{end}/{len(test.body)}')
                self.end_headers()
                if mode == 'truncated':
                    self.wfile.write(body[:min(artifacts.CHUNK_SIZE * 2, len(body) - 1)])
                    self.wfile.flush()
                    self.connection.shutdown(socket.SHUT_WR)
                else:
                    self.wfile.write(body)

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        base = f'http://127.0.0.1:{self.server.server_port}'
        self.downloader = artifacts.Downloader(self.cache, ['huggingface', 'modelscope'], {
            source: base + '/' + source for source in self.modes})

    def tearDown(self):
        self.downloader.close()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.temp.cleanup()

    def target(self):
        return self.cache / 'models--fixture--model' / 'snapshots' / self.spec['revision'] / 'model.safetensors'

    def partial(self, source):
        path = self.cache / 'models--fixture--model' / 'blobs' / (self.record['sha256'] + f'.{source}.part')
        path.parent.mkdir(parents=True, exist_ok=True)
        return path

    def ensure(self, offline=False):
        return self.downloader.ensure(self.spec, 'model.safetensors', self.record, offline)

    def test_primary_success_and_verified_cache_work_without_network_on_restart(self):
        path = self.ensure()
        self.assertTrue(path.is_symlink())
        self.assertEqual(path.read_bytes(), self.body)
        self.assertEqual([source for source, _ in self.requests], ['huggingface'])
        self.modes = dict.fromkeys(self.modes, 'failed')
        self.ensure(offline=True)
        self.assertEqual(len(self.requests), 1)

    def test_failed_primary_falls_back_and_reuses_working_source_for_next_artifact(self):
        self.modes['huggingface'] = 'failed'
        with self.assertLogs(artifacts.LOG, level='INFO') as logs:
            self.ensure()
            self.downloader.ensure(self.spec, 'tokenizer.json', self.record)
        self.assertEqual([source for source, _ in self.requests], ['huggingface', 'modelscope'])
        self.assertEqual(self.downloader.sources[0], 'modelscope')
        # Use another repo to ensure this exercises source promotion, not blob reuse.
        other = dict(self.spec, repo='fixture/second')
        self.downloader.ensure(other, 'config.json', self.record)
        self.assertEqual(self.requests[-1][0], 'modelscope')
        self.assertNotIn('never-print', '\n'.join(logs.output))
        self.assertNotIn('http://', '\n'.join(logs.output))

    def test_corrupt_primary_is_discarded_before_fallback(self):
        self.modes['huggingface'] = 'corrupt'
        self.assertEqual(self.ensure().read_bytes(), self.body)
        self.assertFalse(self.partial('huggingface').exists())
        self.assertEqual([source for source, _ in self.requests], ['huggingface', 'modelscope'])

    def test_network_timeout_switches_source_without_leaking_signed_urls(self):
        original = self.downloader.session.get

        def get(url, **kwargs):
            if '/huggingface/' in url:
                raise artifacts.requests.ConnectTimeout('never-print-signed-url?token=secret')
            return original(url, **kwargs)

        with patch.object(self.downloader.session, 'get', side_effect=get), \
                self.assertLogs(artifacts.LOG, level='INFO') as logs:
            self.assertEqual(self.ensure().read_bytes(), self.body)
        self.assertEqual([source for source, _ in self.requests], ['modelscope'])
        self.assertIn('category=timeout', '\n'.join(logs.output))
        self.assertNotIn('never-print', '\n'.join(logs.output))

    def test_all_failed_sources_do_not_publish_a_snapshot(self):
        self.modes = dict.fromkeys(self.modes, 'failed')
        with self.assertRaisesRegex(RuntimeError, 'all download sources failed'):
            self.ensure()
        self.assertFalse(self.target().exists())

    def test_existing_partial_resumes_at_exact_offset(self):
        self.partial('huggingface').write_bytes(self.body[:10000])
        self.assertEqual(self.ensure().read_bytes(), self.body)
        self.assertTrue(self.requests[0][1]['Range'].startswith('bytes=10000-'))

    def test_server_ignoring_range_restarts_without_appending(self):
        self.partial('huggingface').write_bytes(b'broken prefix')
        self.modes['huggingface'] = 'ignore_range'
        self.assertEqual(self.ensure().read_bytes(), self.body)
        self.assertEqual(len(self.requests), 1)

    def test_invalid_content_range_cannot_poison_the_next_source(self):
        self.partial('huggingface').write_bytes(self.body[:10000])
        self.modes['huggingface'] = 'wrong_range'
        self.assertEqual(self.ensure().read_bytes(), self.body)
        self.assertEqual(self.partial('huggingface').read_bytes(), self.body[:10000])
        self.assertTrue(self.requests[-1][1]['Range'].startswith('bytes=0-'))

    def test_interruption_preserves_partial_for_restart_and_never_publishes_it(self):
        self.modes = dict.fromkeys(self.modes, 'truncated')
        with self.assertRaises(RuntimeError):
            self.ensure()
        self.assertFalse(self.target().exists())
        saved = self.partial('huggingface').stat().st_size
        self.assertGreater(saved, 0)
        self.assertLess(saved, len(self.body))
        self.modes['huggingface'] = 'good'
        self.assertEqual(self.ensure().read_bytes(), self.body)
        self.assertTrue(self.requests[-1][1]['Range'].startswith(f'bytes={saved}-'))

    def test_transient_disconnect_resumes_same_source_without_restarting_service(self):
        self.modes['huggingface'] = 'failed'
        self.modes['modelscope'] = 'disconnect_once'
        self.assertEqual(self.ensure().read_bytes(), self.body)
        self.assertEqual([source for source, _ in self.requests], ['huggingface', 'modelscope', 'modelscope'])
        self.assertTrue(self.requests[-1][1]['Range'].startswith(f'bytes={artifacts.CHUNK_SIZE * 2}-'))

    def test_bounded_ranges_finish_without_requesting_the_whole_large_file(self):
        with patch.object(artifacts, 'RANGE_SIZE', artifacts.CHUNK_SIZE * 2):
            self.assertEqual(self.ensure().read_bytes(), self.body)
        self.assertGreater(len(self.requests), 1)
        for _, headers in self.requests:
            start, stop = map(int, headers['Range'][6:].split('-'))
            self.assertLessEqual(stop - start + 1, artifacts.CHUNK_SIZE * 2)

    def test_restart_prefers_existing_fallback_partial_before_failed_primary(self):
        self.partial('modelscope').write_bytes(self.body[:10000])
        self.modes['huggingface'] = 'failed'
        self.assertEqual(self.ensure().read_bytes(), self.body)
        self.assertEqual([source for source, _ in self.requests], ['modelscope'])
        self.assertTrue(self.requests[0][1]['Range'].startswith('bytes=10000-'))

    def test_bundled_models_work_without_writable_cache_or_network_and_reject_corruption(self):
        path = self.ensure()
        with patch.dict(os.environ, {'MNEME_BUNDLED_MODEL_CACHE': str(self.cache)}), \
                patch.object(artifacts, 'MANIFEST', [self.spec]), \
                patch.object(artifacts, 'Downloader', side_effect=AssertionError('network forbidden')):
            self.assertEqual(artifacts.prepare_cache(), self.cache)
            path.resolve().write_bytes(b'x' * len(self.body))
            with self.assertRaisesRegex(RuntimeError, 'bundled model missing or corrupt'):
                artifacts.prepare_cache()

    def test_offline_missing_or_corrupt_cache_fails_without_network(self):
        with self.assertRaisesRegex(RuntimeError, 'offline mode'):
            self.ensure(offline=True)
        path = self.target()
        path.parent.mkdir(parents=True)
        path.write_bytes(b'x' * len(self.body))
        with self.assertRaisesRegex(RuntimeError, 'offline mode'):
            self.ensure(offline=True)
        self.assertEqual(self.requests, [])
        self.assertEqual(self.ensure().read_bytes(), self.body)

    def test_complete_verified_partial_is_published_without_a_new_request(self):
        self.partial('huggingface').write_bytes(self.body)
        self.assertEqual(self.ensure().read_bytes(), self.body)
        self.assertEqual(self.requests, [])

    def test_offline_startup_and_invalid_source_order(self):
        with patch.dict(os.environ, {'HF_HOME': str(self.cache), 'HF_HUB_CACHE': str(self.cache),
                                    'HF_HUB_OFFLINE': 'YES', 'MNEME_MODEL_SOURCES': 'huggingface,modelscope'}), \
                patch.object(artifacts, 'MANIFEST', [self.spec]):
            self.ensure()
            self.assertEqual(artifacts.prepare_cache(), self.cache)
            self.assertEqual(len(self.requests), 1)
        for sources in ('', 'unknown', 'modelscope,modelscope'):
            with patch.dict(os.environ, {'MNEME_MODEL_SOURCES': sources}):
                with self.assertRaisesRegex(RuntimeError, 'MNEME_MODEL_SOURCES'):
                    artifacts.prepare_cache()

    def test_manifest_has_fixed_revisions_and_valid_digests_for_all_required_files(self):
        manifest = artifacts.MANIFEST
        self.assertEqual(len(manifest), 3)
        self.assertEqual(sum(len(spec['files']) for spec in manifest), 13)
        for spec in manifest:
            self.assertRegex(spec['revision'], r'^[0-9a-f]{40}$')
            for name, record in spec['files'].items():
                self.assertNotIn('..', Path(name).parts)
                self.assertRegex(record['sha256'], r'^[0-9a-f]{64}$')
                self.assertRegex(record['modelscope_revision'], r'^[0-9a-f]{40}$')
                self.assertGreater(record['size'], 0)


if __name__ == '__main__':
    unittest.main()
