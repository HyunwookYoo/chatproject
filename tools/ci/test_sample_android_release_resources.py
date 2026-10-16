"""Focused fixtures for the CI-only Android Release resource observer."""
import contextlib
import errno
import importlib
import io
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
KEYS = {
    'elapsed_s', 'owned_rss_kib', 'owned_live_cpu_ticks', 'host_cpu_pct',
    'mem_available_kib', 'swap_used_kib', 'cpu_psi_some_avg10',
    'memory_psi_some_avg10', 'io_psi_some_avg10',
    'cgroup_memory_current_bytes', 'cgroup_memory_max_bytes',
    'cgroup_oom', 'cgroup_oom_kill',
}


class ResourceProbeTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue((ROOT / 'tools/ci/sample_android_release_resources.py').is_file(),
                        'The numeric CI resource observer is missing')
        self.probe = importlib.import_module('sample_android_release_resources')
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.proc = Path(self.folder.name) / 'proc'
        self.cgroup = Path(self.folder.name) / 'cgroup'
        self.proc.mkdir()
        self.cgroup.mkdir()
        self.marker = '/private/fixture-owned-marker'
        self.env = patch.dict(os.environ, GRADLE_USER_HOME=self.marker)
        self.env.start()
        self.addCleanup(self.env.stop)
        self.uid = patch.object(os, 'getuid', return_value=41, create=True)
        self.uid.start()
        self.addCleanup(self.uid.stop)

    def write(self, path, content):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content.encode() if isinstance(content, str) else content)

    def process(self, pid, uid=41, marker=None, rss=17, ticks=25):
        home = self.marker if marker is None else marker
        self.write(self.proc / str(pid) / 'status',
                   f'Name:\tjava-rustc-private\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nVmRSS:\t{rss} kB\n')
        self.write(self.proc / str(pid) / 'environ',
                   b'SECRET=fixture-secret\0GRADLE_USER_HOME=' + home.encode() + b'\0')
        # Fields 14/15 are utime/stime; the parenthesized name is deliberately tricky.
        self.write(self.proc / str(pid) / 'stat',
                   f'{pid} (private java ) rustc) S ' + '0 ' * 10 + f'{ticks} 0 ' + '0 ' * 9)

    def sample(self, previous=None):
        return self.probe.sample(0, previous, self.proc, self.cgroup)

    def test_exact_uid_and_nul_entry_ownership_keep_output_numeric_and_private(self):
        self.process(101)
        self.process(102, uid=0)
        self.process(103, marker=self.marker + '-suffix')
        self.process(104, marker='prefix-' + self.marker)
        self.process(105, marker='')
        real_read = Path.read_bytes

        def guarded_read(path):
            if path.name in ('cmdline', 'comm'):
                self.fail('Process names/command lines must not be inspected')
            if path.parent.name == '102' and path.name != 'status':
                self.fail('Foreign UID data must not be inspected')
            if path.parent.name in ('103', '104', '105') and path.name == 'stat':
                self.fail('Unowned process metrics must not be inspected')
            return real_read(path)

        with patch.object(Path, 'read_bytes', guarded_read):
            result, _ = self.sample()
        self.assertEqual(result['owned_rss_kib'], 17)
        self.assertEqual(result['owned_live_cpu_ticks'], 25)
        with contextlib.redirect_stdout(io.StringIO()) as output:
            self.probe.emit(result)
        line = output.getvalue()
        self.assertTrue(line.startswith('ANDROID_RELEASE_RESOURCE '))
        parsed = json.loads(line.split(' ', 1)[1])
        self.assertEqual(set(parsed), KEYS)
        self.assertTrue(all(type(value) in (int, float) for value in parsed.values()))
        for private in (self.marker, self.folder.name, 'fixture-secret', 'java', 'rustc',
                        'GRADLE_USER_HOME', '101', '102'):
            self.assertNotIn(private, line)

    def test_unknown_zero_and_disappeared_processes_are_distinguished(self):
        self.process(101, rss=0, ticks=0)
        self.process(102)
        real_read = Path.read_bytes
        for failure, expected in ((FileNotFoundError(errno.ENOENT, 'private'), 0),
                                  (ProcessLookupError(errno.ESRCH, 'private'), 0),
                                  (PermissionError(errno.EACCES, 'private'), -1)):
            for target in ('status', 'environ', 'stat'):
                with self.subTest(failure=type(failure).__name__, target=target):
                    def unavailable(path):
                        if path.parent.name == '102' and path.name == target:
                            raise failure
                        return real_read(path)
                    with patch.object(Path, 'read_bytes', unavailable):
                        with contextlib.redirect_stdout(io.StringIO()) as output:
                            result, _ = self.sample()
                    self.assertEqual(result['owned_rss_kib'], expected)
                    self.assertEqual(result['owned_live_cpu_ticks'], expected)
                    self.assertEqual(output.getvalue(), '')
                    self.assertTrue(all(result[key] == -1 for key in KEYS - {
                        'elapsed_s', 'owned_rss_kib', 'owned_live_cpu_ticks'}))
        self.write(self.proc / '102/stat', 'malformed')
        self.assertEqual(self.sample()[0]['owned_live_cpu_ticks'], -1)
        self.write(self.proc / 'meminfo', 'MemAvailable: 0 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n')
        for kind in ('cpu', 'memory', 'io'):
            self.write(self.proc / 'pressure' / kind, 'some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n')
        self.write(self.proc / 'self/cgroup', '0::/job\n')
        self.write(self.cgroup / 'job/memory.current', '0\n')
        self.write(self.cgroup / 'job/memory.max', 'max\n')
        self.write(self.cgroup / 'job/memory.events', 'oom 0\noom_kill 0\n')
        result, _ = self.sample()
        for key in KEYS - {'elapsed_s', 'owned_rss_kib', 'owned_live_cpu_ticks',
                           'host_cpu_pct', 'cgroup_memory_max_bytes'}:
            self.assertEqual(result[key], 0, key)
        self.assertEqual(result['cgroup_memory_max_bytes'], -1)
        with patch.dict(os.environ, GRADLE_USER_HOME=''):
            self.assertEqual(self.sample()[0]['owned_rss_kib'], -1)
        with patch.dict(os.environ, GRADLE_USER_HOME='no-owned-process'):
            self.assertEqual(self.sample()[0]['owned_live_cpu_ticks'], -1)
        for malformed in ('0::relative\n', '0::/../outside\n', '2:memory:/job\n'):
            self.write(self.proc / 'self/cgroup', malformed)
            self.write(self.cgroup / 'memory.current', '123\n')
            self.assertEqual(self.sample()[0]['cgroup_memory_current_bytes'], -1)

    def test_host_cpu_delta_and_owned_live_ticks_have_different_meanings(self):
        self.process(101, ticks=40)
        self.process(102, ticks=60)
        self.write(self.proc / 'stat', 'cpu 100 10 20 300 40 5 5 20 80 0\n')
        first, previous = self.sample()
        self.assertEqual(first['host_cpu_pct'], -1)
        self.assertEqual(first['owned_live_cpu_ticks'], 100)
        (self.proc / '102/status').unlink()
        self.write(self.proc / 'stat', 'cpu 120 10 30 330 60 5 5 40 90 0\n')
        second, previous = self.sample(previous)
        # Total delta=100; idle+iowait delta=50. Guest is already in user.
        self.assertEqual(second['host_cpu_pct'], 50)
        self.assertEqual(second['owned_live_cpu_ticks'], 40)
        self.assertEqual(self.sample(previous)[0]['host_cpu_pct'], -1)
        self.write(self.proc / 'stat', 'cpu 1 0 0 1 0 0 0 0 0 0\n')
        self.assertEqual(self.sample(previous)[0]['host_cpu_pct'], -1)

    @unittest.skipUnless(sys.platform.startswith('linux'),
                         'Actual YAML bash lifecycle requires native Linux /proc; Windows is not Linux')
    def test_actual_yaml_prelude_preserves_status_and_reaps_only_owned_sampler(self):
        import yaml
        workflow = yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text(encoding='utf-8'))
        body = next(step['run'] for step in workflow['jobs']['android']['steps']
                    if step.get('name') == 'Release APK')
        prelude = body.split('flutter pub get', 1)[0]
        self.assertIn('resource_probe_pid=$!', prelude)
        unrelated = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
        self.addCleanup(unrelated.wait)
        self.addCleanup(unrelated.terminate)
        for status, early_exit in ((23, False), (0, False), (23, True)):
            with self.subTest(status=status, early_exit=early_exit):
                # A FIFO lets the parent await the real first flushed sample before ending the body.
                fifo = Path(self.folder.name) / 'release-body'
                os.mkfifo(fifo)
                script = prelude + '\nprintf "%s\\n" "$resource_probe_pid" >&2\n'
                script += 'read -r finish < "$1"\n'
                if early_exit:
                    script += 'kill "$resource_probe_pid"; wait "$resource_probe_pid" || :\n'
                script += f'exit {status}\n'
                env = dict(os.environ, GRADLE_USER_HOME=self.marker)
                child = subprocess.Popen(['bash', '-e', '-c', script, 'probe-test', str(fifo)],
                                         cwd=ROOT / 'app', env=env, text=True,
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                pid = None
                try:
                    with selectors.DefaultSelector() as selector:
                        selector.register(child.stdout, selectors.EVENT_READ)
                        self.assertTrue(selector.select(timeout=10), 'No immediate flushed sample')
                    line = child.stdout.readline()
                    parsed = json.loads(line.removeprefix('ANDROID_RELEASE_RESOURCE '))
                    self.assertEqual(set(parsed), KEYS)
                    self.assertTrue(all(type(value) in (int, float) for value in parsed.values()))
                    pid = int(child.stderr.readline())
                    with fifo.open('w') as release:
                        release.write('finish\n')
                    stdout, stderr = child.communicate(timeout=10)
                    self.assertEqual(child.returncode, status, stderr)
                    self.assertIsNone(unrelated.poll(), 'Cleanup touched an unrelated process')
                    self.assertEqual(stderr, '')
                    self.assertNotIn('Traceback', stdout)
                    with self.assertRaises(ProcessLookupError):
                        os.kill(pid, 0)
                    pid = None
                finally:
                    if child.poll() is None:
                        child.kill()
                        child.communicate(timeout=10)
                    if pid is not None:
                        try:
                            os.kill(pid, 15)
                        except ProcessLookupError:
                            pass
                    fifo.unlink()


if __name__ == '__main__':
    unittest.main()
