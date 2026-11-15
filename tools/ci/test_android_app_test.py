import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
BASH = "C:/Program Files/Git/bin/bash.exe" if os.name == "nt" else shutil.which("bash")

# `flutter` sleeps FLUTTER_SLEEP seconds and exits FLUTTER_EXIT; `adb` answers the queries the
# script makes and records each one.
FAKE_FLUTTER = '#!/usr/bin/env bash\necho "flutter $*"\nsleep "${FLUTTER_SLEEP:-0}"\nexit "${FLUTTER_EXIT:-0}"\n'
FAKE_ADB = r'''#!/usr/bin/env bash
echo "$*" >> "$ADB_LOG"
case "$*" in
  *"shell pidof"*) [ -n "${APP_PID:-}" ] && echo "$APP_PID" ;;
  *"/proc/"*) printf 'Name:\tchat_app\nState:\tS (sleeping)\nThreads:\t40\nSigQ:\t0/1\nVmRSS:\t1 kB\n' ;;
  *"activity processes"*) echo "*APP* UID 10190 ProcessRecord{1 4242:dev.chatproject.chat_app/u0a190}" ;;
  *"activity activities"*) echo "  topResumedActivity=ActivityRecord{1 u0 dev.chatproject.chat_app/.MainActivity t1}"; echo "  other" ;;
  *"--pid="*) for i in $(seq 400); do echo "pid line $i"; done ;;
  *"ActivityManager"*) echo "I ActivityManager: Start proc 4242:dev.chatproject.chat_app"; echo "I ActivityManager: Start proc 1:other.app" ;;
  *logcat*) echo "E AndroidRuntime: FATAL EXCEPTION" ;;
esac
'''


@unittest.skipUnless(BASH and pathlib.Path(BASH).is_file(), "requires native bash")
class AndroidAppTestScript(unittest.TestCase):
    def run_script(self, *args, flutter_exit=0, flutter_sleep=0, bound=None, pid="4242"):
        with tempfile.TemporaryDirectory(prefix="android app test ") as folder:
            root = pathlib.Path(folder)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            for tool, body in (("flutter", FAKE_FLUTTER), ("adb", FAKE_ADB)):
                path = fake_bin / tool
                path.write_text(body, encoding="utf-8", newline="\n")
                path.chmod(0o755)
            log = root / "adb.log"
            env = dict(os.environ, FLUTTER_EXIT=str(flutter_exit), FLUTTER_SLEEP=str(flutter_sleep),
                       APP_PID=pid, ADB_LOG=log.as_posix(), FIXTURE_BIN=str(fake_bin),
                       FIXTURE_SCRIPT=(ROOT / "tools/ci/android_app_test.sh").as_posix())
            if bound is not None:
                env["APP_TEST_TIMEOUT"] = str(bound)
            prefix = '$(cygpath -u "$FIXTURE_BIN")' if os.name == "nt" else '$FIXTURE_BIN'
            command = 'export PATH="' + prefix + ':$PATH"; exec bash "$FIXTURE_SCRIPT" "$@"'
            result = subprocess.run([BASH, "-c", command, "fixture", *args], env=env, capture_output=True, text=True)
            queries = log.read_text(encoding="utf-8") if log.exists() else ""
            return result, queries

    def test_a_passing_run_prints_no_app_state(self):
        result, queries = self.run_script("integration_test/core_test.dart", "--no-uninstall")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("flutter test integration_test/core_test.dart --no-uninstall -d emulator-5554", result.stdout)
        self.assertEqual(queries, "")

    def test_a_failing_run_keeps_its_exit_code_and_prints_capped_app_state(self):
        result, queries = self.run_script("integration_test/core_test.dart", flutter_exit=3)
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        out = result.stdout
        self.assertIn("::error::integration_test/core_test.dart failed (exit 3)", out)
        self.assertIn("== pid: 4242", out)
        self.assertIn("State:\tS (sleeping)", out)
        self.assertNotIn("VmRSS", out)
        self.assertIn("topResumedActivity=", out)
        self.assertNotIn("  other", out)
        # The app's own log is capped at its last 300 lines.
        self.assertNotIn("pid line 100\n", out)
        self.assertIn("pid line 101\n", out)
        self.assertIn("pid line 400\n", out)
        self.assertIn("Start proc 4242:dev.chatproject.chat_app", out)
        self.assertNotIn("other.app", out)
        self.assertIn("logcat -d -v threadtime --pid=4242", queries)
        self.assertIn("dumpsys activity processes dev.chatproject.chat_app", queries)

    def test_a_run_past_its_bound_is_stopped_and_reported(self):
        result, _ = self.run_script("integration_test/message_test.dart", flutter_sleep=30, bound=1)
        self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
        self.assertIn("::error::integration_test/message_test.dart did not finish within 1 s (exit 124)", result.stdout)

    def test_no_app_process_skips_the_per_pid_queries(self):
        result, queries = self.run_script("integration_test/core_test.dart", flutter_exit=1, pid="")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("== pid: none", result.stdout)
        self.assertNotIn("--pid=", queries)
        self.assertNotIn("/proc/", queries)


if __name__ == "__main__":
    unittest.main()
