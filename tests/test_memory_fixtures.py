"""Frozen holdout protection; no model, credentials or network required."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('memory_e2e_runner', ROOT/'scripts/memory-e2e.py')
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class FrozenFixturesTests(unittest.TestCase):
    def test_existing_frozen_holdouts_are_intact(self):
        fixtures = runner.verify_frozen_fixtures(ROOT/'tests/fixtures/memory')
        self.assertEqual(len(fixtures), 5)
        self.assertEqual(fixtures['e2e-blind5.json']['turns'], 12)
        self.assertEqual(fixtures['e2e-blind5.json']['queries'], 10)

    def test_tampered_gold_and_truncated_coverage_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dataset = root/'e2e-blind.json'
            dataset.write_text(json.dumps({'profiles':[{'turns':[{}],'queries':[{}]}]}))
            manifest = {'sha256':hashlib.sha256(dataset.read_bytes()).hexdigest(),'turns':1,'queries':1}
            path = root/'e2e-blind.manifest.json'
            path.write_text(json.dumps(manifest))
            runner.verify_frozen_fixtures(root)
            dataset.write_text(json.dumps({'profiles':[]}))
            with self.assertRaisesRegex(ValueError,'changed'):
                runner.verify_frozen_fixtures(root)
            manifest['sha256'] = hashlib.sha256(dataset.read_bytes()).hexdigest()
            path.write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError,'coverage mismatch'):
                runner.verify_frozen_fixtures(root)
