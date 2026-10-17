"""Exercise deployment preparation with real temporary files; never use production secrets."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('configure', Path(__file__).resolve().parents[1] / 'scripts/configure.py')
configure = importlib.util.module_from_spec(spec)
spec.loader.exec_module(configure)


class ConfigureTest(unittest.TestCase):
    def test_four_inputs_generate_and_preserve_private_credentials(self):
        with tempfile.TemporaryDirectory() as temp:
            source, dest = Path(temp) / '.env', Path(temp) / 'config'
            original = ('MNEME_LLM_API_KEY=fixture-key\nMNEME_LLM_BASE_URL=https://provider.invalid/chat/completions\n'
                        'MNEME_QQ_APP_ID=12345678\nMNEME_QQ_APP_SECRET=fixture-qq-secret\n')
            source.write_text(original)
            first = configure.configure(source, dest)
            self.assertEqual(first['MNEME_LLM_BASE_URL'], 'https://provider.invalid/v1')
            self.assertEqual(first['MNEME_LLM_MODEL'], 'auto')
            self.assertEqual(first['MNEME_LLM_CONTEXT_TOKENS'], '100000')
            self.assertEqual(first['MNEME_LLM_HISTORY_TURNS'], '1000')
            self.assertEqual(first['MNEME_MEMORY_EXTRACT_CHARS'], '10000')
            self.assertGreaterEqual(len(first['MNEME_API_TOKEN']), 32)
            self.assertGreaterEqual(len(first['MNEME_CONSOLE_PASSWORD']), 10)
            self.assertNotEqual(first['MNEME_CONSOLE_PASSWORD'], first['MNEME_API_TOKEN'])
            self.assertEqual((dest / 'console-password').read_text().strip(), first['MNEME_CONSOLE_PASSWORD'])
            self.assertGreaterEqual(len(first['MNEME_NEO4J_PASSWORD']), 16)
            self.assertNotEqual(first['MNEME_API_TOKEN'], first['MNEME_NEO4J_PASSWORD'])
            self.assertGreaterEqual(len(first['MNEME_TOOLS_TOKEN']), 32)
            self.assertNotEqual(first['MNEME_TOOLS_TOKEN'], first['MNEME_API_TOKEN'])
            self.assertEqual(first['MNEME_TOOLS_ENABLED'], 'false')
            self.assertEqual(first['MNEME_TOOLS_MAX_CALLS'], '200')
            self.assertEqual(first['MNEME_TOOLS_TURN_TIMEOUT_SECONDS'], '1800')
            self.assertEqual((dest / 'tools.env').read_text(), f"MNEME_TOOLS_TOKEN={first['MNEME_TOOLS_TOKEN']}\n")
            (dest / 'config.toml').write_text('persona = "keep me"\n')
            second = configure.configure(source, dest)
            self.assertEqual(first, second)
            source.write_text(original)  # Even an original four-field .env reuses installed secrets.
            third = configure.configure(source, dest)
            self.assertEqual(third, first)
            self.assertEqual((dest / 'config.toml').read_text(), 'persona = "keep me"\n')
            for path in [source, dest / 'app.env', dest / 'neo4j.env', dest / 'console-token', dest / 'console-password', dest / 'tools.env']:
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(dest.stat().st_mode & 0o777, 0o700)
            self.assertEqual((dest / 'console-token').read_text().strip(), first['MNEME_API_TOKEN'])

    def test_partial_qq_credentials_and_credential_urls_fail_before_generating(self):
        with tempfile.TemporaryDirectory() as temp:
            source, dest = Path(temp) / '.env', Path(temp) / 'config'
            for url, extra in [('https://provider.invalid/v1', 'MNEME_QQ_APP_ID=12345678\n'),
                               ('https://key@provider.invalid/v1', '')]:
                source.write_text(f'MNEME_LLM_API_KEY=fixture\nMNEME_LLM_BASE_URL={url}\n{extra}')
                with self.assertRaises(ValueError):
                    configure.configure(source, dest)
                self.assertFalse(dest.exists())

    def test_token_budget_migrates_legacy_environment(self):
        with tempfile.TemporaryDirectory() as temp:
            source, dest = Path(temp) / '.env', Path(temp) / 'config'
            original = 'MNEME_LLM_API_KEY=fixture\nMNEME_LLM_BASE_URL=https://provider.invalid/v1\n'
            source.write_text(original + 'MNEME_LLM_CONTEXT_CHARS=16000\n')
            values = configure.configure(source, dest)
            self.assertEqual(values['MNEME_LLM_CONTEXT_TOKENS'], '100000')
            self.assertNotIn('MNEME_LLM_CONTEXT_CHARS', values)
            self.assertNotIn('MNEME_LLM_CONTEXT_CHARS', (dest / 'app.env').read_text())
            source.write_text(original + 'MNEME_LLM_CONTEXT_TOKENS=200000\n')
            self.assertEqual(configure.configure(source, dest)['MNEME_LLM_CONTEXT_TOKENS'], '200000')

    def test_tool_budget_overrides_legacy_config_and_rejects_invalid_limits(self):
        with tempfile.TemporaryDirectory() as temp:
            source, dest = Path(temp) / '.env', Path(temp) / 'config'
            original = 'MNEME_LLM_API_KEY=fixture\nMNEME_LLM_BASE_URL=https://provider.invalid/v1\n'
            dest.mkdir()
            (dest / 'config.toml').write_text('[tools]\nmax_calls = 10\n')
            source.write_text(original + 'MNEME_TOOLS_MAX_CALLS=200\nMNEME_TOOLS_TURN_TIMEOUT_SECONDS=1800\n')
            configure.configure(source, dest)
            self.assertIn('MNEME_TOOLS_MAX_CALLS=200\n', (dest / 'app.env').read_text())
            self.assertEqual((dest / 'config.toml').read_text(), '[tools]\nmax_calls = 10\n')
            for extra in ['MNEME_TOOLS_MAX_CALLS=201\n', 'MNEME_TOOLS_MAX_CALLS=0\n',
                          'MNEME_TOOLS_TURN_TIMEOUT_SECONDS=3601\n', 'MNEME_TOOLS_MAX_CALLS=abc\n']:
                source.write_text(original + extra)
                with self.assertRaises(ValueError):
                    configure.configure(source, dest)


if __name__ == '__main__':
    unittest.main()
