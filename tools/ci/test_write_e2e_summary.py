import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name('write_e2e_summary.py')
DETAILS = json.loads((SCRIPT.parents[1] / 'e2e' / 'diagnostic_details.json').read_text(encoding='utf-8'))['details']
SCENARIO_KEYS = {'detail': None, 'scenario': 'm2', 'android_crash': None,
                 'last_view': None, 'participant': None, 'rpc_op': None, 'facts': None, 'android': None,
                 'waits': None}


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
                'schema_version': 9, 'profile': 'android', 'stage': 'scenario', 'outcome': 'failure',
                'exitCode': 17, 'peak_rss_kib': 1234,
                'phase': None, 'failure_kind': None, 'command_exit_code': None,
                'detail': None, 'scenario': None, 'android_crash': None,
                'last_view': None, 'participant': None, 'rpc_op': None, 'facts': None, 'android': None,
                'waits': None})

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
            self.assertEqual(len(summary), 18)

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
                    self.assertEqual(len(summary), 18)
                    self.assertEqual(summary['schema_version'], 9)
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
                    self.assertEqual(summary['schema_version'], 9)
                    self.assertEqual({key: summary[key] for key in value}, value)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('Invalid safe diagnostic', result.stderr)
                    self.assertNotIn('PRIVATE_TOKEN', result.stdout + result.stderr)
                    self.assertFalse((root / 'summary.json').exists())

    def test_scenario_detail_and_crash_presence(self):
        base = {'phase': 'scenario', 'failure_kind': 'timeout', 'command_exit_code': None,
                'last_view': None, 'participant': None, 'rpc_op': None, 'facts': None, 'android': None,
                'waits': None}
        cases = [('android', {'scenario': s, 'detail': None, 'android_crash': None}, True)
                 for s in ('m2', 'm3', 'm3-auth', 'm4', 'm6')]
        cases += [('android', {'scenario': 'm4', 'detail': d, 'android_crash': None}, True)
                  for d in ('Message scenario deadline: contact_blocked', 'Android owned activity exited',
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

    def test_deadline_view_and_android_state_are_fixed_values(self):
        probe = {'name': 'android-2', 'screen': 'asleep', 'top': 'systemui', 'process': 'alive_same',
                 'proc_state': 19, 'oom_adj': 900, 'frozen': True, 'exit_reason': 'FREEZER',
                 'exit_subreason': 0, 'exit_importance': 400, 'exit_owned': False,
                 'lifecycle': ['am_proc_start', 'wm_on_resume_called', 'screen_off', 'wm_on_stop_called'],
                 'config_masks': [536870912, 3], 'clock_skew_s': -3600, 'launch_clock_skew_s': 2,
                 'launch_ms_max': 31000, 'load_x100': 812, 'host_load_x100': 650,
                 'app_cpu_pct': 140, 'frames': 9000, 'cpus': 2,
                 'low_memory': False, 'lmk_killed_app': False, 'mem_total_mib': 1967,
                 'mem_available_mib': 300, 'screen_off_timeout_ms': 60000}
        unknown = {**{key: None for key in probe}, 'name': 'android-1', 'screen': 'unknown',
                   'top': 'unknown', 'process': 'unknown'}
        base = {'phase': 'scenario', 'failure_kind': 'timeout', 'command_exit_code': None,
                'detail': 'Account state deadline', 'scenario': 'm3', 'android_crash': False,
                'last_view': None, 'participant': None, 'rpc_op': None, 'facts': None, 'android': None,
                'waits': None}
        cases = [('android', {'last_view': v, 'participant': 'android-2'}, True)
                 for v in ('Unconfigured', 'Active', 'Locked', 'NotAvailable', 'other',
                           'Queued', 'Blocked', 'absent')]
        cases += [('android', {'rpc_op': op, 'participant': 'android-1'}, True)
                  for op in ('account_local_pair_scan', 'message_local_send_text',
                             'mailbox_local_sync_now')]
        cases += [('android', {'android': a}, True) for a in ([], [probe], [unknown, probe])]
        cases += [('android', {'facts': f}, True)
                  for f in ({'owner_pair': 'WaitingQr', 'scanner_phase': 'Candidate'},
                            {'owner_publication': 'Observed', 'scanner_pair': 'none'})]
        cases += [('windows', {'last_view': 'Active', 'participant': 'windows-1'}, True),
                  ('windows', {'android': [probe]}, False)]
        bad = [{'last_view': 'PRIVATE_TOKEN'}, {'last_view': 1}, {'participant': 'PRIVATE_TOKEN'},
               {'rpc_op': 'PRIVATE_TOKEN'}, {'rpc_op': 'account_local_PRIVATE_TOKEN'},
               {'rpc_op': 'network_info'}, {'rpc_op': 1},
               {'facts': {}}, {'facts': []}, {'facts': {'PRIVATE_TOKEN': 'Active'}},
               {'facts': {'owner_pair': 'PRIVATE_TOKEN'}}, {'facts': {'owner_pair': 1}},
               {'android': [{**probe, 'config_masks': [-1]}]},
               {'android': [{**probe, 'config_masks': [1] * 9}]},
               {'android': [{**probe, 'clock_skew_s': 1.5}]},
               {'android': [{**probe, 'app_cpu_pct': -1}]},
               {'android': [{**probe, 'launch_ms_max': True}]},
               {'android': [{**probe, 'cpus': 0}]}, {'android': [{**probe, 'cpus': True}]},
               {'android': {}}, {'android': [probe, probe, probe]}, {'android': ['PRIVATE_TOKEN']},
               {'android': [{**probe, 'extra': 1}]}, {'android': [{k: v for k, v in probe.items() if k != 'top'}]}]
        bad += [{'android': [{**probe, key: 'PRIVATE_TOKEN'}]} for key in probe]
        bad += [{'android': [{**probe, 'lifecycle': ['PRIVATE_TOKEN']}]},
                {'android': [{**probe, 'lifecycle': ['screen_on'] * 33}]},
                {'android': [{**probe, 'proc_state': True}]}, {'android': [{**probe, 'frozen': 1}]},
                {'android': [{**probe, 'mem_total_mib': -1}]}, {'android': [{**probe, 'oom_adj': 5000}]}]
        cases += [('android', patch, False) for patch in bad]
        for profile, extra, accepted in cases:
            with self.subTest(profile=profile, extra=extra), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                diagnostic = root / 'diagnostic.json'
                value = {**base, **extra}
                if profile == 'windows':
                    value['android_crash'] = None
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
                    self.assertNotIn('PRIVATE_TOKEN', result.stdout + result.stderr)
                    self.assertFalse((root / 'summary.json').exists())

    def test_waits_are_listed_names_with_bounded_times(self):
        base = {'phase': 'complete', 'failure_kind': None, 'command_exit_code': None,
                **SCENARIO_KEYS, 'scenario': 'm6'}
        good = [[{'name': 'Mailbox scenario deadline: first_text', 'ms': 41250},
                 {'name': 'account_local_pair_scan', 'ms': 9800},
                 {'name': 'Account state deadline', 'ms': 0}],
                [{'name': 'timeout_probe_event', 'ms': 15000}],
                [{'name': f'account_local_op_{chr(97 + i)}', 'ms': i} for i in range(8)],
                []]
        cases = [(w, True) for w in good]
        bad = [[{'name': 'PRIVATE_TOKEN', 'ms': 1}],
               [{'name': 'account_local_PRIVATE_TOKEN', 'ms': 1}],
               [{'name': 'Mailbox scenario deadline: PRIVATE_TOKEN', 'ms': 1}],
               [{'name': 'network_info', 'ms': 1}],
               [{'name': 'timeout', 'ms': 1}],
               [{'name': 'timeout_probe_event', 'ms': -1}],
               [{'name': 'timeout_probe_event', 'ms': 10_000_001}],
               [{'name': 'timeout_probe_event', 'ms': 1.5}],
               [{'name': 'timeout_probe_event', 'ms': True}],
               [{'name': 'timeout_probe_event', 'ms': 'PRIVATE_TOKEN'}],
               [{'name': 'timeout_probe_event'}],
               [{'name': 'timeout_probe_event', 'ms': 1, 'extra': 'PRIVATE_TOKEN'}],
               [{'name': 'timeout_probe_event', 'ms': 1}, {'name': 'timeout_probe_event', 'ms': 2}],
               [{'name': f'account_local_op_{chr(97 + i)}', 'ms': i} for i in range(9)],
               {}, 'PRIVATE_TOKEN', ['PRIVATE_TOKEN']]
        cases += [(w, False) for w in bad]
        for waits, accepted in cases:
            with self.subTest(waits=waits), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                diagnostic = root / 'diagnostic.json'
                value = {**base, 'waits': waits}
                diagnostic.write_text(json.dumps(value))
                result = self.call(root, '--profile', 'windows', '--stage', 'scenario',
                                   '--outcome', 'success', '--exit-code', '0',
                                   '--diagnostic-file', str(diagnostic))
                if accepted:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    summary = json.loads((root / 'summary.json').read_text())
                    self.assertEqual(summary['waits'], waits)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertNotIn('PRIVATE_TOKEN', result.stdout + result.stderr)
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
