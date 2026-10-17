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
            self.assertGreaterEqual(len(first['MNEME_API_TOKEN']), 32)
            self.assertGreaterEqual(len(first['MNEME_NEO4J_PASSWORD']), 16)
            self.assertNotEqual(first['MNEME_API_TOKEN'], first['MNEME_NEO4J_PASSWORD'])
            self.assertGreaterEqual(len(first['MNEME_TOOLS_TOKEN']), 32)
            self.assertNotEqual(first['MNEME_TOOLS_TOKEN'], first['MNEME_API_TOKEN'])
            self.assertEqual(first['MNEME_TOOLS_ENABLED'], 'false')
            self.assertEqual((dest / 'tools.env').read_text(), f"MNEME_TOOLS_TOKEN={first['MNEME_TOOLS_TOKEN']}\n")
            (dest / 'config.toml').write_text('persona = "keep me"\n')
            second = configure.configure(source, dest)
            self.assertEqual(first, second)
            source.write_text(original)  # Even an original four-field .env reuses installed secrets.
            third = configure.configure(source, dest)
            self.assertEqual(third, first)
            self.assertEqual((dest / 'config.toml').read_text(), 'persona = "keep me"\n')
            for path in [source, dest / 'app.env', dest / 'neo4j.env', dest / 'console-token', dest / 'tools.env']:
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


if __name__ == '__main__':
    unittest.main()
