import pathlib
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
DART = shutil.which("dart")
PROBE = r'''
import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'package:chat_e2e/src/process_runner.dart';
import 'SMOKE_URI';

class Child implements OwnedProcess {
  Child(this.pid, this.code, this.owner);
  final int pid;
  final Completer<int> code;
  final Fake owner;
  bool stopped = false;
  Future<int> get exitCode => code.future;
  Future<void> stop() async {
    if (stopped) return;
    stopped = true;
    owner.stops.add(pid);
    if (!code.isCompleted) code.complete(0);
    if (owner.mode == 'cleanup' && pid == 100) throw StateError('fixture cleanup failure');
  }
}
class Fake implements ProcessRunner {
  Fake(this.mode);
  final String mode;
  final stops = <int>[];
  int next = 100;
  int polls = 0;
  bool launched = false;
  bool forceStopped = false;
  Future<OwnedProcess> start(String exe, List<String> args, {required Directory logDir, String? workingDirectory, String? stdin, bool retainOutput = false}) async {
    final child = Child(next++, Completer<int>(), this);
    var output = '';
    if (mode.startsWith('android-')) {
      if (args.contains('pidof')) output = launched ? '321' : '';
      if (args.contains('package')) output = 'android.permission.INTERNET android.permission.ACCESS_NETWORK_STATE';
      if (args.contains('date')) output = '1790000000.123456789';
      if (args.contains('start')) launched = true;
      if (args.contains('force-stop')) { forceStopped = true; launched = false; }
      if (args.contains('activities')) output = mode == 'android-wrong-activity' ? 'topResumedActivity=ActivityRecord{other.app/.Other}' : 'topResumedActivity=ActivityRecord{dev.chatproject.chat_app/.MainActivity}';
      if (args.contains('uiautomator')) output = '<node package="dev.chatproject.chat_app" text="대화 아직 대화가 없어요 연결 없음 코어 0.1.0" />';
      if (args.contains('logcat')) {
        if (!args.contains('--pid=321') || !args.contains('-T') || !args.contains('1790000000.123456789')) throw StateError('unscoped log read');
        polls++;
        if (mode == 'android-late' && polls > 1) output = 'FATAL EXCEPTION: main';
      }
      child.code.complete(0);
    } else if (child.pid == 100) {
      if (mode == 'crash') child.code.complete(17);
      if (mode.startsWith('contract')) {
        output = jsonEncode({'type': 'release_contract', 'version': 1, 'mode': mode == 'contract-debug' ? 'debug' : 'release',
          'nonce': mode == 'contract-stale' ? 'old' : 'fixture-nonce', 'hooks': 'notAvailable',
          'network': 'stopped', 'endpoint': mode == 'contract-endpoint' ? 'forbidden' : null});
        if (mode == 'contract-partial') output = '{"type":"release_contract"}';
        if (mode == 'contract-marker-only') output = 'Release marker check PASS';
      }
    } else {
      polls++;
      output = jsonEncode({'pid': mode == 'foreign' ? 999 : 100, 'window': mode == 'no-window' ? 0 : 123,
        'texts': mode == 'pid-only' || mode == 'cleanup' ? <String>[] : ['대화', '연결 없음', '아직 대화가 없어요', '코어 0.1.0']});
      if (mode == 'late' && polls >= 2) File('${logDir.path}/100.stderr.log').writeAsStringSync('Unhandled Exception: native failed');
      child.code.complete(0);
    }
    File('${logDir.path}/${child.pid}.stdout.log').writeAsStringSync(output);
    File('${logDir.path}/${child.pid}.stderr.log').writeAsStringSync('');
    return child;
  }
  dynamic noSuchMethod(Invocation i) => throw StateError('unexpected runner API: $i');
}
Future<void> main(List<String> args) async {
  final logs = Directory(args[1]);
  final fake = Fake(args[0]);
  var result = 'PASS';
  try {
    await runSmoke(platform: args[0].startsWith('android-') ? 'android' : 'windows',
      artifact: args[0].startsWith('android-') ? '${args[2]}/chat_app.exe' : args[2], serial: 'emulator-5554', logs: logs, runner: fake,
      contractNonce: args[0].startsWith('contract') ? 'fixture-nonce' : null,
      readyTimeout: const Duration(milliseconds: 80), settleTime: const Duration(milliseconds: 30),
      pollInterval: const Duration(milliseconds: 5));
  } catch (error) { result = 'FAIL: $error'; }
  print(jsonEncode({'result': result, 'stops': fake.stops, 'forceStopped': fake.forceStopped}));
}
'''


@unittest.skipUnless(DART, "requires Dart SDK; executed in Flutter CI job")
class ReleaseSmokeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        result = subprocess.run([DART, "pub", "get", "--enforce-lockfile"], cwd=ROOT / "tools/e2e", capture_output=True, text=True)
        if result.returncode:
            raise RuntimeError(result.stdout + result.stderr)

    def probe(self, mode, succeeds):
        import json
        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            (root / "chat_app.exe").write_bytes(b"fixture")
            script = root / "probe.dart"
            script.write_text(PROBE.replace("SMOKE_URI", (ROOT / "tools/ci/release_smoke.dart").as_uri()), encoding="utf-8")
            result = subprocess.run([DART, "--packages=" + str(ROOT / "tools/e2e/.dart_tool/package_config.json"),
                                     str(script), mode, str(root), str(root)], capture_output=True, text=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            data = json.loads(result.stdout.strip().splitlines()[-1])
            self.assertEqual(data["result"] == "PASS", succeeds, data)
            self.assertIn(100, data["stops"], data)
            return data

    def test_ready_home_and_native_version(self):
        self.probe("ready", True)

    def test_crash_late_exception_foreign_window_and_pid_only_fail(self):
        for mode in ("crash", "late", "foreign", "no-window", "pid-only"):
            with self.subTest(mode=mode):
                self.probe(mode, False)

    def test_primary_failure_survives_cleanup_failure(self):
        data = self.probe("cleanup", False)
        self.assertIn("readiness", data["result"])

    def test_actual_contract_proof_is_distinct_from_marker_check(self):
        self.probe("contract", True)
        for mode in ("contract-stale", "contract-debug", "contract-partial", "contract-endpoint", "contract-marker-only"):
            with self.subTest(mode=mode):
                self.probe(mode, False)

    def test_android_current_activity_and_pid_scoped_log_cleanup(self):
        for mode in ('android-ready', 'android-wrong-activity', 'android-late'):
            with self.subTest(mode=mode):
                data = self.probe(mode, mode == 'android-ready')
                self.assertTrue(data['forceStopped'])


if __name__ == "__main__":
    unittest.main()
