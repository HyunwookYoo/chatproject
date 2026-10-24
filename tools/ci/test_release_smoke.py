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
import 'package:chat_e2e/src/owned_resources.dart';
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
  Fake(this.mode, this.cancel);
  final String mode;
  final RunCancellation cancel;
  final stops = <int>[];
  int next = 100;
  int polls = 0;
  bool launched = false;
  bool forceStopped = false;
  int identityReads = 0;
  int launches = 0;
  int appPid = 100;
  bool finalStopPending = false;
  int finalStopReads = 0;
  final forceStopLaunches = <int>[];
  String? contractRoot;
  Future<OwnedProcess> start(String exe, List<String> args, {required Directory logDir, String? workingDirectory, String? stdin, bool retainOutput = false}) async {
    final child = Child(next++, Completer<int>(), this);
    var output = '';
    if (mode.startsWith('android-')) {
      if (args.contains('pidof')) {
        if (finalStopPending) {
          finalStopReads++;
          if (mode == 'android-contract2-stop-delayed' && finalStopReads == 3) launched = false;
        }
        if (launched) {
          identityReads++;
          output = mode == 'android-cancel-foreign' && identityReads > 1 ? '999' : '${320 + launches}';
          if (mode == 'android-cancel-identity' || mode == 'android-cancel-foreign') cancel.cancel();
          if (finalStopPending && mode == 'android-contract2-stop-replacement') output = '999';
        }
      }
      if (args.contains('package')) output = 'android.permission.INTERNET android.permission.ACCESS_NETWORK_STATE${mode == 'android-backup' ? ' ALLOW_BACKUP' : ''}';
      if (args.contains('date')) output = '1790000000.123456789';
      if (args.contains('start')) {
        launches++;
        launched = true;
        if (mode == 'android-cancel-launch') cancel.cancel();
      }
      if (args.contains('force-stop')) {
        forceStopped = true;
        forceStopLaunches.add(launches);
        finalStopPending = launches == 3 && mode.startsWith('android-contract2-stop-');
        if (!finalStopPending) launched = false;
      }
      if (args.contains('activities')) output = mode == 'android-wrong-activity' ? 'topResumedActivity=ActivityRecord{other.app/.Other}' : 'topResumedActivity=ActivityRecord{dev.chatproject.chat_app/.MainActivity}';
      if (args.contains('uiautomator')) output = '<node package="dev.chatproject.chat_app" text="내 계정 시작하기 새 계정 만들기 복구 문구로 복구 다른 기기로 연결" />';
      if (args.contains('logcat')) {
        if (!args.contains('--pid=${320 + launches}') || !args.contains('-T') || !args.contains('1790000000.123456789')) throw StateError('unscoped log read');
        polls++;
        if (mode == 'android-late' && polls > 1) output = 'FATAL EXCEPTION: main';
        if (mode.startsWith('android-contract2')) {
          final phase = launches == 1 ? 'created' : launches == 2 ? 'reopened' : 'cleaned';
          output = jsonEncode({'type': 'release_contract', 'version': 2, 'mode': 'release',
            'nonce': 'fixture-nonce', 'hooks': phase == 'cleaned' ? 'notRun' : 'notAvailable',
            'network': 'stopped', 'endpoint': null,
            'phase': mode == 'android-contract2-cleanup' && launches == 3 ? 'reopened' : phase,
            'binding': base64Encode(List.filled(20, 7)), 'instance': 'android-$launches'});
        }
      }
      child.code.complete(0);
    } else if (exe.endsWith('chat_app.exe')) {
      appPid = child.pid;
      launches++;
      if (launches > 1 && !stops.contains(100)) throw StateError('first process still owned');
      if (mode == 'crash') child.code.complete(17);
      if (mode.startsWith('contract') && !mode.startsWith('contract2')) {
        output = jsonEncode({'type': 'release_contract', 'version': 1, 'mode': mode == 'contract-debug' ? 'debug' : 'release',
          'nonce': mode == 'contract-stale' ? 'old' : 'fixture-nonce', 'hooks': 'notAvailable',
          'network': 'stopped', 'endpoint': mode == 'contract-endpoint' ? 'forbidden' : null});
        if (mode == 'contract-partial') output = '{"type":"release_contract"}';
        if (mode == 'contract-marker-only') output = 'Release marker check PASS';
      }
      if (mode.startsWith('contract2')) {
        contractRoot = '${args[1]}/release-contract-fixture-nonce';
        final root = Directory(contractRoot!);
        await root.create(recursive: true);
        final binding = base64Encode(List.filled(20, 7));
        final phase = launches == 1 ? 'created' : 'reopened';
        for (final name in ['db.keys', 'db.sqlite3', 'store.id']) {
          await File('${root.path}/$name').writeAsBytes([1]);
        }
        await File('${root.path}/storage.mode').writeAsBytes(List.filled(20, 7));
        await File('${root.path}/owner.json').writeAsString(jsonEncode({
          'nonce': mode == 'contract2-owner' ? 'other' : 'fixture-nonce',
          'phase': phase, 'binding': binding, 'instance': 'instance-$launches',
        }));
        if (mode == 'contract2-malformed') await File('${root.path}/owner.json').writeAsString('public-malformed-owner-sentinel');
        if (mode == 'contract2-unexpected') await File('${root.path}/unowned.txt').writeAsString('preserve');
        if (mode == 'contract2-missing') await File('${root.path}/db.keys').delete();
        output = jsonEncode({'type': 'release_contract', 'version': 2, 'mode': 'release',
          'nonce': 'fixture-nonce', 'hooks': 'notAvailable', 'network': 'stopped', 'endpoint': null,
          'phase': mode == 'contract2-phase' ? 'reopened' : phase,
          'binding': mode == 'contract2-binding' && launches == 2 ? base64Encode(List.filled(20, 8)) : binding,
          'instance': mode == 'contract2-instance' ? 'same' : 'instance-$launches'});
      }
    } else {
      polls++;
      output = jsonEncode({'pid': mode == 'foreign' ? 999 : appPid, 'window': mode == 'no-window' ? 0 : 123,
        'texts': mode == 'pid-only' || mode == 'cleanup' ? <String>[] : ['내 계정 시작하기', '새 계정 만들기', '복구 문구로 복구', '다른 기기로 연결']});
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
  final cancel = RunCancellation();
  final fake = Fake(args[0], cancel);
  var result = 'PASS';
  try {
    await runSmoke(platform: args[0].startsWith('android-') ? 'android' : 'windows',
      artifact: args[0].startsWith('android-') ? '${args[2]}/chat_app.exe' : args[2], serial: 'emulator-5554', logs: logs, runner: fake,
      cancellation: cancel,
      contractNonce: args[0].contains('contract') ? 'fixture-nonce' : null,
      readyTimeout: const Duration(milliseconds: 80), settleTime: const Duration(milliseconds: 30),
      pollInterval: const Duration(milliseconds: 5));
  } catch (error) { result = 'FAIL: $error'; }
  final phaseLogs = fake.launches == 3 ? 'cleaned' : fake.launches == 2 ? 'reopened' : 'created';
  final ownershipFile = File(args[0].contains('contract') ? '${logs.path}/$phaseLogs/ownership.json' : '${logs.path}/ownership.json');
  print(jsonEncode({'result': result, 'stops': fake.stops, 'forceStopped': fake.forceStopped,
    'finalStopReads': fake.finalStopReads, 'forceStopLaunches': fake.forceStopLaunches,
    'contractSuccess': File('${logs.path}/contract-success.json').existsSync(),
    'launches': fake.launches, 'rootRetained': fake.contractRoot != null && Directory(fake.contractRoot!).existsSync(),
    'ownership': jsonDecode(ownershipFile.readAsStringSync())}));
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

    def test_old_contract_without_sql_reopen_evidence_is_rejected(self):
        self.probe("contract", False)

    def test_actual_contract_proof_is_distinct_from_marker_check(self):
        data = self.probe("contract2", True)
        self.assertEqual(data["launches"], 2)
        self.assertFalse(data["rootRetained"])
        for mode in ("contract-stale", "contract-debug", "contract-partial", "contract-endpoint", "contract-marker-only"):
            with self.subTest(mode=mode):
                self.probe(mode, False)

    def test_sql_contract_phase_identity_binding_and_cleanup_fail_closed(self):
        for mode in ('contract2-phase', 'contract2-instance', 'contract2-binding',
                     'contract2-owner', 'contract2-unexpected', 'contract2-missing'):
            with self.subTest(mode=mode):
                data = self.probe(mode, False)
                self.assertTrue(data['rootRetained'])
                self.assertEqual(data['ownership']['processes'], [])

    def test_android_current_activity_and_pid_scoped_log_cleanup(self):
        for mode in ('android-ready', 'android-wrong-activity', 'android-late'):
            with self.subTest(mode=mode):
                data = self.probe(mode, mode == 'android-ready')
                self.assertTrue(data['forceStopped'])

    def test_malformed_owner_error_does_not_echo_file_contents(self):
        data = self.probe('contract2-malformed', False)
        self.assertFalse('public-malformed-owner-sentinel' in data['result'])
        self.assertTrue(data['rootRetained'])

    def test_android_contract_requires_third_cleanup_only_launch(self):
        data = self.probe('android-contract2', True)
        self.assertEqual(data['launches'], 3)
        self.assertTrue(data['forceStopped'])
        data = self.probe('android-contract2-cleanup', False)
        self.assertEqual(data['launches'], 3)
        self.assertEqual(data['ownership']['activities'], [])

    def test_android_release_rejects_backup_enabled_manifest(self):
        self.probe('android-backup', False)

    def test_android_final_stop_requires_observed_absence(self):
        data = self.probe('android-contract2-stop-persistent', False)
        self.assertFalse(data['contractSuccess'])
        self.assertGreater(data['finalStopReads'], 1)
        self.assertEqual(data['ownership']['activities'][0]['pid'], '323')
        self.assertEqual(data['forceStopLaunches'], [1, 2, 3])

    def test_android_final_stop_waits_for_delayed_absence(self):
        data = self.probe('android-contract2-stop-delayed', True)
        self.assertTrue(data['contractSuccess'])
        self.assertEqual(data['finalStopReads'], 3)
        self.assertEqual(data['ownership']['activities'], [])
        self.assertEqual(data['forceStopLaunches'], [1, 2, 3])

    def test_android_final_stop_preserves_replacement(self):
        data = self.probe('android-contract2-stop-replacement', False)
        self.assertFalse(data['contractSuccess'])
        self.assertEqual(data['finalStopReads'], 1)
        self.assertEqual(data['ownership']['activities'][0]['pid'], '323')
        self.assertEqual(data['forceStopLaunches'], [1, 2, 3])

    def test_android_cancellation_drains_launch_and_identity_acquisition(self):
        for mode in ('android-cancel-launch', 'android-cancel-identity'):
            with self.subTest(mode=mode):
                data = self.probe(mode, False)
                self.assertIn('RunCancelled', data['result'])
                self.assertTrue(data['forceStopped'], data)
                self.assertEqual(data['ownership']['activities'], [], data)
                self.assertEqual(data['ownership']['processes'], [], data)

    def test_android_cancellation_preserves_replacement_process(self):
        data = self.probe('android-cancel-foreign', False)
        self.assertIn('RunCancelled', data['result'])
        self.assertFalse(data['forceStopped'], data)
        self.assertEqual(data['ownership']['activities'][0]['pid'], '321', data)


if __name__ == "__main__":
    unittest.main()
