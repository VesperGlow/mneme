import importlib.util
import io
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('image_plan', ROOT / 'scripts/image-plan.py')
image_plan = importlib.util.module_from_spec(spec)
spec.loader.exec_module(image_plan)


class ImagePlanTest(unittest.TestCase):
    def test_image_inputs_are_selected_without_rebuilding_models_for_agent_changes(self):
        self.assertEqual(image_plan.selected(['src/stickers.rs', 'web/app.js', 'migrations/new.sql']), {'mneme'})
        self.assertEqual(image_plan.selected(['services/tools/images.mjs']), {'mneme-tools'})
        self.assertEqual(image_plan.selected(['services/tools/catalog.json']), {'mneme', 'mneme-tools'})
        self.assertEqual(image_plan.selected(['services/models/engine.py']), {'mneme-models'})
        self.assertEqual(image_plan.selected(['README.md', 'docs/console.md']), set())
        self.assertEqual(image_plan.selected(['.dockerignore']), {item['image'] for item in image_plan.IMAGES})

    def test_push_compares_last_success_to_include_changes_from_failed_releases(self):
        history = {'workflow_runs': [{'id': 12, 'head_sha': 'newer-run'}, {'id': 9, 'head_sha': 'last-success'}]}
        env = {'GITHUB_REPOSITORY': 'owner/repo', 'GITHUB_RUN_ID': '10'}
        with patch.object(image_plan.urllib.request, 'urlopen', return_value=io.BytesIO(json.dumps(history).encode())):
            self.assertEqual(image_plan.base_revision({'before': 'failed-release'}, 'push', env), 'last-success')
        with patch.object(image_plan.urllib.request, 'urlopen', side_effect=OSError('offline')):
            self.assertIsNone(image_plan.base_revision({}, 'push', env))

    def test_manual_runs_and_unusable_history_rebuild_everything(self):
        manual = image_plan.plan({}, 'workflow_dispatch', {})
        self.assertTrue(all(item['build'] for item in manual['include']))
        with patch.object(image_plan, 'base_revision', return_value='missing'), patch.object(image_plan.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, 'git')):
            self.assertTrue(all(item['build'] for item in image_plan.plan({}, 'push', {})['include']))
        with patch.object(image_plan, 'base_revision', return_value='base'), patch.object(image_plan.subprocess, 'run'), patch.object(image_plan.subprocess, 'check_output', return_value=b'src/chat.rs\0'):
            planned = image_plan.plan({}, 'push', {})
            self.assertEqual([item['image'] for item in planned['include'] if item['build']], ['mneme'])
            self.assertEqual(len(planned['include']), 3, 'unchanged services still receive commit aliases')


if __name__ == '__main__':
    unittest.main()
