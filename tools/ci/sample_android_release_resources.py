#!/usr/bin/env python3
"""Numeric, best-effort Linux snapshots for the ordinary Android Release step."""
import errno
import json
import math
import os
from pathlib import Path, PurePosixPath
import time

UNAVAILABLE = (OSError, ValueError, IndexError, StopIteration, UnicodeError)


def field(text, key):
    return next(line.split()[1] for line in text.splitlines()
                if line.split() and line.split()[0].rstrip(':') == key)


def number(path, key=None):
    try:
        text = path.read_bytes().decode()
        value = int(field(text, key) if key else text)
        return value if value >= 0 else -1
    except UNAVAILABLE:
        return -1


def owned(proc, marker):
    if not marker:
        return -1, -1
    rss = ticks = count = 0
    try:
        for process in proc.iterdir():
            if not process.name.isdecimal():
                continue
            try:
                status = (process / 'status').read_bytes().decode()
                if int(field(status, 'Uid')) != os.getuid():
                    continue
                if b'GRADLE_USER_HOME=' + os.fsencode(marker) not in (process / 'environ').read_bytes().split(b'\0'):
                    continue
                # Ignore the parenthesized comm field; fields 14/15 are live utime/stime.
                stat = (process / 'stat').read_bytes().rsplit(b')', 1)[1].split()
                resident = int(field(status, 'VmRSS'))
                cpu = int(stat[11]) + int(stat[12])
                if resident < 0 or cpu < 0:
                    return -1, -1
                rss += resident
                ticks += cpu
                count += 1
            except OSError as error:
                if error.errno not in (errno.ENOENT, errno.ESRCH):
                    return -1, -1
    except UNAVAILABLE:
        return -1, -1
    return (rss, ticks) if count else (-1, -1)


def sample(elapsed, previous=None, proc=Path('/proc'), cgroup=Path('/sys/fs/cgroup')):
    rss, ticks = owned(proc, os.environ.get('GRADLE_USER_HOME'))
    result = {
        'elapsed_s': elapsed,
        'owned_rss_kib': rss,
        'owned_live_cpu_ticks': ticks,
        'host_cpu_pct': -1,
        'mem_available_kib': number(proc / 'meminfo', 'MemAvailable'),
        'swap_used_kib': -1,
        'cpu_psi_some_avg10': -1,
        'memory_psi_some_avg10': -1,
        'io_psi_some_avg10': -1,
        'cgroup_memory_current_bytes': -1,
        'cgroup_memory_max_bytes': -1,
        'cgroup_oom': -1,
        'cgroup_oom_kill': -1,
    }
    total, free = (number(proc / 'meminfo', key) for key in ('SwapTotal', 'SwapFree'))
    if total >= free >= 0:
        result['swap_used_kib'] = total - free
    current = None
    try:
        counters = [int(value) for value in (proc / 'stat').read_bytes().splitlines()[0].split()[1:9]]
        if len(counters) == 8 and min(counters) >= 0:
            current = sum(counters), counters[3] + counters[4]
            if previous:
                delta, idle = current[0] - previous[0], current[1] - previous[1]
                if delta > 0 and 0 <= idle <= delta:
                    result['host_cpu_pct'] = 100 * (delta - idle) / delta
    except UNAVAILABLE:
        pass
    for kind in ('cpu', 'memory', 'io'):
        try:
            text = (proc / 'pressure' / kind).read_bytes().decode()
            some = next(line for line in text.splitlines() if line.startswith('some '))
            value = float(next(part.split('=', 1)[1] for part in some.split() if part.startswith('avg10=')))
            if math.isfinite(value) and value >= 0:
                result[kind + '_psi_some_avg10'] = value
        except UNAVAILABLE:
            pass
    try:
        text = (proc / 'self/cgroup').read_bytes().decode()
        relative = PurePosixPath(next(line[3:] for line in text.splitlines() if line.startswith('0::')))
        if relative.is_absolute() and '..' not in relative.parts:
            group = cgroup.joinpath(*relative.parts[1:])
            result['cgroup_memory_current_bytes'] = number(group / 'memory.current')
            result['cgroup_memory_max_bytes'] = number(group / 'memory.max')
            result['cgroup_oom'] = number(group / 'memory.events', 'oom')
            result['cgroup_oom_kill'] = number(group / 'memory.events', 'oom_kill')
    except UNAVAILABLE:
        pass
    return result, current


def emit(result):
    print('ANDROID_RELEASE_RESOURCE ' + json.dumps(result, separators=(',', ':')), flush=True)


if __name__ == '__main__':
    started, previous = time.monotonic(), None
    while True:
        result, previous = sample(round(time.monotonic() - started, 3), previous)
        emit(result)
        time.sleep(30)
