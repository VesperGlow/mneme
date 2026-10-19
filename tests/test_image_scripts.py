"""Exercise selective updates with fake commands and disposable configuration files."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ImageScriptsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.log = self.root / 'calls'
        for name in ['podman', 'systemctl', 'python3']:
            file = self.bin / name
            file.write_text('#!/bin/bash\nprintf "%s\\n" "' + name + ' $*" >> "$MNEME_TEST_LOG"\n')
            file.chmod(0o755)
        self.env = dict(os.environ, PATH=f'{self.bin}:{os.environ["PATH"]}', MNEME_TEST_LOG=str(self.log), MNEME_IMAGE_TAG='sha-fixture')

    def run_script(self, script, *args):
        return subprocess.run(['bash', str(ROOT / 'scripts' / script), *args], cwd=ROOT, env=self.env, text=True, capture_output=True)

    def test_pull_validates_every_service_before_fetching(self):
        self.assertEqual(self.run_script('pull.sh', 'api', 'unknown').returncode, 1)
        self.assertFalse(self.log.exists())
        self.assertEqual(self.run_script('pull.sh', 'api', 'tools', 'api').returncode, 0)
        calls = self.log.read_text().splitlines()
        self.assertEqual(len(calls), 2)
        self.assertIn('mneme-api:sha-fixture', calls[0])
        self.assertIn('mneme-tools:sha-fixture', calls[1])
        self.assertNotIn('mneme-models', self.log.read_text())

    def test_selective_update_preserves_unselected_model_unit_and_restarts_only_requested_services(self):
        config = self.root / '.config/containers/systemd'
        config.mkdir(parents=True)
        (config / 'mneme.pod').write_text('fixture')
        (config / 'mneme-models.container').write_text('Image=pinned-models:keep\n')
        self.env['MNEME_QUADLET_DIR'] = str(config)
        self.env['MNEME_SYSTEMD_DIR'] = str(self.root / 'systemd')
        result = self.run_script('update.sh', 'api', 'tools')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((config / 'mneme-models.container').read_text(), 'Image=pinned-models:keep\n')
        self.assertIn('mneme-api:sha-fixture', (config / 'mneme-api.container').read_text())
        calls = self.log.read_text()
        self.assertIn('systemctl --user restart mneme-api.service mneme-tools.service', calls)
        self.assertNotIn('mneme-models', calls)
        self.assertNotIn('restart mneme.target', calls)


if __name__ == '__main__':
    unittest.main()
