#!/usr/bin/env python3
"""Write the only E2E artifact allowed off the runner: an explicit fixed schema."""
import argparse
import json
import pathlib
import re


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--profile', choices=['android', 'windows'], required=True)
    parser.add_argument('--stage', choices=['preparation', 'scenario'], required=True)
    parser.add_argument('--outcome', choices=['success', 'failure', 'cancelled'], required=True)
    parser.add_argument('--exit-code', type=int, required=True)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--memory-file', type=pathlib.Path)
    args = parser.parse_args()
    if (args.outcome == 'success') != (args.exit_code == 0):
        parser.error('outcome and exit code disagree')
    peak = None
    if args.memory_file and args.memory_file.is_file():
        for line in args.memory_file.read_text(encoding='utf-8', errors='replace').splitlines():
            match = re.fullmatch(r'\s*Maximum resident set size \(kbytes\): (\d+)\s*', line)
            if match:
                peak = int(match[1])
    # Never copy command lines, paths, log messages, tickets, or error text.
    value = {'schema_version': 1, 'profile': args.profile, 'stage': args.stage,
             'outcome': args.outcome, 'exitCode': args.exit_code, 'peak_rss_kib': peak}
    args.output.write_text(json.dumps(value, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
