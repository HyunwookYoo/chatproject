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
SHARED = json.loads(
    (pathlib.Path(__file__).resolve().parents[1] / 'e2e' / 'diagnostic_details.json')
    .read_text(encoding='utf-8'))
DETAILS = frozenset(SHARED['details'])
LAST_VIEWS = frozenset(SHARED['last_view'])
PARTICIPANTS = frozenset(SHARED['participants'])
# Facts a scenario noted about a failure's state: fixed keys -> fixed values.
FACTS = {key: frozenset(values) for key, values in SHARED['facts'].items()}
# One owned emulator's state (tools/e2e/lib/src/android_probe.dart): key -> check.
ANDROID = SHARED['android']


def _int(low, high):
    return lambda v: v is None or (type(v) is int and low <= v <= high)


def _bool(v):
    return v is None or type(v) is bool


def _one_of(values, nullable=False):
    return lambda v: (nullable and v is None) or (isinstance(v, str) and v in values)


def _masks(v):
    return v is None or (isinstance(v, list) and len(v) <= ANDROID['config_limit']
                         and all(type(m) is int and 0 <= m < 1 << 31 for m in v))


def _lifecycle(v):
    return v is None or (isinstance(v, list) and len(v) <= ANDROID['lifecycle_limit']
                         and all(isinstance(t, str) and t in ANDROID['lifecycle'] for t in v))


PROBE = {
    'name': _one_of({'android-1', 'android-2'}),
    'screen': _one_of(ANDROID['screen']),
    'top': _one_of(ANDROID['top']),
    'process': _one_of(ANDROID['process']),
    'proc_state': _int(0, 30),
    'oom_adj': _int(-1000, 1001),
    'frozen': _bool,
    'exit_reason': _one_of(ANDROID['exit_reason'], nullable=True),
    'exit_subreason': _int(0, 1000),
    'exit_importance': _int(0, 1000),
    'exit_owned': _bool,
    'lifecycle': _lifecycle,
    'config_masks': _masks,
    'low_memory': _bool,
    'lmk_killed_app': _bool,
    'mem_total_mib': _int(0, 1 << 20),
    'mem_available_mib': _int(0, 1 << 20),
    'screen_off_timeout_ms': _int(0, (1 << 31) - 1),
    'clock_skew_s': _int(-(1 << 31), (1 << 31) - 1),
    'launch_clock_skew_s': _int(-(1 << 31), (1 << 31) - 1),
    'launch_ms_max': _int(0, 10_000_000),
    'load_x100': _int(0, 1_000_000),
    'host_load_x100': _int(0, 1_000_000),
    'app_cpu_pct': _int(0, 10_000),
    'frames': _int(0, (1 << 31) - 1),
    'cpus': _int(1, 1024),
}
# The command an RPC failed on: a fixed scenario op name, never its arguments.
RPC_OP = re.compile(r'(?:account|message|mailbox)_local_[a-z_]{1,48}')


# m2's own waits, named by their timeout kind.
WAIT_KINDS = frozenset(SHARED['wait_kinds'])
WAITS_LIMIT = 8


def _waits(value):
    """The slowest waits: listed names (a deadline text, a scenario op, an m2 timeout kind), once each."""
    return value is None or (
        isinstance(value, list) and len(value) <= WAITS_LIMIT
        and all(isinstance(w, dict) and set(w) == {'name', 'ms'} and isinstance(w['name'], str)
                and (w['name'] in DETAILS or w['name'] in WAIT_KINDS or RPC_OP.fullmatch(w['name']))
                and type(w['ms']) is int and 0 <= w['ms'] <= 10_000_000 for w in value)
        and len({w['name'] for w in value}) == len(value))


def _facts(value):
    return value is None or (isinstance(value, dict) and value
                             and all(key in FACTS and isinstance(v, str) and v in FACTS[key]
                                     for key, v in value.items()))


def _android(value, profile):
    if value is None:
        return True
    return (profile == 'android' and isinstance(value, list) and len(value) <= 2
            and all(isinstance(probe, dict) and set(probe) == set(PROBE)
                    and all(check(probe[key]) for key, check in PROBE.items())
                    for probe in value))
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
            'detail': None, 'scenario': None, 'android_crash': None,
            'last_view': None, 'participant': None, 'rpc_op': None, 'facts': None,
            'android': None, 'waits': None}
    if args.diagnostic_file and args.diagnostic_file.is_file():
        try:
            diagnostic = json.loads(args.diagnostic_file.read_text(encoding='utf-8'))
            if not isinstance(diagnostic, dict) or set(diagnostic) != set(safe):
                raise ValueError()
            (phase, kind, code, detail, scenario, crash, view, participant, op, facts,
             android, waits) = (diagnostic[key] for key in safe)
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
            # A scenario deadline's last read and participant, as fixed values.
            if view is not None and (not isinstance(view, str) or view not in LAST_VIEWS):
                raise ValueError()
            if participant is not None and (not isinstance(participant, str)
                                            or participant not in PARTICIPANTS):
                raise ValueError()
            if op is not None and (not isinstance(op, str) or not RPC_OP.fullmatch(op)):
                raise ValueError()
            if not _facts(facts):
                raise ValueError()
            if not _android(android, args.profile):
                raise ValueError()
            if not _waits(waits):
                raise ValueError()
            safe = {'phase': phase, 'failure_kind': kind, 'command_exit_code': code,
                    'detail': detail, 'scenario': scenario, 'android_crash': crash,
                    'last_view': view, 'participant': participant, 'rpc_op': op,
                    'facts': facts, 'android': android, 'waits': waits}
        except (OSError, ValueError):
            parser.error('Invalid safe diagnostic')
    # Reconstruct only fixed keys and validated values; never copy log/error text.
    value = {'schema_version': 9, 'profile': args.profile, 'stage': args.stage,
             'outcome': args.outcome, 'exitCode': args.exit_code, 'peak_rss_kib': peak,
             **safe}
    args.output.write_text(json.dumps(value, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
