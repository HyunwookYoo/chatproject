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
                'schema_version': 3, 'profile': 'android', 'stage': 'scenario', 'outcome': 'failure',
                'exitCode': 17, 'peak_rss_kib': 1234,
                'phase': None, 'failure_kind': None, 'command_exit_code': None})

    def test_safe_diagnostic_and_missing_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            diagnostic = root / 'diagnostic.json'
            args = ['--profile', 'android', '--stage', 'scenario', '--outcome', 'failure',
                    '--exit-code', '1', '--diagnostic-file', str(diagnostic)]
            self.assertEqual(self.call(root, *args).returncode, 0)
            self.assertIsNone(json.loads((root / 'summary.json').read_text())['phase'])
            value = {'phase': 'build_android', 'failure_kind': 'command_exit', 'command_exit_code': 17}
            diagnostic.write_text(json.dumps(value))
            result = self.call(root, *args)
            self.assertEqual(result.returncode, 0, result.stderr)
            summary = json.loads((root / 'summary.json').read_text())
            self.assertEqual({key: summary[key] for key in value}, value)
            self.assertEqual(len(summary), 9)

    def test_invalid_projection_fails_without_echoing_private_values(self):
        valid = {'phase': 'build_android', 'failure_kind': 'command_exit', 'command_exit_code': 17}
        for patch in ({'path': 'PRIVATE_TOKEN'}, {'phase': 'PRIVATE_TOKEN'},
                      {'failure_kind': 'PRIVATE_TOKEN'}, {'command_exit_code': True},
                      {'command_exit_code': 'PRIVATE_TOKEN'}, {'command_exit_code': 1.5}):
            with self.subTest(patch=patch), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                diagnostic = root / 'diagnostic.json'
                diagnostic.write_text(json.dumps({**valid, **patch}))
                result = self.call(root, '--profile', 'android', '--stage', 'scenario', '--outcome', 'failure',
                                   '--exit-code', '1', '--diagnostic-file', str(diagnostic))
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn('PRIVATE_TOKEN', result.stdout + result.stderr)
                self.assertFalse((root / 'summary.json').exists())

    def test_closed_avd_and_owned_failure_kinds_require_exact_exit_pairing(self):
        exit_kinds = ('command_exit avd_list_exit avd_create_exit avd_devices_exit '
                      'avd_console_exit avd_snapshot_exit owned_relay_exit owned_emulator_exit '
                      'owned_chat_node_exit owned_windows_exit owned_child_exit').split()
        state_kinds = ('avd_console_invalid avd_serial_conflict avd_config_missing '
                       'avd_config_mismatch avd_snapshot_missing').split()
        cases = [(kind, code, True) for kind in exit_kinds for code in (-9, 0, 17)]
        cases += [(kind, None, True) for kind in state_kinds]
        cases += [(kind, None, False) for kind in exit_kinds]
        cases += [(kind, 17, False) for kind in [*state_kinds, 'state']]
        cases += [(kind, code, False) for kind in exit_kinds for code in (True, '17', 1.5)]
        cases += [(kind, 17, False) for kind in ('PRIVATE_TOKEN_exit', 'chat_relay_exit')]
        cases += [(kind, None, False) for kind in (True, 17, [], {})]
        for kind, code, accepted in cases:
            with self.subTest(kind=kind, code=code), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                diagnostic = root / 'diagnostic.json'
                value = {'phase': 'avd_create', 'failure_kind': kind, 'command_exit_code': code}
                diagnostic.write_text(json.dumps(value))
                result = self.call(root, '--profile', 'android', '--stage', 'scenario',
                                   '--outcome', 'failure', '--exit-code', '1',
                                   '--diagnostic-file', str(diagnostic))
                if accepted:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    summary = json.loads((root / 'summary.json').read_text())
                    self.assertEqual(len(summary), 9)
                    self.assertEqual(summary['schema_version'], 3)
                    self.assertEqual(summary['outcome'], 'failure')
                    self.assertEqual(summary['exitCode'], 1)
                    self.assertEqual({key: summary[key] for key in value}, value)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('Invalid safe diagnostic', result.stderr)
                    self.assertNotIn('PRIVATE_TOKEN', result.stdout + result.stderr)
                    self.assertFalse((root / 'summary.json').exists())

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
