#!/usr/bin/env python3
"""Write the only E2E artifact allowed off the runner: an explicit fixed schema."""
import argparse
import json
import pathlib
import re

PHASES = set('preflight build_node build_windows build_android gradle_stop relay_ready '
             'controller_start windows_start windows_participant sdk_preflight avd_preflight '
             'avd_create avd_boot adb_reverse android_install android_launch android_pid '
             'android_participant node_start node_participant scenario cleanup complete '
             'scenario_app1 scenario_app2 scenario_node scenario_app1_to_app2 '
             'scenario_app1_to_node scenario_app2_to_app1 scenario_app2_to_node '
             'scenario_node_to_app1 scenario_node_to_app2 android_pid_monitor'.split())
FAILURE_KINDS = set('cleanup command_exit timeout cancelled socket filesystem process_spawn '
                    'state argument unsupported format other avd_list_exit avd_create_exit '
                    'avd_devices_exit avd_console_exit avd_console_invalid avd_serial_conflict '
                    'avd_config_missing avd_config_mismatch avd_snapshot_exit avd_snapshot_missing '
                    'owned_relay_exit owned_emulator_exit owned_chat_node_exit owned_windows_exit '
                    'owned_child_exit timeout_network_start timeout_network_info timeout_connect_peer '
                    'timeout_send_probe_text timeout_set_offline timeout_disconnect_peer '
                    'timeout_network_stop timeout_state_event timeout_probe_event timeout_offline_event'.split())
SCENARIOS = {'m2', 'm3', 'm3-auth', 'm4', 'm6'}
# Fixed public failure texts, shared with tools/e2e/lib/src/diagnostic.dart.
DETAILS = frozenset(json.loads(
    (pathlib.Path(__file__).resolve().parents[1] / 'e2e' / 'diagnostic_details.json')
    .read_text(encoding='utf-8'))['details'])
EXIT_KINDS = set('command_exit avd_list_exit avd_create_exit avd_devices_exit avd_console_exit '
                 'avd_snapshot_exit owned_relay_exit owned_emulator_exit owned_chat_node_exit '
                 'owned_windows_exit owned_child_exit'.split())


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--profile', choices=['android', 'windows'], required=True)
    parser.add_argument('--stage', choices=['preparation', 'scenario'], required=True)
    parser.add_argument('--outcome', choices=['success', 'failure', 'cancelled'], required=True)
    parser.add_argument('--exit-code', type=int, required=True)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--memory-file', type=pathlib.Path)
    parser.add_argument('--diagnostic-file', type=pathlib.Path)
    args = parser.parse_args()
    if (args.outcome == 'success') != (args.exit_code == 0):
        parser.error('outcome and exit code disagree')
    peak = None
    if args.memory_file and args.memory_file.is_file():
        for line in args.memory_file.read_text(encoding='utf-8', errors='replace').splitlines():
            match = re.fullmatch(r'\s*Maximum resident set size \(kbytes\): (\d+)\s*', line)
            if match:
                peak = int(match[1])
    safe = {'phase': None, 'failure_kind': None, 'command_exit_code': None,
            'detail': None, 'scenario': None, 'android_crash': None}
    if args.diagnostic_file and args.diagnostic_file.is_file():
        try:
            diagnostic = json.loads(args.diagnostic_file.read_text(encoding='utf-8'))
            if not isinstance(diagnostic, dict) or set(diagnostic) != set(safe):
                raise ValueError()
            phase, kind, code, detail, scenario, crash = (diagnostic[key] for key in safe)
            if not isinstance(phase, str) or phase not in PHASES:
                raise ValueError()
            if kind is not None and (not isinstance(kind, str) or kind not in FAILURE_KINDS):
                raise ValueError()
            if code is not None and type(code) is not int:  # bool is not an exit code.
                raise ValueError()
            if (kind in EXIT_KINDS) != (code is not None):
                raise ValueError()
            if detail is not None and (not isinstance(detail, str) or detail not in DETAILS):
                raise ValueError()
            if not isinstance(scenario, str) or scenario not in SCENARIOS:
                raise ValueError()
            # Presence of Android crash-buffer entries only, never their text.
            if crash is not None and (type(crash) is not bool or args.profile != 'android'):
                raise ValueError()
            safe = {'phase': phase, 'failure_kind': kind, 'command_exit_code': code,
                    'detail': detail, 'scenario': scenario, 'android_crash': crash}
        except (OSError, ValueError):
            parser.error('Invalid safe diagnostic')
    # Reconstruct only fixed keys and validated values; never copy log/error text.
    value = {'schema_version': 4, 'profile': args.profile, 'stage': args.stage,
             'outcome': args.outcome, 'exitCode': args.exit_code, 'peak_rss_kib': peak,
             **safe}
    args.output.write_text(json.dumps(value, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
