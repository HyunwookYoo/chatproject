import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name('write_e2e_summary.py')


class SummaryTests(unittest.TestCase):
    def call(self, folder, *args):
        return subprocess.run([sys.executable, str(SCRIPT), '--output', str(folder / 'summary.json'),
                               *args], capture_output=True, text=True)

    def test_fixed_schema_ignores_every_private_source_line(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / 'time.txt'
            source.write_text('Command being timed: /private/person/token serial-123\n'
                              'ticket secret 192.168.1.2\n\tMaximum resident set size (kbytes): 1234\n')
            result = self.call(root, '--profile', 'android', '--stage', 'scenario', '--outcome', 'failure',
                               '--exit-code', '17', '--memory-file', str(source))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads((root / 'summary.json').read_text()), {
                'schema_version': 1, 'profile': 'android', 'stage': 'scenario', 'outcome': 'failure',
                'exitCode': 17, 'peak_rss_kib': 1234})

    def test_missing_memory_is_null(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            result = self.call(root, '--profile', 'windows', '--stage', 'preparation', '--outcome', 'failure', '--exit-code', '1')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIsNone(json.loads((root / 'summary.json').read_text())['peak_rss_kib'])

    def test_invalid_enum_type_and_success_with_failure_code_rejected(self):
        for value in (['--profile', 'private-path'], ['--exit-code', 'token'], ['--outcome', 'success', '--exit-code', '2']):
            with tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                result = self.call(root, '--profile', 'android', '--stage', 'scenario', '--outcome', 'failure', '--exit-code', '1', *value)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((root / 'summary.json').exists())


if __name__ == '__main__':
    unittest.main()
