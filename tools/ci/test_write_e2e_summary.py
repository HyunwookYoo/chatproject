import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name('write_e2e_summary.py')
DETAILS = json.loads((SCRIPT.parents[1] / 'e2e' / 'diagnostic_details.json').read_text(encoding='utf-8'))['details']
SCENARIO_KEYS = {'detail': None, 'scenario': 'm2', 'android_crash': None}


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
                'schema_version': 4, 'profile': 'android', 'stage': 'scenario', 'outcome': 'failure',
                'exitCode': 17, 'peak_rss_kib': 1234,
                'phase': None, 'failure_kind': None, 'command_exit_code': None,
                'detail': None, 'scenario': None, 'android_crash': None})

    def test_safe_diagnostic_and_missing_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            diagnostic = root / 'diagnostic.json'
            args = ['--profile', 'android', '--stage', 'scenario', '--outcome', 'failure',
                    '--exit-code', '1', '--diagnostic-file', str(diagnostic)]
            self.assertEqual(self.call(root, *args).returncode, 0)
            self.assertIsNone(json.loads((root / 'summary.json').read_text())['phase'])
            value = {'phase': 'build_android', 'failure_kind': 'command_exit', 'command_exit_code': 17,
                     **SCENARIO_KEYS}
            diagnostic.write_text(json.dumps(value))
            result = self.call(root, *args)
            self.assertEqual(result.returncode, 0, result.stderr)
            summary = json.loads((root / 'summary.json').read_text())
            self.assertEqual({key: summary[key] for key in value}, value)
            self.assertEqual(len(summary), 12)

    def test_invalid_projection_fails_without_echoing_private_values(self):
        valid = {'phase': 'build_android', 'failure_kind': 'command_exit', 'command_exit_code': 17,
                 **SCENARIO_KEYS}
        for patch in ({'path': 'PRIVATE_TOKEN'}, {'phase': 'PRIVATE_TOKEN'},
                      {'failure_kind': 'PRIVATE_TOKEN'}, {'command_exit_code': True},
                      {'command_exit_code': 'PRIVATE_TOKEN'}, {'command_exit_code': 1.5},
                      {'detail': 'PRIVATE_TOKEN'}, {'detail': 'Message scenario: PRIVATE_TOKEN'},
                      {'detail': 17}, {'detail': True}, {'scenario': 'PRIVATE_TOKEN'},
                      {'scenario': None}, {'android_crash': 'PRIVATE_TOKEN'},
                      {'android_crash': 1}):
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
                value = {'phase': 'avd_create', 'failure_kind': kind, 'command_exit_code': code,
                         **SCENARIO_KEYS}
                diagnostic.write_text(json.dumps(value))
                result = self.call(root, '--profile', 'android', '--stage', 'scenario',
                                   '--outcome', 'failure', '--exit-code', '1',
                                   '--diagnostic-file', str(diagnostic))
                if accepted:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    summary = json.loads((root / 'summary.json').read_text())
                    self.assertEqual(len(summary), 12)
                    self.assertEqual(summary['schema_version'], 4)
                    self.assertEqual(summary['outcome'], 'failure')
                    self.assertEqual(summary['exitCode'], 1)
                    self.assertEqual({key: summary[key] for key in value}, value)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('Invalid safe diagnostic', result.stderr)
                    self.assertNotIn('PRIVATE_TOKEN', result.stdout + result.stderr)
                    self.assertFalse((root / 'summary.json').exists())

    def test_closed_scenario_timeout_origins(self):
        phases = ('scenario_app1 scenario_app2 scenario_node scenario_app1_to_app2 '
                  'scenario_app1_to_node scenario_app2_to_app1 scenario_app2_to_node '
                  'scenario_node_to_app1 scenario_node_to_app2 android_pid_monitor').split()
        kinds = ('timeout_network_start timeout_network_info timeout_connect_peer '
                 'timeout_send_probe_text timeout_set_offline timeout_disconnect_peer '
                 'timeout_network_stop timeout_state_event timeout_probe_event timeout_offline_event').split()
        cases = [(phase, 'timeout', None, True) for phase in phases]
        cases += [('scenario_app1', kind, None, True) for kind in kinds]
        cases += [('scenario_app1', kind, code, False) for kind in kinds for code in (17, True, '17', 1.5)]
        cases += [(phase, 'timeout', None, False) for phase in ('scenario_PRIVATE_TOKEN', True, 17, [], {})]
        cases += [('scenario_app1', kind, None, False) for kind in ('timeout_PRIVATE_TOKEN', True, 17, [], {})]
        for phase, kind, code, accepted in cases:
            with self.subTest(phase=phase, kind=kind, code=code), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                diagnostic = root / 'diagnostic.json'
                value = {'phase': phase, 'failure_kind': kind, 'command_exit_code': code,
                         **SCENARIO_KEYS}
                diagnostic.write_text(json.dumps(value))
                result = self.call(root, '--profile', 'android', '--stage', 'scenario',
                                   '--outcome', 'failure', '--exit-code', '1',
                                   '--diagnostic-file', str(diagnostic))
                if accepted:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    summary = json.loads((root / 'summary.json').read_text())
                    self.assertEqual(set(summary), {'schema_version', 'profile', 'stage', 'outcome',
                                                    'exitCode', 'peak_rss_kib', *value})
                    self.assertEqual(summary['schema_version'], 4)
                    self.assertEqual({key: summary[key] for key in value}, value)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('Invalid safe diagnostic', result.stderr)
                    self.assertNotIn('PRIVATE_TOKEN', result.stdout + result.stderr)
                    self.assertFalse((root / 'summary.json').exists())

    def test_scenario_detail_and_crash_presence(self):
        base = {'phase': 'scenario', 'failure_kind': 'timeout', 'command_exit_code': None}
        cases = [('android', {'scenario': s, 'detail': None, 'android_crash': None}, True)
                 for s in ('m2', 'm3', 'm3-auth', 'm4', 'm6')]
        cases += [('android', {'scenario': 'm4', 'detail': d, 'android_crash': None}, True)
                  for d in ('Message scenario deadline: message_state', 'Android owned activity exited',
                            'participant disconnected', 'Account scenario: SAS mismatch',
                            'RPC failed: ContactRequired', 'Future not completed')]
        cases += [('android', {'scenario': 'm3', 'detail': None, 'android_crash': c}, True)
                  for c in (True, False)]
        cases += [('windows', {'scenario': 'm3', 'detail': None, 'android_crash': True}, False),
                  ('windows', {'scenario': 'm3', 'detail': None, 'android_crash': False}, False),
                  ('windows', {'scenario': 'm3', 'detail': None, 'android_crash': None}, True)]
        for profile, extra, accepted in cases:
            with self.subTest(profile=profile, extra=extra), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                diagnostic = root / 'diagnostic.json'
                value = {**base, **extra}
                diagnostic.write_text(json.dumps(value))
                result = self.call(root, '--profile', profile, '--stage', 'scenario',
                                   '--outcome', 'failure', '--exit-code', '1',
                                   '--diagnostic-file', str(diagnostic))
                if accepted:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    summary = json.loads((root / 'summary.json').read_text())
                    self.assertEqual({key: summary[key] for key in value}, value)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse((root / 'summary.json').exists())

    def test_detail_list_is_fixed_public_text(self):
        self.assertEqual(DETAILS, sorted(set(DETAILS)))
        for detail in DETAILS:
            self.assertRegex(detail, r'^[A-Za-z0-9 _:.;,=()-]{1,100}$')

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
